//! The 3DOF gyro evaluator: mode resolution (including the legacy `mode`
//! strings) and the orientation integration behind it.

use super::*;

/// Translate legacy `mode` strings to the new (family, axis) split so saved
/// patches keep working without manual migration.
pub(crate) fn gyro_resolve_mode(params: &HashMap<String, Value>) -> (&'static str, &'static str) {
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

pub(crate) fn compute_gyro_3dof(
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
    //   [7] quaternion x (orientation integration)
    //   [8] quaternion y
    //   [9] quaternion z
    //   [10] quaternion w (always 1.0 at initialization, updated on each tick)
    //   [11] prev_reset edge guard for reset tracking
    //   [12] ease-in residual for orientation blend during reset
    //   [13..16] captured world-frame gravity reference for drift correction
    //   [16] gyro-still time accumulator for the yaw auto re-center
    while state.aux_f32.len() < 17 { state.aux_f32.push(0.0); }

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
    // sideways. Magnitude in [0, 1] where 1 ≈ on its side.
    //
    // SIDE tilt is accel X. The device layer normalizes every pad to
    // (x = side, y = forward-tilt, z = vertical/gravity) — see the axis remap
    // in `flexinput_devices::gyro::build`. This read `ay` until 2026-07, which
    // is FORWARD tilt, so lean tracked pitch: it fired when the controller was
    // tipped toward or away from the player rather than rolled left/right.
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
    let lean_val = (ax / acc_mag_full).clamp(-1.0, 1.0);
    let lean_threshold = pf("lean_threshold", 0.3).clamp(0.01, 4.0);
    let lean_active = lean_val.abs() >= lean_threshold;

    // ── Quaternion orientation integration (3DOF pose estimate) ───────────
    //
    // Maintains a continuous orientation estimate by integrating angular
    // velocity over time. Uses aux_f32[7..10] for quaternion x,y,z,w.
    // Gravity-based drift correction can be added later if needed.
    let q_reset_now = get_b(inputs, 1, false);
    let q_reset_edge = q_reset_now && state.aux_f32[11] < 0.5;
    state.aux_f32[11] = if q_reset_now { 1.0 } else { 0.0 };

    // Initialize quaternion to identity on first run or reset
    if state.aux_f32[7] == 0.0 && state.aux_f32[8] == 0.0 && state.aux_f32[9] == 0.0 {
        state.aux_f32[7] = 0.0; // qx
        state.aux_f32[8] = 0.0; // qy
        state.aux_f32[9] = 0.0; // qz
        state.aux_f32[10] = 1.0; // qw (identity)
    }

    // Integrate the TRUE physical angular velocity so this orientation tracks
    // the controller 1:1 in the real world — independent of BOTH the pointer/
    // steering sensitivity AND the device's `gyro_multiplier`. Device gyro
    // signals are NORMALIZED: ±1.0 == ±GYRO_REF_DPS deg/s (see
    // crates/devices/src/gyro.rs), so convert to rad/s before integrating, else
    // the model under-rotates by ~34.9×.
    const GYRO_REF_DPS: f32 = 2000.0;
    let norm_to_rad_s = GYRO_REF_DPS * std::f32::consts::PI / 180.0;
    // `gyro_multiplier` (a device.source calibration knob) is already baked into
    // the gyro signals by `preprocess_dev_sigs`; divide it back out so the
    // orientation stays 1:1 no matter what the user sets it to. The multiplier
    // is stashed per-device under the synthetic pin key "__gyro_mult".
    let dev_gyro_mult = params
        .get("_automap_device_id")
        .and_then(|v| v.as_str())
        .and_then(|dev| dev_sigs.get(&(dev.to_string(), "__gyro_mult".to_string())))
        .and_then(|s| if let Signal::Float(f) = s { Some(*f) } else { None })
        .filter(|m| m.abs() > 1e-6)
        .unwrap_or(1.0);
    // Orientation display scale, applied to the RATE (before integration) — the
    // only place scaling a full 3D pose stays continuous. Scaling the finished
    // quaternion (viewer-side) flips discontinuously as the rotation passes
    // ~180°. Affects ONLY this Orientation output, not the 2D pointer/steering.
    // 1.0 = physical for all known controllers (the device layer normalizes
    // every family to the same ±2000 dps reference).
    let orient_disp_scale = pf("orient_scale", 1.0);
    let orient_scale = norm_to_rad_s / dev_gyro_mult * orient_disp_scale;
    // Polarity comes from the module's EXISTING inv_* toggles — the same ones
    // that orient the 2D output — not hardcoded per-device sign guesses. So a
    // device calibrated once (its inv_yaw/inv_pitch/inv_roll) is correct for
    // BOTH the 2D output and this 3D orientation. `gx` already carries inv_roll
    // (applied at read); apply inv_pitch/inv_yaw here to match the 2D output,
    // which negates by `inv("inv_pitch")` / `inv("inv_yaw")` on its Y / X.
    let pitch_rate = gy * inv("inv_pitch");
    let yaw_rate   = gz * inv("inv_yaw");
    let roll_rate  = gx;
    // Device gyro axes (roll, pitch, yaw) → model rotation axes (X=pitch,
    // Y=yaw, Z=roll). Fixed base signs establish the model's handedness; the
    // inv_* toggles above flip per-device as needed. Body-frame angular
    // velocity, composed intrinsically (q_old * dq).
    let gyro_vec = glam::Vec3::new(pitch_rate, -yaw_rate, -roll_rate) * orient_scale;
    let mag = gyro_vec.length();
    if mag > 1e-6 && dt > 0.0 {
        let axis = gyro_vec / mag;
        let angle = mag * dt;
        let rot_q = glam::Quat::from_axis_angle(axis, angle);
        let cur_q = glam::Quat::from_xyzw(
            state.aux_f32[7],
            state.aux_f32[8],
            state.aux_f32[9],
            state.aux_f32[10],
        );
        // Renormalize to shed accumulated floating-point drift over long runs.
        let new_q = (cur_q * rot_q).normalize();
        state.aux_f32[7] = new_q.x;
        state.aux_f32[8] = new_q.y;
        state.aux_f32[9] = new_q.z;
        state.aux_f32[10] = new_q.w;
    }

    // Reset edge: fade quaternion toward identity over ease_in period
    let q_ease_in = pf("reset_ease_in", 0.25).clamp(0.0, 2.0);
    if q_reset_edge { state.aux_f32[12] = 1.0; }
    if state.aux_f32[12] > 0.001 && q_ease_in > 0.001 {
        let step = (dt / q_ease_in).clamp(0.0, 1.0);
        let cur_q = glam::Quat::from_xyzw(
            state.aux_f32[7],
            state.aux_f32[8],
            state.aux_f32[9],
            state.aux_f32[10],
        );
        // Blend toward identity (0,0,0,1)
        let blend_q = cur_q.slerp(glam::Quat::IDENTITY, step);
        state.aux_f32[7] = blend_q.x;
        state.aux_f32[8] = blend_q.y;
        state.aux_f32[9] = blend_q.z;
        state.aux_f32[10] = blend_q.w;
        state.aux_f32[12] = (state.aux_f32[12] - step).max(0.0);
    } else if q_reset_edge && q_ease_in <= 0.001 {
        // Hard reset
        state.aux_f32[7] = 0.0;
        state.aux_f32[8] = 0.0;
        state.aux_f32[9] = 0.0;
        state.aux_f32[10] = 1.0;
        state.aux_f32[12] = 0.0;
    }

    // ── Accel drift correction (complementary filter) ──────────────────────
    //
    // Pure gyro integration accumulates tilt drift. Whenever the controller
    // isn't being shaken, the accelerometer reads gravity — an absolute
    // attitude reference — so nudge the quaternion to keep gravity mapping
    // to a CAPTURED world-frame reference. Capturing (at first steady
    // reading, and re-capturing through a reset) instead of assuming a
    // fixed world "up" means no absolute axis-sign assumptions: whatever
    // pose the user resets in becomes truth, and only tilt drift relative
    // to it is corrected. Yaw (rotation about gravity) is unobservable from
    // accel; the cross-product correction leaves it untouched.
    //
    // Post-inv_* pins are in the canonical device convention, so the accel
    // vector maps into the model body frame with the same fixed axis map
    // the gyro rates use above: dev (x=roll/fwd, y=pitch/side, z=yaw/vert)
    // → model (y, −z, −x).
    // OFF by default: gravity can't distinguish tilt from linear acceleration,
    // so translation (side-to-side / up-down swings) reads as false rotation
    // even behind the steadiness gates. Auto re-center covers rest drift
    // without that failure mode; this stays as an explicit opt-in.
    let drift_corr = pf("orient_drift", 0.0).clamp(0.0, 1.0);
    if drift_corr > 0.0 && dt > 0.0 {
        let a_model = glam::Vec3::new(ay, -az, -ax);
        let acc_len = a_model.length();
        // Accel pins are normalized ±1 == ±8 G; trust the reading only when
        // its magnitude is near 1 g (anything else isn't just gravity).
        const ONE_G: f32 = 1.0 / 8.0;
        if acc_len > 1e-4 {
            // Trust the reading only when BOTH hold:
            //  - |a| ≈ 1 g, TIGHTLY — side-to-side translation adds lateral
            //    acceleration in quadrature, so even a mild shake pushes the
            //    magnitude off 1 g. The old ×4 falloff still corrected at
            //    ~80 % strength during a 0.3 g shake and visibly rotated the
            //    model while it was only being translated.
            //  - the gyro reads near-still — if the pad isn't rotating, a
            //    moving accel vector is translation by definition, and must
            //    never tilt the pose. Drift correction at rest is the whole
            //    point anyway; during motion the gyro integration rules.
            let steady_mag = (1.0 - ((acc_len / ONE_G) - 1.0).abs() * 25.0).clamp(0.0, 1.0);
            let steady_rot = (1.0 - mag / 0.6).clamp(0.0, 1.0); // fades out by ~35°/s
            let steady = steady_mag * steady_rot;
            let u_body = a_model / acc_len;
            let cur_q = glam::Quat::from_xyzw(
                state.aux_f32[7],
                state.aux_f32[8],
                state.aux_f32[9],
                state.aux_f32[10],
            );
            let u_ref = glam::Vec3::new(state.aux_f32[13], state.aux_f32[14], state.aux_f32[15]);
            if u_ref.length_squared() < 0.5 || q_reset_edge || state.aux_f32[12] > 0.001 {
                // First valid reading, reset edge, or mid reset-ease: (re)capture
                // the reference against the current quaternion instead of
                // correcting, so the blend toward identity can't be fought.
                if steady > 0.5 {
                    let w = cur_q * u_body;
                    state.aux_f32[13] = w.x;
                    state.aux_f32[14] = w.y;
                    state.aux_f32[15] = w.z;
                }
            } else if steady > 0.0 {
                let pred = cur_q * u_body; // measured up, world frame
                let err = pred.cross(u_ref); // axis = correction, |err| = sin(angle)
                // Slider → pull rate: 0.25 (default) ≈ τ 2 s, 1.0 ≈ τ 0.125 s.
                let gain = drift_corr * drift_corr * 8.0;
                let step = (gain * steady * dt).min(1.0);
                let new_q = (glam::Quat::from_scaled_axis(err * step) * cur_q).normalize();
                state.aux_f32[7] = new_q.x;
                state.aux_f32[8] = new_q.y;
                state.aux_f32[9] = new_q.z;
                state.aux_f32[10] = new_q.w;
            }
        }
    }

    // ── Auto re-center (Orientation output only) ───────────────────────────
    //
    // With no absolute reference the pose can end up shifted on ANY axis
    // (yaw worst — nothing pins it — but pitch/roll can wander too). When
    // the gyro magnitude stays under the user threshold for 3 s, ease the
    // whole orientation back to identity (τ ≈ 1 s) until it's centered or
    // the threshold is exceeded again.
    if pb("orient_auto_recenter", false) {
        let thresh = pf("orient_recenter_thresh", 0.005).max(1e-5);
        let g_mag = (gx * gx + gy * gy + gz * gz).sqrt();
        if g_mag < thresh {
            state.aux_f32[16] += dt;
        } else {
            state.aux_f32[16] = 0.0;
        }
        if state.aux_f32[16] >= 3.0 && dt > 0.0 {
            let q = glam::Quat::from_xyzw(
                state.aux_f32[7],
                state.aux_f32[8],
                state.aux_f32[9],
                state.aux_f32[10],
            );
            let step = 1.0 - (-dt / 1.0_f32).exp();
            let new_q = q.slerp(glam::Quat::IDENTITY, step).normalize();
            state.aux_f32[7] = new_q.x;
            state.aux_f32[8] = new_q.y;
            state.aux_f32[9] = new_q.z;
            state.aux_f32[10] = new_q.w;
            // Re-anchor the drift-correction gravity reference against the
            // easing pose, exactly like a manual reset does — otherwise the
            // tilt correction would fight the pull toward identity whenever
            // the controller rests in a non-flat pose.
            let a_model = glam::Vec3::new(ay, -az, -ax);
            let len = a_model.length();
            if len > 1e-4 {
                let w = new_q * (a_model / len);
                state.aux_f32[13] = w.x;
                state.aux_f32[14] = w.y;
                state.aux_f32[15] = w.z;
            }
        }
    } else {
        state.aux_f32[16] = 0.0;
    }

    // Emit orientation as Vec4 (x, y, z, w)
    let orientation_signal = Some(Signal::Vec4(glam::Vec4::new(
        state.aux_f32[7],
        state.aux_f32[8],
        state.aux_f32[9],
        state.aux_f32[10],
    )));

    vec![
        Some(Signal::Vec2(glam::Vec2::new(final_x, final_y))),
        Some(Signal::Float(final_x)),
        Some(Signal::Float(final_y)),
        Some(Signal::Float(lean_val)),
        Some(Signal::Bool(lean_active)),
        orientation_signal,
        // Map (AutoMap) — routing-only, no per-frame value. Slot must
        // exist so its index lines up with the module descriptor; the
        // actual per-pin signals are written into collector_sigs under
        // "lean:{uid}" by the dispatch block in `eval_graph_tick`.
        None,
    ]
}

// ── Curve helpers ─────────────────────────────────────────────────────────────

