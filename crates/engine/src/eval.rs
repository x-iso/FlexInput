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
        let dz = snap.params.get("deadzone").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
        let gm = snap.params.get("gyro_multiplier").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
        let cal = load_device_cal(&snap.params);
        params.insert(dev_id.to_string(), (dz, gm, cal));
    }
    let default_entry = (0.0_f32, 1.0_f32, DeviceCal::default());

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
            if rising { trigger_remaining = PRESS_TRIGGER_PULSE_S; }
            trigger_remaining > 0.0
        }
        PressMode::OnRelease => {
            if falling { trigger_remaining = PRESS_TRIGGER_PULSE_S; }
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

fn combine_signals(a: Signal, b: Signal) -> Signal {
    match (a, b) {
        (Signal::Float(x), Signal::Float(y)) => Signal::Float(x + y),
        (Signal::Vec2(x),  Signal::Vec2(y))  => Signal::Vec2(x + y),
        (Signal::Bool(x),  Signal::Bool(y))  => Signal::Bool(x || y),
        (Signal::Int(x),   Signal::Int(y))   => Signal::Int(x + y),
        (_, b) => b,
    }
}

// ── Sub-patch inner evaluation ────────────────────────────────────────────────

/// Namespaces inner node UIDs under their containing subpatch's UID to avoid
/// collisions in the shared `state` map when multiple subpatches share inner node indices.
#[inline]
pub fn namespaced_uid(outer: usize, inner: usize) -> usize {
    outer.wrapping_shl(20).wrapping_add(inner.wrapping_add(1))
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
            let dev_id = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("");
            let collector_id = snap.params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("");
            let mappings = snap.params.get("mappings").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let key = format!("remap:{}", ns_uid);

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
            derive_stick_cardinals(&mut upstream);

            // Touchpad accumulation (click-mode) — reuse the state's aux_f32
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
            let ns = state.entry(ns_uid).or_insert_with(NodeState::default);
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

            // Per-mapping press-mode pass — same logic as the top-level
            // Remapper arm; uses ns_uid for state keying so subpatch instances
            // don't collide with top-level instances.
            let ns2 = state.entry(ns_uid).or_insert_with(NodeState::default);
            let effective: Vec<bool> = mappings.iter().enumerate().map(|(i, m)| {
                let in_pins: Vec<&str> = m.get("in").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();
                if in_pins.is_empty() { return false; }
                let raw_held = in_pins.iter().all(|p| {
                    read_upstream(p).map(|s| s.as_bool()).unwrap_or(false)
                });
                let mode = PressMode::from_str(
                    m.get("mode").and_then(|v| v.as_str()).unwrap_or("down"));
                let window_ms = m.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(200.0) as f32;
                let sustain   = m.get("sustain").and_then(|v| v.as_bool()).unwrap_or(false);
                let turbo     = m.get("turbo").and_then(|v| v.as_bool()).unwrap_or(false);
                let slots = press_state_get(ns2, i);
                let held = apply_press_mode(raw_held, mode, window_ms, sustain, slots, dt);
                if turbo { apply_turbo(held, window_ms, slots, dt) } else { held }
            }).collect();

            let mut sorted_idx: Vec<usize> = (0..mappings.len()).collect();
            sorted_idx.sort_by(|&a, &b| {
                let la = mappings[a].get("in").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                let lb = mappings[b].get("in").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                lb.cmp(&la)
            });
            let mut triggered: Vec<(Vec<String>, Vec<String>)> = Vec::new();
            let mut claimed_inputs: HashSet<String> = HashSet::new();
            for &i in &sorted_idx {
                let m = &mappings[i];
                let in_pins: Vec<String> = m.get("in").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                let out_pins: Vec<String> = m.get("out").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                if in_pins.is_empty() { continue; }
                if in_pins.iter().any(|p| claimed_inputs.contains(p)) { continue; }
                if !effective[i] { continue; }
                for p in &in_pins { claimed_inputs.insert(p.clone()); }
                triggered.push((in_pins, out_pins));
            }
            for ap in automap::ALL_PINS {
                let suppressed = claimed_inputs.contains(ap.id);
                let sig = if suppressed {
                    match ap.signal_type {
                        SignalType::Bool  => Signal::Bool(false),
                        SignalType::Float => Signal::Float(0.0),
                        SignalType::Vec2  => Signal::Vec2(Vec2::ZERO),
                        SignalType::Int   => Signal::Int(0),
                        _ => continue,
                    }
                } else {
                    match read_upstream(ap.id) {
                        Some(s) => s,
                        None => continue,
                    }
                };
                collector_sigs.insert((key.clone(), ap.id.to_string()), sig);
            }
            let mut all_out_pins: HashSet<String> = HashSet::new();
            for m in &mappings {
                if let Some(arr) = m.get("out").and_then(|v| v.as_array()) {
                    for v in arr { if let Some(s) = v.as_str() { all_out_pins.insert(s.to_string()); } }
                }
            }
            let mut asserted: HashSet<String> = HashSet::new();
            for (_, out_pins) in &triggered { for p in out_pins { asserted.insert(p.clone()); } }
            for out_pin in &all_out_pins {
                let sig_type = automap::ALL_PINS.iter()
                    .find(|p| p.id == out_pin.as_str())
                    .map(|p| p.signal_type)
                    .unwrap_or(SignalType::Bool);
                let on = asserted.contains(out_pin);
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

            computed[idx] = vec![None];
            continue;
        }

        // module.map_action inside subpatch: mirror top-level behaviour but
        // write last_outputs keyed by the namespaced UID so UI/outer bodies
        // can observe inner output state.
        if snap.module_id == "module.map_action" {
            let dev_id = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("");
            let collector_id = snap.params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("");
            let mappings = snap.params.get("mappings").and_then(|v| v.as_array()).cloned().unwrap_or_default();
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
            derive_stick_cardinals(&mut upstream);
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
            let ns = state.entry(ns_uid).or_insert_with(NodeState::default);
            if ns.aux_f32.len() < 3 { ns.aux_f32.resize(3, 0.0); }
            if !touch_click { ns.aux_f32[0] = 0.0; ns.aux_f32[1] = 0.0; ns.aux_f32[2] = 0.0; }
            else {
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
            // Press-mode pass — mirrors Map Action top-level arm; supports both
            // legacy Array<String> and new Object mapping forms.
            let ns_map = state.entry(ns_uid).or_insert_with(NodeState::default);
            let any_trigger = mappings.iter().enumerate().any(|(i, m)| {
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
                if in_pins.is_empty() { return false; }
                let raw_held = in_pins.iter().all(|p| {
                    read_upstream(p).map(|s| s.as_bool()).unwrap_or(false)
                });
                let mode = PressMode::from_str(mode_s);
                let slots = press_state_get(ns_map, i);
                let held = apply_press_mode(raw_held, mode, window_ms, sustain, slots, dt);
                if turbo { apply_turbo(held, window_ms, slots, dt) } else { held }
            });
            computed[idx] = vec![Some(Signal::Bool(any_trigger))];
            last_outputs.insert(ns_uid, computed[idx].clone());
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

        // Display state for inner nodes — keyed by namespaced UID so the UI walk
        // can find them when populating `node.extra.last_signals` / `history`.
        match snap.module_id.as_str() {
            "display.oscilloscope" | "display.readout" => {
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
            let ns = state.entry(snap.node_uid).or_insert_with(NodeState::default);
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
            // or in the new Object form `{ in, mode, window_ms, sustain }`. Resolve
            // both and run each through the press-mode state machine.
            let ns_map = state.entry(snap.node_uid).or_insert_with(NodeState::default);
            let any_trigger = mappings.iter().enumerate().any(|(i, m)| {
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
                if in_pins.is_empty() { return false; }
                let raw_held = in_pins.iter().all(|p| {
                    read_upstream(p).map(|s| s.as_bool()).unwrap_or(false)
                });
                let mode = PressMode::from_str(mode_s);
                let slots = press_state_get(ns_map, i);
                let held = apply_press_mode(raw_held, mode, window_ms, sustain, slots, dt);
                if turbo { apply_turbo(held, window_ms, slots, dt) } else { held }
            });

            computed[idx] = vec![Some(Signal::Bool(any_trigger))];
            // Populate last_outputs for this node so the UI can display per-pin
            // signal glow (mirrors the general-path behaviour below).
            last_outputs.insert(snap.node_uid, computed[idx].clone());
            continue;
        }

        // ── module.remapper: pass-through + per-mapping override + consume ────
        if snap.module_id == "module.remapper" {
            let dev_id = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("");
            let collector_id = snap.params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("");
            let mappings = snap.params.get("mappings").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let key = format!("remap:{}", snap.node_uid);

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
            let ns = state.entry(snap.node_uid).or_insert_with(NodeState::default);
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
            let ns = state.entry(snap.node_uid).or_insert_with(NodeState::default);
            let effective: Vec<bool> = mappings.iter().enumerate().map(|(i, m)| {
                let in_pins: Vec<&str> = m.get("in").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();
                if in_pins.is_empty() { return false; }
                let raw_held = in_pins.iter().all(|p| {
                    read_upstream(p).map(|s| s.as_bool()).unwrap_or(false)
                });
                let mode = PressMode::from_str(
                    m.get("mode").and_then(|v| v.as_str()).unwrap_or("down"));
                let window_ms = m.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(200.0) as f32;
                let sustain   = m.get("sustain").and_then(|v| v.as_bool()).unwrap_or(false);
                let turbo     = m.get("turbo").and_then(|v| v.as_bool()).unwrap_or(false);
                let slots = press_state_get(ns, i);
                let held = apply_press_mode(raw_held, mode, window_ms, sustain, slots, dt);
                if turbo { apply_turbo(held, window_ms, slots, dt) } else { held }
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
            // claimed_inputs tracks pins suppressed by a longer triggered combo, so
            // a shorter combo that overlaps cannot also fire from the same press.
            let mut triggered: Vec<(Vec<String>, Vec<String>)> = Vec::new();
            let mut claimed_inputs: HashSet<String> = HashSet::new();
            for &i in &sorted_idx {
                let m = &mappings[i];
                let in_pins: Vec<String> = m.get("in").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                let out_pins: Vec<String> = m.get("out").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                if in_pins.is_empty() { continue; }
                if in_pins.iter().any(|p| claimed_inputs.contains(p)) { continue; }
                if !effective[i] { continue; }
                for p in &in_pins { claimed_inputs.insert(p.clone()); }
                triggered.push((in_pins, out_pins));
            }

            // Pass-through every canonical pin first, suppressing claimed inputs.
            // Bool pins suppress to false; Float/Vec2 suppress to zero so downstream
            // virtual sinks see no analog leakage from a consumed trigger.
            for ap in automap::ALL_PINS {
                let suppressed = claimed_inputs.contains(ap.id);
                let sig = if suppressed {
                    match ap.signal_type {
                        SignalType::Bool  => Signal::Bool(false),
                        SignalType::Float => Signal::Float(0.0),
                        SignalType::Vec2  => Signal::Vec2(Vec2::ZERO),
                        SignalType::Int   => Signal::Int(0),
                        _ => continue,
                    }
                } else {
                    match read_upstream(ap.id) {
                        Some(s) => s,
                        None => continue,
                    }
                };
                collector_sigs.insert((key.clone(), ap.id.to_string()), sig);
            }

            // Collect every output pin mentioned in ANY mapping (triggered or
            // not). Released mappings must explicitly publish "false" so the
            // downstream sink sees the transition — otherwise sinks with sticky
            // state (e.g. virtual KB/M's learned_keys → enigo) latch the OS
            // key as held until the patch is reloaded.
            let mut all_out_pins: HashSet<String> = HashSet::new();
            for m in &mappings {
                if let Some(arr) = m.get("out").and_then(|v| v.as_array()) {
                    for v in arr {
                        if let Some(s) = v.as_str() {
                            all_out_pins.insert(s.to_string());
                        }
                    }
                }
            }
            // Determine which output pins are currently asserted (any triggered
            // mapping listing them in its `out`).
            let mut asserted: HashSet<String> = HashSet::new();
            for (_, out_pins) in &triggered {
                for p in out_pins { asserted.insert(p.clone()); }
            }
            // Overlay rule per output pin:
            //   - If the mapping fires this tick, write the asserted value.
            //   - If the mapping is released:
            //       * Output pin that the upstream device naturally emits
            //         (e.g. btn_east on a gamepad) — leave the pass-through
            //         value alone so the button still works natively when not
            //         being driven by a mapping.
            //       * Output pin the upstream doesn't emit (e.g. key_q,
            //         mouse_left) — write false/zero so downstream sinks with
            //         sticky state (virtual KB/M) see the release transition.
            for out_pin in &all_out_pins {
                let sig_type = automap::ALL_PINS.iter()
                    .find(|p| p.id == out_pin.as_str())
                    .map(|p| p.signal_type)
                    .unwrap_or(SignalType::Bool);
                let on = asserted.contains(out_pin);
                if on {
                    let sig = match sig_type {
                        SignalType::Float => Signal::Float(1.0),
                        SignalType::Vec2  => continue, // not driven by chord mappings
                        SignalType::Int   => Signal::Int(1),
                        _                 => Signal::Bool(true),
                    };
                    collector_sigs.insert((key.clone(), out_pin.clone()), sig);
                } else {
                    // Released. Only force a zero/false write if the upstream
                    // wouldn't have produced anything for this pin (i.e. KB/M
                    // output pins not present on the upstream gamepad).
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

            computed[idx] = vec![None];
            continue;
        }

        // ── module.automap_collect: inject individual inputs into collector_sigs ──
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
                let key = format!("forksel:{}:{}", snap.node_uid, out_idx);
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
            }
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
            // Per-pin port pinning: if present, this pin reads ONLY from the
            // specified port (clamped to last connected port). Bypasses policy.
            let port_map = snap.params.get("combiner_pin_port")
                .and_then(|v| v.as_object()).cloned().unwrap_or_default();
            let key = format!("combiner:{}", snap.node_uid);

            // Pin-type-aware clamping. Triggers are [0,1]; stick axes / dpad axes
            // / left-right sticks are [-1,1]. Gyro/accel/mouse remain unclamped.
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

            // Read per-port pin signal at a given index, preferring collector
            // override over raw device samples (matches Splitter's behaviour).
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
                // Per-pin port pin: if set, route exclusively from that port
                // (clamped to the last connected port). Skip the rest of the
                // policy machinery entirely.
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

                // Collect raw values from every connected input.
                let mut raw: Vec<Signal> = Vec::with_capacity(input_devs.len());
                for i in 0..input_devs.len() {
                    if let Some(s) = read_pin_at(i, pin.id,
                        &input_devs, &input_collectors, &collector_sigs, dev_sigs)
                    {
                        raw.push(s);
                    }
                }
                if raw.is_empty() { continue; }

                let policy = policy_map.get(pin.id).and_then(|v| v.as_str()).unwrap_or("SORT");
                let resolved: Option<Signal> = match policy {
                    "SORT" => {
                        // Connection-priority: highest port that *offers* the
                        // pin wins, even when its current value is idle. Lower
                        // ports are completely shadowed for this pin. Since
                        // `raw` is built by walking ports top-down and pushing
                        // only when read_pin_at returns Some, the first entry
                        // is exactly the highest-priority port that has it.
                        raw.into_iter().next()
                    }
                    "OR" => match pin.signal_type {
                        flexinput_core::SignalType::Bool => {
                            let any = raw.iter().any(|s| matches!(s, Signal::Bool(true)));
                            Some(Signal::Bool(any))
                        }
                        flexinput_core::SignalType::Vec2 => {
                            // Per-component max-abs, preserving sign of the contributing component.
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
                            // Float / Int: max(|x|) preserving sign.
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
                            // |a - b| folded across all inputs, per component.
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
                            // Polar MULT: multiply input *lengths*, keep
                            // port-0's *direction*. With circular sticks a
                            // full diagonal is length 1, so two full diagonal
                            // deflections both yield length 1 instead of the
                            // per-component product collapsing to (0.5, 0.5).
                            // Direction comes from port 0 because SORT means
                            // "port 0 owns this pin" everywhere else too.
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
                            // For unsigned pins (triggers [0,1]) plain product
                            // is correct. For signed pins, take port-0 sign ×
                            // product of magnitudes so two negative inputs
                            // don't flip into a positive output.
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
                    _ => None, // unknown policy → silent
                };
                if let Some(s) = resolved {
                    collector_sigs.insert((key.clone(), pin.id.to_string()), s);
                }
            }
            computed[idx] = vec![None];
            continue;
        }

        // ── module.automap_selector: gate selected AutoMap input to output ────
        if snap.module_id == "module.automap_selector" {
            // inputs[0] = select, inputs[1..] = AutoMap buses
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
            let selected_dev = input_devs.get(select).map(|s| s.as_str()).unwrap_or("");
            let key = format!("forksel:{}:0", snap.node_uid);
            if !selected_dev.is_empty() {
                for pin in flexinput_core::automap::ALL_PINS {
                    if let Some(sig) = dev_sigs.get(&(selected_dev.to_string(), pin.id.to_string())).copied() {
                        collector_sigs.insert((key.clone(), pin.id.to_string()), sig);
                    }
                }
            }
            computed[idx] = vec![None];
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
                    || src_dev.starts_with("combiner:");
                for (mapped_src, mapped_dst) in automap::resolve_mapping(&src_ids, &dst_ids) {
                    if directly_wired.contains(mapped_dst) { continue; }
                    // For collectors (including fork/selector gates): check collector_sigs first,
                    // then fall back to upstream device.
                    let sig_opt = if is_collector {
                        collector_sigs.get(&(src_dev.clone(), mapped_src.to_string())).copied()
                            .or_else(|| {
                                st.automap_fallback_dev.as_ref().and_then(|fb| {
                                    dev_sigs.get(&(fb.clone(), mapped_src.to_string())).copied()
                                })
                            })
                    } else {
                        dev_sigs.get(&(src_dev.clone(), mapped_src.to_string())).copied()
                    };
                    if let Some(sig) = sig_opt {
                        // Type coercion (Bool↔Float) is performed by the virtual device's
                        // send() via Signal::as_float / as_bool, so we just hand the raw
                        // signal off — semantic groups already routed it to the right pin.
                        sink_outputs
                            .entry((st.device_id.clone(), mapped_dst.to_string()))
                            .or_insert(scale_for_sink(mapped_dst, sig));
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
                        let Some(dst_pin) = flexinput_core::automap::resolve_feedback_pin(
                            virt_out_pin, &dst_pins
                        ) else { continue; };
                        if directly_wired.contains(dst_pin) { continue; }
                        sink_outputs
                            .entry((st.device_id.clone(), dst_pin.to_string()))
                            .or_insert(sig);
                    }
                }
            }

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

        match snap.module_id.as_str() {
            "display.oscilloscope" | "display.readout" => {
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

// ─── Hampel-style outlier suppression ────────────────────────────────────────
// State layout per axis (6 axes: gx, gy, gz, ax, ay, az):
//   `dc_fast` and `dc_estimates` are NOT used here — we reuse `avg_bufs` to
//   hold rolling-window samples per axis. Index 0..6 maps to gx, gy, gz, ax,
//   ay, az respectively. This keeps state lean without adding new fields.
const SPIKE_WINDOW: usize = 20;
const SPIKE_DEFAULT_K: f32 = 4.0;

/// Minimum stddev floor. When the signal is genuinely stationary the
/// computed MAD collapses to ~0, which would make every real motion sample
/// look like an outlier. The floor sets a noise scale below which we don't
/// try to flag anything — anything inside this band is allowed through.
/// Tuned to a gyro/accel noise floor that's still tight enough to catch
/// real 1-sample spikes (which typically jump dozens of units).
const SPIKE_SIGMA_FLOOR: f32 = 0.02;

fn spike_filter(buf: &mut VecDeque<f32>, sample: f32, k: f32) -> f32 {
    // Decide the output BEFORE mutating the buffer. We always push the
    // *original* sample (not the filtered value) into the history —
    // writing the suppressed output back would let one spike poison the
    // baseline forever (MAD would stay at 0 and every subsequent motion
    // sample would then read as an outlier).
    let out = if buf.len() < 6 {
        sample
    } else {
        let mut sorted: Vec<f32> = buf.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = sorted[sorted.len() / 2];
        let mut dev: Vec<f32> = sorted.iter().map(|x| (x - median).abs()).collect();
        dev.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mad = dev[dev.len() / 2];
        // 1.4826 × MAD ≈ robust stddev; clamp to a noise-floor so a
        // quiet signal can't make the threshold collapse to zero.
        let sigma = (mad * 1.4826).max(SPIKE_SIGMA_FLOOR);
        if (sample - median).abs() > k * sigma { median } else { sample }
    };
    buf.push_back(sample);
    if buf.len() > SPIKE_WINDOW { buf.pop_front(); }
    out
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
    let mut gx = pin_or(2, gx_am) * inv("inv_roll");
    let mut gy = pin_or(3, gy_am);
    let mut gz = pin_or(4, gz_am);
    let mut ax = pin_or(5, ax_am) * inv("inv_accel_x");
    let mut ay = pin_or(6, ay_am) * inv("inv_accel_y");
    let mut az = pin_or(7, az_am) * inv("inv_accel_z");

    // Optional outlier-spike suppression. Uses `state.avg_bufs` slots 0..6
    // for per-axis history (`avg_bufs[0]` = gx, `[1]` = gy, etc.).
    if pb("spike_suppress", false) {
        let k = pf("spike_k", SPIKE_DEFAULT_K).clamp(1.0, 12.0);
        while state.avg_bufs.len() < 6 { state.avg_bufs.push(VecDeque::with_capacity(SPIKE_WINDOW)); }
        gx = spike_filter(&mut state.avg_bufs[0], gx, k);
        gy = spike_filter(&mut state.avg_bufs[1], gy, k);
        gz = spike_filter(&mut state.avg_bufs[2], gz, k);
        ax = spike_filter(&mut state.avg_bufs[3], ax, k);
        ay = spike_filter(&mut state.avg_bufs[4], ay, k);
        az = spike_filter(&mut state.avg_bufs[5], az, k);
    }

    // aux_f32 layout:
    //   [0] integrated steering X
    //   [1] integrated steering Y
    //   [2] smoothed gravity X (player/world)
    //   [3] smoothed gravity Y
    //   [4] smoothed gravity Z
    //   [5] prev_reset edge guard
    //   [6] ease-in residual (0..1 progresses while resetting)
    while state.aux_f32.len() < 7 { state.aux_f32.push(0.0); }

    // ── Axis selection: decide which gyro components feed X / Y / Lean ────
    //
    // For Player/World we project gyro onto the gravity-corrected frame.
    // For Pitch+Yaw and Pitch+Roll, the lean axis is the unused rotation:
    //   Pitch+Yaw  → lean = roll  (gx)
    //   Pitch+Roll → lean = yaw   (gz)
    //   Player/World → lean = component of gyro along the gravity-perpendicular
    //                          axis (we use gx for now — controller-roll proxy)
    let (raw_x, raw_y, raw_lean) = match axis {
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
            (world_yaw, gyro_no_yaw.y, gx)
        }
        _ => (gz, gy, gx), // pitch_yaw: gz=yaw→X, gy=pitch→Y, gx=roll→Lean
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
        let recenter_blend = pf("recenter_blend", 0.5).clamp(0.0, 1.0); // 0=roll-only, 1=gravity-only
        let recenter_strength = pf("recenter_strength", 0.0).clamp(0.0, 4.0); // sec⁻¹ pull rate
        let ease_in = pf("reset_ease_in", 0.25).clamp(0.0, 2.0);

        // Integrate both accumulators every tick — `exclude_y` only gates
        // the output, not the integration, so toggling it on/off doesn't
        // leave the Y accumulator stale.
        state.aux_f32[0] += raw_x * dt;
        state.aux_f32[1] += raw_y * dt;

        // Auto-centering: pull the integrated steering accumulator TOWARD
        // the accelerometer-implied steering angle. Physical intuition:
        // when the user holds the gamepad like an upright steering wheel,
        // the controller's roll angle IS the steering angle the user is
        // commanding. The accelerometer reveals that angle but is noisy;
        // the integrated gyro is smooth but drifts. A weak complementary
        // pull combines the two: aux ← aux + α (target − aux), where α
        // = strength × dt and `target` is the accel-derived angle.
        if recenter_strength > 0.0 {
            // Roll source: controller roll about the forward axis. atan2
            // of accel_x over accel_z gives a signed angle in radians.
            let roll_angle = ax.atan2(az);
            // Gravity source: full-vector projection. asin(ax / |a|) is
            // the angle between gravity and the controller's Y axis,
            // signed by the X component. More stable when the controller
            // is pitched far forward/back than the bare atan2 of x/z.
            let acc_mag = (ax * ax + ay * ay + az * az).sqrt().max(1e-3);
            let grav_angle = (ax / acc_mag).clamp(-1.0, 1.0).asin();
            let target = (1.0 - recenter_blend) * roll_angle + recenter_blend * grav_angle;
            let alpha = (recenter_strength * dt).clamp(0.0, 1.0);
            state.aux_f32[0] += alpha * (target - state.aux_f32[0]);
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

    // ── Lean output: analog magnitude + bool above threshold ──────────────
    let lean_threshold = pf("lean_threshold", 0.3).clamp(0.01, 4.0);
    let lean_val = raw_lean;
    let lean_active = lean_val.abs() >= lean_threshold;

    vec![
        Some(Signal::Vec2(glam::Vec2::new(final_x, final_y))),
        Some(Signal::Float(final_x)),
        Some(Signal::Float(final_y)),
        Some(Signal::Float(lean_val)),
        Some(Signal::Bool(lean_active)),
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
