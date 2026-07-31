//! Per-device calibration: deadzones, radial stick scaling, trigger
//! ranges, gyro/accel orientation and sign correction.
//!
//! `preprocess_dev_sigs` is the funnel every physical device signal passes
//! through before any node sees it, so a mapping never has to know which
//! pad produced a value.

use super::*;

/// Stick pin IDs — deadzone applies only here, not to triggers/gyro/accel/buttons.
pub(crate) fn is_stick_pin(pin_id: &str) -> bool {
    matches!(pin_id,
        "left_stick" | "right_stick"
        | "left_stick_x" | "left_stick_y"
        | "right_stick_x" | "right_stick_y"
    )
}

/// IMU pin IDs — gyro multiplier scales these.
pub(crate) fn is_gyro_pin(pin_id: &str) -> bool {
    matches!(pin_id, "gyro_x" | "gyro_y" | "gyro_z")
}

/// Mouse-delta pin IDs — mouse_sensitivity scales these.
pub(crate) fn is_mouse_pin(pin_id: &str) -> bool {
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
pub(crate) struct DeviceCal {
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
    /// Noise-floor deadzone on the gyro TRIPLE's magnitude (post offset /
    /// matrix / sign): |g| below this zeroes all three axes at once, so slow
    /// diagonal rotations don't get per-axis steps. 0.0 = disabled. Effective
    /// value = measured `gyro_noise_floor` × user `gyro_dz_width`, gated by
    /// `gyro_dz_enabled` (all written by the Calibration window).
    gyro_dz:  f32,
    /// Dynamic accel deadzone: deviations from a per-device frozen baseline
    /// below this emit the baseline (noise suppressed); larger deviations
    /// re-anchor it, so the baseline follows real rotation with error
    /// bounded by the floor. 0.0 = disabled.
    accel_dz: f32,
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
    /// "Digital triggers" opt-in (device.source `digital_triggers` param).
    /// When set, each analog trigger becomes a thresholded snap: it outputs
    /// full deflection once the calibrated value crosses the per-side
    /// digital threshold, else 0.0 (staying Float). The `btn_lt_dig` /
    /// `btn_rt_dig` buttons are re-derived from the same threshold so the
    /// pad's own early-firing L2/R2 flag no longer leaks through.
    digital_triggers: bool,
    /// Per-side digital threshold on the calibrated (0..1) trigger value.
    /// Written by the Calibration window's yellow pin. Defaults to 0.5.
    ltrig_threshold: f32,
    rtrig_threshold: f32,
    /// "Suppress touch + misc" opt-in (device.source `suppress_touch_misc`
    /// param). Mutes every pin in `automap::TOUCH_MISC_PINS` so a pad whose
    /// capacitive sensors fire on their own (Steam Controller trackpads and
    /// thumb-rest) can be mapped without them hijacking the capture. The UI
    /// masks the SAME pins out of `last_signals`, so Learn and routing agree.
    suppress_touch_misc: bool,
}

/// Default digital-trigger threshold. Mirrors the Calibration UI's
/// `TRIG_THRESHOLD_DEFAULT` so an uncalibrated pad snaps at the half pull.
pub(crate) const TRIG_DIGITAL_THRESHOLD_DEFAULT: f32 = 0.5;

pub(crate) const IDENTITY_M3: [f32; 9] = [
    1.0, 0.0, 0.0,
    0.0, 1.0, 0.0,
    0.0, 0.0, 1.0,
];

/// Per-device frozen baseline for the dynamic accel noise-floor deadzone
/// (see `DeviceCal::accel_dz`). Keyed by device id; entries are only touched
/// while the feature is enabled, so a stale entry after a device swap just
/// re-anchors on the first over-floor deviation.
pub(crate) static ACCEL_DZ_BASELINE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, [f32; 3]>>> =
    std::sync::OnceLock::new();

/// Exponential ease out of a noise-floor deadzone border: 0 inside the
/// border, then `1 − e^−((v−dz)/dz)` above it — ~63 % one border-width out,
/// ~95 % three widths out. Keeps the exit from the deadzone snap-free while
/// leaving fast motion effectively untouched. Shared by the gyro magnitude
/// gate, the dynamic accel baseline, and the calibration scope's filtered
/// view (which must match what the engine does).
#[inline]
pub fn gyro_dz_ease(v: f32, dz: f32) -> f32 {
    if dz <= 0.0 { return 1.0; }
    if v <= dz { return 0.0; }
    1.0 - (-(v - dz) / dz).exp()
}

impl Default for DeviceCal {
    fn default() -> Self {
        Self {
            gyro_offset:   [0.0; 3],
            accel_offset:  [0.0; 3],
            gyro_sign:     [1.0; 3],
            accel_sign:    [1.0; 3],
            orient_matrix: IDENTITY_M3,
            orient_active: false,
            gyro_dz:  0.0,
            accel_dz: 0.0,
            lstick_center: [0.0; 2],
            rstick_center: [0.0; 2],
            lstick_radial: [1.0; STICK_RADIAL_BUCKETS],
            rstick_radial: [1.0; STICK_RADIAL_BUCKETS],
            ltrig_range:   (0.0, 1.0),
            rtrig_range:   (0.0, 1.0),
            digital_triggers: false,
            ltrig_threshold:  TRIG_DIGITAL_THRESHOLD_DEFAULT,
            rtrig_threshold:  TRIG_DIGITAL_THRESHOLD_DEFAULT,
            suppress_touch_misc: false,
        }
    }
}

pub(crate) fn read_arr3(params: &HashMap<String, Value>, key: &str) -> [f32; 3] {
    params.get(key).and_then(|v| v.as_array()).and_then(|a| {
        if a.len() < 3 { return None; }
        Some([
            a[0].as_f64()? as f32,
            a[1].as_f64()? as f32,
            a[2].as_f64()? as f32,
        ])
    }).unwrap_or([0.0; 3])
}
pub(crate) fn read_arr2(params: &HashMap<String, Value>, key: &str) -> [f32; 2] {
    params.get(key).and_then(|v| v.as_array()).and_then(|a| {
        if a.len() < 2 { return None; }
        Some([a[0].as_f64()? as f32, a[1].as_f64()? as f32])
    }).unwrap_or([0.0; 2])
}
pub(crate) fn read_radial(params: &HashMap<String, Value>, key: &str) -> [f32; STICK_RADIAL_BUCKETS] {
    let mut out = [1.0_f32; STICK_RADIAL_BUCKETS];
    let Some(arr) = params.get(key).and_then(|v| v.as_array()) else { return out; };
    for i in 0..STICK_RADIAL_BUCKETS.min(arr.len()) {
        if let Some(v) = arr[i].as_f64() { out[i] = v as f32; }
    }
    out
}
pub(crate) fn read_range(params: &HashMap<String, Value>, key_min: &str, key_max: &str) -> (f32, f32) {
    let mn = params.get(key_min).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    let mx = params.get(key_max).and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
    (mn, mx)
}

pub(crate) fn read_sign3(params: &HashMap<String, Value>, key: &str) -> [f32; 3] {
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
pub(crate) fn read_orient_matrix(params: &HashMap<String, Value>, key: &str) -> ([f32; 9], bool) {
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
pub(crate) fn mat3_apply(m: &[f32; 9], v: [f32; 3]) -> [f32; 3] {
    [
        m[0] * v[0] + m[1] * v[1] + m[2] * v[2],
        m[3] * v[0] + m[4] * v[1] + m[5] * v[2],
        m[6] * v[0] + m[7] * v[1] + m[8] * v[2],
    ]
}

pub(crate) fn load_device_cal(params: &HashMap<String, Value>) -> DeviceCal {
    let (orient_matrix, orient_active) = read_orient_matrix(params, "gyro_orient_matrix");
    let dz_on = params.get("gyro_dz_enabled").and_then(|v| v.as_bool()).unwrap_or(false);
    let dz_w  = params.get("gyro_dz_width").and_then(|v| v.as_f64()).unwrap_or(2.0) as f32;
    let floor = |key: &str| -> f32 {
        if !dz_on { return 0.0; }
        (params.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32 * dz_w).max(0.0)
    };
    DeviceCal {
        gyro_offset:   read_arr3(params, "gyro_offset"),
        accel_offset:  read_arr3(params, "accel_offset"),
        gyro_sign:     read_sign3(params, "gyro_invert"),
        accel_sign:    read_sign3(params, "accel_invert"),
        orient_matrix,
        orient_active,
        gyro_dz:  floor("gyro_noise_floor"),
        accel_dz: floor("accel_noise_floor"),
        lstick_center: read_arr2(params, "lstick_center"),
        rstick_center: read_arr2(params, "rstick_center"),
        lstick_radial: read_radial(params, "lstick_radial"),
        rstick_radial: read_radial(params, "rstick_radial"),
        ltrig_range:   read_range(params, "ltrig_min", "ltrig_max"),
        rtrig_range:   read_range(params, "rtrig_min", "rtrig_max"),
        digital_triggers: params.get("digital_triggers").and_then(|v| v.as_bool()).unwrap_or(false),
        ltrig_threshold: params.get("ltrig_digital_threshold").and_then(|v| v.as_f64())
            .map(|v| v as f32).unwrap_or(TRIG_DIGITAL_THRESHOLD_DEFAULT),
        rtrig_threshold: params.get("rtrig_digital_threshold").and_then(|v| v.as_f64())
            .map(|v| v as f32).unwrap_or(TRIG_DIGITAL_THRESHOLD_DEFAULT),
        suppress_touch_misc: params.get("suppress_touch_misc")
            .and_then(|v| v.as_bool()).unwrap_or(false),
    }
}

/// Scale a centered stick reading (x, y) so that the magnitude reaches 1.0
/// along every direction, using a per-bucket radial scale profile. Buckets
/// linearly-interpolate so the correction is smooth between sample angles.
pub(crate) fn apply_stick_scale(x: f32, y: f32, profile: &[f32; STICK_RADIAL_BUCKETS]) -> (f32, f32) {
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
pub(crate) fn apply_trigger_range(v: f32, (mn, mx): (f32, f32)) -> f32 {
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
pub(crate) fn post_process_device_pin(
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
pub(crate) fn preprocess_dev_sigs(
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
            let mut out = [
                v[0] * cal.gyro_sign[0],
                v[1] * cal.gyro_sign[1],
                v[2] * cal.gyro_sign[2],
            ];
            // Noise-floor deadzone: gate on the MAGNITUDE of the triple so a
            // slow diagonal rotation never steps per axis. Below the floor
            // the whole vector is noise → zero it (this is what stops the
            // 3D-orientation drift and lean bias at rest). Crossing the
            // border eases in exponentially over ~3 border-widths instead of
            // snapping from 0 to the full value.
            if cal.gyro_dz > 0.0 {
                let mag = (out[0] * out[0] + out[1] * out[1] + out[2] * out[2]).sqrt();
                let s = gyro_dz_ease(mag, cal.gyro_dz);
                out = [out[0] * s, out[1] * s, out[2] * s];
            }
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
            let mut out = [
                v[0] * cal.accel_sign[0],
                v[1] * cal.accel_sign[1],
                v[2] * cal.accel_sign[2],
            ];
            // Dynamic noise-floor deadzone: accel's "center" is gravity and
            // moves with device rotation, so a fixed gate can't work. Hold a
            // per-device baseline instead — deviations under the floor emit
            // the baseline (noise suppressed at rest); past the border the
            // baseline chases the reading with the same exponential ease as
            // the gyro gate, so real rotation follows with no snap and error
            // bounded by ~the floor.
            if cal.accel_dz > 0.0 {
                let map = ACCEL_DZ_BASELINE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
                let mut map = map.lock().unwrap();
                let base = map.entry(dev_id.clone()).or_insert(out);
                let d = ((out[0] - base[0]).powi(2)
                    + (out[1] - base[1]).powi(2)
                    + (out[2] - base[2]).powi(2))
                    .sqrt();
                let s = gyro_dz_ease(d, cal.accel_dz);
                if s > 0.0 {
                    for i in 0..3 { base[i] += (out[i] - base[i]) * s; }
                }
                out = *base;
            }
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

    // ── Pass 3: digital-trigger snap ────────────────────────────────────────
    //
    // When a device.source opted into "Digital triggers", its analog triggers
    // become a thresholded snap: the calibrated (0..1) value outputs full
    // deflection once it crosses the per-side digital threshold, else 0.0 —
    // staying Float so Float-consuming wires (direct or AutoMap) still work.
    // The matching digital buttons (`btn_lt_dig` / `btn_rt_dig`) are re-derived
    // from the SAME threshold, so the pad's own early-firing L2/R2 flag no
    // longer leaks through and the Calibration threshold actually takes effect.
    //
    // Applied at the source (not as the lowest-priority AutoMap-sink fallback
    // used for digital-ONLY pads) because the user explicitly asked for this
    // pad's real analog travel to read as digital everywhere downstream. Pads
    // without an analog `left_trigger`/`right_trigger` (e.g. Switch Pro) have
    // nothing to snap here and fall through to that sink-side bridge unchanged.
    for (dev_id, (_, _, cal)) in &params {
        if !cal.digital_triggers { continue; }
        for (analog_pin, dig_pin, thr) in [
            ("left_trigger",  "btn_lt_dig", cal.ltrig_threshold),
            ("right_trigger", "btn_rt_dig", cal.rtrig_threshold),
        ] {
            let key = (dev_id.clone(), analog_pin.to_string());
            let Some(Signal::Float(v)) = out.get(&key).copied() else { continue };
            // Guard against a threshold pinned at 0.0 firing on rest jitter.
            let over = v >= thr.max(f32::EPSILON);
            out.insert(key, Signal::Float(if over { 1.0 } else { 0.0 }));
            out.insert((dev_id.clone(), dig_pin.to_string()), Signal::Bool(over));
        }
    }

    // ── Pass 4: touch + misc mute ───────────────────────────────────────────
    //
    // "Suppress touch + misc" on a device.source: zero every pin in
    // `TOUCH_MISC_PINS` so a pad whose capacitive sensors fire on their own
    // (Steam Controller trackpads / thumb rest, which sit on the SDL misc pins)
    // stops driving anything while the user builds a mapping. Zeroed rather
    // than removed so presence probes still see the pad's real shape.
    //
    // The UI applies the same mask to `last_signals` (see `mask_suppressed_pins`
    // in app.rs) — that half is what stops a Remapper / Touch Zones / Lean
    // "Learn" from capturing the sensor instead of the button the user pressed.
    // Both halves read this one param, so they cannot disagree.
    for (dev_id, (_, _, cal)) in &params {
        if !cal.suppress_touch_misc { continue; }
        for pin in flexinput_core::automap::TOUCH_MISC_PINS {
            let key = (dev_id.clone(), (*pin).to_string());
            if let Some(sig) = out.get(&key).copied() {
                out.insert(key, sig.zeroed());
            }
        }
    }

    // Stash each device's `gyro_multiplier` under a synthetic pin key so the
    // Gyro 3DOF module can divide it back out of its orientation quaternion
    // (keeping the 3D pose 1:1 while the multiplier still scales the 2D pointer
    // output). "__gyro_mult" is not a real pin, so per-name pin lookups and the
    // canonical pin lists never surface it.
    for (dev_id, (_, gm, _)) in &params {
        out.insert((dev_id.clone(), "__gyro_mult".to_string()), Signal::Float(*gm));
    }

    out
}

pub(crate) fn apply_deadzone(sig: Signal, dz: f32) -> Signal {
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
