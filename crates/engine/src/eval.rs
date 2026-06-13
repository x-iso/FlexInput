use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use glam::Vec2;
use flexinput_core::{Signal, SignalType, automap};
use serde_json::Value;

use crate::graph::{NodeSnap, ProcessingGraph};
use crate::state::NodeState;

// ── Public output type ────────────────────────────────────────────────────────

#[derive(Default)]
pub struct TickOutput {
    /// Latest output per (node_uid, output_pin). Excludes device.source (UI evaluates fresh).
    pub outputs: HashMap<(usize, usize), Option<Signal>>,
    /// Per display node: one scope sample for this tick (uid, per-channel values).
    pub scope_samples: Vec<(usize, Vec<Option<f32>>)>,
    /// Latest inputs per display/response_curve node for UI readout rendering.
    pub last_inputs: HashMap<usize, Vec<Option<Signal>>>,
    /// Latest outputs per twoway_response_curve node (blended engine output for UI arrow).
    pub last_outputs: HashMap<usize, Vec<Option<Signal>>>,
    /// Latest signals destined for each (device_id, pin_id) sink slot.
    pub sink_outputs: HashMap<(String, String), Signal>,
}

impl TickOutput {
    /// Clear all containers in-place. Preserves allocated capacity so the
    /// proc thread can reuse the same `TickOutput` across ticks instead of
    /// dropping and reallocating five HashMaps per call (was hot at 2 kHz).
    pub fn clear(&mut self) {
        self.outputs.clear();
        self.scope_samples.clear();
        self.last_inputs.clear();
        self.last_outputs.clear();
        self.sink_outputs.clear();
    }
}

/// Stick pin IDs — deadzone applies only here, not to triggers/gyro/accel/buttons.
fn is_stick_pin(pin_id: &str) -> bool {
    matches!(pin_id,
        "left_stick" | "right_stick"
        | "left_stick_x" | "left_stick_y"
        | "right_stick_x" | "right_stick_y"
    )
}

/// IMU pin IDs — gyro multiplier scales these.
fn is_gyro_pin(pin_id: &str) -> bool {
    matches!(pin_id, "gyro_x" | "gyro_y" | "gyro_z")
}

/// Mouse-delta pin IDs — mouse_sensitivity scales these.
fn is_mouse_pin(pin_id: &str) -> bool {
    matches!(pin_id, "mouse" | "mouse_x" | "mouse_y")
}

/// Number of angular buckets in the per-stick radial correction profile.
/// 72 buckets = 5° each — matches the calibration UI's edge-tracking
/// resolution so we never lose detail through resampling.
pub const STICK_RADIAL_BUCKETS: usize = 72;

/// Default stick deadzone applied when a `device.source` node omits the
/// `deadzone` param OR no source node exists for a device whose signals
/// still reach AutoMap consumers (Remapper / Map Action / sink AutoMap).
/// Must match the UI's `default_deadzone()` so every code path agrees.
pub const DEFAULT_STICK_DEADZONE: f32 = 0.1;

/// Per-device-source calibration applied before deadzone / gyro_multiplier.
/// All fields are no-ops when uncalibrated (centers=0, scales=1, ranges=identity).
#[derive(Clone, Debug)]
struct DeviceCal {
    gyro_offset:   [f32; 3], // [x, y, z] — subtracted from gyro_x/y/z
    accel_offset:  [f32; 3], // [x, y, z] — subtracted from accel_x/y/z
    /// Per-axis output sign for gyro_{x,y,z}. +1.0 = pass-through, -1.0 = inverted.
    gyro_sign:     [f32; 3],
    /// Per-axis output sign for accel_{x,y,z}.
    accel_sign:    [f32; 3],
    /// 3×3 orientation matrix (row-major) applied to the (offset-removed)
    /// gyro and accel vectors before per-axis sign flip. Compensates for
    /// IMU chips that aren't perfectly aligned with the controller's body
    /// axes (modded controllers, factory mount tilt). Identity = no-op.
    orient_matrix: [f32; 9],
    /// True if `orient_matrix` is non-identity and should be applied.
    orient_active: bool,
    lstick_center: [f32; 2], // subtracted from left_stick_{x,y}
    rstick_center: [f32; 2],
    /// Per-bucket radial scale: lstick_radial[i] = 1.0 / (mean stick radius
    /// at angle bucket i). Applied to the centered (x, y) so the corrected
    /// magnitude reaches 1.0 along every direction. STICK_RADIAL_BUCKETS
    /// entries, indexed by `floor(angle / TAU * N)`.
    lstick_radial: [f32; STICK_RADIAL_BUCKETS],
    rstick_radial: [f32; STICK_RADIAL_BUCKETS],
    // Trigger calibration: raw_min/raw_max define the usable range; output
    // is renormalised to 0..1 across it. Defaults (0.0, 1.0) = no-op.
    ltrig_range:   (f32, f32),
    rtrig_range:   (f32, f32),
}

const IDENTITY_M3: [f32; 9] = [
    1.0, 0.0, 0.0,
    0.0, 1.0, 0.0,
    0.0, 0.0, 1.0,
];

impl Default for DeviceCal {
    fn default() -> Self {
        Self {
            gyro_offset:   [0.0; 3],
            accel_offset:  [0.0; 3],
            gyro_sign:     [1.0; 3],
            accel_sign:    [1.0; 3],
            orient_matrix: IDENTITY_M3,
            orient_active: false,
            lstick_center: [0.0; 2],
            rstick_center: [0.0; 2],
            lstick_radial: [1.0; STICK_RADIAL_BUCKETS],
            rstick_radial: [1.0; STICK_RADIAL_BUCKETS],
            ltrig_range:   (0.0, 1.0),
            rtrig_range:   (0.0, 1.0),
        }
    }
}

fn read_arr3(params: &HashMap<String, Value>, key: &str) -> [f32; 3] {
    params.get(key).and_then(|v| v.as_array()).and_then(|a| {
        if a.len() < 3 { return None; }
        Some([
            a[0].as_f64()? as f32,
            a[1].as_f64()? as f32,
            a[2].as_f64()? as f32,
        ])
    }).unwrap_or([0.0; 3])
}
fn read_arr2(params: &HashMap<String, Value>, key: &str) -> [f32; 2] {
    params.get(key).and_then(|v| v.as_array()).and_then(|a| {
        if a.len() < 2 { return None; }
        Some([a[0].as_f64()? as f32, a[1].as_f64()? as f32])
    }).unwrap_or([0.0; 2])
}
fn read_radial(params: &HashMap<String, Value>, key: &str) -> [f32; STICK_RADIAL_BUCKETS] {
    let mut out = [1.0_f32; STICK_RADIAL_BUCKETS];
    let Some(arr) = params.get(key).and_then(|v| v.as_array()) else { return out; };
    for i in 0..STICK_RADIAL_BUCKETS.min(arr.len()) {
        if let Some(v) = arr[i].as_f64() { out[i] = v as f32; }
    }
    out
}
fn read_range(params: &HashMap<String, Value>, key_min: &str, key_max: &str) -> (f32, f32) {
    let mn = params.get(key_min).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    let mx = params.get(key_max).and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
    (mn, mx)
}

fn read_sign3(params: &HashMap<String, Value>, key: &str) -> [f32; 3] {
    let mut out = [1.0_f32; 3];
    let Some(arr) = params.get(key).and_then(|v| v.as_array()) else { return out; };
    for i in 0..3.min(arr.len()) {
        if let Some(b) = arr[i].as_bool() {
            out[i] = if b { -1.0 } else { 1.0 };
        }
    }
    out
}

/// Read a 9-element float array (row-major Mat3) from params. Returns
/// (matrix, active) where `active` is true if the matrix differs
/// meaningfully from identity.
fn read_orient_matrix(params: &HashMap<String, Value>, key: &str) -> ([f32; 9], bool) {
    let Some(arr) = params.get(key).and_then(|v| v.as_array()) else {
        return (IDENTITY_M3, false);
    };
    if arr.len() < 9 { return (IDENTITY_M3, false); }
    let mut m = IDENTITY_M3;
    for i in 0..9 {
        if let Some(v) = arr[i].as_f64() { m[i] = v as f32; }
    }
    // Active if any element differs from identity by more than ~0.5°
    // worth of rotation (≈ 0.01).
    let mut active = false;
    for i in 0..9 {
        if (m[i] - IDENTITY_M3[i]).abs() > 0.01 { active = true; break; }
    }
    (m, active)
}

/// Apply a row-major 3×3 matrix to a 3-vector: `out = M · v`.
#[inline]
fn mat3_apply(m: &[f32; 9], v: [f32; 3]) -> [f32; 3] {
    [
        m[0] * v[0] + m[1] * v[1] + m[2] * v[2],
        m[3] * v[0] + m[4] * v[1] + m[5] * v[2],
        m[6] * v[0] + m[7] * v[1] + m[8] * v[2],
    ]
}

fn load_device_cal(params: &HashMap<String, Value>) -> DeviceCal {
    let (orient_matrix, orient_active) = read_orient_matrix(params, "gyro_orient_matrix");
    DeviceCal {
        gyro_offset:   read_arr3(params, "gyro_offset"),
        accel_offset:  read_arr3(params, "accel_offset"),
        gyro_sign:     read_sign3(params, "gyro_invert"),
        accel_sign:    read_sign3(params, "accel_invert"),
        orient_matrix,
        orient_active,
        lstick_center: read_arr2(params, "lstick_center"),
        rstick_center: read_arr2(params, "rstick_center"),
        lstick_radial: read_radial(params, "lstick_radial"),
        rstick_radial: read_radial(params, "rstick_radial"),
        ltrig_range:   read_range(params, "ltrig_min", "ltrig_max"),
        rtrig_range:   read_range(params, "rtrig_min", "rtrig_max"),
    }
}

/// Scale a centered stick reading (x, y) so that the magnitude reaches 1.0
/// along every direction, using a per-bucket radial scale profile. Buckets
/// linearly-interpolate so the correction is smooth between sample angles.
fn apply_stick_scale(x: f32, y: f32, profile: &[f32; STICK_RADIAL_BUCKETS]) -> (f32, f32) {
    let r = (x * x + y * y).sqrt();
    if r < 1e-5 { return (x, y); }
    let theta = y.atan2(x); // -π..π
    let norm = (theta / std::f32::consts::TAU + 1.0) % 1.0; // 0..1
    let fpos = norm * STICK_RADIAL_BUCKETS as f32;
    let i0 = fpos.floor() as usize % STICK_RADIAL_BUCKETS;
    let i1 = (i0 + 1) % STICK_RADIAL_BUCKETS;
    let t  = fpos - fpos.floor();
    let s  = profile[i0] * (1.0 - t) + profile[i1] * t;
    (x * s, y * s)
}

/// Renormalise a trigger reading from [raw_min, raw_max] to [0, 1] clamped.
fn apply_trigger_range(v: f32, (mn, mx): (f32, f32)) -> f32 {
    if (mx - mn).abs() < 1e-4 { return v; }
    ((v - mn) / (mx - mn)).clamp(0.0, 1.0)
}

/// device.source per-pin post-processing pipeline:
///   1. Calibration offsets / scales / ranges (from the node's params)
///   2. Gyro multiplier (gyro pins only)
///   3. Stick deadzone (stick pins only)
///
/// `imu_pre_applied` = true skips the per-pin offset+sign step for
/// gyro/accel pins because `preprocess_dev_sigs` already did them
/// together with the orientation-matrix transform.
fn post_process_device_pin(
    pin_id: &str,
    sig: Signal,
    dz: f32,
    gm: f32,
    cal: &DeviceCal,
    imu_pre_applied: bool,
) -> Signal {
    // 1. Calibration. Offsets first, then optional axis sign flip.
    let sig = match (pin_id, sig) {
        ("gyro_x",  Signal::Float(v)) if !imu_pre_applied => Signal::Float((v - cal.gyro_offset[0])  * cal.gyro_sign[0]),
        ("gyro_y",  Signal::Float(v)) if !imu_pre_applied => Signal::Float((v - cal.gyro_offset[1])  * cal.gyro_sign[1]),
        ("gyro_z",  Signal::Float(v)) if !imu_pre_applied => Signal::Float((v - cal.gyro_offset[2])  * cal.gyro_sign[2]),
        ("accel_x", Signal::Float(v)) if !imu_pre_applied => Signal::Float((v - cal.accel_offset[0]) * cal.accel_sign[0]),
        ("accel_y", Signal::Float(v)) if !imu_pre_applied => Signal::Float((v - cal.accel_offset[1]) * cal.accel_sign[1]),
        ("accel_z", Signal::Float(v)) if !imu_pre_applied => Signal::Float((v - cal.accel_offset[2]) * cal.accel_sign[2]),
        ("left_stick_x", Signal::Float(v))  => Signal::Float(v - cal.lstick_center[0]),
        ("left_stick_y", Signal::Float(v))  => Signal::Float(v - cal.lstick_center[1]),
        ("right_stick_x", Signal::Float(v)) => Signal::Float(v - cal.rstick_center[0]),
        ("right_stick_y", Signal::Float(v)) => Signal::Float(v - cal.rstick_center[1]),
        ("left_stick",  Signal::Vec2(v)) => {
            let cx = v.x - cal.lstick_center[0];
            let cy = v.y - cal.lstick_center[1];
            let (sx, sy) = apply_stick_scale(cx, cy, &cal.lstick_radial);
            Signal::Vec2(Vec2::new(sx, sy))
        }
        ("right_stick", Signal::Vec2(v)) => {
            let cx = v.x - cal.rstick_center[0];
            let cy = v.y - cal.rstick_center[1];
            let (sx, sy) = apply_stick_scale(cx, cy, &cal.rstick_radial);
            Signal::Vec2(Vec2::new(sx, sy))
        }
        ("left_trigger",  Signal::Float(v)) => Signal::Float(apply_trigger_range(v, cal.ltrig_range)),
        ("right_trigger", Signal::Float(v)) => Signal::Float(apply_trigger_range(v, cal.rtrig_range)),
        (_, s) => s,
    };

    // 2 + 3. Gyro × multiplier, then stick deadzone.
    if is_stick_pin(pin_id) {
        apply_deadzone(sig, dz)
    } else if is_gyro_pin(pin_id) && (gm - 1.0).abs() > f32::EPSILON {
        match sig {
            Signal::Float(v) => Signal::Float(v * gm),
            other => other,
        }
    } else {
        sig
    }
}

/// Build a copy of `dev_sigs` with each device.source node's calibration,
/// deadzone and gyro_multiplier applied to its own device's pins. Called once
/// at the top of `eval_graph_tick` so AutoMap (which reads dev_sigs directly)
/// sees the same processed values as direct wires.
fn preprocess_dev_sigs(
    graph: &ProcessingGraph,
    dev_sigs: &HashMap<(String, String), Signal>,
) -> HashMap<(String, String), Signal> {
    let mut params: HashMap<String, (f32, f32, DeviceCal)> = HashMap::new();
    for snap in &graph.nodes {
        if snap.module_id != "device.source" { continue; }
        let Some(dev_id) = snap.device_id.as_deref() else { continue; };
        let dz = snap.params.get("deadzone").and_then(|v| v.as_f64())
            .map(|v| v as f32).unwrap_or(DEFAULT_STICK_DEADZONE);
        let gm = snap.params.get("gyro_multiplier").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
        let cal = load_device_cal(&snap.params);
        params.insert(dev_id.to_string(), (dz, gm, cal));
    }
    // Fallback for devices whose signals reach AutoMap consumers without a
    // `device.source` node in the graph. Stick deadzone still applies (using
    // the default) so analog stick→key / stick→button mappings don't pass raw
    // sub-deadzone jitter through. gyro_multiplier stays at unity.
    let default_entry = (DEFAULT_STICK_DEADZONE, 1.0_f32, DeviceCal::default());

    // ── Pass 1: 3-axis matrix transform for gyro/accel triples ──────────────
    //
    // The orientation matrix needs all 3 axes at once, which the per-pin
    // pipeline can't see. We collect each device's gyro/accel triples,
    // apply offset → matrix → sign, and pre-write the corrected values
    // into a side map that pass 2 reads from instead of the raw signal.
    let mut imu_override: HashMap<(String, String), Signal> = HashMap::new();
    for (dev_id, (_, _, cal)) in &params {
        let read_f = |pin: &str| -> Option<f32> {
            dev_sigs.get(&(dev_id.clone(), pin.to_string())).and_then(|s| {
                if let Signal::Float(v) = s { Some(*v) } else { None }
            })
        };
        let gxyz = (read_f("gyro_x"), read_f("gyro_y"), read_f("gyro_z"));
        if let (Some(gx), Some(gy), Some(gz)) = gxyz {
            let v = [
                gx - cal.gyro_offset[0],
                gy - cal.gyro_offset[1],
                gz - cal.gyro_offset[2],
            ];
            let v = if cal.orient_active { mat3_apply(&cal.orient_matrix, v) } else { v };
            let out = [
                v[0] * cal.gyro_sign[0],
                v[1] * cal.gyro_sign[1],
                v[2] * cal.gyro_sign[2],
            ];
            imu_override.insert((dev_id.clone(), "gyro_x".into()), Signal::Float(out[0]));
            imu_override.insert((dev_id.clone(), "gyro_y".into()), Signal::Float(out[1]));
            imu_override.insert((dev_id.clone(), "gyro_z".into()), Signal::Float(out[2]));
        }
        let axyz = (read_f("accel_x"), read_f("accel_y"), read_f("accel_z"));
        if let (Some(ax), Some(ay), Some(az)) = axyz {
            let v = [
                ax - cal.accel_offset[0],
                ay - cal.accel_offset[1],
                az - cal.accel_offset[2],
            ];
            let v = if cal.orient_active { mat3_apply(&cal.orient_matrix, v) } else { v };
            let out = [
                v[0] * cal.accel_sign[0],
                v[1] * cal.accel_sign[1],
                v[2] * cal.accel_sign[2],
            ];
            imu_override.insert((dev_id.clone(), "accel_x".into()), Signal::Float(out[0]));
            imu_override.insert((dev_id.clone(), "accel_y".into()), Signal::Float(out[1]));
            imu_override.insert((dev_id.clone(), "accel_z".into()), Signal::Float(out[2]));
        }
    }

    // ── Pass 2: per-pin pipeline ──────────────────────────────────────────────
    let mut out = HashMap::with_capacity(dev_sigs.len());
    for (key, sig) in dev_sigs.iter() {
        let entry = params.get(&key.0).cloned().unwrap_or_else(|| default_entry.clone());
        // Use the matrix-transformed value when this is a gyro/accel pin
        // that we pre-processed; the per-pin function will still apply
        // gyro_multiplier (gyro only) but skip its own offset + sign.
        let src = imu_override.get(key).copied().unwrap_or(*sig);
        let is_imu = matches!(key.1.as_str(),
            "gyro_x" | "gyro_y" | "gyro_z" |
            "accel_x" | "accel_y" | "accel_z");
        out.insert(
            key.clone(),
            post_process_device_pin(&key.1, src, entry.0, entry.1, &entry.2, is_imu),
        );
    }

    // NOTE: the digital-trigger override (driving the virtual analog trigger from
    // a digital ZL/ZR) is intentionally NOT applied here at the source level — it
    // would clobber the `left_trigger` signal for everyone downstream, breaking
    // manual AutoMap injection and Remapper analog output. Instead it's applied as
    // a lowest-priority fallback at the AutoMap sink write (see the trigger handling
    // around the `resolve_mapping` consumer in `eval_graph_tick`).
    out
}

fn apply_deadzone(sig: Signal, dz: f32) -> Signal {
    if dz <= 0.0 { return sig; }
    match sig {
        Signal::Float(v) => {
            let av = v.abs();
            if av < dz { Signal::Float(0.0) }
            else { Signal::Float(v.signum() * (av - dz) / (1.0 - dz).max(f32::EPSILON)) }
        }
        Signal::Vec2(v) => {
            let len = v.length();
            if len < dz { Signal::Vec2(Vec2::ZERO) }
            else { Signal::Vec2(v / len * (len - dz) / (1.0 - dz).max(f32::EPSILON)) }
        }
        other => other,
    }
}

/// Derive synthetic cardinal-direction Bool pins from the analog stick axes
/// in `upstream`, in place. Used by the Remapper so a user can map e.g.
/// "L.Stick Up" → "key_w". A cardinal fires when its axis crosses 0.5 and
/// dominates the perpendicular axis by 1.5× — so pushing slightly off-axis
/// still captures a single direction, but a genuine diagonal fires both
/// cardinals as a chord.
// ── Press-mode state machine (Remapper / Map Action mapping options) ─────────
//
// Each Remapper / Map Action mapping carries an optional "press mode" that
// transforms the raw held-state of the input chord into the gate that fires
// the mapping. State is allocated per-mapping in `state.aux_f32` (4 floats
// per mapping). Slots:
//   [0] prev_input        — 0/1 held-state from the previous tick (rising/
//                           falling edge detection).
//   [1] press_start       — accumulated seconds since the press began (Long
//                           uses this directly; Short and Double window-test
//                           this against the configured window_ms).
//   [2] trigger_remaining — seconds left for an artificial output pulse. The
//                           Short replay (replay actual press duration) and
//                           on-press / on-release 10 ms triggers all decrement
//                           this each tick.
//   [3] double_state      — 0 = idle, 1 = saw 1st rising, 2 = saw 1st falling,
//                           3 = saw 2nd rising (output ON during this state).
//                           Window-checked against [1] at each transition.

const PRESS_SLOTS_PER_MAPPING: usize = 5;
/// Short trigger duration emitted by on-press, on-release, and long-press
/// non-sustain modes. 10 ms gives downstream counters / edge detectors a
/// clean visible pulse without lingering as a held key.
const PRESS_TRIGGER_PULSE_S: f32 = 0.010;

#[derive(Copy, Clone)]
enum PressMode {
    Down,        // pass-through (default)
    Short,       // on-off within window → replay original held duration
    Long,        // held longer than window → sustain OR 10ms trigger
    Double,      // double-tap within window → ON during 2nd press
    OnPress,     // 10ms trigger on rising edge
    OnRelease,   // 10ms trigger on falling edge
}

impl PressMode {
    fn from_str(s: &str) -> Self {
        match s {
            "short"      => Self::Short,
            "long"       => Self::Long,
            "double"     => Self::Double,
            "on_press"   => Self::OnPress,
            "on_release" => Self::OnRelease,
            _            => Self::Down,
        }
    }
}

/// Read 4-slot state for a mapping. Resizes `press_state` if the node hasn't
/// allocated this mapping's slots yet so callers don't have to.
fn press_state_get(state: &mut NodeState, mapping_idx: usize) -> &mut [f32] {
    let need = (mapping_idx + 1) * PRESS_SLOTS_PER_MAPPING;
    if state.press_state.len() < need {
        state.press_state.resize(need, 0.0);
    }
    let start = mapping_idx * PRESS_SLOTS_PER_MAPPING;
    &mut state.press_state[start..start + PRESS_SLOTS_PER_MAPPING]
}

/// Bit indices used by gesture_state for stick-cardinal visited bitmaps.
/// Order: left(0), right(1), up(2), down(3). Same indexing for both
/// left_stick and right_stick — bitmap[0] = left_stick visited cardinals,
/// bitmap[1] = right_stick.
const GESTURE_BIT_LEFT:  u8 = 1 << 0;
const GESTURE_BIT_RIGHT: u8 = 1 << 1;
const GESTURE_BIT_UP:    u8 = 1 << 2;
const GESTURE_BIT_DOWN:  u8 = 1 << 3;

/// Stick deflection threshold to "activate" gesture tracking. Below this,
/// the stick is considered neutral and the visited bitmap resets.
const GESTURE_ACTIVATE_MAG: f32 = 0.5;
/// Hysteresis: once activated, the bitmap is only cleared when the stick
/// falls back below this (lower) threshold. Prevents spurious resets when
/// the stick passes through a quadrant boundary.
const GESTURE_RESET_MAG: f32 = 0.3;

/// Map a stick-cardinal pin id to (stick_index, bit). Returns None if the
/// pin isn't a stick cardinal.
fn gesture_pin_to_bit(pin_id: &str) -> Option<(usize, u8)> {
    match pin_id {
        "left_stick_left"   => Some((0, GESTURE_BIT_LEFT)),
        "left_stick_right"  => Some((0, GESTURE_BIT_RIGHT)),
        "left_stick_up"     => Some((0, GESTURE_BIT_UP)),
        "left_stick_down"   => Some((0, GESTURE_BIT_DOWN)),
        "right_stick_left"  => Some((1, GESTURE_BIT_LEFT)),
        "right_stick_right" => Some((1, GESTURE_BIT_RIGHT)),
        "right_stick_up"    => Some((1, GESTURE_BIT_UP)),
        "right_stick_down"  => Some((1, GESTURE_BIT_DOWN)),
        _ => None,
    }
}

/// Compute the set of cardinal bits currently active for a stick given its
/// (x, y) values. Uses the same 8-zone dominant-axis rule as
/// `derive_stick_cardinals`: pure-axis ±0.5+ when one axis dominates;
/// diagonal contributes BOTH neighboring cardinals when both axes are
/// large enough. Returns 0 when the stick is neutral.
fn gesture_active_bits(x: f32, y: f32) -> u8 {
    let mag = (x * x + y * y).sqrt();
    if mag < GESTURE_ACTIVATE_MAG { return 0; }
    let mut bits = 0u8;
    // 22.5° quadrant: a cardinal is "active" when its axis component is
    // at least 0.5× the other axis (i.e., the stick is in that octant).
    let ax = x.abs();
    let ay = y.abs();
    if x >  0.0 && ax > ay * 0.5 { bits |= GESTURE_BIT_RIGHT; }
    if x <  0.0 && ax > ay * 0.5 { bits |= GESTURE_BIT_LEFT; }
    if y >  0.0 && ay > ax * 0.5 { bits |= GESTURE_BIT_UP; }
    if y <  0.0 && ay > ax * 0.5 { bits |= GESTURE_BIT_DOWN; }
    bits
}

/// Read 2-slot gesture state for a mapping. Resizes if needed.
fn gesture_state_get(state: &mut NodeState, mapping_idx: usize) -> &mut [u8; 2] {
    if state.gesture_state.len() <= mapping_idx {
        state.gesture_state.resize(mapping_idx + 1, [0, 0]);
    }
    &mut state.gesture_state[mapping_idx]
}

/// If `in_pins` contains at least one stick cardinal, return the per-stick
/// required bitmaps (left, right) for the cardinal subset. Non-cardinal pins
/// (buttons, triggers) are ignored here — the caller must enforce their hold
/// state separately. Returns None when the chord has no stick cardinals, so
/// the caller falls back to the standard simultaneous-press rule.
fn gesture_required_bits(in_pins: &[&str]) -> Option<[u8; 2]> {
    if in_pins.is_empty() { return None; }
    let mut req = [0u8; 2];
    let mut any_cardinal = false;
    for &p in in_pins {
        if let Some((stick, bit)) = gesture_pin_to_bit(p) {
            req[stick] |= bit;
            any_cardinal = true;
        }
    }
    if any_cardinal { Some(req) } else { None }
}

/// Update the per-mapping gesture state for one tick and return whether the
/// gesture is "complete" (all required cardinals visited at least once
/// across both sticks). `upstream` provides current stick axis values.
fn gesture_tick(
    required: [u8; 2],
    visited: &mut [u8; 2],
    upstream: &HashMap<String, Signal>,
) -> bool {
    for (stick_idx, axis_pins) in [
        (0usize, ("left_stick_x",  "left_stick_y")),
        (1usize, ("right_stick_x", "right_stick_y")),
    ] {
        let req_bits = required[stick_idx];
        if req_bits == 0 { continue; }
        let x = upstream.get(axis_pins.0).map(|s| sig_scalar(*s)).unwrap_or(0.0);
        let y = upstream.get(axis_pins.1).map(|s| sig_scalar(*s)).unwrap_or(0.0);
        let mag = (x * x + y * y).sqrt();
        if mag < GESTURE_RESET_MAG {
            visited[stick_idx] = 0;
        } else {
            visited[stick_idx] |= gesture_active_bits(x, y);
        }
    }
    // Complete iff every required bit on every stick has been visited.
    (visited[0] & required[0]) == required[0]
        && (visited[1] & required[1]) == required[1]
}

/// Apply the configured press mode to the raw input gate. Returns the
/// transformed gate the mapping should treat as "held this tick".
///
/// `window_ms` is interpreted per mode (Short = max press duration to count
/// as a tap; Long = min hold duration; Double = max time from 1st rising to
/// 2nd rising). `sustain` is meaningful for Long only — when false, fire a
/// 10 ms trigger on threshold crossing instead of holding while pressed.
fn apply_press_mode(
    raw_held: bool,
    mode: PressMode,
    window_ms: f32,
    sustain: bool,
    slots: &mut [f32],
    dt: f32,
) -> bool {
    let prev_held = slots[0] > 0.5;
    let rising  = raw_held && !prev_held;
    let falling = !raw_held && prev_held;
    let window_s = (window_ms.max(0.0)) / 1000.0;

    // Trigger-pulse countdown is shared across modes; non-zero values force
    // the output ON until the timer expires regardless of input state.
    let mut trigger_remaining = (slots[2] - dt).max(0.0);

    let out = match mode {
        PressMode::Down => raw_held,
        PressMode::OnPress => {
            // `window_ms` sets the emitted trigger duration (floored at the
            // 10 ms minimum pulse so a 0/tiny value still registers).
            if rising { trigger_remaining = window_s.max(PRESS_TRIGGER_PULSE_S); }
            trigger_remaining > 0.0
        }
        PressMode::OnRelease => {
            if falling { trigger_remaining = window_s.max(PRESS_TRIGGER_PULSE_S); }
            trigger_remaining > 0.0
        }
        PressMode::Long => {
            // press_start (slots[1]) accumulates seconds while held.
            if rising {
                slots[1] = 0.0;
                // double_state field is repurposed as "armed" flag for the
                // non-sustain trigger so we fire once per press.
                slots[3] = 0.0;
            }
            if raw_held {
                slots[1] += dt;
            } else {
                slots[1] = 0.0;
                slots[3] = 0.0;
            }
            let threshold_crossed = slots[1] >= window_s && window_s > 0.0;
            if sustain {
                threshold_crossed
            } else {
                if threshold_crossed && slots[3] < 0.5 {
                    slots[3] = 1.0; // armed → fire exactly once per press
                    trigger_remaining = PRESS_TRIGGER_PULSE_S;
                }
                trigger_remaining > 0.0
            }
        }
        PressMode::Short => {
            // Tracks the live press; on release we know its duration and can
            // replay it. If the press never released within the window, the
            // chord is suppressed entirely (mapping never fires).
            //
            // double_state field stores remaining replay seconds. When > 0
            // we are in playback and the user's input is ignored until done.
            let mut replay_remaining = slots[3];
            if replay_remaining > 0.0 {
                replay_remaining = (replay_remaining - dt).max(0.0);
                slots[3] = replay_remaining;
                slots[0] = if raw_held { 1.0 } else { 0.0 };
                slots[1] = 0.0;
                slots[2] = trigger_remaining;
                return replay_remaining > 0.0;
            }
            if rising {
                slots[1] = 0.0;
            }
            if raw_held {
                slots[1] += dt;
                if slots[1] > window_s && window_s > 0.0 {
                    // Held too long — give up on this press; user has to
                    // release and tap again. Suppressed until release.
                    slots[1] = f32::INFINITY;
                }
            }
            if falling {
                let held_s = slots[1];
                slots[1] = 0.0;
                if held_s.is_finite() && (window_s <= 0.0 || held_s <= window_s) && held_s > 0.0 {
                    // Qualifying tap → replay the press duration as output.
                    slots[3] = held_s;
                    slots[0] = 0.0;
                    slots[2] = trigger_remaining;
                    return true;
                }
            }
            false
        }
        PressMode::Double => {
            // double_state: 0 idle / 1 after 1st rising / 2 after 1st falling
            //               / 3 during 2nd press (output ON)
            // press_start: seconds since 1st rising edge (window check).
            let mut s = slots[3] as i32;
            if s != 0 {
                slots[1] += dt;
                if window_s > 0.0 && slots[1] > window_s {
                    s = 0; // window expired before completing the gesture
                    slots[1] = 0.0;
                }
            }
            if rising {
                if s == 0 {
                    s = 1;
                    slots[1] = 0.0;
                } else if s == 2 {
                    s = 3; // 2nd rising → output starts
                }
            }
            if falling {
                if s == 1 { s = 2; }
                else if s == 3 {
                    // 2nd falling → output ends, gesture consumed.
                    s = 0;
                    slots[1] = 0.0;
                }
            }
            slots[3] = s as f32;
            s == 3
        }
    };

    slots[0] = if raw_held { 1.0 } else { 0.0 };
    slots[2] = trigger_remaining;
    out
}

/// Post-process the press-mode output with a turbo on/off cycle. When `held`
/// is true the output cycles based on `gap_ms` as the full period (half on,
/// half off). When `held` is false the phase resets so the next press starts
/// at the ON portion. State lives in `slots[4]` (turbo phase seconds).
fn apply_turbo(held: bool, gap_ms: f32, slots: &mut [f32], dt: f32) -> bool {
    if !held {
        slots[4] = 0.0;
        return false;
    }
    let period_s = (gap_ms.max(20.0)) / 1000.0;
    let mut phase = slots[4] + dt;
    if phase >= period_s { phase -= period_s * (phase / period_s).floor(); }
    slots[4] = phase;
    phase < period_s * 0.5
}

/// Maximum tap/PWM frequency for analog→digital modulation at full deflection.
/// Shared by the Remapper, Map Action, and 3DOF-Lean analog dispatch so all
/// three feel identical. Turbo doubles this.
pub const ANALOG_DIGITAL_MAX_FREQ_HZ: f32 = 20.0;

/// Drive a DIGITAL (button/key) destination from an analog input magnitude.
/// Three behaviours, selected by `sustain` (Hold) and `turbo`:
///
///   - **Plain** (Hold off): a tap train. `window_ms` is the *minimum* tap
///     period (lowest frequency); the period shortens as `mag → 1` up to
///     `MAX_FREQ_HZ` (×2 with Turbo). Each tap is a clean 50%-duty square
///     wave so it always reads as a distinct tap rather than a held key.
///   - **Hold** (sustain on): PWM. `window_ms` is the fixed pulse PERIOD and
///     `mag` is the duty cycle — `mag=0` → flat off, `mag=1` → full gate.
///     With Turbo the period also shortens with `mag` (PWM + freq-mod).
///   - **Turbo, Hold off**: same tap train as Plain but at ×2 max frequency.
///
/// `mag` is the post-deadzone input deflection in 0..1. `slots[0]` holds the
/// phase accumulator (seconds in `[0, period)`); it is reset when `mag` is ~0.
/// Returns whether the digital output is asserted this tick.
fn analog_digital_pulse(
    mag: f32,
    window_ms: f32,
    sustain: bool,
    turbo: bool,
    slots: &mut [f32],
    dt: f32,
) -> bool {
    let mag = mag.clamp(0.0, 1.0);
    if mag < 0.01 {
        slots[0] = 0.0;
        return false;
    }
    let max_freq = if turbo {
        ANALOG_DIGITAL_MAX_FREQ_HZ * 2.0
    } else {
        ANALOG_DIGITAL_MAX_FREQ_HZ
    };

    if sustain {
        // ── Hold = PWM: duty cycle tracks amplitude ──────────────────────
        // Period is fixed by window_ms; Turbo additionally scales it down
        // with amplitude so a harder push pulses faster as well as wider.
        let base_period = (window_ms / 1000.0).max(0.020);
        let period = if turbo {
            (1.0 / (mag * max_freq)).clamp(0.020, base_period)
        } else {
            base_period
        };
        let on_s = (mag * period).clamp(0.0, period);
        slots[0] += dt;
        if slots[0] >= period { slots[0] -= period; }
        // mag≈1 → on_s≈period → effectively always on (full gate).
        slots[0] < on_s || on_s >= period
    } else {
        // ── Plain / Turbo = tap train: frequency tracks amplitude ────────
        // `window_ms` sets ONLY the minimum period (lowest tap frequency);
        // a harder push shortens the period up to `max_freq`. Each tap is a
        // clean 50%-duty square wave so it always reads as a distinct tap —
        // NOT a near-held key (the old `tap_on = window_ms` made the duty
        // ~90% at the period floor, which felt held).
        let min_period = (window_ms / 1000.0).max(1.0 / max_freq);
        let period = (1.0 / (mag * max_freq)).max(min_period);
        slots[0] += dt;
        if slots[0] >= period { slots[0] -= period; }
        slots[0] < period * 0.5
    }
}

/// When a processed Vec2 (`left_stick`/`right_stick`/`dpad`) arrives on the
/// AutoMap collector but its X/Y axis pins did NOT (they fell back to raw
/// device samples), the Vec2 is authoritative — a Vec Response Curve wired on
/// the whole stick into a Collector port must drive the axes (and the cardinals
/// derived from them). Split such a Vec2 into its axis pins in `upstream`.
///
/// Per-axis overrides are untouched: if the axes ARE present on the collector,
/// they win (the user processed them individually).
fn vec2_authoritative_axis_fill(
    upstream: &mut HashMap<String, Signal>,
    collector_id: &str,
    collector_sigs: &HashMap<(String, String), Signal>,
) {
    if collector_id.is_empty() { return; }
    let coll = |pin: &str| collector_sigs.get(&(collector_id.to_string(), pin.to_string())).copied();
    for (vec2, xa, ya) in [
        ("left_stick",  "left_stick_x",  "left_stick_y"),
        ("right_stick", "right_stick_x", "right_stick_y"),
        ("dpad",        "dpad_x",        "dpad_y"),
    ] {
        // Need a Vec2 from the collector to be authoritative about.
        let Some(Signal::Vec2(v)) = coll(vec2) else { continue; };
        // Axes absent on the collector → derive them from the Vec2 outright.
        // Axes present but DISAGREE with the Vec2 → the Vec2 was processed
        // (e.g. a Vec Response Curve on the whole stick) while the axes are the
        // unprocessed pass-through, so the Vec2 wins. When they agree (the
        // common raw case, or per-axis processing that also updated the Vec2),
        // leave the axes alone.
        let ax = coll(xa).map(|s| s.as_float());
        let ay = coll(ya).map(|s| s.as_float());
        let disagree = |axis: Option<f32>, comp: f32| match axis {
            Some(a) => (a - comp).abs() > 1e-3,
            None => true,
        };
        if disagree(ax, v.x) { upstream.insert(xa.to_string(), Signal::Float(v.x)); }
        if disagree(ay, v.y) { upstream.insert(ya.to_string(), Signal::Float(v.y)); }
    }
}

fn derive_stick_cardinals(upstream: &mut HashMap<String, Signal>) {
    // Tuned for round-gate sticks where 45° physically caps at ~0.707.
    //   T_CARDINAL — minimum push for a single cardinal to fire when
    //     the perpendicular axis is quiet.
    //   T_DIAGONAL — when BOTH axes exceed this, fire both cardinals as
    //     a chord. Lower than T_CARDINAL so a 45° push at ~0.5/0.5 still
    //     registers as a diagonal (avoid the narrow band that the
    //     dominance rule alone couldn't cover on a circular gate).
    //   DOM — perpendicular-dominance ratio so a slight off-axis push
    //     still counts as a single cardinal.
    const T_CARDINAL: f32 = 0.5;
    const T_DIAGONAL: f32 = 0.4;
    const DOM: f32 = 1.5;
    for (xpin, ypin, up, down, left, right) in [
        ("left_stick_x",  "left_stick_y",
         "left_stick_up",  "left_stick_down",
         "left_stick_left", "left_stick_right"),
        ("right_stick_x", "right_stick_y",
         "right_stick_up", "right_stick_down",
         "right_stick_left", "right_stick_right"),
    ] {
        let x = upstream.get(xpin).map(|s| sig_scalar(*s)).unwrap_or(0.0);
        let y = upstream.get(ypin).map(|s| sig_scalar(*s)).unwrap_or(0.0);
        let ax = x.abs();
        let ay = y.abs();
        let diagonal = ax > T_DIAGONAL && ay > T_DIAGONAL;
        let right_on = diagonal && x >  T_DIAGONAL
            || x >  T_CARDINAL && (ay < T_CARDINAL ||  x >  DOM * ay);
        let left_on  = diagonal && x < -T_DIAGONAL
            || x < -T_CARDINAL && (ay < T_CARDINAL || -x >  DOM * ay);
        let up_on    = diagonal && y >  T_DIAGONAL
            || y >  T_CARDINAL && (ax < T_CARDINAL ||  y >  DOM * ax);
        let down_on  = diagonal && y < -T_DIAGONAL
            || y < -T_CARDINAL && (ax < T_CARDINAL || -y >  DOM * ax);
        upstream.insert(up.to_string(),    Signal::Bool(up_on));
        upstream.insert(down.to_string(),  Signal::Bool(down_on));
        upstream.insert(left.to_string(),  Signal::Bool(left_on));
        upstream.insert(right.to_string(), Signal::Bool(right_on));
    }
}

/// Reserved collector-pin-name prefix marking a pin that an upstream consumer
/// (Remapper) CLAIMED. A downstream Combiner reads these to suppress the same
/// pin on lower-priority inputs (hierarchy), unless that port's policy is ADD.
const CONSUMED_PREFIX: &str = "__consumed__:";

/// Write `__consumed__:{pin}` markers into `collector_sigs` under `key` for
/// every pin a Remapper claimed — both the claimed cardinals/buttons and the
/// underlying stick axes of any claimed cardinal (so a Combiner suppresses the
/// raw axis too, not just the synthetic cardinal).
fn publish_consumed_markers(
    key: &str,
    claimed_digital: &HashSet<String>,
    claimed_analog: &HashSet<String>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
) {
    let mark = |pin: &str, collector_sigs: &mut HashMap<(String, String), Signal>| {
        collector_sigs.insert((key.to_string(), format!("{CONSUMED_PREFIX}{pin}")), Signal::Int(1));
    };
    for pin in claimed_digital.iter().chain(claimed_analog.iter()) {
        mark(pin, collector_sigs);
        // Suppress the underlying axis AND bundled Vec2 too (covers sticks and
        // D-pad), otherwise the virtual device regenerates the consumed
        // direction from the still-raw axis / Vec2.
        if let Some((axis_pin, _)) = cardinal_axis_for_suppression(pin) {
            mark(axis_pin, collector_sigs);
            if let Some(v) = vec2_pin_for_axis(axis_pin) { mark(v, collector_sigs); }
        }
    }
}

fn combine_signals(a: Signal, b: Signal) -> Signal {
    match (a, b) {
        (Signal::Float(x), Signal::Float(y)) => Signal::Float(x + y),
        (Signal::Vec2(x),  Signal::Vec2(y))  => Signal::Vec2(x + y),
        (Signal::Bool(x),  Signal::Bool(y))  => Signal::Bool(x || y),
        (Signal::Int(x),   Signal::Int(y))   => Signal::Int(x + y),
        (_, b) => b,
    }
}

// ── AutoMap consumer publishing helpers (shared by top-level + subgraph) ──────
//
// These three modules (Fork, Combiner, Selector) write into `collector_sigs`
// under "<kind>:{key_uid}" so downstream consumers can resolve them via the
// AutoMap routing scheme. `key_uid` is the publishing UID:
//   - top-level: `snap.node_uid` (raw)
//   - subgraph:  `namespaced_uid(outer_uid, snap.node_uid)`
//
// The subgraph form must use the namespaced UID so the keys match what
// `find_automap_device_rec` in the UI produces when it walks the wire chain
// across the sub-patch boundary.

/// Feedback Control node: inject wired inlet values into the physical source
/// pad's haptic channel and read outlet taps from the virtual destination's
/// feedback. Shared by the top-level loop and `eval_subgraph`.
///
/// Injection key: `("feedback_inject:{_fb_source_dev}", inlet_pin_id)`. The
/// physical `device.source` sink drains this in its feedback pass, keyed by its
/// own device id — so the bridge needs no per-uid plumbing and works at any
/// sub-patch depth. Multiple injectors targeting one pad combine additively.
///
/// Returns the node's full output vector: output[0] = AutoMap pass-through
/// (None placeholder), outputs[1..] = outlet taps (per `_fb_outlet_ids`).
fn feedback_control_publish(
    snap: &NodeSnap,
    computed: &[Vec<Option<Signal>>],
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
) -> Vec<Option<Signal>> {
    // Resolve wired inputs for this node (input[0] = AutoMap bus, ignored as a
    // value; inputs[1..] = inlets parallel to `_fb_inlet_ids`).
    let inputs: Vec<Option<Signal>> = snap.input_sources.iter()
        .map(|src| src.and_then(|(si, op)| {
            computed.get(si).and_then(|v| v.get(op)).copied().flatten()
        }))
        .collect();

    // ── Inlet injection ──────────────────────────────────────────────────────
    let source_dev = snap.params.get("_fb_source_dev").and_then(|v| v.as_str()).unwrap_or("");
    if !source_dev.is_empty() {
        let inlet_ids = snap.params.get("_fb_inlet_ids").and_then(|v| v.as_array());
        if let Some(inlet_ids) = inlet_ids {
            let key = format!("feedback_inject:{source_dev}");
            for (i, pin_v) in inlet_ids.iter().enumerate() {
                let Some(pin_id) = pin_v.as_str() else { continue; };
                if pin_id.is_empty() { continue; }
                // inputs[i + 1] — skip the AutoMap bus at input[0].
                if let Some(sig) = inputs.get(i + 1).and_then(|s| *s) {
                    // Additive combine when several injectors hit the same pin
                    // on the same device (last-writer-wins would silently drop).
                    let entry = collector_sigs.entry((key.clone(), pin_id.to_string()));
                    use std::collections::hash_map::Entry;
                    match entry {
                        Entry::Occupied(mut o) => { *o.get_mut() = combine_signals(*o.get(), sig); }
                        Entry::Vacant(v)       => { v.insert(sig); }
                    }
                }
            }
        }
    }

    // ── Outlet taps ──────────────────────────────────────────────────────────
    let dest_dev = snap.params.get("_fb_dest_dev").and_then(|v| v.as_str()).unwrap_or("");
    let outlet_ids = snap.params.get("_fb_outlet_ids").and_then(|v| v.as_array());
    let mut out: Vec<Option<Signal>> = vec![None; snap.n_outputs];
    if let Some(outlet_ids) = outlet_ids {
        for (i, pin_v) in outlet_ids.iter().enumerate() {
            let Some(pin_id) = pin_v.as_str() else { continue; };
            // output[i + 1] — skip the AutoMap pass-through at output[0].
            let out_idx = i + 1;
            if out_idx >= out.len() { break; }
            if dest_dev.is_empty() { continue; }
            if let Some(&sig) = dev_sigs.get(&(dest_dev.to_string(), pin_id.to_string())) {
                out[out_idx] = Some(sig);
            }
        }
    }
    out
}

fn automap_fork_publish(
    snap: &NodeSnap,
    key_uid: usize,
    computed: &[Vec<Option<Signal>>],
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
) {
    let dev_id = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("");
    let collector_id_upstream = snap.params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("");
    // inputs[0] = AutoMap (ignored as value), inputs[1] = select
    let select = match snap.input_sources.get(1)
        .and_then(|src| src.and_then(|(si, op)| computed.get(si).and_then(|v| v.get(op)).copied().flatten()))
    {
        Some(Signal::Float(f)) => {
            let n = snap.n_outputs.max(1);
            ((f.clamp(0.0, 1.0) * (n as f32 - 1.0 + 0.5)).floor() as usize).min(n - 1)
        }
        Some(Signal::Bool(b)) => if b { 1 } else { 0 },
        _ => 0,
    };
    for out_idx in 0..snap.n_outputs {
        if out_idx != select { continue; }
        let key = format!("forksel:{}:{}", key_uid, out_idx);
        for pin in flexinput_core::automap::ALL_PINS {
            let sig = if !collector_id_upstream.is_empty() {
                collector_sigs.get(&(collector_id_upstream.to_string(), pin.id.to_string())).copied()
                    .or_else(|| dev_sigs.get(&(dev_id.to_string(), pin.id.to_string())).copied())
            } else {
                dev_sigs.get(&(dev_id.to_string(), pin.id.to_string())).copied()
            };
            if let Some(sig) = sig {
                collector_sigs.insert((key.clone(), pin.id.to_string()), sig);
            }
        }
        if !collector_id_upstream.is_empty() {
            let copies: Vec<(String, Signal)> = collector_sigs.iter()
                .filter(|((d, p), _)| {
                    d == collector_id_upstream
                        && !automap::ALL_PINS.iter().any(|ap| ap.id == p.as_str())
                })
                .map(|((_, p), s)| (p.clone(), *s))
                .collect();
            for (pin, sig) in copies {
                collector_sigs.insert((key.clone(), pin), sig);
            }
        }
    }
}

fn automap_combiner_publish(
    snap: &NodeSnap,
    key_uid: usize,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
) {
    let input_devs = snap.params.get("_automap_input_devs")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect::<Vec<_>>())
        .unwrap_or_default();
    let input_collectors = snap.params.get("_automap_input_collectors")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect::<Vec<_>>())
        .unwrap_or_default();
    let policy_map = snap.params.get("combiner_pin_policy")
        .and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let port_map = snap.params.get("combiner_pin_port")
        .and_then(|v| v.as_object()).cloned().unwrap_or_default();
    // Per-PORT default policy: `{ "0": "ADD", "1": "SORT", … }`. Applies to any
    // pin offered by that port that has no per-pin override. When several ports
    // offer a pin, the lowest-index (highest-priority) port that actually
    // carries the pin this tick wins the default. Falls back to global SORT.
    let port_default_map = snap.params.get("combiner_port_default")
        .and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let key = format!("combiner:{}", key_uid);

    fn clamp_for_pin(pin_id: &str, v: f32) -> f32 {
        match pin_id {
            "left_trigger" | "right_trigger" => v.clamp(0.0, 1.0),
            "left_stick_x" | "left_stick_y"
            | "right_stick_x" | "right_stick_y"
            | "dpad_x" | "dpad_y" => v.clamp(-1.0, 1.0),
            _ => v,
        }
    }
    fn clamp_vec2_for_pin(pin_id: &str, v: glam::Vec2) -> glam::Vec2 {
        if matches!(pin_id, "left_stick" | "right_stick" | "dpad") {
            glam::Vec2::new(v.x.clamp(-1.0, 1.0), v.y.clamp(-1.0, 1.0))
        } else { v }
    }

    fn read_pin_at(
        i: usize, pin_id: &str,
        input_devs: &[String], input_collectors: &[String],
        collector_sigs: &HashMap<(String, String), Signal>,
        dev_sigs: &HashMap<(String, String), Signal>,
    ) -> Option<Signal> {
        let collector_id = input_collectors.get(i).map(|s| s.as_str()).unwrap_or("");
        let dev_id       = input_devs.get(i).map(|s| s.as_str()).unwrap_or("");
        if !collector_id.is_empty() {
            collector_sigs.get(&(collector_id.to_string(), pin_id.to_string())).copied()
                .or_else(|| {
                    if !dev_id.is_empty() {
                        dev_sigs.get(&(dev_id.to_string(), pin_id.to_string())).copied()
                    } else { None }
                })
        } else if !dev_id.is_empty() {
            dev_sigs.get(&(dev_id.to_string(), pin_id.to_string())).copied()
        } else {
            None
        }
    }

    for pin in flexinput_core::automap::ALL_PINS {
        if let Some(port_v) = port_map.get(pin.id) {
            if let Some(port_u) = port_v.as_u64() {
                let n_inputs = input_devs.len();
                if n_inputs == 0 { continue; }
                let port = (port_u as usize).min(n_inputs - 1);
                if let Some(sig) = read_pin_at(port, pin.id,
                    &input_devs, &input_collectors, &collector_sigs, dev_sigs)
                {
                    collector_sigs.insert((key.clone(), pin.id.to_string()), sig);
                }
                continue;
            }
        }

        // Effective policy: per-pin override > per-port default (from the
        // lowest-index port that actually carries this pin) > global SORT.
        let policy: &str = if let Some(p) = policy_map.get(pin.id).and_then(|v| v.as_str()) {
            p
        } else {
            let mut from_port = "SORT";
            if !port_default_map.is_empty() {
                for i in 0..input_devs.len() {
                    let offers = read_pin_at(i, pin.id,
                        &input_devs, &input_collectors, &collector_sigs, dev_sigs).is_some();
                    if !offers { continue; }
                    if let Some(p) = port_default_map.get(&i.to_string()).and_then(|v| v.as_str()) {
                        from_port = p;
                        break;
                    }
                }
            }
            from_port
        };

        // Hierarchy suppression: if an upstream Remapper on ANY input collector
        // CONSUMED this pin (mapped it away), the higher-level decision wins for
        // every port — the pin is dropped from all inputs, including raw-device
        // ports that still carry it. EXCEPTION: ADD explicitly opts into mixing,
        // so a Combiner port set to ADD keeps its value.
        let marker = format!("{CONSUMED_PREFIX}{}", pin.id);
        let any_consumed = input_collectors.iter().any(|cid| {
            !cid.is_empty() && collector_sigs.contains_key(&(cid.to_string(), marker.clone()))
        });
        if any_consumed && policy != "ADD" {
            // Hierarchy: a pin an upstream Remapper claimed is owned by that
            // Remapper. Take ITS (already per-side-suppressed) value — which
            // clears only the mapped direction, leaving e.g. dpad_right intact —
            // and ignore the raw ports entirely. The consuming collector is the
            // FIRST input collector that carries the marker.
            //
            // We must WRITE the value (even when it's off/zero), never drop it:
            // a virtual device latches the last value for any pin it stops
            // receiving, so a dropped consumed D-pad/button would stay stuck.
            let consuming = input_collectors.iter().find(|cid| {
                !cid.is_empty()
                    && collector_sigs.contains_key(&((*cid).clone(), marker.clone()))
            }).cloned();
            let owned = consuming
                .and_then(|cid| collector_sigs.get(&(cid, pin.id.to_string())).copied());
            let sig = owned.unwrap_or(match pin.signal_type {
                flexinput_core::SignalType::Bool => Signal::Bool(false),
                flexinput_core::SignalType::Vec2 => Signal::Vec2(glam::Vec2::ZERO),
                flexinput_core::SignalType::Int  => Signal::Int(0),
                _ => Signal::Float(0.0),
            });
            collector_sigs.insert((key.clone(), pin.id.to_string()), sig);
            // Re-publish the marker so a Combiner-of-Combiners keeps honouring
            // the hierarchy.
            collector_sigs.insert((key.clone(), marker.clone()), Signal::Int(1));
            continue;
        }

        let mut raw: Vec<Signal> = Vec::with_capacity(input_devs.len());
        for i in 0..input_devs.len() {
            if let Some(s) = read_pin_at(i, pin.id,
                &input_devs, &input_collectors, &collector_sigs, dev_sigs)
            {
                raw.push(s);
            }
        }
        if raw.is_empty() { continue; }
        let resolved: Option<Signal> = match policy {
            "SORT" => raw.into_iter().next(),
            "OR" => match pin.signal_type {
                flexinput_core::SignalType::Bool => {
                    let any = raw.iter().any(|s| matches!(s, Signal::Bool(true)));
                    Some(Signal::Bool(any))
                }
                flexinput_core::SignalType::Vec2 => {
                    let pick = |sel: fn(&glam::Vec2) -> f32| {
                        raw.iter().filter_map(|s| match s {
                            Signal::Vec2(v) => Some(sel(v)), _ => None
                        }).fold(0.0_f32, |acc, x|
                            if x.abs() > acc.abs() { x } else { acc })
                    };
                    Some(Signal::Vec2(clamp_vec2_for_pin(pin.id,
                        glam::Vec2::new(pick(|v| v.x), pick(|v| v.y)))))
                }
                _ => {
                    let f = raw.iter().filter_map(|s| sig_to_f32(Some(*s))).fold(0.0_f32, |acc, x|
                        if x.abs() > acc.abs() { x } else { acc });
                    Some(Signal::Float(clamp_for_pin(pin.id, f)))
                }
            },
            "AND" => match pin.signal_type {
                flexinput_core::SignalType::Bool => {
                    let all = raw.iter().all(|s| matches!(s, Signal::Bool(true)));
                    Some(Signal::Bool(all))
                }
                flexinput_core::SignalType::Vec2 => {
                    let pick = |sel: fn(&glam::Vec2) -> f32| {
                        let mut it = raw.iter().filter_map(|s| match s {
                            Signal::Vec2(v) => Some(sel(v)), _ => None
                        });
                        let mut best = it.next().unwrap_or(0.0);
                        for x in it {
                            if x.abs() < best.abs() { best = x; }
                        }
                        best
                    };
                    Some(Signal::Vec2(clamp_vec2_for_pin(pin.id,
                        glam::Vec2::new(pick(|v| v.x), pick(|v| v.y)))))
                }
                _ => {
                    let mut it = raw.iter().filter_map(|s| sig_to_f32(Some(*s)));
                    let mut best = it.next().unwrap_or(0.0);
                    for x in it {
                        if x.abs() < best.abs() { best = x; }
                    }
                    Some(Signal::Float(clamp_for_pin(pin.id, best)))
                }
            },
            "XOR" => match pin.signal_type {
                flexinput_core::SignalType::Bool => {
                    let parity = raw.iter()
                        .filter(|s| matches!(s, Signal::Bool(true))).count() % 2 == 1;
                    Some(Signal::Bool(parity))
                }
                flexinput_core::SignalType::Vec2 => {
                    let fold = |sel: fn(&glam::Vec2) -> f32| -> f32 {
                        let xs: Vec<f32> = raw.iter().filter_map(|s| match s {
                            Signal::Vec2(v) => Some(sel(v)), _ => None
                        }).collect();
                        if xs.is_empty() { return 0.0; }
                        xs.iter().skip(1).fold(xs[0], |acc, &x| (acc - x).abs())
                    };
                    Some(Signal::Vec2(clamp_vec2_for_pin(pin.id,
                        glam::Vec2::new(fold(|v| v.x), fold(|v| v.y)))))
                }
                _ => {
                    let xs: Vec<f32> = raw.iter().filter_map(|s| sig_to_f32(Some(*s))).collect();
                    let v = if xs.is_empty() { 0.0 }
                        else { xs.iter().skip(1).fold(xs[0], |acc, &x| (acc - x).abs()) };
                    Some(Signal::Float(clamp_for_pin(pin.id, v)))
                }
            },
            "ADD" => match pin.signal_type {
                flexinput_core::SignalType::Bool => {
                    let any = raw.iter().any(|s| matches!(s, Signal::Bool(true)));
                    Some(Signal::Bool(any))
                }
                flexinput_core::SignalType::Vec2 => {
                    let sum = raw.iter().fold(glam::Vec2::ZERO, |acc, s| match s {
                        Signal::Vec2(v) => acc + *v, _ => acc
                    });
                    Some(Signal::Vec2(clamp_vec2_for_pin(pin.id, sum)))
                }
                _ => {
                    let s: f32 = raw.iter().filter_map(|s| sig_to_f32(Some(*s))).sum();
                    Some(Signal::Float(clamp_for_pin(pin.id, s)))
                }
            },
            "MULT" => match pin.signal_type {
                flexinput_core::SignalType::Bool => {
                    let all = raw.iter().all(|s| matches!(s, Signal::Bool(true)));
                    Some(Signal::Bool(all))
                }
                flexinput_core::SignalType::Vec2 => {
                    let first = match raw.first() {
                        Some(Signal::Vec2(v)) => *v,
                        _ => glam::Vec2::ZERO,
                    };
                    let mag_product = raw.iter().fold(1.0_f32, |acc, s| match s {
                        Signal::Vec2(v) => acc * v.length(),
                        _ => acc,
                    });
                    let dir = if first.length() > 0.0 {
                        first.normalize()
                    } else {
                        glam::Vec2::ZERO
                    };
                    Some(Signal::Vec2(clamp_vec2_for_pin(pin.id, dir * mag_product)))
                }
                _ => {
                    let is_signed = !matches!(pin.id,
                        "left_trigger" | "right_trigger");
                    let nums: Vec<f32> = raw.iter()
                        .filter_map(|s| sig_to_f32(Some(*s))).collect();
                    let v = if is_signed {
                        let sign = nums.first().copied().unwrap_or(0.0);
                        let mag = nums.iter().fold(1.0_f32, |a, b| a * b.abs());
                        if sign < 0.0 { -mag } else { mag }
                    } else {
                        nums.iter().fold(1.0_f32, |a, b| a * b)
                    };
                    Some(Signal::Float(clamp_for_pin(pin.id, v)))
                }
            },
            _ => None,
        };
        if let Some(s) = resolved {
            collector_sigs.insert((key.clone(), pin.id.to_string()), s);
        }
    }

    // Off-spec pass-through (Remapper's keyboard/mouse pins etc.).
    {
        let mut extras: HashMap<String, Signal> = HashMap::new();
        for collector_id in input_collectors.iter().rev() {
            if collector_id.is_empty() { continue; }
            for ((dev, pin), &sig) in collector_sigs.iter() {
                if dev != collector_id { continue; }
                if automap::ALL_PINS.iter().any(|p| p.id == pin.as_str()) { continue; }
                extras.insert(pin.clone(), sig);
            }
        }
        for (pin, sig) in extras {
            let dest_key = (key.clone(), pin);
            if !collector_sigs.contains_key(&dest_key) {
                collector_sigs.insert(dest_key, sig);
            }
        }
    }
}

fn automap_selector_publish(
    snap: &NodeSnap,
    key_uid: usize,
    computed: &[Vec<Option<Signal>>],
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
) {
    let n_inputs = snap.input_sources.len().saturating_sub(1).max(1);
    let select = match snap.input_sources.get(0)
        .and_then(|src| src.and_then(|(si, op)| computed.get(si).and_then(|v| v.get(op)).copied().flatten()))
    {
        Some(Signal::Float(f)) => {
            let n = n_inputs as f32;
            ((f.clamp(0.0, 1.0) * n).floor() as usize).min(n_inputs - 1)
        }
        Some(Signal::Bool(b)) => if b { 1 } else { 0 },
        _ => 0,
    };
    let input_devs = snap.params.get("_automap_input_devs")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect::<Vec<_>>())
        .unwrap_or_default();
    let input_collectors = snap.params.get("_automap_input_collectors")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect::<Vec<_>>())
        .unwrap_or_default();
    let selected_dev = input_devs.get(select).map(|s| s.as_str()).unwrap_or("").to_string();
    let selected_collector = input_collectors.get(select).map(|s| s.as_str()).unwrap_or("").to_string();
    let key = format!("forksel:{}:0", key_uid);
    for pin in flexinput_core::automap::ALL_PINS {
        let sig = if !selected_collector.is_empty() {
            collector_sigs.get(&(selected_collector.clone(), pin.id.to_string())).copied()
                .or_else(|| {
                    if !selected_dev.is_empty() {
                        dev_sigs.get(&(selected_dev.clone(), pin.id.to_string())).copied()
                    } else { None }
                })
        } else if !selected_dev.is_empty() {
            dev_sigs.get(&(selected_dev.clone(), pin.id.to_string())).copied()
        } else {
            None
        };
        if let Some(sig) = sig {
            collector_sigs.insert((key.clone(), pin.id.to_string()), sig);
        }
    }
    if !selected_collector.is_empty() {
        let copies: Vec<(String, Signal)> = collector_sigs.iter()
            .filter(|((d, p), _)| {
                d == &selected_collector
                    && !automap::ALL_PINS.iter().any(|ap| ap.id == p.as_str())
            })
            .map(|((_, p), s)| (p.clone(), *s))
            .collect();
        for (pin, sig) in copies {
            collector_sigs.insert((key.clone(), pin), sig);
        }
    }
}

// ── Sub-patch inner evaluation ────────────────────────────────────────────────

/// Namespaces inner node UIDs under their containing subpatch's UID to avoid
/// collisions in the shared `state` map (and the `remap:`/`collector:` keys the
/// UI's AutoMap resolver derives) when multiple subpatches — including nested
/// ones — share inner node indices.
///
/// The previous `(outer << 20) + inner + 1` was NOT injective: a left-shift by
/// 20 discards high bits, so two different `(outer, inner)` pairs (e.g. a
/// top-level subpatch's Remapper and a differently-nested one) could alias to
/// the same UID. That made the two distinct nodes write the SAME collector key,
/// each clobbering the other's per-frame output (observed as a Remapper's
/// suppressed D-pad direction leaking through). This uses a splitmix64-style
/// finalizer over a 128→64 fold of the two operands, which is effectively
/// collision-free for the small integer node ids in play.
///
/// MUST stay identical between the engine eval and the UI's `find_automap_device`
/// walkers — both call this same function so their keys agree.
#[inline]
pub fn namespaced_uid(outer: usize, inner: usize) -> usize {
    // Reserve a marker bit so a namespaced uid never collides with a raw
    // top-level node uid (which are small snarl indices). +1 on inner keeps
    // inner==0 distinguishable.
    let mut z = (outer as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add((inner as u64).wrapping_add(1));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    // Force the high bit set so these never alias a plain (small) node uid.
    (z | 0x8000_0000_0000_0000) as usize
}

/// Evaluates the inner graph of a sub-patch node.
/// Returns the per-node computed signal vectors in inner flat-graph order.
/// `outer_uid` is the UID of the containing meta-module node, used for state namespacing.
/// Inner display nodes (oscilloscope, response_curve, etc.) push samples into
/// `scope_samples`/`last_inputs` keyed by `namespaced_uid` so the UI can render
/// live feedback on inner module bodies (and their pinned mirrors on the outer body).
/// AutoMap collectors inside the subpatch inject into `collector_sigs` using a
/// namespaced key so downstream sinks can pick them up via the same routing path.
fn eval_subgraph(
    graph: &ProcessingGraph,
    outer_inputs: &[Option<Signal>],
    state: &mut HashMap<usize, NodeState>,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
    scope_samples: &mut Vec<(usize, Vec<Option<f32>>)>,
    last_inputs: &mut HashMap<usize, Vec<Option<Signal>>>,
    last_outputs: &mut HashMap<usize, Vec<Option<Signal>>>,
    outer_uid: usize,
    dt: f32,
) -> Vec<Vec<Option<Signal>>> {
    let n = graph.nodes.len();
    let mut computed: Vec<Vec<Option<Signal>>> = vec![vec![]; n];

    for (idx, snap) in graph.nodes.iter().enumerate() {
        // Compute a namespaced UID for this inner node early so inner-node
        // special cases can publish into `collector_sigs` using the same
        // keying scheme the UI's AutoMap resolver expects.
        let ns_uid = namespaced_uid(outer_uid, snap.node_uid);
        // Inlet: produce the corresponding outer input signal.
        if snap.module_id == "subpatch.inlet" {
            let pin_idx = snap.params.get("pin_index")
                .and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            computed[idx] = vec![outer_inputs.get(pin_idx).copied().flatten()];
            continue;
        }

        // Nested subpatch within this subpatch.
        if let Some(ref sg) = snap.inline_subgraph {
            let inner_inputs: Vec<Option<Signal>> = snap.input_sources.iter()
                .map(|src| src.and_then(|(si, op)| {
                    computed.get(si).and_then(|v| v.get(op)).copied().flatten()
                }))
                .collect();
            let nested_uid = namespaced_uid(outer_uid, snap.node_uid);
            let inner_computed = eval_subgraph(
                &sg.graph, &inner_inputs, state, dev_sigs, collector_sigs,
                scope_samples, last_inputs, last_outputs, nested_uid, dt,
            );
            computed[idx] = sg.outlet_locs.iter()
                .map(|loc| loc.and_then(|(ni, np)| inner_computed.get(ni).and_then(|v| v.get(np)).copied().flatten()))
                .collect();
            continue;
        }

        // AutoMap collector inside a subpatch: inject signals into collector_sigs
        // using a namespaced key so it matches what find_automap_device produced.
        // Mirrors the top-level arm: pass-through upstream first, then apply
        // explicit collected-pin overrides.
        if snap.module_id == "module.automap_collect" {
            let inputs: Vec<Option<Signal>> = snap.input_sources.iter()
                .map(|src| src.and_then(|(si, op)| {
                    computed.get(si).and_then(|v| v.get(op)).copied().flatten()
                }))
                .collect();
            let collect_ids = snap.params.get("_collect_pin_ids")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect::<Vec<_>>())
                .unwrap_or_default();
            let uid_key = format!("collector:{}", ns_uid);

            // Phase 1: pass-through from upstream AutoMap source.
            // See top-level arm for the rationale on iterating actual
            // collector_sigs entries rather than `ALL_PINS`.
            let upstream_dev = snap.params.get("_automap_device_id")
                .and_then(|v| v.as_str()).unwrap_or("").to_string();
            let upstream_collector = snap.params.get("_automap_collector_id")
                .and_then(|v| v.as_str()).unwrap_or("").to_string();
            if !upstream_collector.is_empty() {
                let copies: Vec<(String, Signal)> = collector_sigs.iter()
                    .filter(|((dev, _), _)| dev == &upstream_collector)
                    .map(|((_, pin), sig)| (pin.clone(), *sig))
                    .collect();
                for (pin, sig) in copies {
                    collector_sigs.insert((uid_key.clone(), pin), sig);
                }
                if !upstream_dev.is_empty() {
                    for pin in flexinput_core::automap::ALL_PINS {
                        let key = (uid_key.clone(), pin.id.to_string());
                        if collector_sigs.contains_key(&key) { continue; }
                        if let Some(&sig) = dev_sigs.get(&(upstream_dev.clone(), pin.id.to_string())) {
                            collector_sigs.insert(key, sig);
                        }
                    }
                }
            } else if !upstream_dev.is_empty() {
                for pin in flexinput_core::automap::ALL_PINS {
                    if let Some(&sig) = dev_sigs.get(&(upstream_dev.clone(), pin.id.to_string())) {
                        collector_sigs.insert((uid_key.clone(), pin.id.to_string()), sig);
                    }
                }
            }

            // Phase 2: explicit collected-pin overrides.
            for (i, pin_id) in collect_ids.iter().enumerate() {
                if let Some(sig) = inputs.get(i + 1).and_then(|s| *s) {
                    if !pin_id.is_empty() {
                        collector_sigs.insert((uid_key.clone(), pin_id.clone()), sig);
                    }
                }
            }
            computed[idx] = vec![None];
            continue;
        }

        // Handle remapper inside a subpatch the same way the top-level loop
        // does, but publish under the namespaced remap key so downstream
        // sinks (or outer graph routing) can pick up the overrides.
        if snap.module_id == "module.remapper" {
            // Shared Remapper evaluation (identical at top level and in sub-patches).
            eval_remapper_node(snap, ns_uid, dev_sigs, collector_sigs, state, dt);

            computed[idx] = vec![None];
            continue;
        }

        // module.map_action inside subpatch: mirror top-level behaviour but
        // write last_outputs keyed by the namespaced UID so UI/outer bodies
        // can observe inner output state.
        if snap.module_id == "module.map_action" {
            // Shared Map Action evaluation (identical at top level and sub-patch).
            computed[idx] = eval_map_action_node(snap, ns_uid, dev_sigs, collector_sigs, state, dt);
            last_outputs.insert(ns_uid, computed[idx].clone());
            continue;
        }

        // AutoMap fork / combiner / selector inside a sub-patch. Same helpers
        // as the top-level loop, but keyed under `ns_uid` so downstream sinks
        // (and the UI's `find_automap_device_rec` walker, which folds the outer
        // chain when it crosses the subpatch boundary) look up the right entry
        // in `collector_sigs`. Without these arms, the sub-patch falls through
        // to `compute_node` which doesn't touch `collector_sigs` at all —
        // upstream Remapper / Collector overrides get dropped on the floor.
        if snap.module_id == "module.automap_fork" {
            automap_fork_publish(snap, ns_uid, &computed, dev_sigs, collector_sigs);
            computed[idx] = vec![None; snap.n_outputs];
            last_outputs.insert(ns_uid, computed[idx].clone());
            continue;
        }
        if snap.module_id == "module.automap_combiner" {
            automap_combiner_publish(snap, ns_uid, dev_sigs, collector_sigs);
            computed[idx] = vec![None];
            last_outputs.insert(ns_uid, computed[idx].clone());
            continue;
        }
        if snap.module_id == "module.automap_selector" {
            automap_selector_publish(snap, ns_uid, &computed, dev_sigs, collector_sigs);
            computed[idx] = vec![None];
            last_outputs.insert(ns_uid, computed[idx].clone());
            continue;
        }
        // Feedback Control inside a sub-patch — the common case (device pins
        // aren't reachable there, which is the whole reason this node exists).
        // The injection key is the PHYSICAL device id (stamped at build time),
        // not the uid, so no namespacing is needed; outlets read dev_sigs by the
        // stamped virtual destination id. Identical to the top-level arm.
        if snap.module_id == "module.feedback_control" {
            let out = feedback_control_publish(snap, &computed, dev_sigs, collector_sigs);
            last_outputs.insert(ns_uid, out.clone());
            computed[idx] = out;
            continue;
        }

        let inputs: Vec<Option<Signal>> = snap.input_sources.iter()
            .map(|src| src.and_then(|(si, op)| {
                computed.get(si).and_then(|v| v.get(op)).copied().flatten()
            }))
            .collect();

        let node_state = state.entry(ns_uid).or_insert_with(NodeState::default);
        if let Some(ref vals) = snap.aux_f32_override {
            node_state.aux_f32 = vals.clone();
        }
        let node_outputs = compute_node(snap, &inputs, node_state, dev_sigs, collector_sigs, dt);

        // ── 3DOF lean dispatch (subgraph eval) ───────────────────────────
        //
        // Mirror of the top-level dispatch — the 3DOF module commonly sits
        // inside a sub-patch (gyro pre-processing wrapped behind a clean
        // interface). Without this block, lean mappings inside a subpatch
        // wouldn't fire even though their UI works the same way.
        //
        // Uses `ns_uid` (namespaced UID) so the collector key matches what
        // `find_automap_device_rec` in app.rs computes when something
        // downstream traces back through the subpatch boundary.
        if snap.module_id == "processing.gyro_3dof" {
            lean_dispatch_into_collector_sigs(
                snap, ns_uid, &node_outputs, node_state, collector_sigs, dt,
            );
        }

        // Display state for inner nodes — keyed by namespaced UID so the UI walk
        // can find them when populating `node.extra.last_signals` / `history`.
        match snap.module_id.as_str() {
            "display.oscilloscope" | "display.readout" => {
                let sample = inputs.iter().map(|s| sig_to_f32(*s)).collect();
                scope_samples.push((ns_uid, sample));
                last_inputs.insert(ns_uid, inputs.clone());
            }
            "display.trigscope" => {
                // inputs[0] is trigger; inputs[1..] are data channels.
                // Emit all inputs so the UI can do trigger-edge detection.
                let sample = inputs.iter().map(|s| sig_to_f32(*s)).collect();
                scope_samples.push((ns_uid, sample));
                last_inputs.insert(ns_uid, inputs.clone());
            }
            "display.vectorscope" => {
                let sample = inputs.iter().flat_map(|sig| match sig {
                    Some(Signal::Vec2(v)) => [Some(v.x), Some(v.y)],
                    _ => [None, None],
                }).collect();
                scope_samples.push((ns_uid, sample));
                last_inputs.insert(ns_uid, inputs.clone());
            }
            "module.response_curve" | "module.vec_response_curve" => {
                last_inputs.insert(ns_uid, inputs.clone());
            }
            "module.twoway_response_curve" => {
                last_inputs.insert(ns_uid, inputs.clone());
            }
            "processing.gyro_3dof" => {
                last_inputs.insert(ns_uid, node_outputs.clone());
            }
            "generator.envelope" => {
                last_inputs.insert(ns_uid, node_state.last_signals.clone());
            }
            _ => {}
        }
        // Populate last_outputs for every node — used by the UI to drive the
        // per-pin signal glow on downstream device-sink inputs.
        last_outputs.insert(ns_uid, node_outputs.clone());

        computed[idx] = node_outputs;
    }

    computed
}

// ── Main graph tick ───────────────────────────────────────────────────────────

/// Evaluate one tick into `out`. The caller owns `out` and is expected to
/// reuse the same `TickOutput` across ticks — we `.clear()` at the top so
/// the HashMaps keep their allocated capacity between calls instead of
/// being dropped and reallocated. At 2 kHz this was a non-trivial source
/// of allocator pressure even on empty graphs.
pub fn eval_graph_tick(
    graph: &ProcessingGraph,
    state: &mut HashMap<usize, NodeState>,
    dev_sigs: &HashMap<(String, String), Signal>,
    dt: f32,
    out: &mut TickOutput,
) {
    puffin::profile_function!();
    out.clear();
    let n = graph.nodes.len();
    let mut computed: Vec<Vec<Option<Signal>>> = vec![vec![]; n];

    // Apply per-device source-side post-processing (stick deadzone + gyro
    // multiplier) ONCE up front so every downstream consumer — direct wires,
    // AutoMap split/collector, sink AutoMap, remapper — sees the processed
    // values. Avoids the prior leak where AutoMap pulled raw dev_sigs and
    // bypassed the source node's params.
    let dev_sigs_owned: HashMap<(String, String), Signal> = {
        puffin::profile_scope!("preprocess_dev_sigs");
        preprocess_dev_sigs(graph, dev_sigs)
    };
    let dev_sigs = &dev_sigs_owned;

    // Destructure with `ref mut` so the rest of the function can keep
    // using bare names (outputs, scope_samples, …) as mutable references.
    // Borrows live until the end of the function; final
    // `TickOutput { … }` packing is no longer needed.
    let TickOutput {
        ref mut outputs,
        ref mut scope_samples,
        ref mut last_inputs,
        ref mut last_outputs,
        ref mut sink_outputs,
    } = *out;
    // Signals injected by AutoMap Collector nodes, keyed by ("collector:{uid}", pin_id).
    let mut collector_sigs: HashMap<(String, String), Signal> = HashMap::new();

    {
    puffin::profile_scope!("main_node_loop");
    for (idx, snap) in graph.nodes.iter().enumerate() {
        // ── module.map_action: AutoMap in → Bool out based on stored mappings ──
        if snap.module_id == "module.map_action" {
            // Shared Map Action evaluation (identical at top level and sub-patch).
            computed[idx] = eval_map_action_node(snap, snap.node_uid, dev_sigs, &collector_sigs, state, dt);
            last_outputs.insert(snap.node_uid, computed[idx].clone());
            continue;
        }

        // ── module.remapper: pass-through + per-mapping override + consume ────
        if snap.module_id == "module.remapper" {
            // Shared Remapper evaluation (identical at top level and in sub-patches).
            eval_remapper_node(snap, snap.node_uid, dev_sigs, &mut collector_sigs, state, dt);

            computed[idx] = vec![None];
            continue;
        }

        // ── module.automap_collect: inject individual inputs into collector_sigs ──
        //
        // Two-phase write into collector_sigs[("collector:{uid}", pin)]:
        //   1. Pass-through pins from the upstream AutoMap bus — pulled
        //      either from upstream `collector_sigs` (if upstream is a
        //      Remapper/Collector/Fork/Selector/Combiner/Lean) or from
        //      raw `dev_sigs` (if upstream is a physical device). This
        //      ensures mapped output pins from an upstream Remapper still
        //      reach downstream sinks even though the user didn't add
        //      those pins via the Collector's "+" dropdown.
        //   2. Explicit collected-pin overrides — values wired by the user
        //      to the collector's individual input ports. These win over
        //      pass-through for the same pin id.
        if snap.module_id == "module.automap_collect" {
            let inputs: Vec<Option<Signal>> = snap.input_sources.iter()
                .map(|src| src.and_then(|(si, op)| {
                    computed.get(si).and_then(|v| v.get(op)).copied().flatten()
                }))
                .collect();
            let collect_ids = snap.params.get("_collect_pin_ids")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect::<Vec<_>>())
                .unwrap_or_default();
            let uid_key = format!("collector:{}", snap.node_uid);

            // Phase 1: pass-through from upstream AutoMap source.
            // Iterating upstream's actual collector_sigs entries (not just
            // `ALL_PINS`) is required so off-spec pin names — Remapper's
            // mapped keyboard keys like `key_f`, custom mouse buttons, etc.
            // — also flow through. `ALL_PINS` only covers canonical pin ids.
            let upstream_dev = snap.params.get("_automap_device_id")
                .and_then(|v| v.as_str()).unwrap_or("").to_string();
            let upstream_collector = snap.params.get("_automap_collector_id")
                .and_then(|v| v.as_str()).unwrap_or("").to_string();
            if !upstream_collector.is_empty() {
                // Copy EVERY entry from the upstream collector key.
                let copies: Vec<(String, Signal)> = collector_sigs.iter()
                    .filter(|((dev, _), _)| dev == &upstream_collector)
                    .map(|((_, pin), sig)| (pin.clone(), *sig))
                    .collect();
                for (pin, sig) in copies {
                    collector_sigs.insert((uid_key.clone(), pin), sig);
                }
                // For canonical pins not present on the upstream collector
                // (e.g. when upstream is a Remapper that only writes mapped
                // pins, not pass-through), fall back to raw device samples.
                if !upstream_dev.is_empty() {
                    for pin in flexinput_core::automap::ALL_PINS {
                        let key = (uid_key.clone(), pin.id.to_string());
                        if collector_sigs.contains_key(&key) { continue; }
                        if let Some(&sig) = dev_sigs.get(&(upstream_dev.clone(), pin.id.to_string())) {
                            collector_sigs.insert(key, sig);
                        }
                    }
                }
            } else if !upstream_dev.is_empty() {
                // No upstream collector — pure raw device pass-through.
                for pin in flexinput_core::automap::ALL_PINS {
                    if let Some(&sig) = dev_sigs.get(&(upstream_dev.clone(), pin.id.to_string())) {
                        collector_sigs.insert((uid_key.clone(), pin.id.to_string()), sig);
                    }
                }
            }

            // Phase 2: explicit collected-pin overrides (win over pass-through).
            for (i, pin_id) in collect_ids.iter().enumerate() {
                if let Some(sig) = inputs.get(i + 1).and_then(|s| *s) {
                    if !pin_id.is_empty() {
                        collector_sigs.insert((uid_key.clone(), pin_id.clone()), sig);
                    }
                }
            }
            computed[idx] = vec![None]; // AutoMap passthrough: no signal value
            continue;
        }

        // ── module.automap_fork: gate AutoMap bus to selected output ─────────
        if snap.module_id == "module.automap_fork" {
            automap_fork_publish(snap, snap.node_uid, &computed, dev_sigs, &mut collector_sigs);
            computed[idx] = vec![None; snap.n_outputs];
            continue;
        }

        // ── module.automap_combiner: merge N AutoMap inputs per per-pin policy ──
        // Default policy SORT: walk inputs top-down (lowest port = highest priority);
        // first asserted value wins. Per-pin overrides in `combiner_pin_policy`:
        //   - OR  : Bool = logical OR;  Float = max(|x|) preserving sign of max
        //   - AND : Bool = logical AND; Float = min(|x|) preserving sign of min
        //   - XOR : Bool = parity;      Float = |a - b| (folded across all inputs)
        //   - ADD : sum, clamped per pin (triggers [0,1], sticks/axes [-1,1])
        //   - MULT: product, clamped per pin
        // Writes into collector_sigs under "combiner:{uid}".
        if snap.module_id == "module.automap_combiner" {
            automap_combiner_publish(snap, snap.node_uid, dev_sigs, &mut collector_sigs);
            computed[idx] = vec![None];
            continue;
        }
        // ── module.automap_selector: gate selected AutoMap input to output ────
        if snap.module_id == "module.automap_selector" {
            automap_selector_publish(snap, snap.node_uid, &computed, dev_sigs, &mut collector_sigs);
            computed[idx] = vec![None];
            continue;
        }
        // ── module.feedback_control: inject inlets into the physical pad's
        //    feedback channel; tap outlets from the virtual destination. ──────
        if snap.module_id == "module.feedback_control" {
            let out = feedback_control_publish(snap, &computed, dev_sigs, &mut collector_sigs);
            last_outputs.insert(snap.node_uid, out.clone());
            computed[idx] = out;
            continue;
        }
        // ── device.sink: collect combined inputs, populate sink_outputs ──────
        if let Some(ref st) = snap.sink_target {
            // Pins that have at least one actual direct wire (non-empty multi_sources).
            // These take priority over auto-mapped signals for the same pin.
            let directly_wired: HashSet<&str> = st.pin_ids.iter().enumerate()
                .filter(|(i, pid)| !pid.is_empty() && st.multi_sources.get(*i).map_or(false, |s| !s.is_empty()))
                .map(|(_, pid)| pid.as_str())
                .collect();

            // Per-sink scaling params. Currently: mouse sensitivity on
            // virtual.keymouse — applied to mouse_x / mouse_y / mouse pins.
            let mouse_sens = snap.params.get("mouse_sensitivity")
                .and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let scale_for_sink = |pin_id: &str, sig: Signal| -> Signal {
                if st.device_id.starts_with("virtual.keymouse") && is_mouse_pin(pin_id)
                    && (mouse_sens - 1.0).abs() > f32::EPSILON
                {
                    match sig {
                        Signal::Float(v) => Signal::Float(v * mouse_sens),
                        Signal::Vec2(v)  => Signal::Vec2(v * mouse_sens),
                        other => other,
                    }
                } else { sig }
            };

            // Direct-wire inputs (possibly multi-source per pin, combined additively).
            //
            // Self-sink nodes (device.source whose feedback inputs loop back to
            // their own outputs, directly or via a Splitter/Math chain) are
            // deferred to a post-pass below: their upstream chain only fills
            // `computed[]` after this iteration runs, so we wait until the main
            // loop completes before reading.
            if !st.is_self_sink {
                for (in_idx, pin_id) in st.pin_ids.iter().enumerate() {
                    if pin_id.is_empty() { continue; }
                    let mut combined: Option<Signal> = None;
                    if let Some(sources) = st.multi_sources.get(in_idx) {
                        for &(src_idx, out_pin) in sources {
                            if let Some(Some(sig)) = computed.get(src_idx).and_then(|v| v.get(out_pin)) {
                                combined = Some(match combined {
                                    None => *sig,
                                    Some(prev) => combine_signals(prev, *sig),
                                });
                            }
                        }
                    }
                    if let Some(sig) = combined {
                        sink_outputs.insert((st.device_id.clone(), pin_id.clone()), scale_for_sink(pin_id, sig));
                    }
                }
            }

            // AutoMap: semantic-map source device pins → sink device pins.
            // Uses resolve_mapping() for cross-family translation (e.g. btn_cross → btn_south).
            if let Some((ref src_dev, ref src_pins)) = st.automap_source {
                let dst_ids: Vec<&str> = st.pin_ids.iter()
                    .filter(|pid| !pid.is_empty())
                    .map(|pid| pid.as_str())
                    .collect();
                let src_ids: Vec<&str> = src_pins.iter()
                    .filter(|p| !p.is_empty() && p.as_str() != "automap_out")
                    .map(|p| p.as_str())
                    .collect();
                let is_collector = src_dev.starts_with("collector:")
                    || src_dev.starts_with("forksel:")
                    || src_dev.starts_with("remap:")
                    || src_dev.starts_with("combiner:")
                    || src_dev.starts_with("lean:");
                // Digital→analog trigger bridges (`btn_lt_dig`→`left_trigger`,
                // `btn_rt_dig`→`right_trigger`) are a LOWEST-PRIORITY fallback:
                // they only fill the analog trigger when no primary source — the
                // real analog `left_trigger`/`right_trigger`, a manually-injected
                // AutoMap value, or a Remapper analog mapping — already drove it.
                // Deferred to a second pass so primaries (processed first) win.
                let mut deferred_digital_triggers: Vec<(&str, &str)> = Vec::new();
                let resolve_sig = |mapped_src: &str| -> Option<Signal> {
                    if is_collector {
                        collector_sigs.get(&(src_dev.clone(), mapped_src.to_string())).copied()
                            .or_else(|| {
                                st.automap_fallback_dev.as_ref().and_then(|fb| {
                                    dev_sigs.get(&(fb.clone(), mapped_src.to_string())).copied()
                                })
                            })
                    } else {
                        dev_sigs.get(&(src_dev.clone(), mapped_src.to_string())).copied()
                    }
                };
                for (mapped_src, mapped_dst) in automap::resolve_mapping(&src_ids, &dst_ids) {
                    if directly_wired.contains(mapped_dst) { continue; }
                    let is_digital_trigger_bridge =
                        matches!((mapped_src, mapped_dst),
                            ("btn_lt_dig", "left_trigger") | ("btn_rt_dig", "right_trigger"));
                    if is_digital_trigger_bridge {
                        // Only honour the bridge when the upstream source opted in
                        // (or is a digital-only pad). Otherwise a pad with real
                        // analog triggers would have its digital button leak into
                        // the analog trigger.
                        if st.digital_trigger_bridge {
                            deferred_digital_triggers.push((mapped_src, mapped_dst));
                        }
                        continue;
                    }
                    if let Some(sig) = resolve_sig(mapped_src) {
                        // Type coercion (Bool↔Float) is performed by the virtual device's
                        // send() via Signal::as_float / as_bool, so we just hand the raw
                        // signal off — semantic groups already routed it to the right pin.
                        sink_outputs
                            .entry((st.device_id.clone(), mapped_dst.to_string()))
                            .or_insert(scale_for_sink(mapped_dst, sig));
                    }
                }
                // Second pass: digital-trigger fallback. Writes the analog trigger
                // ONLY when a primary source didn't (real analog, manual injection,
                // or Remapper analog). The digital button drives the FULL value —
                // pressed → 1.0, released → 0.0. We must write the 0.0 on release
                // too, otherwise the trigger latches at its last pressed value and
                // never lets go. On a mixed pad the real analog trigger always
                // writes a primary (even 0.0), so `contains_key` skips this and the
                // real analog wins as intended.
                for (mapped_src, mapped_dst) in deferred_digital_triggers {
                    let key = (st.device_id.clone(), mapped_dst.to_string());
                    if sink_outputs.contains_key(&key) { continue; }
                    if let Some(sig) = resolve_sig(mapped_src) {
                        let v = if sig.as_bool() { 1.0 } else { 0.0 };
                        sink_outputs.insert(key, Signal::Float(v));
                    }
                }
                // Wildcard pass-through for virtual keyboard/mouse sinks: forward EVERY
                // collector-injected signal to the sink as-is (using the source pin name
                // verbatim).  The sink's send() handles arbitrary key names through its
                // learned_keys fallback, so users can drive any custom key (F1, Space,
                // letters, …) by adding it to the Collector via the Learn-key UI.
                if is_collector && st.device_id.starts_with("virtual.keymouse") {
                    for ((dev, pin), &sig) in collector_sigs.iter() {
                        if dev != src_dev { continue; }
                        if directly_wired.contains(pin.as_str()) { continue; }
                        sink_outputs
                            .entry((st.device_id.clone(), pin.clone()))
                            .or_insert(scale_for_sink(pin, sig));
                    }
                }
            }

            // Resolve Vec2 vs individual axis conflicts (they write the same hardware registers).
            // Priority: directly-wired axes beat auto-mapped Vec2; Vec2 wins in all other cases.
            const STICK_GROUPS: &[(&str, &[&str])] = &[
                ("left_stick",  &["left_stick_x", "left_stick_y"]),
                ("right_stick", &["right_stick_x", "right_stick_y"]),
                ("dpad",        &["dpad_x", "dpad_y"]),
            ];
            for &(vec2_pin, axis_pins) in STICK_GROUPS {
                let has_vec2     = sink_outputs.contains_key(&(st.device_id.clone(), vec2_pin.to_string()));
                let has_any_axis = axis_pins.iter().any(|p| sink_outputs.contains_key(&(st.device_id.clone(), p.to_string())));
                if !has_vec2 || !has_any_axis { continue; }
                let vec2_direct     = directly_wired.contains(vec2_pin);
                let any_axis_direct = axis_pins.iter().any(|p| directly_wired.contains(*p));
                if any_axis_direct && !vec2_direct {
                    sink_outputs.remove(&(st.device_id.clone(), vec2_pin.to_string()));
                } else {
                    for &axis_pin in axis_pins {
                        sink_outputs.remove(&(st.device_id.clone(), axis_pin.to_string()));
                    }
                }
            }

            // AutoMap feedback channel: signals flow BACKWARD along AutoMap wires
            // from virtual sinks to physical haptic inputs. Each virtual sink that
            // auto-maps FROM this device contributes its rumble/lightbar outputs
            // to matching haptic input pins on this device, silently and without
            // explicit user wiring. Direct wires (in `directly_wired`) take priority.
            if !st.feedback_sources.is_empty() {
                let dst_pins: Vec<&str> = st.pin_ids.iter()
                    .filter(|p| !p.is_empty())
                    .map(|p| p.as_str())
                    .collect();
                for virt_dev in &st.feedback_sources {
                    for (virt_out_pin, _) in flexinput_core::automap::FEEDBACK_PAIRS.iter() {
                        let Some(&sig) = dev_sigs.get(&(virt_dev.clone(), virt_out_pin.to_string())) else {
                            continue;
                        };
                        let dst = flexinput_core::automap::resolve_feedback_pin(virt_out_pin, &dst_pins);
                        // Diagnostic: when FLEXINPUT_DEBUG_RUMBLE is set, log
                        // feedback routing for non-zero rumble so we can see if
                        // the virtual pad's value reaches the physical sink and
                        // which dst pin it resolves to (or that it doesn't).
                        if std::env::var_os("FLEXINPUT_DEBUG_RUMBLE").is_some()
                            && matches!(virt_out_pin, &"rumble_strong" | &"rumble_weak")
                            && sig.as_float().abs() > 0.01
                        {
                            eprintln!(
                                "[fb] {virt_dev} {virt_out_pin}={:.2} -> {} dst={:?} dst_pins={:?}",
                                sig.as_float(), st.device_id, dst, dst_pins
                            );
                        }
                        let Some(dst_pin) = dst else { continue; };
                        if directly_wired.contains(dst_pin) { continue; }
                        sink_outputs
                            .entry((st.device_id.clone(), dst_pin.to_string()))
                            .or_insert(sig);
                    }
                }
            }

            // (Feedback Control injection is drained in a post-pass after the
            //  main loop — see below — so every injector node has run first.)

            // device.source nodes with haptic inputs, and device.sink nodes with
            // feedback output pins, still need output computation — don't skip them.
            if snap.module_id != "device.source" && snap.n_outputs == 0 {
                computed[idx] = vec![];
                continue;
            }
        }

        // ── inline sub-patch: run inner graph and map outlet outputs ──────────
        if let Some(ref sg) = snap.inline_subgraph {
            let outer_inputs: Vec<Option<Signal>> = snap.input_sources.iter()
                .map(|src| src.and_then(|(si, op)| {
                    computed.get(si).and_then(|v| v.get(op)).copied().flatten()
                }))
                .collect();
            let inner_computed = eval_subgraph(
                &sg.graph, &outer_inputs, state, dev_sigs, &mut collector_sigs,
                scope_samples, last_inputs, last_outputs, snap.node_uid, dt,
            );
            let out: Vec<Option<Signal>> = sg.outlet_locs.iter()
                .map(|loc| loc.and_then(|(ni, np)| inner_computed.get(ni).and_then(|v| v.get(np)).copied().flatten()))
                .collect();
            last_outputs.insert(snap.node_uid, out.clone());
            computed[idx] = out;
            continue;
        }

        let inputs: Vec<Option<Signal>> = snap.input_sources.iter()
            .map(|src| src.and_then(|(src_idx, out_pin)| {
                computed.get(src_idx).and_then(|v| v.get(out_pin)).copied().flatten()
            }))
            .collect();

        let node_state = state.entry(snap.node_uid).or_insert_with(NodeState::default);

        // Apply any pending state override (e.g. counter reset from UI).
        if let Some(ref vals) = snap.aux_f32_override {
            node_state.aux_f32 = vals.clone();
        }

        let node_outputs = compute_node(snap, &inputs, node_state, dev_sigs, &collector_sigs, dt);

        // ── 3DOF lean dispatch: emit per-pin signals via the Map output ──
        //
        // The lean_left / lean_right sections each own an array of mappings
        // (Map-Action-shaped: `{ out, mode, window_ms, sustain, turbo }`).
        // A mapping's raw-held bool is whether the lean magnitude has
        // crossed `lean_threshold` on the corresponding side. The held
        // value flows through the standard press-mode pipeline (down /
        // short / long / double / on_press / on_release / turbo) the same
        // way Map Action does. Asserted output pins are written into
        // `collector_sigs` under "lean:{uid}" so a downstream AutoMap
        // collector (or subpatch outlet) routes them to gamepad/KB sinks.
        //
        // Analog mode is special: instead of treating the side as a
        // raw_held bool, it drives the destination through the shared
        // `analog_digital_pulse` modulator — Hold → PWM (duty = |lean|),
        // Turbo → ×2 max frequency, plain → tap train whose frequency
        // tracks |lean|. Released mappings and below-threshold leans
        // produce no pulses. Press-state slot [0] tracks the phase seconds.
        if snap.module_id == "processing.gyro_3dof" {
            lean_dispatch_into_collector_sigs(
                snap, snap.node_uid, &node_outputs, node_state,
                &mut collector_sigs, dt,
            );
        }

        match snap.module_id.as_str() {
            "display.oscilloscope" | "display.readout" => {
                let sample = inputs.iter().map(|s| sig_to_f32(*s)).collect();
                scope_samples.push((snap.node_uid, sample));
                last_inputs.insert(snap.node_uid, inputs.clone());
            }
            "display.trigscope" => {
                let sample = inputs.iter().map(|s| sig_to_f32(*s)).collect();
                scope_samples.push((snap.node_uid, sample));
                last_inputs.insert(snap.node_uid, inputs.clone());
            }
            "display.vectorscope" => {
                let sample = inputs.iter().flat_map(|sig| match sig {
                    Some(Signal::Vec2(v)) => [Some(v.x), Some(v.y)],
                    _ => [None, None],
                }).collect();
                scope_samples.push((snap.node_uid, sample));
                last_inputs.insert(snap.node_uid, inputs.clone());
            }
            "module.response_curve" | "module.vec_response_curve" => {
                last_inputs.insert(snap.node_uid, inputs.clone());
            }
            "module.twoway_response_curve" => {
                last_inputs.insert(snap.node_uid, inputs.clone());
            }
            "generator.envelope" => {
                // last_signals = [output, phase]; UI reads phase from index 1 for playhead
                last_inputs.insert(snap.node_uid, node_state.last_signals.clone());
            }
            // Export outputs (not inputs) so the UI body can show a live readout.
            "processing.gyro_3dof" => {
                last_inputs.insert(snap.node_uid, node_outputs.clone());
            }
            _ => {}
        }
        // Populate last_outputs for every node — used by the UI to drive the
        // per-pin signal glow on downstream device-sink inputs.
        last_outputs.insert(snap.node_uid, node_outputs.clone());

        // Exclude device.source from the exported outputs; UI evaluates those fresh.
        if snap.module_id != "device.source" {
            for (out_pin, sig) in node_outputs.iter().enumerate() {
                outputs.insert((snap.node_uid, out_pin), *sig);
            }
        }

        computed[idx] = node_outputs;
    }
    } // end main_node_loop

    // Post-pass: device.source self-sinks (feedback inputs wired back to their
    // own outputs, possibly through Splitter/Math chains). Their multi_sources
    // can only be read after the main loop has filled `computed[]` for the
    // whole graph — by then any chain that loops through this node has its
    // value, and we can route it into sink_outputs like any other sink.
    puffin::profile_scope!("self_sink_post_pass");
    for (idx, snap) in graph.nodes.iter().enumerate() {
        let Some(ref st) = snap.sink_target else { continue; };
        if !st.is_self_sink { continue; }
        for (in_idx, pin_id) in st.pin_ids.iter().enumerate() {
            if pin_id.is_empty() { continue; }
            let mut combined: Option<Signal> = None;
            if let Some(sources) = st.multi_sources.get(in_idx) {
                for &(src_idx, out_pin) in sources {
                    let sig_opt: Option<Signal> = computed.get(src_idx)
                        .and_then(|v| v.get(out_pin))
                        .copied()
                        .flatten()
                        .or_else(|| {
                            // Direct self-wire: source's own `computed[idx]` is
                            // the result of this same tick's compute_node, which
                            // for device.source mirrors dev_sigs anyway.
                            if src_idx != idx { return None; }
                            let src_pin = snap.output_pin_ids.get(out_pin)?;
                            if src_pin.is_empty() { return None; }
                            dev_sigs.get(&(st.device_id.clone(), src_pin.clone())).copied()
                        });
                    if let Some(sig) = sig_opt {
                        combined = Some(match combined {
                            None => sig,
                            Some(prev) => combine_signals(prev, sig),
                        });
                    }
                }
            }
            if let Some(sig) = combined {
                sink_outputs.insert((st.device_id.clone(), pin_id.clone()), sig);
            }
        }
    }

    // Post-pass: Feedback Control injection drain. Runs AFTER the main loop so
    // every `module.feedback_control` node — at the top level or nested in any
    // sub-patch — has already written its inlet values into `collector_sigs`
    // under `feedback_inject:{physical_dev_id}`. For each physical sink, route
    // those values to the device's haptic inputs (direct pin-id match first,
    // then `resolve_feedback_pin` rumble/lightbar aliasing). Direct wires and
    // the auto-feedback in the main loop both win via `or_insert`.
    //
    // Cheap early-out: skip entirely unless at least one injector wrote this
    // tick (the common case is no Feedback Control nodes at all).
    let has_injection = collector_sigs.keys()
        .any(|(dev, _)| dev.starts_with("feedback_inject:"));
    if has_injection {
        puffin::profile_scope!("feedback_inject_post_pass");
        for snap in graph.nodes.iter() {
            let Some(ref st) = snap.sink_target else { continue; };
            if st.device_id.starts_with("virtual.") { continue; }
            let inject_key = format!("feedback_inject:{}", st.device_id);
            let dst_pins: Vec<&str> = st.pin_ids.iter()
                .filter(|p| !p.is_empty())
                .map(|p| p.as_str())
                .collect();
            // Pins with at least one real direct wire keep priority.
            let directly_wired: std::collections::HashSet<&str> = st.pin_ids.iter().enumerate()
                .filter(|(i, pid)| !pid.is_empty() && st.multi_sources.get(*i).map_or(false, |s| !s.is_empty()))
                .map(|(_, pid)| pid.as_str())
                .collect();
            for pin in flexinput_core::automap::FEEDBACK_INLET_PINS {
                let Some(&sig) = collector_sigs.get(&(inject_key.clone(), pin.id.to_string()))
                else { continue; };
                let dst_pin = if dst_pins.iter().any(|&p| p == pin.id) {
                    Some(pin.id)
                } else {
                    flexinput_core::automap::resolve_feedback_pin(pin.id, &dst_pins)
                };
                let Some(dst_pin) = dst_pin else { continue; };
                if directly_wired.contains(dst_pin) { continue; }
                // Precedence: direct wire > injection > auto-feedback. The
                // main-loop auto-feedback pass may have already `or_insert`-ed a
                // value for this pin — typically `0.0` (the virtual sink's idle
                // rumble when no game is driving it). A plain `or_insert` here
                // would let that idle `0.0` mask the user's explicit injection,
                // producing only a brief buzz on the rising edge. Instead COMBINE
                // additively (clamped) so injection adds on top of any real game
                // rumble and overrides idle silence.
                use std::collections::hash_map::Entry;
                match sink_outputs.entry((st.device_id.clone(), dst_pin.to_string())) {
                    Entry::Occupied(mut o) => {
                        let merged = combine_signals(*o.get(), sig);
                        *o.get_mut() = clamp_feedback_signal(dst_pin, merged);
                    }
                    Entry::Vacant(v) => { v.insert(sig); }
                }
            }
        }
    }
}

/// Clamp a combined feedback value to the valid range for its haptic pin so
/// additive merging (game rumble + injected effect) can't overflow. Amplitudes
/// and most haptic pins are 0–1; everything falls back to 0–1 which is correct
/// for the rumble/lightbar/amp pins the Feedback Control node injects.
fn clamp_feedback_signal(_pin: &str, sig: Signal) -> Signal {
    match sig {
        Signal::Float(f) => Signal::Float(f.clamp(0.0, 1.0)),
        other => other,
    }
}

// ── Per-node dispatch ─────────────────────────────────────────────────────────

fn compute_node(
    snap: &NodeSnap,
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &HashMap<(String, String), Signal>,
    dt: f32,
) -> Vec<Option<Signal>> {
    puffin::profile_function!();
    match snap.module_id.as_str() {
        "device.source" => {
            // Deadzone + gyro multiplier already applied in `preprocess_dev_sigs`
            // at the top of `eval_graph_tick` so AutoMap/splitter/collector see
            // the same processed values via raw dev_sigs reads.
            let dev_id = snap.device_id.as_deref().unwrap_or("");
            (0..snap.n_outputs).map(|i| {
                let pin_id = snap.output_pin_ids.get(i).map(|s| s.as_str()).unwrap_or("");
                if pin_id.is_empty() { return None; }
                dev_sigs.get(&(dev_id.to_string(), pin_id.to_string())).copied()
            }).collect()
        }
        "module.automap_split" => {
            let dev_id = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("");
            // The collector_id (set by build_processing_graph) is the closest
            // upstream collector in the AutoMap wire chain. Splitter prefers its
            // injected/overridden signals over the raw device samples so the
            // probe reflects the most recent state along the chain.
            let collector_id = snap.params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("");
            (0..snap.n_outputs).map(|i| {
                let pin_id = snap.output_pin_ids.get(i).map(|s| s.as_str()).unwrap_or("");
                // "automap_pass" or empty = the AutoMap passthrough slot — no signal value.
                if pin_id.is_empty() || pin_id == "automap_pass" { return None; }
                if !collector_id.is_empty() {
                    if let Some(&sig) = collector_sigs.get(&(collector_id.to_string(), pin_id.to_string())) {
                        return Some(sig);
                    }
                }
                dev_sigs.get(&(dev_id.to_string(), pin_id.to_string())).copied()
            }).collect()
        }
        "module.constant" | "module.knob" => {
            let v = snap.params.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            vec![Some(Signal::Float(v))]
        }
        "module.switch" => {
            // The engine is the sole authority on `active`. The UI signals its
            // intent through a monotonically-increasing `ui_toggle_seq` counter
            // (bumped on each button click); the engine compares against its
            // last-seen value and toggles. This avoids the two-writer race that
            // happens when both UI and engine modify the same `active` param.
            //
            //   aux_f32[0] = current `active`         (0/1)
            //   aux_f32[1] = previous `latch` level   (0/1)
            //   aux_f32[2] = last-seen ui_toggle_seq  (truncated to f32; suitable
            //                for counters well past patch lifetime — wraparound
            //                isn't a concern in practice and a mismatch just
            //                toggles once).
            //   aux_f32[3] = init flag                (0 until first tick)
            while state.aux_f32.len() < 4 { state.aux_f32.push(0.0); }
            let initialised = state.aux_f32[3] > 0.5;
            let prev_active = state.aux_f32[0] > 0.5;
            let prev_latch  = state.aux_f32[1] > 0.5;
            let prev_seq    = state.aux_f32[2];

            let saved_active = snap.params.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
            let cur_seq = snap.params.get("ui_toggle_seq")
                .and_then(|v| v.as_u64()).unwrap_or(0) as f32;
            let direct = inputs.get(0).copied().flatten()
                .and_then(|s| s.coerce_to(SignalType::Bool))
                .map(|s| matches!(s, Signal::Bool(true))).unwrap_or(false);
            let latch  = inputs.get(1).copied().flatten()
                .and_then(|s| s.coerce_to(SignalType::Bool))
                .map(|s| matches!(s, Signal::Bool(true))).unwrap_or(false);

            // First tick after load: adopt the persisted `active` so saved
            // patches reopen in their stored state.
            let mut active = if initialised { prev_active } else { saved_active };

            // UI clicks since last tick: toggle once per increment of the
            // sequence counter. We can't replay individual clicks if many
            // happened between ticks, so collapse to "differs → toggle once".
            if initialised && cur_seq != prev_seq {
                active = !active;
            }
            // Latch rising edge → toggle.
            if latch && !prev_latch {
                active = !active;
            }
            // Direct HIGH → force ON; falling edge does not force OFF.
            if direct {
                active = true;
            }

            state.aux_f32[0] = if active { 1.0 } else { 0.0 };
            state.aux_f32[1] = if latch  { 1.0 } else { 0.0 };
            state.aux_f32[2] = cur_seq;
            state.aux_f32[3] = 1.0;

            let out = vec![Some(Signal::Bool(active))];
            state.last_signals = out.clone();
            out
        }
        "module.dropdown" => {
            let n = snap.params.get("options")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            let idx = snap.params.get("selected_index")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            if n == 0 {
                vec![Some(Signal::Float(0.0)), Some(Signal::Int(0))]
            } else {
                let idx = idx.min(n - 1);
                // Centre-of-bucket quantisation: matches the inverse mapping
                // used by `Selector`/`Split` ((sel*N).floor()), so wiring the
                // float output into a Selector with N inputs selects bucket
                // `idx` exactly.
                let f = (idx as f32 + 0.5) / n as f32;
                vec![Some(Signal::Float(f)), Some(Signal::Int(idx as i32))]
            }
        }
        "generator.oscillator" => {
            let out = compute_oscillator(inputs, state, &snap.params, dt);
            state.last_signals = out.clone();
            out
        }
        "generator.envelope" => {
            // last_signals set inside compute_envelope: [output, phase]
            compute_envelope(inputs, state, &snap.params, dt)
        }
        "module.delay" => {
            let out = compute_delay(inputs, state, &snap.params);
            state.last_signals = out.clone();
            out
        }
        "module.average" => {
            let out = compute_average(inputs, state, &snap.params);
            state.last_signals = out.clone();
            out
        }
        "module.dc_filter" => {
            let out = compute_dc_filter(inputs, state, &snap.params, dt);
            state.last_signals = out.clone();
            out
        }
        "module.twoway_response_curve" => {
            let out = compute_twoway_response_curve(inputs, state, &snap.params, dt);
            state.last_signals = out.clone();
            out
        }
        "logic.has_changed" => {
            let out = compute_has_changed(inputs, state);
            state.last_signals = out.clone();
            out
        }
        "logic.delay" => {
            let out = compute_logic_delay(inputs, state, &snap.params, dt);
            state.last_signals = out.clone();
            out
        }
        "logic.counter" => {
            let out = compute_counter(inputs, state, &snap.params);
            state.last_signals = out.clone();
            out
        }
        "processing.gyro_3dof" => {
            let out = compute_gyro_3dof(inputs, state, &snap.params, dev_sigs, collector_sigs, dt);
            state.last_signals = out.clone();
            out
        }
        "module.response_curve" | "module.vec_response_curve" => {
            state.last_signals = inputs.to_vec();
            (0..snap.n_outputs).map(|out_idx| {
                eval_pure(&snap.module_id, out_idx, inputs, &snap.params, snap.n_outputs)
            }).collect()
        }
        "display.oscilloscope" | "display.vectorscope" | "display.readout" => vec![],
        "device.sink" => {
            if snap.n_outputs == 0 { return vec![]; }
            let dev_id = snap.device_id.as_deref().unwrap_or("");
            let dz = snap.params.get("deadzone").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            (0..snap.n_outputs).map(|i| {
                let pin_id = snap.output_pin_ids.get(i).map(|s| s.as_str()).unwrap_or("");
                if pin_id.is_empty() { return None; }
                let sig = dev_sigs.get(&(dev_id.to_string(), pin_id.to_string())).copied()?;
                Some(if dz > 0.0 && is_stick_pin(pin_id) { apply_deadzone(sig, dz) } else { sig })
            }).collect()
        }
        "subpatch.inlet" => vec![],
        "subpatch.outlet" => vec![inputs.first().copied().flatten()],
        id => {
            (0..snap.n_outputs).map(|out_idx| {
                eval_pure(id, out_idx, inputs, &snap.params, snap.n_outputs)
            }).collect()
        }
    }
}

// ── Pure module evaluation ────────────────────────────────────────────────────

pub fn eval_pure(
    id: &str,
    out_idx: usize,
    inputs: &[Option<Signal>],
    params: &HashMap<String, Value>,
    n_outputs: usize,
) -> Option<Signal> {
    let param_f = |name: &str, default: f32| -> f32 {
        params.get(name).and_then(|v| v.as_f64()).map(|f| f as f32).unwrap_or(default)
    };

    match id {
        "math.add" => {
            if inputs.iter().any(|s| matches!(s, Some(Signal::Vec2(_)))) {
                let sum = (0..inputs.len())
                    .map(|i| get_v2(inputs, i, 0.0))
                    .fold(Vec2::ZERO, |acc, v| acc + v);
                Some(Signal::Vec2(sum))
            } else {
                Some(Signal::Float((0..inputs.len()).map(|i| get_f(inputs, i, 0.0)).sum()))
            }
        }
        "math.subtract" => {
            if inputs.iter().any(|s| matches!(s, Some(Signal::Vec2(_)))) {
                let first = get_v2(inputs, 0, 0.0);
                let rest = (1..inputs.len()).map(|i| get_v2(inputs, i, 0.0)).fold(Vec2::ZERO, |acc, v| acc + v);
                Some(Signal::Vec2(first - rest))
            } else {
                let first = get_f(inputs, 0, 0.0);
                let rest: f32 = (1..inputs.len()).map(|i| get_f(inputs, i, 0.0)).sum();
                Some(Signal::Float(first - rest))
            }
        }
        "math.multiply" => {
            if inputs.iter().any(|s| matches!(s, Some(Signal::Vec2(_)))) {
                let first = get_v2(inputs, 0, 0.0);
                let scale = (1..inputs.len()).map(|i| get_v2(inputs, i, 1.0)).fold(Vec2::ONE, |acc, v| acc * v);
                Some(Signal::Vec2(first * scale))
            } else {
                let first = get_f(inputs, 0, 0.0);
                let rest: f32 = (1..inputs.len()).map(|i| get_f(inputs, i, 1.0)).product();
                Some(Signal::Float(first * rest))
            }
        }
        "math.divide" => {
            if inputs.iter().any(|s| matches!(s, Some(Signal::Vec2(_)))) {
                let mut v = get_v2(inputs, 0, 0.0);
                for i in 1..inputs.len() {
                    let d = get_v2(inputs, i, 1.0);
                    v = Vec2::new(
                        if d.x == 0.0 { 0.0 } else { v.x / d.x },
                        if d.y == 0.0 { 0.0 } else { v.y / d.y },
                    );
                }
                Some(Signal::Vec2(v))
            } else {
                let mut v = get_f(inputs, 0, 0.0);
                for i in 1..inputs.len() {
                    let d = get_f(inputs, i, 1.0);
                    v = if d == 0.0 { 0.0 } else { v / d };
                }
                Some(Signal::Float(v))
            }
        }
        "math.abs" => match inputs.get(0).and_then(|s| *s) {
            Some(Signal::Vec2(v)) => Some(Signal::Vec2(v.abs())),
            other => Some(Signal::Float(other.map(|s| s.as_float()).unwrap_or(0.0).abs())),
        },
        "math.negate" => match inputs.get(0).and_then(|s| *s) {
            Some(Signal::Vec2(v)) => Some(Signal::Vec2(-v)),
            other => Some(Signal::Float(-other.map(|s| s.as_float()).unwrap_or(0.0))),
        },
        "math.clamp"  => {
            let min = if inputs.get(1).and_then(|s| *s).is_some() { get_f(inputs, 1, -1.0) } else { param_f("min", -1.0) };
            let max = if inputs.get(2).and_then(|s| *s).is_some() { get_f(inputs, 2,  1.0) } else { param_f("max",  1.0) };
            match inputs.get(0).and_then(|s| *s) {
                Some(Signal::Vec2(v)) => Some(Signal::Vec2(v.clamp(Vec2::splat(min), Vec2::splat(max)))),
                other => Some(Signal::Float(other.map(|s| s.as_float()).unwrap_or(0.0).clamp(min, max))),
            }
        }
        "math.map_range" => {
            let in_min  = if inputs.get(1).and_then(|s| *s).is_some() { get_f(inputs, 1, -1.0) } else { param_f("in_min",  -1.0) };
            let in_max  = if inputs.get(2).and_then(|s| *s).is_some() { get_f(inputs, 2,  1.0) } else { param_f("in_max",   1.0) };
            let out_min = if inputs.get(3).and_then(|s| *s).is_some() { get_f(inputs, 3, -1.0) } else { param_f("out_min", -1.0) };
            let out_max = if inputs.get(4).and_then(|s| *s).is_some() { get_f(inputs, 4,  1.0) } else { param_f("out_max",  1.0) };
            let map = |v: f32| -> f32 {
                let t = if (in_max - in_min).abs() < f32::EPSILON { 0.0 }
                        else { (v - in_min) / (in_max - in_min) };
                out_min + t * (out_max - out_min)
            };
            match inputs.get(0).and_then(|s| *s) {
                Some(Signal::Vec2(v)) => Some(Signal::Vec2(Vec2::new(map(v.x), map(v.y)))),
                other => Some(Signal::Float(map(other.map(|s| s.as_float()).unwrap_or(0.0)))),
            }
        }
        "logic.and"       => Some(Signal::Bool(get_b(inputs, 0, false) && get_b(inputs, 1, false))),
        "logic.or"        => Some(Signal::Bool(get_b(inputs, 0, false) || get_b(inputs, 1, false))),
        "logic.not"       => Some(Signal::Bool(!get_b(inputs, 0, false))),
        "logic.xor"       => Some(Signal::Bool(get_b(inputs, 0, false) ^ get_b(inputs, 1, false))),
        "logic.equal"     => Some(Signal::Bool(get_f(inputs, 0, 0.0) == get_f(inputs, 1, 0.0))),
        "logic.not_equal" => Some(Signal::Bool(get_f(inputs, 0, 0.0) != get_f(inputs, 1, 0.0))),
        "logic.greater_than" => {
            let (a, b) = (get_f(inputs, 0, 0.0), get_f(inputs, 1, 0.0));
            let or_eq = params.get("or_equal").and_then(|v| v.as_bool()).unwrap_or(false);
            Some(Signal::Bool(if or_eq { a >= b } else { a > b }))
        }
        "logic.less_than" => {
            let (a, b) = (get_f(inputs, 0, 0.0), get_f(inputs, 1, 0.0));
            let or_eq = params.get("or_equal").and_then(|v| v.as_bool()).unwrap_or(false);
            Some(Signal::Bool(if or_eq { a <= b } else { a < b }))
        }
        "module.selector" => {
            if out_idx != 0 { return None; }
            let n_inputs = inputs.len().saturating_sub(1);
            let sel = get_f(inputs, 0, 0.0);
            let interp = params.get("interpolate").and_then(|v| v.as_bool()).unwrap_or(false);
            if interp && n_inputs >= 2 {
                let pos = sel.clamp(0.0, 1.0) * (n_inputs - 1) as f32;
                let lo = pos.floor() as usize;
                let hi = (lo + 1).min(n_inputs - 1);
                let t = pos.fract();
                let lo_v = inputs.get(lo + 1).and_then(|s| *s).map(|s| s.as_float()).unwrap_or(0.0);
                let hi_v = inputs.get(hi + 1).and_then(|s| *s).map(|s| s.as_float()).unwrap_or(0.0);
                Some(Signal::Float(lo_v * (1.0 - t) + hi_v * t))
            } else {
                let n = n_inputs as f32;
                let idx = (sel.clamp(0.0, 1.0) * n).floor() as usize;
                let idx = idx.min(n_inputs.saturating_sub(1));
                inputs.get(idx + 1).and_then(|s| *s)
            }
        }
        "module.split" => {
            let sel = get_f(inputs, 0, 0.0);
            let raw = inputs.get(1).and_then(|s| *s);
            let n   = n_outputs;
            let interp = params.get("interpolate").and_then(|v| v.as_bool()).unwrap_or(false);
            let zero_like = |sig: Option<Signal>| -> Signal {
                match sig {
                    Some(Signal::Vec2(_)) => Signal::Vec2(glam::Vec2::ZERO),
                    Some(Signal::Bool(_)) => Signal::Bool(false),
                    Some(Signal::Int(_))  => Signal::Int(0),
                    _                     => Signal::Float(0.0),
                }
            };
            if interp && n >= 2 {
                let pos = sel.clamp(0.0, 1.0) * (n - 1) as f32;
                let lo  = pos.floor() as usize;
                let hi  = (lo + 1).min(n - 1);
                let t   = pos.fract();
                match raw {
                    Some(Signal::Vec2(v)) => {
                        if out_idx == lo && lo == hi { Some(Signal::Vec2(v)) }
                        else if out_idx == lo        { Some(Signal::Vec2(v * (1.0 - t))) }
                        else if out_idx == hi        { Some(Signal::Vec2(v * t)) }
                        else                         { Some(Signal::Vec2(glam::Vec2::ZERO)) }
                    }
                    _ => {
                        let val = raw.map(|s| s.as_float()).unwrap_or(0.0);
                        if out_idx == lo && lo == hi { Some(Signal::Float(val)) }
                        else if out_idx == lo        { Some(Signal::Float(val * (1.0 - t))) }
                        else if out_idx == hi        { Some(Signal::Float(val * t)) }
                        else                         { Some(Signal::Float(0.0)) }
                    }
                }
            } else {
                let idx = (sel.clamp(0.0, 1.0) * n as f32).floor() as usize;
                let idx = idx.min(n.saturating_sub(1));
                if out_idx == idx { Some(raw.unwrap_or(Signal::Float(0.0))) } else { Some(zero_like(raw)) }
            }
        }
        "module.response_curve" => {
            if out_idx >= n_outputs { return None; }
            let x       = get_f(inputs, out_idx, 0.0);
            let pts     = curve_points_from_params(params);
            let biases  = biases_from_params(params);
            let abs     = params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true);
            let in_max  = params.get("in_max") .and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32;
            let in_min  = params.get("in_min") .and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
            let out_max = params.get("out_max").and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32;
            let out_min = params.get("out_min").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
            Some(Signal::Float(apply_curve(x, &pts, &biases, abs, in_min, in_max, out_min, out_max, read_scale_t(params))))
        }
        "module.vec_response_curve" => {
            if out_idx >= n_outputs { return None; }
            let vec = match inputs.get(out_idx).and_then(|s| *s) {
                Some(Signal::Vec2(v)) => v,
                _ => return Some(Signal::Vec2(glam::Vec2::ZERO)),
            };
            let mag = vec.length();
            if mag < f32::EPSILON { return Some(Signal::Vec2(glam::Vec2::ZERO)); }
            let pts     = curve_points_from_params(params);
            let biases  = biases_from_params(params);
            let in_max  = params.get("in_max") .and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let out_max = params.get("out_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let out_mag = apply_curve(mag, &pts, &biases, true, 0.0, in_max, 0.0, out_max, read_scale_t(params));
            Some(Signal::Vec2(vec / mag * out_mag))
        }
        "module.vec_to_axis" => {
            let vec = match inputs.first().and_then(|s| *s) {
                Some(Signal::Vec2(v)) => v,
                _ => glam::Vec2::ZERO,
            };
            match out_idx { 0 => Some(Signal::Float(vec.x)), 1 => Some(Signal::Float(vec.y)), _ => None }
        }
        "module.axis_to_vec" => {
            if out_idx != 0 { return None; }
            let x = match inputs.first().and_then(|s| *s) { Some(Signal::Float(f)) => f, _ => 0.0 };
            let y = match inputs.get(1).and_then(|s| *s)  { Some(Signal::Float(f)) => f, _ => 0.0 };
            Some(Signal::Vec2(glam::Vec2::new(x, y)))
        }
        _ => None,
    }
}

// ── Stateful compute functions ────────────────────────────────────────────────

fn compute_oscillator(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
    dt: f32,
) -> Vec<Option<Signal>> {
    let shape     = params.get("shape")     .and_then(|v| v.as_str()) .unwrap_or("sine");
    let freq_unit = params.get("freq_unit") .and_then(|v| v.as_str()) .unwrap_or("hz");
    let bipolar   = params.get("bipolar")   .and_then(|v| v.as_bool()).unwrap_or(true);

    let freq_wired  = inputs.get(0).and_then(|s| *s).is_some();
    let phase_wired = inputs.get(1).and_then(|s| *s).is_some();

    let base_freq = params.get("freq_param").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
    // When freq is wired the input is a normalized multiplier [0,1] (or bipolar) applied
    // to the base frequency set in the node. This lets you sweep 0→base_freq with a
    // unipolar source or modulate depth with another oscillator.
    let freq_val  = if freq_wired  { get_f(inputs, 0, 1.0).max(0.0) * base_freq } else { base_freq };
    let phase_off = if phase_wired { get_f(inputs, 1, 0.0) } else { params.get("phase_param").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32 };
    let retrig    = get_b(inputs, 2, false);

    let period_s = match freq_unit {
        "hz" => if freq_val > 0.0 { 1.0 / freq_val } else { 1.0 },
        _    => (freq_val / 1000.0).max(0.0001),
    }.max(0.0001);

    while state.aux_f32.len() < 2 { state.aux_f32.push(0.0); }

    let retrig_edge = retrig && state.aux_f32[1] < 0.5;
    state.aux_f32[1] = if retrig { 1.0 } else { 0.0 };
    if retrig_edge { state.aux_f32[0] = 0.0; }

    state.aux_f32[0] = (state.aux_f32[0] + dt / period_s) % 1.0;
    let phase  = (state.aux_f32[0] + phase_off).rem_euclid(1.0);
    let val    = osc_sample(shape, phase);
    let output = if bipolar { val } else { (val + 1.0) * 0.5 };
    vec![Some(Signal::Float(output))]
}

pub fn osc_sample(shape: &str, phase: f32) -> f32 {
    match shape {
        "sine"     => (phase * std::f32::consts::TAU).sin(),
        "triangle" => if phase < 0.5 { 4.0 * phase - 1.0 } else { 3.0 - 4.0 * phase },
        "saw"      => 2.0 * phase - 1.0,
        "square"   => if phase < 0.5 { 1.0 } else { -1.0 },
        _          => 0.0,
    }
}

// ── Envelope Generator ────────────────────────────────────────────────────────
//
// Behavior is set by three combinable flags — Hold, Bounce, Loop — rather than a
// single mode. `envelope_flags` resolves them (with a fallback that maps the old
// `mode` string for patches saved before the switch). The eight combinations:
//
//   (none)        one-shot: a single 0→1 pass on trigger.
//   Hold          attack→sustain, hold while held, release →1.
//   Loop          sawtooth 0↔1 while held; returns to 0 on release.
//   Bounce        forward while held (sustains at 1), reverses to 0 on release.
//   Hold+Bounce   bounce, value held flat at the sustain level through the
//                 post-sustain time buffer (the "B+Hold" buffer mode).
//   Hold+Loop     attack→sustain, then sawtooth loop between sustain and 1.
//   Bounce+Loop   ping-pong 0↔1 while held; recedes to 0 on release.
//   Hold+Bounce+Loop  attack→sustain, then ping-pong between sustain and 1.
//
// State layout in aux_f32:
//   [0] = current phase (0..1 along the curve X axis)
//   [1] = previous trigger value (0/1)
//   [2] = stage: 0=idle/done, 1=attack, 2=sustain-active, 3=release
//   [3] = discontinuity epoch (bumped on teleports; UI breaks the trail on change)
//   [4] = bounce ping-pong direction (+1 forward, -1 backward)
//
// last_signals = [output, phase, epoch, applied_time] for the UI.

/// Resolve the (hold, bounce, loop) envelope flags, falling back to the legacy
/// `mode` string for patches saved before flags existed.
pub fn envelope_flags(params: &HashMap<String, Value>) -> (bool, bool, bool) {
    let has_new = params.contains_key("hold")
        || params.contains_key("bounce")
        || params.contains_key("loop");
    if has_new {
        let g = |k: &str| params.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
        (g("hold"), g("bounce"), g("loop"))
    } else {
        match params.get("mode").and_then(|v| v.as_str()).unwrap_or("oneshot") {
            "hold"        => (true,  false, false),
            "loop"        => (false, false, true),
            "bounce"      => (false, true,  false),
            "bounce_hold" => (true,  true,  false),
            _             => (false, false, false),
        }
    }
}

fn compute_envelope(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
    dt: f32,
) -> Vec<Option<Signal>> {
    let (hold, bounce, loopf) = envelope_flags(params);
    let timebase   = params.get("timebase").and_then(|v| v.as_str()).unwrap_or("ms");
    let time_param = params.get("time_mul").and_then(|v| v.as_f64()).unwrap_or(500.0) as f32;
    let sustain_x  = params.get("sustain") .and_then(|v| v.as_f64()).unwrap_or(0.5)   as f32;
    let sustain_c  = sustain_x.clamp(0.0, 1.0);
    let pts    = curve_points_from_params(params);
    let biases = biases_from_params(params);

    let trig_wired = inputs.get(0).and_then(|s| *s).is_some();
    let time_wired = inputs.get(1).and_then(|s| *s).is_some();
    let time_val   = if time_wired { get_f(inputs, 1, time_param).max(0.0) } else { time_param };

    let period_s = match timebase {
        "s"  => time_val.max(0.0001),
        "hz" => if time_val > 0.0 { 1.0 / time_val } else { 1.0 },
        _    => (time_val / 1000.0).max(0.0001),
    };
    let dt_phase = (dt / period_s).min(1.0);

    while state.aux_f32.len() < 5 { state.aux_f32.push(0.0); }
    let mut phase = state.aux_f32[0];
    let prev_trig = state.aux_f32[1];
    let mut stage = state.aux_f32[2];
    // Discontinuity epoch: bumped every time `phase` is set non-continuously
    // (retrigger, loop wrap, loop release-reset, hold early-release jump). The
    // UI reads this from last_signals[2] and breaks the trail across an epoch
    // change so the dot teleports (old trail fades in place) rather than drawing
    // a bridging streak across the jump.
    let mut epoch = state.aux_f32[3];
    // Bounce ping-pong direction (+1 forward, -1 backward). Default forward.
    let mut dir = if state.aux_f32[4] == 0.0 { 1.0 } else { state.aux_f32[4] };

    let trigger   = get_b(inputs, 0, false);
    let trig_edge = trigger && prev_trig < 0.5;

    if bounce {
        // ── Bounce family ─────────────────────────────────────────────────────
        // Continuous motion (no teleports), so the trail follows the dot. With
        // Hold the active region is [sustain, 1] (climb to sustain first); with
        // Loop the dot ping-pongs in that region instead of sustaining at 1.
        let lo = if hold { sustain_c } else { 0.0 };
        if trigger {
            if loopf {
                if phase < lo {
                    phase = (phase + dt_phase).min(lo);
                    dir = 1.0;
                } else {
                    phase += dir * dt_phase;
                    if phase >= 1.0 { phase = 1.0; dir = -1.0; }
                    if phase <= lo  { phase = lo;  dir =  1.0; }
                }
            } else {
                // Forward, sustaining at the end (value frozen at sustain when
                // Hold is set — the post-sustain time buffer, see sample below).
                phase = (phase + dt_phase).min(1.0);
                dir = 1.0;
            }
        } else {
            phase = (phase - dt_phase).max(0.0);
            dir = 1.0;
        }
    } else if hold {
        // ── Hold family (no bounce) ───────────────────────────────────────────
        if trig_edge { phase = 0.0; stage = 1.0; epoch += 1.0; }
        if stage == 1.0 {
            // Attack: climb to the sustain point.
            phase += dt_phase;
            if phase >= sustain_c {
                phase = sustain_c;
                stage = 2.0;
            } else if !trigger {
                // Released before reaching sustain. Jump onto the release side
                // (X >= sustain) at the point whose curve value best matches the
                // current output — similar level or higher, never a downward jump.
                let current_y = sample_curve(&pts, phase, &biases);
                let steps = 200u32;
                let mut best_x = sustain_c;
                let mut best_d = f32::INFINITY;
                for i in 0..=steps {
                    let x = sustain_c + (1.0 - sustain_c) * i as f32 / steps as f32;
                    let d = (sample_curve(&pts, x, &biases) - current_y).abs();
                    if d < best_d { best_d = d; best_x = x; }
                }
                phase = best_x;
                stage = 3.0;
                epoch += 1.0; // teleport across the sustain point
            }
        }
        if stage == 2.0 {
            if !trigger {
                stage = 3.0; // begin release
            } else if loopf {
                // Hold+Loop: sawtooth loop between sustain and 1.
                let span = (1.0 - sustain_c).max(1e-4);
                let advanced = phase + dt_phase;
                if advanced >= 1.0 {
                    epoch += 1.0;
                    phase = sustain_c + (advanced - 1.0).rem_euclid(span);
                } else {
                    phase = advanced;
                }
            }
            // Plain Hold: phase stays parked at sustain.
        }
        if stage == 3.0 {
            // Release: run forward to the end.
            phase += dt_phase;
            if phase >= 1.0 { phase = 1.0; stage = 0.0; }
        }
    } else if loopf {
        // ── Loop (no hold, no bounce) ─────────────────────────────────────────
        if trig_wired && !trigger {
            if phase != 0.0 { epoch += 1.0; }
            phase = 0.0;
        } else {
            if trig_edge { phase = 0.0; epoch += 1.0; }
            let advanced = phase + dt_phase;
            if advanced >= 1.0 { epoch += 1.0; } // wrapped around
            phase = advanced % 1.0;
        }
    } else {
        // ── One-shot ──────────────────────────────────────────────────────────
        if trig_edge { phase = 0.0; stage = 1.0; epoch += 1.0; }
        if stage == 1.0 {
            phase += dt_phase;
            if phase >= 1.0 { phase = 1.0; stage = 0.0; }
        }
    }

    state.aux_f32[0] = phase;
    state.aux_f32[1] = if trigger { 1.0 } else { 0.0 };
    state.aux_f32[2] = stage;
    state.aux_f32[3] = epoch;
    state.aux_f32[4] = dir;

    // Hold+Bounce (no loop) freezes the value at the sustain level through the
    // post-sustain buffer; every other combination samples the live phase.
    let buffer_mode = hold && bounce && !loopf;
    let sample_phase = if buffer_mode { phase.min(sustain_c) } else { phase };
    let output = sample_curve(&pts, sample_phase, &biases).clamp(0.0, 1.0);
    state.last_signals = vec![
        Some(Signal::Float(output)),
        Some(Signal::Float(phase)),
        Some(Signal::Float(epoch)),
        // Applied time value in the current unit — the UI shows this in the
        // grayed-out time box when the Time input is wired.
        Some(Signal::Float(time_val)),
    ];
    vec![Some(Signal::Float(output))]
}

fn compute_delay(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
) -> Vec<Option<Signal>> {
    let delay_secs = params.get("delay_ms").and_then(|v| v.as_f64()).unwrap_or(100.0)
        .clamp(0.0, 60_000.0) as f32 / 1000.0;
    let now = Instant::now();

    while state.delay_bufs.len() < inputs.len() {
        state.delay_bufs.push(VecDeque::new());
    }

    let mut results = Vec::with_capacity(inputs.len());
    for (ch, inp) in inputs.iter().enumerate() {
        let Some(v) = sig_to_f32(*inp) else { results.push(None); continue; };
        let buf = &mut state.delay_bufs[ch];
        buf.push_back((now, v));

        let mut output = buf.front().map(|(_, v)| *v);
        for (ts, val) in buf.iter() {
            if now.duration_since(*ts).as_secs_f32() >= delay_secs { output = Some(*val); }
            else { break; }
        }

        let max_age = delay_secs + 1.0;
        while buf.len() > 2 {
            let oldest_age = now.duration_since(buf.front().unwrap().0).as_secs_f32();
            if oldest_age > max_age { buf.pop_front(); } else { break; }
        }

        results.push(output.map(Signal::Float));
    }
    results
}

fn compute_average(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
) -> Vec<Option<Signal>> {
    let buf_size = params.get("buf_size").and_then(|v| v.as_f64())
        .map(|f| f as u64).unwrap_or(10).clamp(1, 10_000) as usize;
    let spike_mad = params.get("spike_mad").and_then(|v| v.as_f64()).unwrap_or(0.0).max(0.0);

    while state.avg_bufs.len()    < inputs.len() { state.avg_bufs.push(VecDeque::new()); }
    while state.avg_bufs_v2.len() < inputs.len() { state.avg_bufs_v2.push(VecDeque::new()); }

    let mut results = Vec::with_capacity(inputs.len());
    for (ch, inp) in inputs.iter().enumerate() {
        match inp {
            Some(Signal::Vec2(v)) => {
                let buf = &mut state.avg_bufs_v2[ch];
                buf.push_back(*v);
                while buf.len() > buf_size { buf.pop_front(); }

                let avg = if spike_mad > 0.0 && buf.len() >= 3 {
                    Vec2::new(
                        mad_average(buf.iter().map(|v| v.x), spike_mad as f32),
                        mad_average(buf.iter().map(|v| v.y), spike_mad as f32),
                    )
                } else {
                    buf.iter().copied().sum::<Vec2>() / buf.len() as f32
                };
                results.push(Some(Signal::Vec2(avg)));
            }
            inp => {
                let Some(v) = sig_to_f32(*inp) else { results.push(None); continue; };
                let buf = &mut state.avg_bufs[ch];
                buf.push_back(v);
                while buf.len() > buf_size { buf.pop_front(); }

                let avg = if spike_mad > 0.0 && buf.len() >= 3 {
                    mad_average(buf.iter().copied(), spike_mad as f32)
                } else {
                    buf.iter().sum::<f32>() / buf.len() as f32
                };
                results.push(Some(Signal::Float(avg)));
            }
        }
    }
    results
}

fn mad_average(values: impl Iterator<Item = f32> + Clone, spike_mad: f32) -> f32 {
    let mut sorted: Vec<f32> = values.collect();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let median = sorted_median(&sorted);
    let mut devs: Vec<f32> = sorted.iter().map(|&x| (x - median).abs()).collect();
    devs.sort_by(|a, b| a.total_cmp(b));
    let mad = sorted_median(&devs);
    if mad < 1e-9 {
        sorted.iter().sum::<f32>() / sorted.len() as f32
    } else {
        let thresh = spike_mad * mad;
        let kept: Vec<f32> = sorted.iter().cloned().filter(|&x| (x - median).abs() <= thresh).collect();
        if kept.is_empty() { sorted.iter().sum::<f32>() / sorted.len() as f32 }
        else { kept.iter().sum::<f32>() / kept.len() as f32 }
    }
}

fn sorted_median(sorted: &[f32]) -> f32 {
    let n = sorted.len();
    if n == 0 { return 0.0; }
    if n % 2 == 0 { (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0 } else { sorted[n / 2] }
}

const DC_THRESHOLD: f64    = 0.005;
const DC_STABILITY: f64    = 0.02;
const DC_FAST_TC_SECS: f64 = 0.05;

fn compute_dc_filter(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
    dt: f32,
) -> Vec<Option<Signal>> {
    let window_secs = params.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(500.0)
        .clamp(10.0, 60_000.0) as f32 / 1000.0;
    let decay_secs = params.get("decay_ms").and_then(|v| v.as_f64()).unwrap_or(200.0)
        .clamp(10.0, 60_000.0) / 1000.0;

    let dt64       = dt as f64;
    let alpha_fast = 1.0 - (-dt64 / DC_FAST_TC_SECS).exp();
    let alpha_est  = 1.0 - (-dt64 / window_secs as f64).exp();
    let alpha_corr = 1.0 - (-dt64 / decay_secs).exp();
    let blend_step = dt as f64 / decay_secs;

    while state.dc_fast.len()        < inputs.len() { state.dc_fast.push(0.0); }
    while state.dc_estimates.len()   < inputs.len() { state.dc_estimates.push(0.0); }
    while state.dc_corrections.len() < inputs.len() { state.dc_corrections.push(0.0); }
    while state.dc_timers.len()      < inputs.len() { state.dc_timers.push(0.0); }
    while state.dc_frozen.len()      < inputs.len() { state.dc_frozen.push(0.0); }
    while state.dc_blend.len()       < inputs.len() { state.dc_blend.push(0.0); }

    let mut results = Vec::with_capacity(inputs.len());
    for (ch, inp) in inputs.iter().enumerate() {
        let Some(v) = sig_to_f32(*inp) else { results.push(None); continue; };
        let v64 = v as f64;

        state.dc_fast[ch]      += alpha_fast * (v64 - state.dc_fast[ch]);
        state.dc_estimates[ch] += alpha_est  * (v64 - state.dc_estimates[ch]);

        let is_stable  = (state.dc_fast[ch] - state.dc_estimates[ch]).abs() < DC_STABILITY;
        let is_nonzero = state.dc_estimates[ch].abs() > DC_THRESHOLD;

        if is_stable && is_nonzero { state.dc_timers[ch] = (state.dc_timers[ch] + dt).min(window_secs + 1.0); }
        else                       { state.dc_timers[ch] = 0.0; }

        let output = if is_stable {
            if state.dc_timers[ch] >= window_secs {
                state.dc_corrections[ch] += alpha_corr * (state.dc_estimates[ch] - state.dc_corrections[ch]);
            } else {
                state.dc_corrections[ch] += alpha_corr * (0.0 - state.dc_corrections[ch]);
            }
            let out = v64 - state.dc_corrections[ch];
            state.dc_frozen[ch] = out;
            state.dc_blend[ch]  = 0.0;
            out
        } else {
            state.dc_blend[ch] = (state.dc_blend[ch] + blend_step).min(1.0);
            let b   = state.dc_blend[ch];
            let out = state.dc_frozen[ch] * (1.0 - b) + v64 * b;
            state.dc_corrections[ch] = v64 - out;
            out
        };
        results.push(Some(Signal::Float(output as f32)));
    }
    results
}

fn compute_twoway_response_curve(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
    dt: f32,
) -> Vec<Option<Signal>> {
    let n_ch = inputs.len();

    // Grow per-channel state vectors lazily.
    while state.twoway_lane.len()       < n_ch { state.twoway_lane.push(1); }
    while state.twoway_dir_buf.len()    < n_ch { state.twoway_dir_buf.push(VecDeque::new()); }
    while state.twoway_blend.len()      < n_ch { state.twoway_blend.push(1.0); }
    while state.twoway_prev_input.len() < n_ch { state.twoway_prev_input.push(0.0); }
    while state.twoway_old_output.len() < n_ch { state.twoway_old_output.push(0.0); }

    // Shared params (applied to both curves).
    let abs     = params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true);
    let in_max  = params.get("in_max") .and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32;
    let in_min  = params.get("in_min") .and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
    let out_max = params.get("out_max").and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32;
    let out_min = params.get("out_min").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
    let scale_t = read_scale_t(params);

    let vec_mode = params.get("vec_mode").and_then(|v| v.as_bool()).unwrap_or(false);

    // Up-lane (rising) curve params.
    let pts_up   = curve_points_from_params(params);
    let biases_up = biases_from_params(params);

    // Down-lane (falling) curve uses "_dn"-suffixed params, falling back to up-lane.
    let pts_dn: Vec<[f32; 2]> = params.get("points_dn")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|p| {
            let a = p.as_array()?;
            Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
        }).collect())
        .unwrap_or_else(|| pts_up.clone());
    let biases_dn: Vec<f32> = params.get("biases_dn")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|b| b.as_f64().map(|f| f as f32)).collect())
        .unwrap_or_else(|| biases_up.clone());

    // Hysteresis params.
    let hyst_pct  = params.get("hysteresis_pct").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
    let hyst_ms   = params.get("hysteresis_ms") .and_then(|v| v.as_f64()).unwrap_or(20.0) as f32;
    let interp_ms = params.get("interp_ms")     .and_then(|v| v.as_f64()).unwrap_or(50.0) as f32;

    let hyst_ticks = ((hyst_ms / 1000.0) / dt).ceil() as usize;
    let hyst_ticks = hyst_ticks.max(1);

    let abs_max   = in_max.abs().max(in_min.abs()).max(f32::EPSILON);
    let threshold = hyst_pct / 100.0 * abs_max;

    let interp_step = if interp_ms > 0.0 { dt / (interp_ms / 1000.0) } else { 1.0 };

    let mut results = Vec::with_capacity(n_ch);

    for ch in 0..n_ch {
        let raw_input = if vec_mode {
            match inputs.get(ch).and_then(|s| *s) {
                Some(Signal::Vec2(v)) => v.length(),
                Some(Signal::Float(f)) => f,
                _ => { results.push(None); continue; }
            }
        } else {
            match inputs.get(ch).and_then(|s| *s) {
                Some(Signal::Float(f)) => f,
                _ => { results.push(None); continue; }
            }
        };

        // Use magnitude in abs/vec mode; signed value in bipolar mode.
        let hyst_input = if abs || vec_mode { raw_input.abs() } else { raw_input };

        // Hysteresis: sliding-window peak/trough detector.
        // twoway_dir_buf stores the last hyst_ticks samples of hyst_input.
        // running_max = highest value in window → if current falls threshold below it → Down.
        // running_min = lowest  value in window → if current rises threshold above it → Up.
        // Works at any speed: a fast release immediately shows a large gap from the window max.
        let win = &mut state.twoway_dir_buf[ch];
        win.push_back(hyst_input);
        while win.len() > hyst_ticks { win.pop_front(); }

        let running_max = win.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let running_min = win.iter().copied().fold(f32::INFINITY,     f32::min);

        // Use a minimum window of 1 tick so single-tick reversals are detected immediately.
        let fell_from_peak = hyst_input < running_max - threshold;
        let rose_from_trough = hyst_input > running_min + threshold;

        let prev_lane = state.twoway_lane[ch];
        if rose_from_trough && prev_lane != 1 {
            state.twoway_old_output[ch] = apply_curve(raw_input, &pts_dn, &biases_dn, abs, in_min, in_max, out_min, out_max, scale_t);
            state.twoway_lane[ch]  =  1;
            state.twoway_blend[ch] = 0.0;
            state.twoway_dir_buf[ch].clear();
            state.twoway_dir_buf[ch].push_back(hyst_input);
        } else if fell_from_peak && prev_lane != -1 {
            state.twoway_old_output[ch] = apply_curve(raw_input, &pts_up, &biases_up, abs, in_min, in_max, out_min, out_max, scale_t);
            state.twoway_lane[ch]  = -1;
            state.twoway_blend[ch] = 0.0;
            state.twoway_dir_buf[ch].clear();
            state.twoway_dir_buf[ch].push_back(hyst_input);
        }

        // Advance blend.
        state.twoway_blend[ch] = (state.twoway_blend[ch] + interp_step).min(1.0);
        let blend = state.twoway_blend[ch];

        // Evaluate active-lane curve at current input.
        let new_output = if state.twoway_lane[ch] >= 0 {
            apply_curve(raw_input, &pts_up, &biases_up, abs, in_min, in_max, out_min, out_max, scale_t)
        } else {
            apply_curve(raw_input, &pts_dn, &biases_dn, abs, in_min, in_max, out_min, out_max, scale_t)
        };

        // Blend from old-lane-output-at-switch-point toward new-lane-output-at-current-input.
        // When both curves are identical, old_output == new_output so blend has no effect.
        let output = blend * new_output + (1.0 - blend) * state.twoway_old_output[ch];

        let sig = if vec_mode {
            match inputs.get(ch).and_then(|s| *s) {
                Some(Signal::Vec2(v)) => {
                    let mag = v.length();
                    if mag < f32::EPSILON { Signal::Vec2(glam::Vec2::ZERO) }
                    else { Signal::Vec2(v / mag * output) }
                }
                _ => Signal::Float(output),
            }
        } else {
            Signal::Float(output)
        };

        results.push(Some(sig));
    }

    results
}

fn compute_has_changed(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
) -> Vec<Option<Signal>> {
    let cur = inputs.first().copied().flatten();
    while state.prev_signals.len() < 1 { state.prev_signals.push(None); }
    let prev = state.prev_signals[0];
    state.prev_signals[0] = cur;

    let (changed, increased, decreased) = match (prev, cur) {
        (Some(p), Some(c)) => {
            let ch = p != c;
            let (ps, cs) = (sig_scalar(p), sig_scalar(c));
            (ch, cs > ps, cs < ps)
        }
        (None, Some(_)) => (true, false, false),
        _ => (false, false, false),
    };
    vec![Some(Signal::Bool(changed)), Some(Signal::Bool(increased)), Some(Signal::Bool(decreased))]
}

fn compute_logic_delay(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
    dt: f32,
) -> Vec<Option<Signal>> {
    let mode      = params.get("mode").and_then(|v| v.as_str()).unwrap_or("delay_false");
    let time      = params.get("time").and_then(|v| v.as_f64()).unwrap_or(100.0) as f32;
    let use_ms    = params.get("unit").and_then(|v| v.as_str()).unwrap_or("ms") == "ms";
    let threshold = if use_ms { time / 1000.0 } else { time };
    let tick      = if use_ms { dt } else { 1.0 };

    while state.aux_f32.len() < 2 { state.aux_f32.push(0.0); }
    let mode_code = if mode == "delay_true" { 0.0f32 } else { 1.0f32 };
    if state.aux_f32[1] != mode_code {
        state.aux_f32[0] = if mode == "delay_true" { 0.0 } else { threshold };
        state.aux_f32[1] = mode_code;
    }

    let input = inputs.first().copied().flatten()
        .and_then(|s| s.coerce_to(SignalType::Bool))
        .map(|s| matches!(s, Signal::Bool(true)))
        .unwrap_or(false);

    let timer  = &mut state.aux_f32[0];
    let output = match mode {
        "delay_true" => { if input { *timer += tick; *timer >= threshold } else { *timer = 0.0; false } }
        _            => { if input { *timer = 0.0; true } else { *timer += tick; *timer < threshold } }
    };
    vec![Some(Signal::Bool(output))]
}

fn compute_counter(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
) -> Vec<Option<Signal>> {
    let mode       = params.get("mode")      .and_then(|v| v.as_str()) .unwrap_or("loop");
    let normalized = params.get("normalized").and_then(|v| v.as_bool()).unwrap_or(false);

    let step_wired = inputs.get(3).and_then(|s| *s).is_some();
    let min_wired  = inputs.get(4).and_then(|s| *s).is_some();
    let max_wired  = inputs.get(5).and_then(|s| *s).is_some();

    let step = (if step_wired { get_f(inputs, 3, 1.0)  } else { params.get("step_param").and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32 }).max(f32::EPSILON);
    let min  =  if min_wired  { get_f(inputs, 4, 0.0)  } else { params.get("min_param") .and_then(|v| v.as_f64()).unwrap_or(0.0)  as f32 };
    let max  =  if max_wired  { get_f(inputs, 5, 10.0) } else { params.get("max_param") .and_then(|v| v.as_f64()).unwrap_or(10.0) as f32 };

    let max_steps = ((max - min) / step).round().max(0.0) as i32;

    while state.aux_f32.len() < 5 { state.aux_f32.push(0.0); }
    if state.aux_f32[1] == 0.0 { state.aux_f32[1] = 1.0; }

    let inc   = get_b(inputs, 0, false);
    let dec   = get_b(inputs, 1, false);
    let reset = get_b(inputs, 2, false);

    let inc_edge   = inc   && state.aux_f32[2] < 0.5;
    let dec_edge   = dec   && state.aux_f32[3] < 0.5;
    let reset_edge = reset && state.aux_f32[4] < 0.5;

    state.aux_f32[2] = if inc   { 1.0 } else { 0.0 };
    state.aux_f32[3] = if dec   { 1.0 } else { 0.0 };
    state.aux_f32[4] = if reset { 1.0 } else { 0.0 };

    let mut count = state.aux_f32[0] as i32;
    let mut dir   = state.aux_f32[1];

    if reset_edge {
        count = 0; dir = 1.0;
    } else {
        match mode {
            "loop" => {
                if inc_edge { count = (count + 1).rem_euclid(max_steps + 1); }
                if dec_edge { count = (count - 1).rem_euclid(max_steps + 1); }
            }
            "limit" => {
                if inc_edge { count = (count + 1).min(max_steps); }
                if dec_edge { count = (count - 1).max(0); }
            }
            "bounce" => {
                if max_steps > 0 {
                    if inc_edge { count += 1; }
                    if dec_edge { count -= 1; }
                    if count > max_steps { count = 2 * max_steps - count; }
                    if count < 0         { count = -count; }
                }
            }
            _ => {
                if inc_edge { count += 1; }
                if dec_edge { count = (count - 1).max(0); }
            }
        }
    }

    if mode != "unlimited" { count = count.clamp(0, max_steps); }
    state.aux_f32[0] = count as f32;
    state.aux_f32[1] = dir;

    let output = if normalized {
        if max_steps > 0 { count as f32 / max_steps as f32 } else { 0.0 }
    } else {
        min + count as f32 * step
    };
    vec![Some(Signal::Float(output))]
}

/// For Analog mode, a synthetic stick-cardinal Bool (left_stick_right, etc.)
/// captured during Learn is reinterpreted as "drive the underlying analog
/// axis in that direction." Returns (axis_pin_id, sign) where sign is +1
/// for the positive direction (right/up) or -1 for the negative direction
/// (left/down). Returns None when the pin isn't a stick cardinal — those
/// fall through to normal pulse-train Bool emission.
fn analog_axis_for_cardinal(pin_id: &str) -> Option<(&'static str, f32)> {
    match pin_id {
        "left_stick_right"  => Some(("left_stick_x",   1.0)),
        "left_stick_left"   => Some(("left_stick_x",  -1.0)),
        "left_stick_up"     => Some(("left_stick_y",   1.0)),
        "left_stick_down"   => Some(("left_stick_y",  -1.0)),
        "right_stick_right" => Some(("right_stick_x",  1.0)),
        "right_stick_left"  => Some(("right_stick_x", -1.0)),
        "right_stick_up"    => Some(("right_stick_y",  1.0)),
        "right_stick_down"  => Some(("right_stick_y", -1.0)),
        _ => None,
    }
}

/// Like `analog_axis_for_cardinal` but ALSO covers D-pad directions. Used only
/// for SUPPRESSION (zeroing the underlying axis + Vec2 when a cardinal is
/// claimed), NOT for analog output routing — the D-pad is a quantized digital
/// hat, so we don't want to drive `dpad_x/y` as a continuous analog axis. But
/// when a Remapper consumes `dpad_up`, the raw `dpad_y`/`dpad` Vec2 must be
/// suppressed too, otherwise the virtual device regenerates the Bool D-pad from
/// them and the claimed direction leaks straight through.
fn cardinal_axis_for_suppression(pin_id: &str) -> Option<(&'static str, f32)> {
    match pin_id {
        "dpad_right" => Some(("dpad_x",  1.0)),
        "dpad_left"  => Some(("dpad_x", -1.0)),
        "dpad_up"    => Some(("dpad_y",  1.0)),
        "dpad_down"  => Some(("dpad_y", -1.0)),
        _ => analog_axis_for_cardinal(pin_id),
    }
}

/// The bundled Vec2 pin id and component (true = y) for a stick/dpad axis pin.
fn vec2_pin_for_axis(axis_pin: &str) -> Option<&'static str> {
    match axis_pin {
        "left_stick_x"  | "left_stick_y"  => Some("left_stick"),
        "right_stick_x" | "right_stick_y" => Some("right_stick"),
        "dpad_x"        | "dpad_y"        => Some("dpad"),
        _ => None,
    }
}

/// Per-side suppression derived from a Remapper's CLAIMED cardinals (sticks +
/// D-pad). For each affected axis pin we record `(neg, pos)` — which side to
/// clamp to zero. Claiming `dpad_left` clamps only the negative side of
/// `dpad_x`, leaving `dpad_x` positive and `dpad_y` entirely untouched. The
/// matching cardinal Bool pins are returned separately to be zeroed directly.
/// Works for both digital and analog claims — the D-pad is digital but the
/// per-side rule is identical, and it preserves the directions the user did
/// NOT map.
struct CardinalSuppression {
    /// axis pin → (clamp_negative, clamp_positive)
    axis_sides: HashMap<&'static str, (bool, bool)>,
    /// cardinal Bool pins to force false (only the claimed directions)
    bool_pins: HashSet<&'static str>,
}

fn cardinal_suppression(claimed: &HashSet<String>) -> CardinalSuppression {
    let mut axis_sides: HashMap<&'static str, (bool, bool)> = HashMap::new();
    let mut bool_pins: HashSet<&'static str> = HashSet::new();
    for cardinal in claimed {
        if let Some((axis, sign)) = cardinal_axis_for_suppression(cardinal) {
            let entry = axis_sides.entry(axis).or_insert((false, false));
            if sign > 0.0 { entry.1 = true; } else { entry.0 = true; }
            // Canonical cardinal name for the claimed direction.
            if let Some(name) = CARDINAL_PIN_IDS.iter().find(|n| *n == cardinal) {
                bool_pins.insert(name);
            }
        }
    }
    CardinalSuppression { axis_sides, bool_pins }
}

/// All stick + D-pad cardinal Bool pin ids (used to resolve `&'static str`).
const CARDINAL_PIN_IDS: &[&str] = &[
    "left_stick_up", "left_stick_down", "left_stick_left", "left_stick_right",
    "right_stick_up", "right_stick_down", "right_stick_left", "right_stick_right",
    "dpad_up", "dpad_down", "dpad_left", "dpad_right",
];

/// Shared Remapper pass-through + suppression pass, called identically by the
/// top-level and sub-patch Remapper arms (so the two never diverge). For every
/// canonical pin it writes `collector_sigs[(key, pin)]`:
///   - consumed input pins → explicit off
///   - claimed cardinals → per-side axis/Vec2 clamp + Bool off (sticks + D-pad)
///   - unmapped pins → raw pass-through
/// Then recomputes synthetic stick cardinals from the clamped axes and publishes
/// the consumed-pin markers for downstream Combiner hierarchy suppression.
/// Evaluate a Remapper node — shared by the top-level loop and the sub-patch
/// (`eval_subgraph`) loop so the two can never diverge. `uid` is the publishing
/// id: `snap.node_uid` at top level, the namespaced uid inside a sub-patch. It
/// keys `collector_sigs["remap:{uid}"]`, the per-node `state`, and `last_outputs`.
fn eval_remapper_node(
    snap: &NodeSnap,
    uid: usize,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
    state: &mut HashMap<usize, NodeState>,
    dt: f32,
) {
            let dev_id = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("");
            let collector_id = snap.params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("");
            let mappings = snap.params.get("mappings").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let key = format!("remap:{}", uid);

            // Snapshot upstream values for every canonical pin once, so we can
            // freely mutate collector_sigs below without aliasing the read side.
            let mut upstream: HashMap<String, Signal> = HashMap::new();
            for ap in automap::ALL_PINS {
                let sig = if !collector_id.is_empty() {
                    collector_sigs.get(&(collector_id.to_string(), ap.id.to_string())).copied()
                } else { None }
                .or_else(|| {
                    if !dev_id.is_empty() {
                        dev_sigs.get(&(dev_id.to_string(), ap.id.to_string())).copied()
                    } else { None }
                });
                if let Some(s) = sig { upstream.insert(ap.id.to_string(), s); }
            }
            // A processed Vec2 on the collector is authoritative over raw axes.
            vec2_authoritative_axis_fill(&mut upstream, collector_id, &*collector_sigs);
            // Derive synthetic cardinal-direction Bool pins from each stick's
            // (x, y) so they can participate in mapping triggers just like
            // buttons. See `derive_stick_cardinals` for the dominant-axis rule.
            derive_stick_cardinals(&mut upstream);

            // Derive touchpad zone pins. Two parallel variants:
            //   touch_*       — fire whenever a finger is in that zone, click
            //                   or not. Up to 2 zones at once (one per finger).
            //                   No accumulation; transient, instantaneous.
            //   touchpad_*    — fire only while btn_touchpad is held. While
            //                   held, every zone any finger has visited stays
            //                   asserted (swipe accumulation) so a drag
            //                   across all three zones produces a 3-pin chord.
            //                   Release of btn_touchpad clears the accumulator.
            // Per-zone override: if touchpad_N (click variant) fires, touch_N
            // (touch-only) is forced false so a click-mapped zone takes over
            // from a touch-mapped one rather than firing both.
            let touch_click = upstream.get("btn_touchpad")
                .map(|s| s.as_bool()).unwrap_or(false);
            let zone_of_x = |x: f32| -> usize {
                if x < -1.0/3.0 { 0 } else if x > 1.0/3.0 { 2 } else { 1 }
            };
            // Touch-only zones — each active finger asserts exactly one zone
            // (the one its X currently sits in). Moving a finger from zone A
            // to zone B drops A and asserts B for that finger. With two
            // fingers active, two zones can fire simultaneously. No swipe
            // accumulation here — that's reserved for the click variant.
            let mut touch_only = [false; 3];
            for (xpin, apin) in [("touch1_x","touch1_active"),
                                 ("touch2_x","touch2_active")] {
                let active = upstream.get(apin).map(|s| s.as_bool()).unwrap_or(false);
                if !active { continue; }
                let x = upstream.get(xpin).map(|s| sig_scalar(*s)).unwrap_or(0.0);
                touch_only[zone_of_x(x)] = true;
            }
            // Click-variant zones — accumulated in per-node aux_f32.
            let ns = state.entry(uid).or_insert_with(NodeState::default);
            if ns.aux_f32.len() < 3 { ns.aux_f32.resize(3, 0.0); }
            if !touch_click {
                ns.aux_f32[0] = 0.0;
                ns.aux_f32[1] = 0.0;
                ns.aux_f32[2] = 0.0;
            } else {
                for (xpin, apin) in [("touch1_x","touch1_active"),
                                     ("touch2_x","touch2_active")] {
                    let active = upstream.get(apin).map(|s| s.as_bool()).unwrap_or(false);
                    if !active { continue; }
                    let x = upstream.get(xpin).map(|s| sig_scalar(*s)).unwrap_or(0.0);
                    ns.aux_f32[zone_of_x(x)] = 1.0;
                }
            }
            let click_zone = [
                ns.aux_f32[0] > 0.5,
                ns.aux_f32[1] > 0.5,
                ns.aux_f32[2] > 0.5,
            ];
            let any_zone = click_zone[0] || click_zone[1] || click_zone[2];
            // Click suppresses all touch-only zones — once btn_touchpad
            // fires, the click variants own the touchpad.
            if touch_click {
                touch_only[0] = false;
                touch_only[1] = false;
                touch_only[2] = false;
            }
            upstream.insert("touchpad_left".to_string(),   Signal::Bool(click_zone[0]));
            upstream.insert("touchpad_center".to_string(), Signal::Bool(click_zone[1]));
            upstream.insert("touchpad_right".to_string(),  Signal::Bool(click_zone[2]));
            // touchpad_any — "click anywhere on the pad". Available via the
            // Special… dropdown only (not auto-captured) so users opt in.
            // Fires together with the specific-zone pin additively.
            upstream.insert("touchpad_any".to_string(),    Signal::Bool(touch_click && any_zone));
            upstream.insert("touch_left".to_string(),      Signal::Bool(touch_only[0]));
            upstream.insert("touch_center".to_string(),    Signal::Bool(touch_only[1]));
            upstream.insert("touch_right".to_string(),     Signal::Bool(touch_only[2]));

            let read_upstream = |pin_id: &str| -> Option<Signal> { upstream.get(pin_id).copied() };

            // Per-mapping press mode is stored under `mode` + `window_ms` +
            // `sustain` on each mapping. The state machine must run for every
            // mapping every tick (not just claimed ones) so Short / Long /
            // Double detect edges without dropouts. Compute `effective_held`
            // for each in original index order, then run the sort + claim pass
            // using those values instead of re-reading raw input state.
            //
            // Analog mode is gated differently from digital modes:
            //   - Non-cardinal `in` pins must all be held (combo gate).
            //   - If any cardinal `in` pin exists, its axis magnitude must
            //     exceed GESTURE_ACTIVATE_MAG so we know the stick is being
            //     pushed in (one of) the mapped direction(s).
            //   - Pure cardinal `in`: just magnitude check, no gesture trace.
            //   - Press-mode pipeline is bypassed; analog mode owns its own
            //     "active" definition. Turbo on analog button-outputs is
            //     applied during the publish pass below.
            let ns = state.entry(uid).or_insert_with(NodeState::default);
            let effective: Vec<bool> = mappings.iter().enumerate().map(|(i, m)| {
                let in_pins: Vec<&str> = m.get("in").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();
                if in_pins.is_empty() { return false; }
                let mode_s = m.get("mode").and_then(|v| v.as_str()).unwrap_or("down");
                if mode_s == "analog" {
                    // Buttons (non-cardinal) all held? Cardinals: any
                    // non-zero magnitude is enough — analog mode passes the
                    // live magnitude through, no activation threshold.
                    let mut has_cardinal = false;
                    let mut any_cardinal_active = false;
                    let mut all_buttons_held = true;
                    for p in &in_pins {
                        if analog_axis_for_cardinal(p).is_some() {
                            has_cardinal = true;
                            if analog_cardinal_input_value(&upstream, p) > 0.0 {
                                any_cardinal_active = true;
                            }
                        } else if !read_upstream(p).map(|s| s.as_bool()).unwrap_or(false) {
                            all_buttons_held = false;
                        }
                    }
                    // Pure-button analog mappings (no cardinal in) reduce to
                    // "all held" — same as Down mode. Reasonable fallback.
                    return all_buttons_held && (!has_cardinal || any_cardinal_active);
                }
                // Stick-gesture path: when every `in` pin is a stick cardinal,
                // the chord can never be "simultaneously held" (a single stick
                // can't be Left AND Right at the same instant). Instead we
                // track which cardinals have been visited during the active
                // gesture and fire when all required cardinals across both
                // sticks have been visited at least once.
                let raw_held = if let Some(required) = gesture_required_bits(&in_pins) {
                    let buttons_held = in_pins.iter().all(|p| {
                        if gesture_pin_to_bit(p).is_some() { return true; }
                        read_upstream(p).map(|s| s.as_bool()).unwrap_or(false)
                    });
                    let visited = gesture_state_get(ns, i);
                    buttons_held && gesture_tick(required, visited, &upstream)
                } else {
                    in_pins.iter().all(|p| {
                        read_upstream(p).map(|s| s.as_bool()).unwrap_or(false)
                    })
                };
                let mode = PressMode::from_str(mode_s);
                let window_ms = m.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(200.0) as f32;
                let sustain   = m.get("sustain").and_then(|v| v.as_bool()).unwrap_or(false);
                let turbo     = m.get("turbo").and_then(|v| v.as_bool()).unwrap_or(false);
                let slots = press_state_get(ns, i);
                let held = apply_press_mode(raw_held, mode, window_ms, sustain, slots, dt);
                if turbo { apply_turbo(held, window_ms, slots, dt) } else { held }
            }).collect();

            // Physical-hold state per mapping, INDEPENDENT of press mode — true
            // whenever the mapping's input chord is currently held/deflected.
            // Used for input SUPPRESSION: a consumed input must stay suppressed
            // for as long as it is held, even when the press-mode gate (on-press
            // pulse, double-tap window, etc.) is momentarily closed. Otherwise
            // the raw input would leak through while the user keeps holding it.
            let held_now: Vec<bool> = mappings.iter().map(|m| {
                let in_pins: Vec<&str> = m.get("in").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();
                if in_pins.is_empty() { return false; }
                in_pins.iter().all(|p| {
                    if analog_axis_for_cardinal(p).is_some() {
                        analog_cardinal_input_value(&upstream, p) > 0.0
                    } else {
                        read_upstream(p).map(|s| s.as_bool()).unwrap_or(false)
                    }
                })
            }).collect();

            // Determine which mappings are currently triggered. Sort indices
            // by descending input-set size so longer combos win conflicts;
            // original indices are preserved so we can look up `effective`
            // and mapping fields afterwards.
            let mut sorted_idx: Vec<usize> = (0..mappings.len()).collect();
            sorted_idx.sort_by(|&a, &b| {
                let la = mappings[a].get("in").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                let lb = mappings[b].get("in").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                lb.cmp(&la)
            });

            // Trigger pass 1: identify triggered mappings and the pins they consume.
            //
            // Suppression rule for overlapping mappings:
            //   - A mapping is suppressed iff a STRICTLY LONGER triggered
            //     mapping has already claimed all of its inputs (longer
            //     chord wins over shorter sub-chord).
            //   - Mappings with the SAME input set are allowed to coexist
            //     so users can fan one button out to multiple outputs:
            //     `Y → X` and `Y → Y` both fire when Y is pressed.
            //
            // Analog mappings with IDENTICAL input chords have an extra
            // last-wins override applied during the publish pass below
            // (user-error guard for conflicting analog writes).
            let mut triggered: Vec<(Vec<String>, Vec<String>, bool, usize)> = Vec::new(); // (in, out, is_analog, orig_idx)
            let mut triggered_claims: Vec<(usize, Vec<String>)> = Vec::new();
            for &i in &sorted_idx {
                let m = &mappings[i];
                let in_pins: Vec<String> = m.get("in").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                let out_pins: Vec<String> = m.get("out").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                if in_pins.is_empty() { continue; }
                if !effective[i] { continue; }
                let my_len = in_pins.len();
                let suppressed = triggered_claims.iter().any(|(claim_len, claim_pins)| {
                    *claim_len > my_len && in_pins.iter().all(|p| claim_pins.contains(p))
                });
                if suppressed { continue; }
                let is_analog = m.get("mode").and_then(|v| v.as_str()) == Some("analog");
                let mut sorted_in = in_pins.clone();
                sorted_in.sort();
                triggered_claims.push((my_len, sorted_in));
                triggered.push((in_pins, out_pins, is_analog, i));
            }

            // Claimed inputs split by mode so pass-through suppression for
            // analog cardinal claims can use axis-side clamping rather than
            // hard-zeroing the entire axis.
            //
            // Suppression follows PHYSICAL HOLD (`held_now`), not the press-mode
            // gate (`effective`/`triggered`): once a mapping consumes an input,
            // that input is suppressed for as long as it's held, regardless of
            // press mode. EXCEPTION — an input a mapping routes back to ITSELF
            // (e.g. `dpad_left → dpad_left`, a deliberate pass-through) is NOT
            // suppressed, so the user can keep an input while also reacting to it.
            let mut self_mapped: HashSet<String> = HashSet::new();
            for m in &mappings {
                let ins: Vec<&str> = m.get("in").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
                let outs: Vec<&str> = m.get("out").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
                for p in &ins {
                    if outs.contains(p) { self_mapped.insert((*p).to_string()); }
                }
            }
            let mut claimed_inputs_digital: HashSet<String> = HashSet::new();
            let mut claimed_inputs_analog: HashSet<String>  = HashSet::new();
            for (i, m) in mappings.iter().enumerate() {
                if !held_now[i] { continue; }
                let is_analog = m.get("mode").and_then(|v| v.as_str()) == Some("analog");
                let in_pins: Vec<String> = m.get("in").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                let target = if is_analog { &mut claimed_inputs_analog } else { &mut claimed_inputs_digital };
                for p in in_pins {
                    if self_mapped.contains(&p) { continue; }
                    target.insert(p);
                }
            }
            // Pass-through + per-side suppression (sticks + D-pad) + consumed
            // markers — shared with the sub-patch arm so they never diverge.
            remapper_pass_through_and_suppress(
                &key, &upstream,
                &claimed_inputs_digital, &claimed_inputs_analog,
                collector_sigs,
            );

            // ── Analog publish pass ──────────────────────────────────────
            //
            // Apply identical input+output-chord override (last wins). Build the
            // set of analog mappings to actually emit, suppressing any earlier
            // analog mapping that is a TRUE duplicate (same inputs AND same
            // outputs) of a later one. Mappings sharing an input but targeting
            // different outputs (e.g. left_stick_up→right_trigger alongside
            // left_stick_up→left_stick_up to keep the stick) both fire.
            let mut analog_emit_idx: Vec<usize> = Vec::new();
            {
                // Walk triggered in original mapping order so "later in the
                // user's list wins". `triggered` was built in sorted_idx
                // (longest-first) order; recover original order via the
                // orig_idx we stored.
                let mut analog_indices: Vec<usize> = (0..triggered.len())
                    .filter(|&t| triggered[t].2)
                    .collect();
                analog_indices.sort_by_key(|&t| triggered[t].3);
                let sorted_set = |v: &Vec<String>| -> Vec<String> {
                    let mut s = v.clone(); s.sort(); s
                };
                let mut keep: Vec<bool> = vec![true; analog_indices.len()];
                for a in 0..analog_indices.len() {
                    if !keep[a] { continue; }
                    let (ref ain, ref aout, _, _) = triggered[analog_indices[a]];
                    let (a_in, a_out) = (sorted_set(ain), sorted_set(aout));
                    for b in (a + 1)..analog_indices.len() {
                        let (ref bin, ref bout, _, _) = triggered[analog_indices[b]];
                        if a_in == sorted_set(bin) && a_out == sorted_set(bout) {
                            // Later (higher index) wins → suppress earlier dup.
                            keep[a] = false;
                            break;
                        }
                    }
                }
                for (a, t_idx) in analog_indices.iter().enumerate() {
                    if keep[a] { analog_emit_idx.push(*t_idx); }
                }
            }

            // Accumulate cardinal-axis writes additively; track button-output
            // emissions per output-pin for turbo / sustain handling.
            let mut analog_axis_acc: HashMap<&'static str, f32> = HashMap::new();
            let mut analog_button_out: HashSet<String> = HashSet::new();
            let mut analog_out_pins: HashSet<String> = HashSet::new();
            for &t_idx in &analog_emit_idx {
                let (ref in_pins, ref out_pins, _, orig_i) = triggered[t_idx];
                let m = &mappings[orig_i];
                let turbo  = m.get("turbo").and_then(|v| v.as_bool()).unwrap_or(false);
                let sustain = m.get("sustain").and_then(|v| v.as_bool()).unwrap_or(false);
                let window_ms = m.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(200.0) as f32;
                let slots = press_state_get(ns, orig_i);
                // Zip in↔out by index; drop the excess from whichever side
                // is longer.
                let n = in_pins.len().min(out_pins.len());
                for (in_p, out_p) in in_pins[..n].iter().zip(out_pins[..n].iter()) {
                    analog_out_pins.insert(out_p.clone());
                    let in_is_cardinal  = analog_axis_for_cardinal(in_p).is_some();
                    let out_axis_opt    = analog_axis_for_cardinal(out_p);
                    let out_trigger     = analog_trigger_out(out_p);
                    let mag_from_input = if in_is_cardinal {
                        analog_cardinal_input_value(&upstream, in_p)
                    } else {
                        // Non-cardinal in pin in this slot — when paired with
                        // a cardinal out, drive it at full magnitude while the
                        // gate is open (the effective[] check guaranteed all
                        // non-cardinal buttons are held).
                        1.0
                    };
                    if let Some((axis_pin, sign)) = out_axis_opt {
                        let contrib = sign * mag_from_input;
                        // Sum across all (mapping × in/out pair) contributions.
                        let entry = analog_axis_acc.entry(axis_pin).or_insert(0.0);
                        *entry += contrib;
                    } else if let Some(trigger_pin) = out_trigger {
                        // One-sided 0..1 trigger axis — drive it with the input's
                        // live magnitude (converts analog stick direction into
                        // analog trigger travel, incl. on pads lacking analog
                        // triggers like Switch Pro).
                        let entry = analog_axis_acc.entry(trigger_pin).or_insert(0.0);
                        *entry += mag_from_input.max(0.0);
                    } else {
                        // Non-cardinal out: button / key. Analog magnitude
                        // drives a freq-modulated tap train (or PWM under
                        // Hold) so a digital destination still reflects HOW
                        // FAR the stick is pushed — matching the 3DOF-Lean
                        // analog→digital behaviour.
                        let active = analog_digital_pulse(
                            mag_from_input, window_ms, sustain, turbo, slots, dt,
                        );
                        if active {
                            analog_button_out.insert(out_p.clone());
                        }
                    }
                }
            }
            // Commit axis accumulator: clamp ±1 then write.
            for (axis_pin, v) in &analog_axis_acc {
                let clamped = v.clamp(-1.0, 1.0);
                collector_sigs.insert((key.clone(), (*axis_pin).to_string()), Signal::Float(clamped));
            }
            // Update bundled Vec2 pins so downstream sinks that read the
            // Vec2 form (`left_stick`/`right_stick`) see the analog-driven
            // values too. Without this, the sink's Vec2-vs-axis conflict
            // resolver picks the Vec2 (which still carries the suppressed
            // pass-through) and drops the analog axis writes.
            for (vec2_pin, x_axis, y_axis) in [
                ("left_stick", "left_stick_x", "left_stick_y"),
                ("right_stick", "right_stick_x", "right_stick_y"),
            ] {
                let x_override = analog_axis_acc.get(&x_axis).copied();
                let y_override = analog_axis_acc.get(&y_axis).copied();
                if x_override.is_none() && y_override.is_none() { continue; }
                let cur = collector_sigs.get(&(key.clone(), vec2_pin.to_string()))
                    .and_then(|s| if let Signal::Vec2(v) = s { Some(*v) } else { None })
                    .unwrap_or(Vec2::ZERO);
                let x = x_override.map(|v| v.clamp(-1.0, 1.0)).unwrap_or(cur.x);
                let y = y_override.map(|v| v.clamp(-1.0, 1.0)).unwrap_or(cur.y);
                collector_sigs.insert((key.clone(), vec2_pin.to_string()), Signal::Vec2(Vec2::new(x, y)));
            }

            // ── Digital publish pass (existing semantics) ────────────────
            //
            // Collect every output pin mentioned in any DIGITAL mapping so
            // released ones can publish false/0. Analog-only out pins are
            // handled by the analog pass above.
            let mut digital_all_out_pins: HashSet<String> = HashSet::new();
            for (i, m) in mappings.iter().enumerate() {
                let is_analog = m.get("mode").and_then(|v| v.as_str()) == Some("analog");
                if is_analog { continue; }
                let _ = i;
                if let Some(arr) = m.get("out").and_then(|v| v.as_array()) {
                    for v in arr {
                        if let Some(s) = v.as_str() {
                            digital_all_out_pins.insert(s.to_string());
                        }
                    }
                }
            }
            let mut digital_asserted: HashSet<String> = HashSet::new();
            for (_, out_pins, is_analog, _) in &triggered {
                if *is_analog { continue; }
                for p in out_pins { digital_asserted.insert(p.clone()); }
            }
            for out_pin in &digital_all_out_pins {
                let sig_type = automap::ALL_PINS.iter()
                    .find(|p| p.id == out_pin.as_str())
                    .map(|p| p.signal_type)
                    .unwrap_or(SignalType::Bool);
                let on = digital_asserted.contains(out_pin);
                if on {
                    let sig = match sig_type {
                        SignalType::Float => Signal::Float(1.0),
                        SignalType::Vec2  => continue,
                        SignalType::Int   => Signal::Int(1),
                        _                 => Signal::Bool(true),
                    };
                    collector_sigs.insert((key.clone(), out_pin.clone()), sig);
                } else {
                    if upstream.contains_key(out_pin.as_str()) { continue; }
                    // If an analog mapping has already written this same out
                    // pin (e.g., the user fans a button to it from a different
                    // mapping), don't overwrite with zero.
                    if analog_button_out.contains(out_pin)
                        || analog_axis_acc.iter().any(|(ap, _)| *ap == out_pin.as_str())
                    {
                        continue;
                    }
                    let sig = match sig_type {
                        SignalType::Float => Signal::Float(0.0),
                        SignalType::Vec2  => continue,
                        SignalType::Int   => Signal::Int(0),
                        _                 => Signal::Bool(false),
                    };
                    collector_sigs.insert((key.clone(), out_pin.clone()), sig);
                }
            }

            // ── Analog button-out emissions + release pass ───────────────
            //
            // Bool/Int analog out pins: write true while active. For released
            // analog out pins (mapping inactive this tick), write false/0
            // only when upstream doesn't naturally emit it (mirrors digital
            // release rule).
            let mut analog_button_pins: HashSet<String> = HashSet::new();
            for m in &mappings {
                let is_analog = m.get("mode").and_then(|v| v.as_str()) == Some("analog");
                if !is_analog { continue; }
                if let Some(arr) = m.get("out").and_then(|v| v.as_array()) {
                    for v in arr {
                        if let Some(s) = v.as_str() {
                            // Triggers are analog axes (handled by analog_axis_acc),
                            // not buttons — exclude them from the binary on/off
                            // release pass or it would clobber the analog value.
                            if analog_axis_for_cardinal(s).is_none()
                                && analog_trigger_out(s).is_none()
                            {
                                analog_button_pins.insert(s.to_string());
                            }
                        }
                    }
                }
            }
            for out_pin in &analog_button_pins {
                if digital_asserted.contains(out_pin) { continue; } // digital wins for this pin
                let on = analog_button_out.contains(out_pin);
                let sig_type = automap::ALL_PINS.iter()
                    .find(|p| p.id == out_pin.as_str())
                    .map(|p| p.signal_type)
                    .unwrap_or(SignalType::Bool);
                if on {
                    let sig = match sig_type {
                        SignalType::Float => Signal::Float(1.0),
                        SignalType::Vec2  => continue,
                        SignalType::Int   => Signal::Int(1),
                        _                 => Signal::Bool(true),
                    };
                    collector_sigs.insert((key.clone(), out_pin.clone()), sig);
                } else {
                    if upstream.contains_key(out_pin.as_str()) { continue; }
                    let sig = match sig_type {
                        SignalType::Float => Signal::Float(0.0),
                        SignalType::Vec2  => continue,
                        SignalType::Int   => Signal::Int(0),
                        _                 => Signal::Bool(false),
                    };
                    collector_sigs.insert((key.clone(), out_pin.clone()), sig);
                }
            }
}

/// Evaluate a Map Action node — shared by the top-level and sub-patch loops.
/// Returns the 2-element output vec [Bool gate, Float analog]. `uid` is the
/// publishing id (snap.node_uid at top level, namespaced uid in a sub-patch);
/// it keys the per-node `state`.
fn eval_map_action_node(
    snap: &NodeSnap,
    uid: usize,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &HashMap<(String, String), Signal>,
    state: &mut HashMap<usize, NodeState>,
    dt: f32,
) -> Vec<Option<Signal>> {
            let dev_id = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("");
            let collector_id = snap.params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("");
            let mappings = snap.params.get("mappings").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            // Snapshot upstream values for every canonical pin once.
            let mut upstream: HashMap<String, Signal> = HashMap::new();
            for ap in automap::ALL_PINS {
                let sig = if !collector_id.is_empty() {
                    collector_sigs.get(&(collector_id.to_string(), ap.id.to_string())).copied()
                } else { None }
                .or_else(|| {
                    if !dev_id.is_empty() {
                        dev_sigs.get(&(dev_id.to_string(), ap.id.to_string())).copied()
                    } else { None }
                });
                if let Some(s) = sig { upstream.insert(ap.id.to_string(), s); }
            }
            // A processed Vec2 on the collector is authoritative over raw axes.
            vec2_authoritative_axis_fill(&mut upstream, collector_id, &collector_sigs);
            // Derive synthetic pins (stick cardinals + touchpad variants)
            derive_stick_cardinals(&mut upstream);
            // Touchpad handling mirrors Remapper's behaviour (click accumulation)
            let touch_click = upstream.get("btn_touchpad").map(|s| s.as_bool()).unwrap_or(false);
            let zone_of_x = |x: f32| -> usize {
                if x < -1.0/3.0 { 0 } else if x > 1.0/3.0 { 2 } else { 1 }
            };
            let mut touch_only = [false; 3];
            for (xpin, apin) in [("touch1_x","touch1_active"), ("touch2_x","touch2_active")] {
                let active = upstream.get(apin).map(|s| s.as_bool()).unwrap_or(false);
                if !active { continue; }
                let x = upstream.get(xpin).map(|s| sig_scalar(*s)).unwrap_or(0.0);
                touch_only[zone_of_x(x)] = true;
            }
            // Click-variant zones are stored in per-node state; reuse NodeState aux_f32
            let ns = state.entry(uid).or_insert_with(NodeState::default);
            if ns.aux_f32.len() < 3 { ns.aux_f32.resize(3, 0.0); }
            if !touch_click {
                ns.aux_f32[0] = 0.0; ns.aux_f32[1] = 0.0; ns.aux_f32[2] = 0.0;
            } else {
                for (xpin, apin) in [("touch1_x","touch1_active"), ("touch2_x","touch2_active")] {
                    let active = upstream.get(apin).map(|s| s.as_bool()).unwrap_or(false);
                    if !active { continue; }
                    let x = upstream.get(xpin).map(|s| sig_scalar(*s)).unwrap_or(0.0);
                    ns.aux_f32[zone_of_x(x)] = 1.0;
                }
            }
            let click_zone = [ ns.aux_f32[0] > 0.5, ns.aux_f32[1] > 0.5, ns.aux_f32[2] > 0.5 ];
            let any_zone = click_zone[0] || click_zone[1] || click_zone[2];
            if touch_click { touch_only = [false;3]; }
            upstream.insert("touchpad_left".to_string(),   Signal::Bool(click_zone[0]));
            upstream.insert("touchpad_center".to_string(), Signal::Bool(click_zone[1]));
            upstream.insert("touchpad_right".to_string(),  Signal::Bool(click_zone[2]));
            upstream.insert("touchpad_any".to_string(),    Signal::Bool(touch_click && any_zone));
            upstream.insert("touch_left".to_string(),      Signal::Bool(touch_only[0]));
            upstream.insert("touch_center".to_string(),    Signal::Bool(touch_only[1]));
            upstream.insert("touch_right".to_string(),     Signal::Bool(touch_only[2]));

            let read_upstream = |pin_id: &str| -> Option<Signal> { upstream.get(pin_id).copied() };

            // Mappings may be in legacy Array<String> form (chord only, mode=down)
            // or in the new Object form `{ in, mode, window_ms, sustain }`.
            //
            // Output signal kind depends on which mode(s) are present:
            //   - All-digital mappings → emit Bool ("any active").
            //   - Any analog mapping present → emit Float (max magnitude
            //     across all active analog mappings, falling back to 1.0
            //     when only a digital mapping is active so digital triggers
            //     still drive Float-consuming wires at full deflection).
            let ns_map = state.entry(uid).or_insert_with(NodeState::default);
            let mut any_trigger = false;
            let mut any_analog_present = false;
            let mut max_analog_mag: f32 = 0.0;
            for (i, m) in mappings.iter().enumerate() {
                let (in_pins, mode_s, window_ms, sustain, turbo) = if let Some(arr) = m.as_array() {
                    let pins: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
                    (pins, "down", 200.0_f32, false, false)
                } else {
                    let pins: Vec<&str> = m.get("in").and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                        .unwrap_or_default();
                    let mode = m.get("mode").and_then(|v| v.as_str()).unwrap_or("down");
                    let win  = m.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(200.0) as f32;
                    let sus  = m.get("sustain").and_then(|v| v.as_bool()).unwrap_or(false);
                    let tur  = m.get("turbo").and_then(|v| v.as_bool()).unwrap_or(false);
                    (pins, mode, win, sus, tur)
                };
                if in_pins.is_empty() { continue; }
                if mode_s == "analog" {
                    any_analog_present = true;
                    // Combo gate (same as Remapper): all non-cardinal pins
                    // held AND any cardinal contributing a non-zero mag.
                    // Track the strongest cardinal magnitude for Float out.
                    let mut has_cardinal = false;
                    let mut any_cardinal_active = false;
                    let mut all_buttons_held = true;
                    let mut local_max: f32 = 0.0;
                    for p in &in_pins {
                        if analog_axis_for_cardinal(p).is_some() {
                            has_cardinal = true;
                            let mag = analog_cardinal_input_value(&upstream, p);
                            if mag > 0.0 { any_cardinal_active = true; }
                            if mag > local_max { local_max = mag; }
                        } else if !read_upstream(p).map(|s| s.as_bool()).unwrap_or(false) {
                            all_buttons_held = false;
                        }
                    }
                    let active = all_buttons_held && (!has_cardinal || any_cardinal_active);
                    // For pure-button analog (no cardinal), magnitude defaults
                    // to 1.0 while gated so the Float output reads full.
                    let mag = if !active {
                        0.0
                    } else if has_cardinal { local_max } else { 1.0 };
                    // out_analog: pure magnitude (max across active mappings).
                    if mag > max_analog_mag { max_analog_mag = mag; }
                    // out (Bool): freq-modulated tap train / PWM (Hold) / ×2
                    // (Turbo) driven by the magnitude, so a digital destination
                    // reflects how far the input is pushed.
                    let slots = press_state_get(ns_map, i);
                    if analog_digital_pulse(mag, window_ms, sustain, turbo, slots, dt) {
                        any_trigger = true;
                    }
                    continue;
                }
                // All-cardinal chords on a single stick can't be
                // simultaneously held — use the gesture-visited bitmap so
                // half-circles and full sweeps complete the combo. Mirrors
                // Remapper's digital path.
                let raw_held = if let Some(required) = gesture_required_bits(&in_pins) {
                    let buttons_held = in_pins.iter().all(|p| {
                        if gesture_pin_to_bit(p).is_some() { return true; }
                        read_upstream(p).map(|s| s.as_bool()).unwrap_or(false)
                    });
                    let visited = gesture_state_get(ns_map, i);
                    buttons_held && gesture_tick(required, visited, &upstream)
                } else {
                    in_pins.iter().all(|p| {
                        read_upstream(p).map(|s| s.as_bool()).unwrap_or(false)
                    })
                };
                let mode = PressMode::from_str(mode_s);
                let slots = press_state_get(ns_map, i);
                let held = apply_press_mode(raw_held, mode, window_ms, sustain, slots, dt);
                let held = if turbo { apply_turbo(held, window_ms, slots, dt) } else { held };
                if held { any_trigger = true; }
            }

            // Two outputs: out (Bool gate/tap-train) + out_analog (Float mag).
            // out_analog falls back to 1.0 when only digital mappings drove the
            // gate so a Float-consuming wire still sees full deflection.
            let analog_mag = if max_analog_mag > 0.0 {
                max_analog_mag
            } else if any_trigger && !any_analog_present { 1.0 } else { max_analog_mag };
            return vec![
                Some(Signal::Bool(any_trigger)),
                Some(Signal::Float(analog_mag.clamp(0.0, 1.0))),
            ];
}

fn remapper_pass_through_and_suppress(
    key: &str,
    upstream: &HashMap<String, Signal>,
    claimed_digital: &HashSet<String>,
    claimed_analog: &HashSet<String>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
) {
    let mut all_claimed: HashSet<String> = claimed_digital.clone();
    all_claimed.extend(claimed_analog.iter().cloned());
    let suppression = cardinal_suppression(&all_claimed);

    for ap in automap::ALL_PINS {
        let raw = if all_claimed.contains(ap.id) {
            None
        } else {
            upstream.get(ap.id).copied()
        };
        let Some(raw) = raw else {
            if all_claimed.contains(ap.id) {
                let off = match ap.signal_type {
                    SignalType::Bool  => Signal::Bool(false),
                    SignalType::Float => Signal::Float(0.0),
                    SignalType::Vec2  => Signal::Vec2(Vec2::ZERO),
                    SignalType::Int   => Signal::Int(0),
                    _ => continue,
                };
                collector_sigs.insert((key.to_string(), ap.id.to_string()), off);
            }
            continue;
        };
        let sig = suppress_signal_for_pin(ap.id, raw, &suppression);
        collector_sigs.insert((key.to_string(), ap.id.to_string()), sig);
    }

    // Recompute synthetic stick cardinals from the (possibly clamped) axes so
    // downstream consumers see consistent cardinal bools.
    {
        let mut local_up: HashMap<String, Signal> = HashMap::new();
        for axis in ["left_stick_x", "left_stick_y", "right_stick_x", "right_stick_y"] {
            if let Some(&sig) = collector_sigs.get(&(key.to_string(), axis.to_string())) {
                local_up.insert(axis.to_string(), sig);
            }
        }
        derive_stick_cardinals(&mut local_up);
        for (k, v) in local_up {
            if k.contains("_stick_") && (k.ends_with("_up") || k.ends_with("_down")
                || k.ends_with("_left") || k.ends_with("_right"))
            {
                collector_sigs.insert((key.to_string(), k), v);
            }
        }
    }

    publish_consumed_markers(key, claimed_digital, claimed_analog, collector_sigs);
}

/// Apply per-side `CardinalSuppression` to one pin's raw pass-through value:
///   - axis Float (`dpad_x`, `left_stick_y`, …): clamp the consumed side(s).
///   - bundled Vec2 (`dpad`, `left_stick`, …): clamp each component's side(s).
///   - claimed cardinal Bool: forced false.
///   - everything else: unchanged.
/// Only the directions the user mapped are affected; the rest pass through.
fn suppress_signal_for_pin(
    pin_id: &str,
    raw: Signal,
    sup: &CardinalSuppression,
) -> Signal {
    // Claimed cardinal Bool → off.
    if sup.bool_pins.contains(pin_id) {
        return Signal::Bool(false);
    }
    // Axis Float → side clamp.
    if let Some(&(neg, pos)) = sup.axis_sides.get(pin_id) {
        if let Signal::Float(v) = raw {
            return Signal::Float(apply_axis_clamp(v, (neg, pos)));
        }
        return raw;
    }
    // Bundled Vec2 → per-component side clamp.
    let axes: Option<(&str, &str)> = match pin_id {
        "left_stick"  => Some(("left_stick_x",  "left_stick_y")),
        "right_stick" => Some(("right_stick_x", "right_stick_y")),
        "dpad"        => Some(("dpad_x",         "dpad_y")),
        _ => None,
    };
    if let Some((xa, ya)) = axes {
        let xs = sup.axis_sides.get(xa).copied().unwrap_or((false, false));
        let ys = sup.axis_sides.get(ya).copied().unwrap_or((false, false));
        if xs == (false, false) && ys == (false, false) {
            return raw;
        }
        if let Signal::Vec2(v) = raw {
            return Signal::Vec2(Vec2::new(
                apply_axis_clamp(v.x, xs),
                apply_axis_clamp(v.y, ys),
            ));
        }
    }
    raw
}

/// Map an analog-mode output pin to its one-sided trigger axis, if it is one.
/// Triggers are 0..1 (no negative side), so analog mappings drive them with the
/// input's unsigned magnitude. Returns the trigger pin id, or None for non-trigger
/// outputs (which the caller treats as cardinal axes or buttons).
///
/// The digital trigger buttons (`btn_lt_dig`/`btn_rt_dig`) also map here: a
/// Remapper captures its output by chord-learning, so on a pad whose trigger is
/// a digital button (Switch Pro ZL/ZR) the captured `out` pin is the digital
/// button, not the analog trigger. In ANALOG mode the user's intent is analog
/// travel, so we route the digital-trigger-button target to its analog pin.
fn analog_trigger_out(pin_id: &str) -> Option<&'static str> {
    match pin_id {
        "left_trigger"  | "btn_lt_dig" => Some("left_trigger"),
        "right_trigger" | "btn_rt_dig" => Some("right_trigger"),
        _ => None,
    }
}

/// Return the signed analog magnitude an input cardinal currently contributes
/// to its axis: 0.0 when the stick is neutral or pushed in the opposite
/// direction; up to ±1.0 at full deflection in the cardinal's direction.
/// Used by analog-mode Remapper / Map Action to drive output axes from
/// input cardinals' live magnitudes (no gesture gate).
fn analog_cardinal_input_value(upstream: &HashMap<String, Signal>, pin_id: &str) -> f32 {
    let Some((axis_pin, cardinal_sign)) = analog_axis_for_cardinal(pin_id) else { return 0.0; };
    let axis_val = upstream.get(axis_pin).map(|s| sig_scalar(*s)).unwrap_or(0.0);
    let signed = axis_val * cardinal_sign;
    signed.max(0.0).min(1.0)
}


/// Apply axis-side suppression to a stick axis Float value. `(neg, pos)` —
/// when `neg` is true, clamp negative values to 0; when `pos` is true,
/// clamp positive values to 0.
fn apply_axis_clamp(v: f32, suppress: (bool, bool)) -> f32 {
    let (neg, pos) = suppress;
    let mut out = v;
    if neg && out < 0.0 { out = 0.0; }
    if pos && out > 0.0 { out = 0.0; }
    out
}

/// Shared lean-dispatch for the 3DOF module. Called from both the
/// top-level eval loop and the subgraph eval loop with the appropriate
/// UID (snap.node_uid for top-level, ns_uid for subpatches). Writes to
/// `collector_sigs[("lean:UID", pin_id)]` for every output pin named in
/// any `lean_left` / `lean_right` mapping.
fn lean_dispatch_into_collector_sigs(
    snap: &NodeSnap,
    uid: usize,
    node_outputs: &[Option<Signal>],
    node_state: &mut NodeState,
    collector_sigs: &mut HashMap<(String, String), Signal>,
    dt: f32,
) {
    let lean_val = node_outputs.get(3)
        .and_then(|s| *s)
        .map(|s| s.as_float())
        .unwrap_or(0.0);
    let lean_threshold = snap.params.get("lean_threshold")
        .and_then(|v| v.as_f64()).map(|f| f as f32).unwrap_or(0.3);
    let left_active  = lean_val <= -lean_threshold;
    let right_active = lean_val >=  lean_threshold;
    let lean_mag = lean_val.abs().min(1.0);

    let lean_left  = snap.params.get("lean_left")
        .and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let lean_right = snap.params.get("lean_right")
        .and_then(|v| v.as_array()).cloned().unwrap_or_default();

    // Collect every output pin mentioned in any mapping so released ones
    // can publish false/0. Stick cardinals always also publish to their
    // underlying analog axis (Analog and non-Analog modes both emit on
    // the axis — cardinals aren't valid sink pin ids on their own, so
    // without the axis remap nothing reaches the destination device).
    let mut all_out_pins: HashSet<String> = HashSet::new();
    for m in lean_left.iter().chain(lean_right.iter()) {
        if let Some(arr) = m.get("out").and_then(|v| v.as_array()) {
            for v in arr {
                if let Some(s) = v.as_str() {
                    all_out_pins.insert(s.to_string());
                    if let Some((axis_pin, _)) = analog_axis_for_cardinal(s) {
                        all_out_pins.insert(axis_pin.to_string());
                    }
                }
            }
        }
    }

    let mut asserted: HashMap<String, Signal> = HashMap::new();

    for (side_idx, side_pair) in [
        (left_active, &lean_left), (right_active, &lean_right),
    ].iter().enumerate() {
        let (active, mappings) = side_pair;
        let base_idx = if side_idx == 0 { 0 } else { lean_left.len() };
        for (i, m) in mappings.iter().enumerate() {
            let out_pins: Vec<String> = m.get("out").and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            if out_pins.is_empty() { continue; }
            let mode_s    = m.get("mode").and_then(|v| v.as_str()).unwrap_or("down");
            let window_ms = m.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(200.0) as f32;
            let sustain   = m.get("sustain").and_then(|v| v.as_bool()).unwrap_or(false);
            let turbo     = m.get("turbo").and_then(|v| v.as_bool()).unwrap_or(false);

            let slots = press_state_get(node_state, base_idx + i);

            let (held_now, analog_val_opt): (bool, Option<f32>) = if mode_s == "analog" {
                if !*active || lean_mag < 0.01 {
                    slots[0] = 0.0;
                    (false, Some(0.0))
                } else {
                    // Shared analog→digital modulation: Hold → PWM (duty =
                    // lean_mag), Turbo → ×2 max frequency, plain → tap train
                    // whose frequency tracks lean_mag. Float destinations
                    // ignore `pulse_on` and use `lean_mag` directly below.
                    let pulse_on = analog_digital_pulse(
                        lean_mag, window_ms, sustain, turbo, slots, dt,
                    );
                    (pulse_on, Some(lean_mag))
                }
            } else {
                let mode = PressMode::from_str(mode_s);
                let held = apply_press_mode(*active, mode, window_ms, sustain, slots, dt);
                let held = if turbo { apply_turbo(held, window_ms, slots, dt) } else { held };
                (held, None)
            };

            let is_analog_mode = mode_s == "analog";
            for p in &out_pins {
                // Cardinal → analog-axis remap (all press modes):
                // A stick-cardinal like `left_stick_right` represents the
                // user's INTENT to drive that axis in that direction. The
                // cardinal pin id isn't a valid sink pin on any virtual
                // gamepad — the actual emit must go to the underlying
                // axis (left_stick_x / left_stick_y) with the cardinal's
                // sign (right/up = +, left/down = -). In Analog mode the
                // magnitude tracks lean_mag; in other press modes it's a
                // gated full-deflection write (±1.0 when held, 0 when not).
                if let Some((axis_pin, cardinal_sign)) = analog_axis_for_cardinal(p.as_str()) {
                    let mag = if is_analog_mode {
                        if !*active { 0.0 } else { analog_val_opt.unwrap_or(1.0) }
                    } else if held_now {
                        1.0
                    } else {
                        0.0
                    };
                    if mag > 0.0 {
                        let new_v = cardinal_sign * mag;
                        let sig = Signal::Float(new_v);
                        // Combine if multiple mappings target the same axis
                        // — use the larger-magnitude write (winning sign).
                        asserted
                            .entry(axis_pin.to_string())
                            .and_modify(|existing| {
                                if let Signal::Float(prev) = existing {
                                    if new_v.abs() > prev.abs() {
                                        *existing = Signal::Float(new_v);
                                    }
                                }
                            })
                            .or_insert(sig);
                    }
                    continue;
                }
                let sig_type = automap::ALL_PINS.iter()
                    .find(|x| x.id == p.as_str())
                    .map(|x| x.signal_type).unwrap_or(SignalType::Bool);
                let emit = match (is_analog_mode, sig_type) {
                    (true, SignalType::Float) => *active,
                    (true, SignalType::Vec2)  => false,
                    (true, _)                 => held_now,
                    (false, _)                => held_now,
                };
                if !emit { continue; }
                let sig = match sig_type {
                    SignalType::Float => {
                        let mag = analog_val_opt.unwrap_or(1.0);
                        let signed = if is_analog_mode {
                            if side_idx == 0 { -mag } else { mag }
                        } else { mag };
                        Signal::Float(signed)
                    }
                    SignalType::Vec2 => continue,
                    SignalType::Int   => Signal::Int(1),
                    _                 => Signal::Bool(true),
                };
                asserted.entry(p.clone()).or_insert(sig);
            }
        }
    }

    let key = format!("lean:{}", uid);
    for p in &all_out_pins {
        let sig_type = automap::ALL_PINS.iter().find(|x| x.id == p.as_str())
            .map(|x| x.signal_type).unwrap_or(SignalType::Bool);
        let sig = asserted.get(p).copied().unwrap_or_else(|| {
            match sig_type {
                SignalType::Float => Signal::Float(0.0),
                SignalType::Vec2  => Signal::Vec2(Vec2::ZERO),
                SignalType::Int   => Signal::Int(0),
                _                 => Signal::Bool(false),
            }
        });
        collector_sigs.insert((key.clone(), p.clone()), sig);
    }
}

/// Translate legacy `mode` strings to the new (family, axis) split so saved
/// patches keep working without manual migration.
fn gyro_resolve_mode(params: &HashMap<String, Value>) -> (&'static str, &'static str) {
    if let Some(family) = params.get("family").and_then(|v| v.as_str()) {
        let axis = params.get("axis").and_then(|v| v.as_str()).unwrap_or("pitch_yaw");
        let f: &'static str = match family { "steering" => "steering", _ => "pointer" };
        let a: &'static str = match axis {
            "pitch_roll" => "pitch_roll",
            "player"     => "player",
            "world"      => "world",
            _            => "pitch_yaw",
        };
        return (f, a);
    }
    // Legacy fallback: old `mode` string.
    match params.get("mode").and_then(|v| v.as_str()).unwrap_or("local") {
        "player" => ("pointer",  "player"),
        "world"  => ("pointer",  "world"),
        "laser"  => ("steering", "pitch_yaw"),
        _        => ("pointer",  "pitch_yaw"),
    }
}

fn compute_gyro_3dof(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &HashMap<(String, String), Signal>,
    dt: f32,
) -> Vec<Option<Signal>> {
    let (family, axis) = gyro_resolve_mode(params);

    let inv = |name: &str| -> f32 {
        if params.get(name).and_then(|v| v.as_bool()).unwrap_or(false) { -1.0 } else { 1.0 }
    };
    let pf = |name: &str, default: f32| -> f32 {
        params.get(name).and_then(|v| v.as_f64()).map(|x| x as f32).unwrap_or(default)
    };
    let pb = |name: &str, default: bool| -> bool {
        params.get(name).and_then(|v| v.as_bool()).unwrap_or(default)
    };

    // Auto-map path: read all six axes from the connected device.
    let (gx_am, gy_am, gz_am, ax_am, ay_am, az_am) =
        if let Some(dev_id) = params.get("_automap_device_id").and_then(|v| v.as_str()) {
            let collector_id = params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("");
            let get = |pin: &str| -> f32 {
                if !collector_id.is_empty() {
                    if let Some(Signal::Float(f)) = collector_sigs.get(&(collector_id.to_string(), pin.to_string())) {
                        return *f;
                    }
                }
                match dev_sigs.get(&(dev_id.to_string(), pin.to_string())) {
                    Some(Signal::Float(f)) => *f,
                    _ => 0.0,
                }
            };
            let az_raw = {
                let pin = "accel_z";
                if !collector_id.is_empty() {
                    if let Some(Signal::Float(f)) = collector_sigs.get(&(collector_id.to_string(), pin.to_string())) {
                        *f
                    } else {
                        match dev_sigs.get(&(dev_id.to_string(), pin.to_string())) {
                            Some(Signal::Float(f)) => *f,
                            _ => 1.0,
                        }
                    }
                } else {
                    match dev_sigs.get(&(dev_id.to_string(), pin.to_string())) {
                        Some(Signal::Float(f)) => *f,
                        _ => 1.0,
                    }
                }
            };
            (get("gyro_x"), get("gyro_y"), get("gyro_z"), get("accel_x"), get("accel_y"), az_raw)
        } else {
            (0.0, 0.0, 0.0, 0.0, 0.0, 1.0)
        };

    // Direct pin overrides (inputs 2–7: Gyro X/Y/Z, Accel X/Y/Z).
    let pin_or = |idx: usize, fallback: f32| -> f32 {
        if inputs.get(idx).and_then(|s| *s).is_some() { get_f(inputs, idx, fallback) } else { fallback }
    };
    let gx = pin_or(2, gx_am) * inv("inv_roll");
    let gy = pin_or(3, gy_am);
    let gz = pin_or(4, gz_am);
    let ax = pin_or(5, ax_am) * inv("inv_accel_x");
    let ay = pin_or(6, ay_am) * inv("inv_accel_y");
    let az = pin_or(7, az_am) * inv("inv_accel_z");
    // (Spike suppression moved to the device polling layer — see
    // `flexinput_devices::gyro::apply_spike_filter`. The engine sees an
    // already-clean IMU stream.)

    // aux_f32 layout:
    //   [0] integrated steering X
    //   [1] integrated steering Y
    //   [2] smoothed gravity X (player/world)
    //   [3] smoothed gravity Y
    //   [4] smoothed gravity Z
    //   [5] prev_reset edge guard
    //   [6] ease-in residual (0..1 progresses while resetting)
    while state.aux_f32.len() < 7 { state.aux_f32.push(0.0); }

    // ── Axis selection: decide which gyro components feed X / Y ───────────
    //
    // For Player/World we project gyro onto the gravity-corrected frame.
    // For Pitch+Yaw and Pitch+Roll, the X/Y feed is gyro rates as before.
    //
    // Lean is derived separately below from accel tilt (NOT a gyro rate) so
    // that holding a tilted controller still asserts a steady lean signal
    // and rocking back through center doesn't produce a spurious opposite
    // lean. See the lean derivation block after this match.
    let (raw_x, raw_y, _raw_lean_unused) = match axis {
        "pitch_roll" => (gx, gy, gz),
        "player" | "world" => {
            let gyro  = glam::Vec3::new(gx, gy, gz);
            let accel = glam::Vec3::new(ax, ay, az);
            let tau = if axis == "world" { 3.0_f32 } else { 1.0_f32 };
            let alpha = 1.0 - (-dt / tau).exp();
            let acc_mag = accel.length();
            if acc_mag > 0.01 {
                let norm = accel / acc_mag;
                state.aux_f32[2] += alpha * (norm.x - state.aux_f32[2]);
                state.aux_f32[3] += alpha * (norm.y - state.aux_f32[3]);
                state.aux_f32[4] += alpha * (norm.z - state.aux_f32[4]);
            }
            let sg = glam::Vec3::new(state.aux_f32[2], state.aux_f32[3], state.aux_f32[4]);
            let sg_len = sg.length();
            let g_hat = if sg_len > 0.01 { sg / sg_len } else { glam::Vec3::new(0.0, 0.0, 1.0) };
            let world_yaw   = gyro.dot(g_hat);
            let gyro_no_yaw = gyro - world_yaw * g_hat;
            (world_yaw, gyro_no_yaw.y, 0.0)
        }
        _ => (gz, gy, 0.0), // pitch_yaw: gz=yaw→X, gy=pitch→Y
    };

    // ── Steering integration + auto-recentering ───────────────────────────
    let reset_now = get_b(inputs, 1, false);
    let reset_edge = reset_now && state.aux_f32[5] < 0.5;
    state.aux_f32[5] = if reset_now { 1.0 } else { 0.0 };

    let (out_x, out_y) = if family == "steering" {
        // `exclude_y` suppresses the Y *output* — keeps X integrating as
        // usual, but Y stays at zero. Use when the steering axis is the
        // only thing you want from this module (e.g. a vehicle's wheel).
        let exclude_y = pb("steering_exclude_y", false);
        let recenter_strength = pf("recenter_strength", 0.0).clamp(0.0, 4.0); // sec⁻¹ pull rate
        let ease_in = pf("reset_ease_in", 0.25).clamp(0.0, 2.0);

        // Integrate both accumulators every tick — `exclude_y` only gates
        // the output, not the integration, so toggling it on/off doesn't
        // leave the Y accumulator stale.
        state.aux_f32[0] += raw_x * dt;
        state.aux_f32[1] += raw_y * dt;

        // X recenter — gated by axis (yaw isn't observable when flat).
        //   Pitch+Yaw  : heading = atan2(ay, ax), weight ≈ |sin tilt|
        //   Pitch+Roll : heading = atan2(ay, az), weight ≈ cos pitch
        //   Player/World: skipped (azimuth around gravity unobservable
        //                  from accel alone).
        //
        // Y recenter is intentionally NOT implemented as an independent
        // atan2 — the per-axis approach couples X and Y badly (Y motion
        // → large ax → atan2(ay, ax) whiplash → spurious X drift). The
        // proper fix is to maintain a continuous 3DOF pose estimate and
        // project both axes from it; that rework is pending. Until then,
        // Y centers only via the manual reset (ease_in).
        if recenter_strength > 0.0 && (axis == "pitch_yaw" || axis == "pitch_roll") {
            let acc_mag = (ax * ax + ay * ay + az * az).sqrt().max(1e-3);
            let (heading, weight) = if axis == "pitch_roll" {
                let w = (ay * ay + az * az).sqrt() / acc_mag;
                (ay.atan2(az), w)
            } else {
                // pitch_yaw
                let w = (ax * ax + ay * ay).sqrt() / acc_mag;
                (ay.atan2(ax), w)
            };
            let two_pi = std::f32::consts::TAU;
            let mut delta = heading - state.aux_f32[0];
            // Wrap to (-π, π] without depending on rem_euclid edge cases.
            delta -= two_pi * ((delta / two_pi) + 0.5).floor();
            let alpha = (recenter_strength * weight * dt).clamp(0.0, 1.0);
            state.aux_f32[0] += alpha * delta;
        }

        // Reset edge: start an ease-in toward zero. While ease-in > 0 we
        // blend the steering accumulator toward 0 over `ease_in` seconds.
        if reset_edge { state.aux_f32[6] = 1.0; }
        if state.aux_f32[6] > 0.001 && ease_in > 0.001 {
            let step = (dt / ease_in).clamp(0.0, 1.0);
            state.aux_f32[0] *= 1.0 - step;
            state.aux_f32[1] *= 1.0 - step;
            state.aux_f32[6] = (state.aux_f32[6] - step).max(0.0);
        } else if reset_edge && ease_in <= 0.001 {
            state.aux_f32[0] = 0.0;
            state.aux_f32[1] = 0.0;
            state.aux_f32[6] = 0.0;
        }

        let x_out = state.aux_f32[0];
        let y_out = if exclude_y { 0.0 } else { state.aux_f32[1] };
        (x_out, y_out)
    } else {
        // Pointer family: pass-through angular velocity (or projected component).
        // Reset has no effect — there's no accumulator.
        (raw_x, raw_y)
    };

    // Apply yaw/pitch inversions to final output (NOT inside dot-product math).
    let final_x = out_x * inv("inv_yaw");
    let final_y = out_y * inv("inv_pitch");

    // ── Lean output: tilt fraction from accelerometer ─────────────────────
    //
    // Lean is the controller's signed side-tilt as a fraction of full
    // sideways. Positive = right lean (right grip drops, +ay in FlexInput
    // accel convention). Magnitude in [0, 1] where 1 ≈ on its side.
    //
    // This is derived from accel ONLY, not gyro rate, so:
    //   - Holding a tilted controller produces a STEADY non-zero lean.
    //   - Returning to neutral smoothly ramps back to 0 (no spurious
    //     opposite spike like raw gyro rate would give).
    //
    // For Pitch+Roll / Player / World modes the rotation around gravity
    // is not directly observable from accel; we still use the same side-
    // tilt measure since "is the controller tilted sideways" is the
    // intuitive lean axis regardless of how X/Y are derived.
    let acc_mag_full = (ax * ax + ay * ay + az * az).sqrt().max(1e-3);
    let lean_val = (ay / acc_mag_full).clamp(-1.0, 1.0);
    let lean_threshold = pf("lean_threshold", 0.3).clamp(0.01, 4.0);
    let lean_active = lean_val.abs() >= lean_threshold;

    vec![
        Some(Signal::Vec2(glam::Vec2::new(final_x, final_y))),
        Some(Signal::Float(final_x)),
        Some(Signal::Float(final_y)),
        Some(Signal::Float(lean_val)),
        Some(Signal::Bool(lean_active)),
        // Map (AutoMap) — routing-only, no per-frame value. Slot must
        // exist so its index lines up with the module descriptor; the
        // actual per-pin signals are written into collector_sigs under
        // "lean:{uid}" by the dispatch block in `eval_graph_tick`.
        None,
    ]
}

// ── Curve helpers ─────────────────────────────────────────────────────────────

pub fn sample_curve(pts: &[[f32; 2]], x: f32, biases: &[f32]) -> f32 {
    match pts.len() {
        0 => x,
        1 => pts[0][1],
        _ => {
            if x <= pts[0][0] { return pts[0][1]; }
            let last = pts.len() - 1;
            if x >= pts[last][0] { return pts[last][1]; }
            let seg = pts.windows(2).position(|w| x <= w[1][0]).unwrap_or(last - 1);
            let p1 = pts[seg]; let p2 = pts[seg + 1];
            let t    = (x - p1[0]) / (p2[0] - p1[0]);
            let bias = biases.get(seg).copied().unwrap_or(0.0);
            let base = p1[1] + (p2[1] - p1[1]) * t;
            base + bias * 4.0 * t * (1.0 - t)
        }
    }
}

pub fn apply_curve(
    x: f32, pts: &[[f32; 2]], biases: &[f32],
    absolute: bool, in_min: f32, in_max: f32, out_min: f32, out_max: f32, scale_t: f32,
) -> f32 {
    if absolute {
        let sign     = if x < 0.0 { -1.0f32 } else { 1.0 };
        let abs_max  = in_max.abs().max(in_min.abs()).max(f32::EPSILON);
        let abs_norm = (x.abs() / abs_max).clamp(0.0, 1.0);
        let scaled   = curve_scale(abs_norm, scale_t);
        let curve_y  = sample_curve(pts, scaled, biases).clamp(0.0, 1.0);
        let out_y    = curve_scale_inv(curve_y, scale_t);
        sign * out_y * out_max.abs().max(out_min.abs())
    } else {
        let in_range  = (in_max - in_min).abs().max(f32::EPSILON);
        let out_range = out_max - out_min;
        let norm      = ((x - in_min) / in_range * 2.0 - 1.0).clamp(-1.0, 1.0);
        let sign      = if norm < 0.0 { -1.0f32 } else { 1.0 };
        let scaled    = sign * curve_scale(norm.abs(), scale_t);
        let curve_y   = sample_curve(pts, scaled, biases);
        let sign_out  = if curve_y < 0.0 { -1.0f32 } else { 1.0 };
        let out_y     = sign_out * curve_scale_inv(curve_y.abs(), scale_t);
        out_min + (out_y.clamp(-1.0, 1.0) + 1.0) * 0.5 * out_range
    }
}

pub fn curve_scale(x: f32, t: f32) -> f32 {
    if t.abs() < 1e-4 { return x; }
    x.clamp(0.0, 1.0).powf(2.0f32.powf(t * 3.0))
}

pub fn curve_scale_inv(y: f32, t: f32) -> f32 {
    if t.abs() < 1e-4 { return y; }
    y.clamp(0.0, 1.0).powf(1.0 / 2.0f32.powf(t * 3.0))
}

pub fn curve_points_from_params(params: &HashMap<String, Value>) -> Vec<[f32; 2]> {
    let absolute = params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true);
    params.get("points").and_then(|v| v.as_array()).map(|arr| {
        arr.iter().filter_map(|pt| {
            let a = pt.as_array()?;
            Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
        }).collect()
    }).unwrap_or_else(|| {
        if absolute { vec![[0.0, 0.0], [1.0, 1.0]] } else { vec![[-1.0, -1.0], [1.0, 1.0]] }
    })
}

pub fn biases_from_params(params: &HashMap<String, Value>) -> Vec<f32> {
    params.get("biases").and_then(|v| v.as_array()).map(|arr| {
        arr.iter().map(|v| v.as_f64().unwrap_or(0.0) as f32).collect()
    }).unwrap_or_default()
}

pub fn read_scale_t(params: &HashMap<String, Value>) -> f32 {
    params.get("scale_t").and_then(|v| v.as_f64()).map(|f| f as f32)
        .unwrap_or_else(|| match params.get("in_scale").and_then(|v| v.as_i64()).unwrap_or(0) {
            1 => -0.5,
            2 =>  0.5,
            _ =>  0.0,
        })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn sig_to_f32(s: Option<Signal>) -> Option<f32> {
    match s {
        Some(Signal::Float(f)) => Some(f),
        Some(Signal::Bool(b))  => Some(if b { 1.0 } else { 0.0 }),
        Some(Signal::Vec2(v))  => Some(v.length()),
        Some(Signal::Int(i))   => Some(i as f32),
        None => None,
    }
}

pub fn get_f(inputs: &[Option<Signal>], i: usize, default: f32) -> f32 {
    inputs.get(i).and_then(|s| *s).map(|s| s.as_float()).unwrap_or(default)
}

pub fn get_b(inputs: &[Option<Signal>], i: usize, default: bool) -> bool {
    inputs.get(i).and_then(|s| *s).map(|s| s.as_bool()).unwrap_or(default)
}

/// Lift input slot to Vec2: Vec2 passes through, scalars are splatted, None → splat(default).
fn get_v2(inputs: &[Option<Signal>], i: usize, default: f32) -> Vec2 {
    match inputs.get(i).and_then(|s| *s) {
        Some(Signal::Vec2(v)) => v,
        Some(other) => Vec2::splat(other.as_float()),
        None => Vec2::splat(default),
    }
}

fn sig_scalar(s: Signal) -> f32 {
    match s {
        Signal::Float(f) => f,
        Signal::Int(i)   => i as f32,
        Signal::Bool(b)  => if b { 1.0 } else { 0.0 },
        Signal::Vec2(v)  => v.length(),
    }
}

#[cfg(test)]
mod trigger_tests {
    use super::*;
    use crate::graph::SinkTarget;

    fn canonical_pins() -> Vec<String> {
        automap::ALL_PINS.iter().map(|p| p.id.to_string()).collect()
    }

    fn empty_node(uid: usize, module_id: &str) -> NodeSnap {
        NodeSnap {
            node_uid: uid,
            module_id: module_id.to_string(),
            params: HashMap::new(),
            n_outputs: 0,
            input_sources: Vec::new(),
            device_id: None,
            output_pin_ids: Vec::new(),
            aux_f32_override: None,
            sink_target: None,
            inline_subgraph: None,
        }
    }

    fn sink_node(uid: usize, device_id: &str, src_dev: &str, bridge: bool) -> NodeSnap {
        let mut n = empty_node(uid, "device.sink");
        n.sink_target = Some(SinkTarget {
            device_id: device_id.to_string(),
            // All canonical pins are valid sink destinations.
            pin_ids: canonical_pins(),
            multi_sources: vec![Vec::new(); canonical_pins().len()],
            automap_source: Some((src_dev.to_string(), canonical_pins())),
            automap_fallback_dev: Some("gilrs:switch_pro:0".to_string()),
            feedback_sources: Vec::new(),
            is_self_sink: false,
            digital_trigger_bridge: bridge,
        });
        n
    }

    // Remapper in analog mode mapping a stick cardinal → right_trigger should
    // produce a CONTINUOUS value tracking how far the stick is pushed, not a
    // binary 0/1. Regression guard for the "stick→trigger outputs binary" bug.
    #[test]
    fn remapper_analog_stick_to_trigger_is_continuous() {
        let remap_uid = 1usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String("gilrs:switch_pro:0".into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["right_trigger"], "mode": "analog" }
        ]));

        // Zero-deadzone source so this test measures continuity, not the
        // deadzone curve (deadzone is covered by the dedicated tests above).
        let src = source_node(3, "gilrs:switch_pro:0", 0.0);
        let sink = sink_node(2, "virtual.xinput:0", &format!("remap:{remap_uid}"), true);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };

        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Stick pushed halfway up (y = +0.5).
        let mut dev = HashMap::new();
        dev.insert(("gilrs:switch_pro:0".to_string(), "left_stick_y".to_string()), Signal::Float(0.5));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let v = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()))
            .map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((v - 0.5).abs() < 0.05, "half stick push should give ~0.5 trigger, got {v}");

        // Full push → full trigger.
        dev.insert(("gilrs:switch_pro:0".to_string(), "left_stick_y".to_string()), Signal::Float(1.0));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let v = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()))
            .map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((v - 1.0).abs() < 0.05, "full stick push should give ~1.0 trigger, got {v}");

        // Neutral stick → trigger releases to 0.
        dev.insert(("gilrs:switch_pro:0".to_string(), "left_stick_y".to_string()), Signal::Float(0.0));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let v = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()))
            .map(|s| s.as_float()).unwrap_or(0.0);
        assert!(v.abs() < 0.05, "neutral stick should release trigger to 0, got {v}");
    }

    // A Remapper captures its output by chord-learning, so on a Switch Pro the
    // user maps to the DIGITAL ZR button (`btn_rt_dig`), not `right_trigger`.
    // In analog mode that digital-trigger target must still produce continuous
    // analog travel on the virtual pad — not a binary press.
    #[test]
    fn remapper_analog_to_digital_trigger_button_is_continuous() {
        let remap_uid = 1usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String("gilrs:switch_pro:0".into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["btn_rt_dig"], "mode": "analog" }
        ]));
        let src = source_node(3, "gilrs:switch_pro:0", 0.0);
        let sink = sink_node(2, "virtual.xinput:0", &format!("remap:{remap_uid}"), true);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        let mut dev = HashMap::new();
        dev.insert(("gilrs:switch_pro:0".to_string(), "left_stick_y".to_string()), Signal::Float(0.5));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let v = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()))
            .map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((v - 0.5).abs() < 0.05, "analog map to digital ZR should give ~0.5 analog RT, got {v}");
    }

    // Two analog mappings sharing an input but with different outputs must BOTH
    // fire: left_stick_up→right_trigger AND left_stick_up→left_stick_up should
    // drive the trigger AND keep the stick output (not replace one another).
    #[test]
    fn analog_same_input_different_outputs_both_fire() {
        let remap_uid = 1usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String("gilrs:switch_pro:0".into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["right_trigger"],  "mode": "analog" },
            { "in": ["left_stick_up"], "out": ["left_stick_up"],  "mode": "analog" }
        ]));
        let sink = sink_node(2, "virtual.xinput:0", &format!("remap:{remap_uid}"), true);
        let graph = ProcessingGraph { nodes: vec![remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        let mut dev = HashMap::new();
        dev.insert(("gilrs:switch_pro:0".to_string(), "left_stick_y".to_string()), Signal::Float(1.0));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);

        let rt = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()))
            .map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((rt - 1.0).abs() < 0.05, "trigger mapping should still fire, got RT={rt}");
        // The stick output must be preserved (left_stick_y stays at +1).
        let ly = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "left_stick_y".to_string()))
            .map(|s| s.as_float());
        let lstick = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "left_stick".to_string()))
            .and_then(|s| if let Signal::Vec2(v) = s { Some(v.y) } else { None });
        let y = ly.or(lstick).unwrap_or(-1.0);
        assert!((y - 1.0).abs() < 0.05, "stick output should be preserved, got left_stick_y={y}");
    }

    // The implicit digital→analog bridge must RELEASE: pressing then releasing
    // the Switch digital ZR should drive the virtual analog RT to 1.0 then back
    // to 0.0 (regression guard for the "stuck at full press" bug).
    #[test]
    fn digital_bridge_presses_and_releases() {
        // Direct device → sink (no remapper); src_dev is the physical device.
        let sink = sink_node(1, "virtual.xinput:0", "gilrs:switch_pro:0", true);
        let graph = ProcessingGraph { nodes: vec![sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // ZR pressed.
        let mut dev = HashMap::new();
        dev.insert(("gilrs:switch_pro:0".to_string(), "btn_rt_dig".to_string()), Signal::Bool(true));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let v = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()))
            .map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((v - 1.0).abs() < 0.01, "pressed ZR should give full RT, got {v}");

        // ZR released → must go back to 0, not latch.
        dev.insert(("gilrs:switch_pro:0".to_string(), "btn_rt_dig".to_string()), Signal::Bool(false));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let v = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()))
            .map(|s| s.as_float()).unwrap_or(-1.0);
        assert!(v.abs() < 0.01, "released ZR should release RT to 0, got {v}");
    }

    // With the bridge DISABLED (analog-capable pad, toggle off), the digital
    // button must NOT leak into the analog trigger.
    #[test]
    fn digital_bridge_disabled_does_not_leak() {
        let sink = sink_node(1, "virtual.xinput:0", "gilrs:xinput:0", false);
        let graph = ProcessingGraph { nodes: vec![sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        let mut dev = HashMap::new();
        dev.insert(("gilrs:xinput:0".to_string(), "btn_rt_dig".to_string()), Signal::Bool(true));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let leaked = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()));
        assert!(leaked.is_none(), "bridge off: digital button must not drive analog trigger, got {leaked:?}");
    }

    /// Build a `device.source` node carrying a `deadzone` param so
    /// `preprocess_dev_sigs` picks it up for the named device.
    fn source_node(uid: usize, device_id: &str, deadzone: f32) -> NodeSnap {
        let mut n = empty_node(uid, "device.source");
        n.device_id = Some(device_id.to_string());
        n.params.insert("deadzone".into(), Value::from(deadzone as f64));
        n
    }

    // A direct AutoMap wire (device.source → sink) must apply the source
    // node's stick deadzone. A small stick value inside the deadzone must
    // collapse to 0 at the sink; a value past it must pass through (rescaled).
    #[test]
    fn automap_stick_respects_source_deadzone() {
        let src = source_node(1, "gilrs:xinput:0", 0.2);
        let sink = sink_node(2, "virtual.xinput:0", "gilrs:xinput:0", false);
        let graph = ProcessingGraph { nodes: vec![src, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Stick nudged to 0.1 — inside the 0.2 deadzone → sink must read 0.
        let mut dev = HashMap::new();
        dev.insert(("gilrs:xinput:0".to_string(), "left_stick_x".to_string()), Signal::Float(0.1));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let x = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "left_stick_x".to_string()))
            .map(|s| s.as_float());
        let lstick_x = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "left_stick".to_string()))
            .and_then(|s| if let Signal::Vec2(v) = s { Some(v.x) } else { None });
        let v = x.or(lstick_x).unwrap_or(0.0);
        assert!(v.abs() < 1e-4, "stick inside deadzone must collapse to 0 at sink, got {v}");

        // Stick pushed to 0.6 — past the deadzone → passes through (rescaled).
        dev.insert(("gilrs:xinput:0".to_string(), "left_stick_x".to_string()), Signal::Float(0.6));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let x = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "left_stick_x".to_string()))
            .map(|s| s.as_float());
        let lstick_x = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "left_stick".to_string()))
            .and_then(|s| if let Signal::Vec2(v) = s { Some(v.x) } else { None });
        let v = x.or(lstick_x).unwrap_or(0.0);
        assert!(v > 0.01, "stick past deadzone must reach sink, got {v}");
    }

    /// device.source → Remapper (analog stick-cardinal → key) → keymouse sink.
    /// Reproduces the user-reported case: WASD via analog-mode stick mapping.
    fn keymouse_sink_from_remap(uid: usize, remap_uid: usize) -> NodeSnap {
        let mut n = empty_node(uid, "device.sink");
        n.sink_target = Some(SinkTarget {
            device_id: "virtual.keymouse:0".to_string(),
            pin_ids: canonical_pins(),
            multi_sources: vec![Vec::new(); canonical_pins().len()],
            automap_source: Some((format!("remap:{remap_uid}"), canonical_pins())),
            automap_fallback_dev: None,
            feedback_sources: Vec::new(),
            is_self_sink: false,
            digital_trigger_bridge: false,
        });
        n
    }

    #[test]
    fn analog_stick_to_key_respects_source_deadzone() {
        let dev = "gilrs:xinput:0";
        let remap_uid = 2usize;
        let src = source_node(1, dev, 0.3); // 0.3 deadzone on the device.
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["key_w"], "mode": "analog" }
        ]));
        let sink = keymouse_sink_from_remap(3, remap_uid);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Stick pushed UP to 0.15 — INSIDE the 0.3 deadzone. key_w must NOT fire.
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "left_stick_y".to_string()), Signal::Float(0.15));
        eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);
        let w = out.sink_outputs.get(&("virtual.keymouse:0".to_string(), "key_w".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false);
        assert!(!w, "stick inside deadzone must NOT fire key_w, but it did");

        // Stick pushed UP to 0.8 — past the deadzone. key_w must fire.
        sigs.insert((dev.to_string(), "left_stick_y".to_string()), Signal::Float(0.8));
        eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);
        let w = out.sink_outputs.get(&("virtual.keymouse:0".to_string(), "key_w".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false);
        assert!(w, "stick past deadzone must fire key_w, but it didn't");
    }

    /// Even when no device.source node carries the deadzone (the device feeds
    /// AutoMap consumers like the Remapper without its source node present in
    /// the graph), stick pins must still get the default deadzone rather than
    /// passing through raw. Regression guard for the "analog stick→key ignores
    /// deadzone" report.
    #[test]
    fn analog_stick_to_key_default_deadzone_without_source_node() {
        let dev = "gilrs:xinput:0";
        let remap_uid = 2usize;
        // No source_node: only remapper + sink. The default deadzone must
        // still apply (DEFAULT_STICK_DEADZONE), not 0.
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["key_w"], "mode": "analog" }
        ]));
        let sink = keymouse_sink_from_remap(3, remap_uid);
        let graph = ProcessingGraph { nodes: vec![remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // 0.05 is below the default deadzone → key_w must NOT fire.
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "left_stick_y".to_string()), Signal::Float(0.05));
        eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);
        let w = out.sink_outputs.get(&("virtual.keymouse:0".to_string(), "key_w".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false);
        assert!(!w, "small stick (below default deadzone) must NOT fire key_w");
    }

    /// Run the graph for `ticks` frames at `dt`, holding `stick_y`, and return
    /// (on_count, edge_count) for the keymouse `out_pin`. Edges count rising
    /// transitions so we can tell a tap train apart from a steady gate.
    fn count_pulses(
        dev: &str, remap_uid: usize, out_pin: &str, mode_extra: serde_json::Value,
        stick_y: f32, ticks: usize, dt: f32,
    ) -> (usize, usize) {
        let src = source_node(1, dev, 0.0); // zero deadzone: measure modulation only.
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        let mut mapping = serde_json::json!(
            { "in": ["left_stick_up"], "out": [out_pin], "mode": "analog" }
        );
        if let Some(obj) = mapping.as_object_mut() {
            if let Some(extra) = mode_extra.as_object() {
                for (k, v) in extra { obj.insert(k.clone(), v.clone()); }
            }
        }
        remap.params.insert("mappings".into(), serde_json::Value::Array(vec![mapping]));
        let sink = keymouse_sink_from_remap(3, remap_uid);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "left_stick_y".to_string()), Signal::Float(stick_y));

        let mut on = 0usize;
        let mut edges = 0usize;
        let mut prev = false;
        for _ in 0..ticks {
            eval_graph_tick(&graph, &mut state, &sigs, dt, &mut out);
            let w = out.sink_outputs.get(&("virtual.keymouse:0".to_string(), out_pin.to_string()))
                .map(|s| s.as_bool()).unwrap_or(false);
            if w { on += 1; }
            if w && !prev { edges += 1; }
            prev = w;
        }
        (on, edges)
    }

    // Plain analog → digital must produce a TAP TRAIN, and a harder push must
    // tap MORE often than a light push (frequency tracks amplitude).
    #[test]
    fn analog_digital_tap_train_frequency_tracks_amplitude() {
        let extra = serde_json::json!({ "window_ms": 30.0 });
        // 1 second at 1ms ticks for clean frequency counting.
        let (_, edges_light) = count_pulses("gilrs:xinput:0", 2, "key_w", extra.clone(), 0.3, 1000, 0.001);
        let (_, edges_hard)  = count_pulses("gilrs:xinput:0", 2, "key_w", extra, 1.0, 1000, 0.001);
        assert!(edges_light >= 2, "light push should still tap a few times, got {edges_light}");
        assert!(edges_hard > edges_light,
            "harder push must tap more often: hard={edges_hard} light={edges_light}");
    }

    // Hold mode → PWM: duty cycle (ON fraction) must track amplitude. A
    // light push has a low duty; full deflection is (near) always on.
    #[test]
    fn analog_digital_hold_pwm_duty_tracks_amplitude() {
        let extra = serde_json::json!({ "window_ms": 40.0, "sustain": true });
        let (on_light, _) = count_pulses("gilrs:xinput:0", 2, "key_w", extra.clone(), 0.25, 1000, 0.001);
        let (on_full, _)  = count_pulses("gilrs:xinput:0", 2, "key_w", extra, 1.0, 1000, 0.001);
        let duty_light = on_light as f32 / 1000.0;
        let duty_full  = on_full as f32 / 1000.0;
        assert!(duty_light > 0.05 && duty_light < 0.6,
            "light Hold duty should be low-ish, got {duty_light}");
        assert!(duty_full > 0.9, "full deflection Hold should be near-always-on, got {duty_full}");
    }

    // Turbo (no Hold) doubles the max frequency, so at full deflection it
    // taps more often than plain analog at the same window_ms.
    #[test]
    fn analog_digital_turbo_doubles_frequency() {
        let plain = serde_json::json!({ "window_ms": 30.0 });
        let turbo = serde_json::json!({ "window_ms": 30.0, "turbo": true });
        let (_, edges_plain) = count_pulses("gilrs:xinput:0", 2, "key_w", plain, 1.0, 1000, 0.001);
        let (_, edges_turbo) = count_pulses("gilrs:xinput:0", 2, "key_w", turbo, 1.0, 1000, 0.001);
        assert!(edges_turbo > edges_plain,
            "turbo must tap faster at full deflection: turbo={edges_turbo} plain={edges_plain}");
    }

    // Unit-level coverage of the shared analog→digital modulator.
    #[test]
    fn analog_digital_pulse_unit() {
        let dt = 0.001;
        let run = |mag: f32, window_ms: f32, sustain: bool, turbo: bool| -> (usize, usize) {
            let mut slots = [0.0f32; PRESS_SLOTS_PER_MAPPING];
            let mut on = 0usize;
            let mut edges = 0usize;
            let mut prev = false;
            for _ in 0..1000 {
                let v = analog_digital_pulse(mag, window_ms, sustain, turbo, &mut slots, dt);
                if v { on += 1; }
                if v && !prev { edges += 1; }
                prev = v;
            }
            (on, edges)
        };

        // Zero magnitude → never on.
        assert_eq!(run(0.0, 30.0, false, false).0, 0, "mag 0 must be silent");

        // Plain tap train: more deflection → more taps.
        let (_, e_light) = run(0.3, 30.0, false, false);
        let (_, e_hard)  = run(1.0, 30.0, false, false);
        assert!(e_hard > e_light, "freq must rise with mag: {e_hard} > {e_light}");

        // Regression: at the REALISTIC default window_ms (200ms) the plain
        // tap train must be ~50% duty (a clean tap), NOT a near-held key.
        // The old tap_on=window_ms made this ~90% duty → felt held.
        let (on_default, edges_default) = run(1.0, 200.0, false, false);
        let duty = on_default as f32 / 1000.0;
        assert!(duty > 0.35 && duty < 0.65,
            "plain tap at default window must be ~50% duty, got {duty} (held-key regression)");
        assert!(edges_default >= 4, "must actually tap multiple times in 1s, got {edges_default}");

        // Hold PWM: duty tracks magnitude; full → always on.
        let (on_q, _)    = run(0.25, 40.0, true, false);
        let (on_full, _) = run(1.0, 40.0, true, false);
        assert!(on_q > 0 && (on_q as f32 / 1000.0) < 0.6, "quarter duty should be low, got {on_q}/1000");
        assert!(on_full as f32 / 1000.0 > 0.9, "full Hold should be near always-on, got {on_full}/1000");

        // Turbo doubles frequency at full deflection.
        let (_, e_plain) = run(1.0, 30.0, false, false);
        let (_, e_turbo) = run(1.0, 30.0, false, true);
        assert!(e_turbo > e_plain, "turbo faster: {e_turbo} > {e_plain}");
    }

    // on_press / on_release now honor `window_ms` as the emitted trigger
    // duration (floored at the 10ms minimum pulse).
    #[test]
    fn on_press_release_trigger_duration_tracks_window_ms() {
        let dt = 0.001; // 1 ms/tick

        // Drive a press: hold for `hold_ticks`, then release; count how many
        // ticks the output stays ON after the relevant edge.
        let run_on_press = |window_ms: f32| -> usize {
            let mut slots = [0.0f32; PRESS_SLOTS_PER_MAPPING];
            let mut on = 0usize;
            // rising edge at tick 0; hold a few ticks then release.
            for t in 0..1000 {
                let raw = t < 5; // pressed for 5 ms
                if apply_press_mode(raw, PressMode::OnPress, window_ms, false, &mut slots, dt) {
                    on += 1;
                }
            }
            on
        };

        // ~50 ms window → ~50 on-ticks (within tolerance for the dt countdown).
        let n50 = run_on_press(50.0);
        assert!((45..=55).contains(&n50), "50ms on_press should stay ~50 ticks, got {n50}");
        // ~200 ms window → ~200 on-ticks.
        let n200 = run_on_press(200.0);
        assert!((190..=210).contains(&n200), "200ms on_press should stay ~200 ticks, got {n200}");
        // Longer window → strictly longer trigger.
        assert!(n200 > n50, "larger window_ms must lengthen the trigger");

        // Floor: a 0 ms window still emits at least the 10ms minimum pulse.
        let n0 = run_on_press(0.0);
        assert!(n0 >= 9 && n0 <= 12, "0ms window floors to ~10ms pulse, got {n0}");

        // on_release fires on the falling edge with the same duration rule.
        let run_on_release = |window_ms: f32| -> usize {
            let mut slots = [0.0f32; PRESS_SLOTS_PER_MAPPING];
            let mut on = 0usize;
            for t in 0..1000 {
                let raw = t < 5; // release happens at tick 5
                if apply_press_mode(raw, PressMode::OnRelease, window_ms, false, &mut slots, dt) {
                    on += 1;
                }
            }
            on
        };
        let r100 = run_on_release(100.0);
        assert!((95..=105).contains(&r100), "100ms on_release should stay ~100 ticks, got {r100}");
    }

    // Processing wired BEFORE a Collector (explicit input port) must be what
    // the downstream Remapper sees — not the raw device sample. Here a
    // `module.constant` stands in for a Response Curve that re-maps the stick
    // amplitude: the device pushes left_stick_y small (raw), but the constant
    // feeds left_stick_y = 0.9 into the collector port. The Remapper's analog
    // stick→key mapping must therefore see ~0.9, firing key_w.
    #[test]
    fn processing_through_collector_drives_remapper_amplitude() {
        let dev = "gilrs:xinput:0";
        let src = source_node(1, dev, 0.0);

        // Constant node → emulates Response Curve output (Float 0.9).
        let mut konst = empty_node(2, "module.constant");
        konst.n_outputs = 1;
        konst.params.insert("value".into(), Value::from(0.9_f64));

        // Collector: AutoMap bus (input 0, from device) + explicit port for
        // left_stick_y (input 1, from the constant). _collect_pin_ids[0] names
        // that port's pin.
        let mut collect = empty_node(3, "module.automap_collect");
        collect.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        collect.params.insert("_collect_pin_ids".into(),
            Value::Array(vec![Value::String("left_stick_y".into())]));
        // input_sources: [0]=bus (device.source idx 0 out 0), [1]=constant out 0.
        collect.input_sources = vec![Some((0, 0)), Some((1, 0))];

        // Remapper reads the collector, maps left_stick_up (analog) → key_w.
        let remap_uid = 4usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_collector_id".into(),
            Value::String("collector:3".into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["key_w"], "mode": "analog", "window_ms": 30.0 }
        ]));
        let sink = keymouse_sink_from_remap(5, remap_uid);

        let graph = ProcessingGraph { nodes: vec![src, konst, collect, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Device stick is near-neutral (0.05) — raw would not fire. The
        // collector override (0.9) should drive the Remapper instead.
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "left_stick_y".to_string()), Signal::Float(0.05));

        // Run several ticks; key_w should tap at least once (proves the
        // processed 0.9 amplitude reached the Remapper, not the raw 0.05).
        let mut fired = false;
        for _ in 0..200 {
            eval_graph_tick(&graph, &mut state, &sigs, 0.001, &mut out);
            if out.sink_outputs.get(&("virtual.keymouse:0".to_string(), "key_w".to_string()))
                .map(|s| s.as_bool()).unwrap_or(false)
            {
                fired = true; break;
            }
        }
        assert!(fired, "processed amplitude through Collector must drive the Remapper (key_w never fired)");
    }

    // Combiner hierarchy: a pin a Remapper CONSUMED (mapped away) must not leak
    // through a lower-priority raw-device port under a non-ADD policy, but ADD
    // explicitly opts back into mixing.
    fn combiner_node(
        uid: usize, remap_uid: usize, raw_dev: &str, policy: &str,
    ) -> NodeSnap {
        let mut n = empty_node(uid, "module.automap_combiner");
        // Port 0 = Remapper collector; Port 1 = raw device.
        n.params.insert("_automap_input_devs".into(), Value::Array(vec![
            Value::String(String::new()),
            Value::String(raw_dev.into()),
        ]));
        n.params.insert("_automap_input_collectors".into(), Value::Array(vec![
            Value::String(format!("remap:{remap_uid}")),
            Value::String(String::new()),
        ]));
        let mut policy_obj = serde_json::Map::new();
        policy_obj.insert("btn_south".into(), Value::String(policy.into()));
        n.params.insert("combiner_pin_policy".into(), Value::Object(policy_obj));
        n.input_sources = vec![Some((0, 0)), Some((1, 0))]; // shape only
        n
    }

    fn run_combiner_leak(policy: &str) -> bool {
        let dev = "gilrs:xinput:0";
        let src = source_node(1, dev, 0.0);
        // Remapper consumes btn_south (maps it to btn_west), so btn_south is
        // claimed and should be suppressed downstream.
        let remap_uid = 2usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["btn_south"], "out": ["btn_west"], "mode": "down" }
        ]));
        let combiner = combiner_node(3, remap_uid, dev, policy);
        // Sink auto-maps FROM the combiner.
        let sink = sink_node(4, "virtual.xinput:0", "combiner:3", false);
        let graph = ProcessingGraph { nodes: vec![src, remap, combiner, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Physical btn_south held.
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "btn_south".to_string()), Signal::Bool(true));
        eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);
        // Did btn_south leak to the sink?
        out.sink_outputs.get(&("virtual.xinput:0".to_string(), "btn_south".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false)
    }

    #[test]
    fn combiner_suppresses_consumed_pin_unless_add() {
        // SORT (default) and OR must NOT leak the consumed btn_south.
        assert!(!run_combiner_leak("SORT"), "SORT must suppress consumed btn_south");
        assert!(!run_combiner_leak("OR"),   "OR must suppress consumed btn_south");
        // ADD explicitly mixes → the raw-port btn_south is allowed through.
        assert!(run_combiner_leak("ADD"), "ADD must let the raw btn_south mix through");
    }

    // Per-PORT default policy: setting the raw port's default to ADD opts that
    // port back into mixing for ALL its pins (no per-pin override needed), so a
    // consumed pin leaks through exactly as an explicit per-pin ADD would.
    #[test]
    fn combiner_per_port_default_add_opts_into_mixing() {
        let dev = "gilrs:xinput:0";
        let src = source_node(1, dev, 0.0);
        let remap_uid = 2usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["btn_south"], "out": ["btn_west"], "mode": "down" }
        ]));
        // Combiner with NO per-pin policy, but port 1 (raw) default = ADD.
        let mut combiner = empty_node(3, "module.automap_combiner");
        combiner.params.insert("_automap_input_devs".into(), Value::Array(vec![
            Value::String(String::new()), Value::String(dev.into()),
        ]));
        combiner.params.insert("_automap_input_collectors".into(), Value::Array(vec![
            Value::String(format!("remap:{remap_uid}")), Value::String(String::new()),
        ]));
        let mut port_def = serde_json::Map::new();
        port_def.insert("1".into(), Value::String("ADD".into()));
        combiner.params.insert("combiner_port_default".into(), Value::Object(port_def));
        combiner.input_sources = vec![Some((0, 0)), Some((1, 0))];

        let sink = sink_node(4, "virtual.xinput:0", "combiner:3", false);
        let graph = ProcessingGraph { nodes: vec![src, remap, combiner, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "btn_south".to_string()), Signal::Bool(true));
        eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);

        let leaked = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "btn_south".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false);
        assert!(leaked, "port-default ADD must opt the raw port into mixing (btn_south should pass)");
    }

    // D-pad PER-SIDE suppression: mapping only `dpad_left` away must suppress
    // the left direction across ALL three representations (Bool, dpad_x
    // negative side, dpad Vec2 x-negative) — but leave `dpad_right`, the
    // positive X side, and the entire Y axis / up-down untouched.
    #[test]
    fn dpad_left_mapped_away_suppresses_only_left_side() {
        let dev = "gilrs:xinput:0";
        let src = source_node(1, dev, 0.0);
        let remap_uid = 2usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["dpad_left"], "out": ["btn_south"], "mode": "down" }
        ]));
        let sink = sink_node(3, "virtual.xinput:0", &format!("remap:{remap_uid}"), false);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Physical D-pad: LEFT held (claimed) AND DOWN held (NOT claimed).
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "dpad_left".to_string()), Signal::Bool(true));
        sigs.insert((dev.to_string(), "dpad_down".to_string()), Signal::Bool(true));
        sigs.insert((dev.to_string(), "dpad_x".to_string()),    Signal::Float(-1.0));
        sigs.insert((dev.to_string(), "dpad_y".to_string()),    Signal::Float(-1.0));
        sigs.insert((dev.to_string(), "dpad".to_string()),      Signal::Vec2(Vec2::new(-1.0, -1.0)));
        eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);

        let get_b = |p: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), p.to_string())).map(|s| s.as_bool()).unwrap_or(false);
        let get_f = |p: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), p.to_string())).map(|s| s.as_float()).unwrap_or(0.0);
        let dpad_vec = || out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), "dpad".to_string()))
            .and_then(|s| if let Signal::Vec2(v) = s { Some(*v) } else { None }).unwrap_or(Vec2::ZERO);

        // Mapped target fires.
        assert!(get_b("btn_south"), "dpad_left→btn_south must fire");

        // The sink resolves Vec2-vs-axis conflicts by keeping ONE form, so read
        // the effective X/Y as (axis pin) OR (Vec2 component) — whichever the
        // sink kept.
        let eff_x = if out.sink_outputs.contains_key(&("virtual.xinput:0".to_string(), "dpad_x".to_string())) {
            get_f("dpad_x") } else { dpad_vec().x };
        let eff_y = if out.sink_outputs.contains_key(&("virtual.xinput:0".to_string(), "dpad_y".to_string())) {
            get_f("dpad_y") } else { dpad_vec().y };

        // LEFT is fully suppressed across all representations.
        assert!(!get_b("dpad_left"), "dpad_left Bool must be suppressed");
        assert!(eff_x >= -1e-4, "dpad left (x-negative) must be clamped, got {eff_x}");

        // DOWN (not claimed) must SURVIVE.
        assert!(get_b("dpad_down"), "unmapped dpad_down Bool must pass through");
        assert!((eff_y - (-1.0)).abs() < 1e-4, "dpad_y (down) must be untouched, got {eff_y}");
    }

    // Vec2-authoritative: when the device provides a strong `left_stick` Vec2
    // but near-zero axis floats, a Collector forwards both, and the Remapper
    // must derive its axes (and cardinals) from the Vec2 — so an analog
    // stick→key mapping fires. Guards the "processed whole-stick Vec2 before a
    // Collector doesn't reach the Remapper" gap.
    #[test]
    fn processed_vec2_on_collector_drives_remapper_axes() {
        let dev = "gilrs:xinput:0";
        let src = source_node(1, dev, 0.0);

        // Collector: pure AutoMap bus pass-through from the device (no explicit
        // ports). Phase-1 forwards left_stick Vec2 AND the axis floats.
        let mut collect = empty_node(3, "module.automap_collect");
        collect.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        collect.input_sources = vec![Some((0, 0))];

        let remap_uid = 4usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_collector_id".into(), Value::String("collector:3".into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["key_w"], "mode": "analog", "window_ms": 30.0 }
        ]));
        let sink = keymouse_sink_from_remap(5, remap_uid);
        let graph = ProcessingGraph { nodes: vec![src, collect, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Axes near-zero, but the left_stick VEC2 pushed up (y=0.9).
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(0.0));
        sigs.insert((dev.to_string(), "left_stick_y".to_string()), Signal::Float(0.0));
        sigs.insert((dev.to_string(), "left_stick".to_string()), Signal::Vec2(Vec2::new(0.0, 0.9)));

        let mut fired = false;
        for _ in 0..200 {
            eval_graph_tick(&graph, &mut state, &sigs, 0.001, &mut out);
            if out.sink_outputs.get(&("virtual.keymouse:0".to_string(), "key_w".to_string()))
                .map(|s| s.as_bool()).unwrap_or(false) { fired = true; break; }
        }
        assert!(fired, "processed left_stick Vec2 must drive the axes the Remapper reads (key_w never fired)");
    }

    // A consumed input must stay suppressed for as long as it is HELD, even in
    // a press mode whose output gate is momentary (on-press fires a ~10ms pulse
    // then closes). Regression for "on-press mapping fires its output then leaks
    // the raw input while still held".
    #[test]
    fn consumed_input_suppressed_while_held_in_on_press_mode() {
        let dev = "gilrs:xinput:0";
        let src = source_node(1, dev, 0.0);
        let remap_uid = 2usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["dpad_left"], "out": ["btn_west"], "mode": "on_press" }
        ]));
        let sink = sink_node(3, "virtual.xinput:0", &format!("remap:{remap_uid}"), false);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Hold D-pad LEFT across many frames (well past the on-press pulse).
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "dpad_left".to_string()), Signal::Bool(true));
        sigs.insert((dev.to_string(), "dpad_x".to_string()),    Signal::Float(-1.0));
        sigs.insert((dev.to_string(), "dpad".to_string()),      Signal::Vec2(Vec2::new(-1.0, 0.0)));

        let mut leaked_after_pulse = false;
        for frame in 0..60 {
            eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);
            // After the first few frames (pulse over), dpad_left must NOT leak.
            if frame >= 10 {
                let dl = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "dpad_left".to_string()))
                    .map(|s| s.as_bool()).unwrap_or(false);
                let dx = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "dpad_x".to_string()))
                    .map(|s| s.as_float()).unwrap_or(0.0);
                let dvx = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "dpad".to_string()))
                    .and_then(|s| if let Signal::Vec2(v) = s { Some(v.x) } else { None }).unwrap_or(0.0);
                // effective left value via whichever representation the sink kept
                let eff = if dl { -1.0 } else { dx.min(dvx) };
                if eff < -1e-4 { leaked_after_pulse = true; }
            }
        }
        assert!(!leaked_after_pulse, "held dpad_left leaked through after the on-press pulse ended");
    }

    // The self-map exception: a mapping that routes an input back to ITSELF must
    // NOT suppress it (deliberate pass-through), even alongside another mapping.
    #[test]
    fn self_mapped_input_is_not_suppressed() {
        let dev = "gilrs:xinput:0";
        let src = source_node(1, dev, 0.0);
        let remap_uid = 2usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["dpad_left"], "out": ["btn_west"],  "mode": "on_press" },
            { "in": ["dpad_left"], "out": ["dpad_left"],  "mode": "down" }
        ]));
        let sink = sink_node(3, "virtual.xinput:0", &format!("remap:{remap_uid}"), false);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "dpad_left".to_string()), Signal::Bool(true));
        for _ in 0..20 { eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out); }

        let dl = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "dpad_left".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false);
        assert!(dl, "self-mapped dpad_left must pass through (not be suppressed)");
    }
}
