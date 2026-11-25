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
            "orient"     => "orient",
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

/// The neutral hold named by `hold_pitch` / `hold_yaw` / `hold_roll` (degrees), as
/// the rotation that takes a vector measured on the pad into the frame of that
/// hold. `None` when all three are zero, so the stock module stays bit-exact.
///
/// Written in the gyro's body basis (forward, right, down), which is right-handed,
/// as an aircraft attitude: yaw about down (+ nose turned right), then pitch about
/// right (+ far edge raised — the sign of the JSM module's `LOCAL_AXIS_OFFSET`),
/// then roll about forward (+ right grip down — the sign of Lean).
pub(crate) fn gyro_hold_rotation(params: &HashMap<String, Value>) -> Option<glam::Quat> {
    let deg = |k: &str| params.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    let (pitch, yaw, roll) = (deg("hold_pitch"), deg("hold_yaw"), deg("hold_roll"));
    if pitch == 0.0 && yaw == 0.0 && roll == 0.0 {
        return None;
    }
    Some(
        glam::Quat::from_rotation_z(yaw.to_radians())
            * glam::Quat::from_rotation_y(pitch.to_radians())
            * glam::Quat::from_rotation_x(roll.to_radians()),
    )
}

/// A canonical accel reading in the orientation's model frame (X = right,
/// Y = up, Z = back), the frame the gyro rates are integrated in.
///
/// The accel tuple is (forward, left, up), so right is `-y`, up is `z` and back
/// is `-x`. The gyro's own map, `(y, -z, -x)`, is NOT this: its tuple is
/// (forward, right, down), and reusing it here mirrors the reading about the
/// roll axis — tilt corrections then pull pitch the wrong way.
pub(crate) fn accel_to_model(ax: f32, ay: f32, az: f32) -> glam::Vec3 {
    glam::Vec3::new(-ay, az, -ax)
}

/// Where the Orientation output (and Edge turn) takes its pose from:
/// `orient_source`, absent = Auto.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OrientSource {
    /// Always this module's own integration.
    Computed,
    /// Only the pose the device fuses itself; nothing when it publishes none.
    Device,
    /// The device's when it publishes one, else Computed. The default.
    Auto,
}

pub(crate) fn gyro_orient_source(params: &HashMap<String, Value>) -> OrientSource {
    match params.get("orient_source").and_then(|v| v.as_str()) {
        Some("computed") => OrientSource::Computed,
        Some("device") => OrientSource::Device,
        _ => OrientSource::Auto,
    }
}

/// The device's own orientation off the bus, when it carries one. Canonical,
/// normalised.
fn device_orientation(
    params: &HashMap<String, Value>,
    dev_sigs: &BusMap,
    collector_sigs: &BusMap,
) -> Option<glam::Quat> {
    let dev_id = params.get("_automap_device_id").and_then(|v| v.as_str())?;
    let collector_id = params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("");
    let sig = (!collector_id.is_empty())
        .then(|| collector_sigs.get_pin(collector_id, "orientation"))
        .flatten()
        .or_else(|| dev_sigs.get_pin(dev_id, "orientation"));
    match sig {
        Some(Signal::Vec4(v)) if v.length_squared() > 1e-6 => {
            Some(glam::Quat::from_xyzw(v.x, v.y, v.z, v.w).normalize())
        }
        _ => None,
    }
}

/// How far a canonical orientation has turned from identity, in degrees, as
/// `(yaw, pitch, roll)`: + = nose turned right, nose up, right grip down — the
/// signs of `gyro_z`, `gyro_y` and `gyro_x`. Read off where the pad's forward
/// and right axes now point; canonical is (forward, left, up).
pub(crate) fn pose_angles(q: glam::Quat) -> glam::Vec3 {
    let f = q * glam::Vec3::X;
    let right = q * glam::Vec3::NEG_Y;
    let yaw = (-f.y).atan2(f.x);
    let pitch = f.z.atan2((f.x * f.x + f.y * f.y).sqrt());
    let roll = (-right.z).clamp(-1.0, 1.0).asin();
    glam::Vec3::new(yaw, pitch, roll) * (180.0 / std::f32::consts::PI)
}

/// Edge turn settings, from the `edge_*` params. `None` = off.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EdgeCfg {
    /// The gradient: past the zone, turn faster the further out.
    pub ramp: bool,
    /// Momentum: past the zone, don't let the camera slow below the speed it
    /// was turning at on the way out.
    pub momentum: bool,
    /// Where the edge starts, degrees from neutral. Pure gyro inside.
    pub zone: f32,
    /// Degrees past the zone over which momentum and the gyro hand-off fade in
    /// (0 = all at once).
    pub blend: f32,
    /// Degrees past the zone where Ramp reaches `max_dps` (0 = at once).
    pub ramp_width: f32,
    pub max_dps: f32,
    /// Momentum's share of the exit speed (1 = carries on as fast as it left).
    pub gain: f32,
    /// Momentum fade time constant, s (0 = holds for as long as you stay out).
    pub decay: f32,
    /// The gyro's direct share once fully past the blend: 1 = the gyro keeps
    /// turning the camera as usual (the edge only adds), 0 = the edge takes
    /// over completely, like a stick.
    pub gyro_keep: f32,
    /// Horizontal only: pitch neither counts toward the zone nor turns.
    pub x_only: bool,
}

pub(crate) fn edge_cfg(params: &HashMap<String, Value>) -> Option<EdgeCfg> {
    let (ramp, momentum) = match params.get("edge_mode").and_then(|v| v.as_str()) {
        Some("ramp") => (true, false),
        Some("momentum") => (false, true),
        Some("both") => (true, true),
        _ => return None,
    };
    let f = |k: &str, d: f32| params.get(k).and_then(|v| v.as_f64()).map_or(d, |x| x as f32);
    Some(EdgeCfg {
        ramp,
        momentum,
        zone: f("edge_zone_deg", 30.0).clamp(0.0, 170.0),
        blend: f("edge_blend_deg", 15.0).clamp(0.0, 170.0),
        ramp_width: f("edge_ramp_deg", 30.0).clamp(0.0, 170.0),
        max_dps: f("edge_max_dps", 180.0).clamp(0.0, 4000.0),
        gain: f("edge_momentum", 1.0).clamp(0.0, 10.0),
        decay: f("edge_decay_s", 0.0).clamp(0.0, 30.0),
        gyro_keep: f("edge_gyro_keep", 1.0).clamp(0.0, 1.0),
        x_only: params.get("edge_x_only").and_then(|v| v.as_bool()).unwrap_or(false),
    })
}

/// 0 → 1 across `width` degrees past `from`, eased at both ends (smoothstep),
/// so whatever it weights starts and finishes with no kink. `width` 0 = a step.
fn edge_ease(r: f32, from: f32, width: f32) -> f32 {
    if r <= from {
        0.0
    } else if width <= 0.0 {
        1.0
    } else {
        let t = ((r - from) / width).min(1.0);
        t * t * (3.0 - 2.0 * t)
    }
}

/// What Edge turn does this tick: scale the gyro's own X/Y by `gyro`, then add
/// `add_dps` (degrees per second, the outputs' sign convention).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct EdgeOut {
    pub gyro: glam::Vec2,
    pub add_dps: glam::Vec2,
}

/// One tick of Edge turn. `d` is the deflection from neutral in degrees (X, Y
/// in the outputs' sign convention). `st` (9 floats) carries: [0] last radius,
/// [1..3] last deflection, [3..5] smoothed deflection speed, [5..7] momentum,
/// [8] primed.
///
/// Nothing switches at the zone's edge — everything eases in past it, so the
/// gyro's own feel bleeds into the edge's instead of being cut over to it:
///
/// * **Ramp** — like a stick past its deadzone: 0 at the zone, `max_dps` at
///   `ramp_width` beyond, eased in, pointed the way the pad is deflected.
/// * **Momentum** — the fastest the pad was moving outward since it left the
///   zone (× `gain`) becomes a FLOOR on the camera's speed that way, eased in
///   over `blend`. While the pad keeps turning that fast it adds nothing — the
///   camera just keeps going as it was — and as the pad slows the edge makes up
///   the difference. Pulling back brakes it (the gyro then turns against it,
///   and the floor eases out toward the zone). `decay` fades the floor itself.
/// * **gyro_keep** — the gyro's direct share eases from 1 down to this across
///   `blend`: 1 keeps gyro aim everywhere, 0 hands over to the edge entirely.
pub(crate) fn edge_turn(st: &mut [f32], d: glam::Vec2, dt: f32, c: &EdgeCfg) -> EdgeOut {
    use glam::Vec2;
    const SPEED_TAU: f32 = 0.03;
    let d = if c.x_only { Vec2::new(d.x, 0.0) } else { d };
    let r = d.length();
    let primed = st[8] > 0.5;
    let raw_v = if primed && dt > 0.0 { (d - Vec2::new(st[1], st[2])) / dt } else { Vec2::ZERO };
    let mut v = Vec2::new(st[3], st[4]);
    v += (raw_v - v) * (1.0 - (-dt / SPEED_TAU).exp());
    let mut m = Vec2::new(st[5], st[6]);
    let dir = if r > 0.0 { d / r } else { Vec2::ZERO };
    if r <= c.zone || !primed {
        // Inside — or starting out already past the zone, which isn't an exit.
        m = if r <= c.zone { Vec2::ZERO } else { m };
    } else {
        // The fastest outward the pad has gone since leaving.
        let out_speed = v.dot(dir);
        if out_speed > 0.0 && out_speed > m.length() {
            m = v;
        }
        if c.decay > 0.0 {
            m *= (-dt / c.decay).exp();
        }
    }
    st[0] = r;
    st[1] = d.x;
    st[2] = d.y;
    st[3] = v.x;
    st[4] = v.y;
    st[5] = m.x;
    st[6] = m.y;
    st[8] = 1.0;

    let blend = edge_ease(r, c.zone, c.blend);
    let g = 1.0 - (1.0 - c.gyro_keep) * blend;
    let gyro = if c.x_only { Vec2::new(g, 1.0) } else { Vec2::splat(g) };

    let mut add = Vec2::ZERO;
    if c.ramp {
        add += dir * edge_ease(r, c.zone, c.ramp_width) * c.max_dps;
    }
    if c.momentum {
        let speed = m.length() * c.gain;
        if speed > 0.0 {
            let m_dir = m / m.length();
            // What already moves the camera that way: the gyro's remaining
            // share of the pad's own motion, and the ramp. Only an outward
            // contribution counts — pulling back must not be made up for.
            let moving = (gyro * v).dot(m_dir).max(0.0) + add.dot(m_dir).max(0.0);
            add += m_dir * blend * (speed - moving).max(0.0);
        }
    }
    EdgeOut { gyro, add_dps: add }
}

/// Edge turn as the module runs it, for a picture of it (the 3DOF preview): one tick of `dt`
/// at deflection `d` (degrees off neutral, the X/Y outputs' sign convention) on `state`, the
/// nine floats [`edge_turn`] keeps, which the caller holds. Returns what the gyro's own share
/// is scaled by and what is added, in degrees per second - `None` while `params` have Edge
/// turn off.
pub fn edge_turn_preview(
    params: &HashMap<String, Value>,
    state: &mut [f32; 9],
    d: glam::Vec2,
    dt: f32,
) -> Option<(glam::Vec2, glam::Vec2)> {
    let cfg = edge_cfg(params)?;
    let e = edge_turn(state, d, dt, &cfg);
    Some((e.gyro, e.add_dps))
}

pub(crate) fn compute_gyro_3dof(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
    dev_sigs: &BusMap,
    collector_sigs: &BusMap,
    dt: f32,
) -> Vec<Option<Signal>> {
    let (family, axis) = gyro_resolve_mode(params);
    // Panic mode resets the module, as its Reset input does (`request_gyro_reset`).
    let forced_reset = std::mem::take(&mut state.reset_request);

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
                    if let Some(Signal::Float(f)) = collector_sigs.get_pin(&collector_id, &pin) {
                        return *f;
                    }
                }
                match dev_sigs.get_pin(&dev_id, &pin) {
                    Some(Signal::Float(f)) => *f,
                    _ => 0.0,
                }
            };
            let az_raw = {
                let pin = "accel_z";
                if !collector_id.is_empty() {
                    if let Some(Signal::Float(f)) = collector_sigs.get_pin(&collector_id, &pin) {
                        *f
                    } else {
                        match dev_sigs.get_pin(&dev_id, &pin) {
                            Some(Signal::Float(f)) => *f,
                            _ => 1.0,
                        }
                    }
                } else {
                    match dev_sigs.get_pin(&dev_id, &pin) {
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
    // The physical roll rate, before its invert: what turns gravity in the pad's frame.
    let gx_phys = pin_or(2, gx_am);
    let gx = gx_phys * inv("inv_roll");
    let gy = pin_or(3, gy_am);
    let gz = pin_or(4, gz_am);
    // Accel arrives in the canonical AutoMap frame (see `HidReading`):
    //   x = forward  (+ nose/USB up)
    //   y = side     (+ right grip down)
    //   z = vertical (+ flat, face up)
    // — identical for every pad, and the same axis ROLES the gyro uses (roll
    // about x, pitch about y, yaw about z). (It used to assume a legacy layout
    // with x = side, which only matched the gyro on the Switch Pro — hence the
    // slanted Player/World response on Sony pads.)
    //
    // ⛔ Same roles, NOT the same basis. Written against the pad's own axes,
    // accel is (forward, LEFT, UP) and gyro is (forward, RIGHT, DOWN): the
    // parsers pick each sensor's signs for its own consumer (+1 g lying flat;
    // + = turn right / nose up), and those choices mirror y and z between the
    // two. Anything that combines them — Player/World's projection, the hold
    // offsets, the drift correction — must convert one into the other's basis.
    // Treating the tuples as one vector looks right lying flat and comes apart
    // as the pad tilts.
    let ax = pin_or(5, ax_am) * inv("inv_accel_x");
    let ay = pin_or(6, ay_am) * inv("inv_accel_y");
    let az = pin_or(7, az_am) * inv("inv_accel_z");
    // (Spike suppression moved to the device polling layer — see
    // `flexinput_devices::gyro::apply_spike_filter`. The engine sees an
    // already-clean IMU stream.)

    // ── Neutral hold: the readings the 2D outputs and Lean measure ────────
    //
    // The hold offsets name how the pad sits when it is held at rest, and
    // everything below the Orientation block is measured from there instead of
    // from flat. What that means depends on whether the mode measures against
    // gravity:
    //
    //   Pitch+Yaw / Pitch+Roll  gyro AND gravity are re-measured about the
    //       neutral hold — `LOCAL_AXIS_OFFSET` from the JSM module, on all three
    //       axes. A handheld tilted back 40°: yaw becomes the turn about the
    //       tilted hold's up rather than the face normal, and Lean stops reading
    //       the tilt as half a lean.
    //   Player / World          gravity ONLY. These already adapt to how the
    //       pad is held, and rotating the gyro with gravity would cancel out of
    //       their projection anyway. What the offset changes is which way is
    //       "down": lying on your right side with the pad held normally in front
    //       of you, the pad IS rolled 90° right, and naming that makes turns
    //       about your own up axis read as turns again.
    //
    // The Orientation output keeps the physical readings: it is a pose, not an
    // aim, and the 3D viewer should show the pad as it really is.
    let hold = gyro_hold_rotation(params);
    let measures_gravity = matches!(axis, "player" | "world");
    let (hgx, hgy, hgz) = match hold {
        // The gyro tuple is already in the right-handed body basis the rotation
        // is written in: (forward, right, down).
        Some(q) if !measures_gravity => {
            let v = q * glam::Vec3::new(gx, gy, gz);
            (v.x, v.y, v.z)
        }
        _ => (gx, gy, gz),
    };
    let (hax, hay, haz) = match hold {
        // The accel tuple is in (forward, left, up) — `diag(1, -1, -1)` from the
        // gyro's basis — so it goes into the gyro basis for the rotation and
        // comes back out after it. That flip is a half turn about forward, so
        // rotating the raw components would still get roll right but turn the
        // pitch and yaw offsets the wrong way.
        Some(q) => {
            let v = q * glam::Vec3::new(ax, -ay, -az);
            (v.x, -v.y, -v.z)
        }
        None => (ax, ay, az),
    };
    // `ROLL_CONTRIBUTION` from the JSM module (and its custom-curve fork's
    // `YAW_PLUS_ROLL`): a share of roll joins the turn, Pitch+Yaw only. Same
    // percentage and same sign — positive is the share a far-edge-raised hold
    // loses from yaw into roll, so turning the body comes back whole; negative
    // makes rolling right turn right, like a wheel.
    let roll_share = pf("roll_contribution", 0.0).clamp(-100.0, 100.0) / 100.0;

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
    //   [17..21] device orientation captured at the last Reset
    //   [21..30] Edge turn state (see `edge_turn`)
    while state.aux_f32.len() < 17 { state.aux_f32.push(0.0); }

    // ── Smoothed gravity: how is the pad being HELD? ──────────────────────
    //
    // Low-passed accel direction. Gravity is the only sustained acceleration a
    // hand-held pad sees, so this settles onto "down" in the pad's own frame,
    // and therefore describes the pad's resting orientation independently of
    // whatever gesture is happening right now.
    //
    // That independence is the point. The lean gate below has to ask "is the
    // pad held flat, or nose-up?", and asking the INSTANTANEOUS accel would let
    // a lean answer its own question — tilting far enough always rotates the
    // pad out of the orientation being tested, so confidence would collapse at
    // exactly the extremes of the gesture. The smoothed estimate lags the
    // gesture and keeps the answer about how the pad is being held.
    //
    // (Player, World and Orient read down from JSM's own estimate instead,
    // below: carried by the gyro, it follows the gesture as it happens.)
    let tau = match axis {
        "world" => 3.0_f32,
        "player" => 1.0_f32,
        _ => 2.0_f32,
    };
    let accel_v = glam::Vec3::new(hax, hay, haz);
    let acc_len = accel_v.length();
    if acc_len > 0.01 {
        let alpha = 1.0 - (-dt / tau).exp();
        let norm = accel_v / acc_len;
        state.aux_f32[2] += alpha * (norm.x - state.aux_f32[2]);
        state.aux_f32[3] += alpha * (norm.y - state.aux_f32[3]);
        state.aux_f32[4] += alpha * (norm.z - state.aux_f32[4]);
    }
    let sg = glam::Vec3::new(state.aux_f32[2], state.aux_f32[3], state.aux_f32[4]);
    let sg_len = sg.length();
    let g_hat = if sg_len > 0.01 { sg / sg_len } else { glam::Vec3::new(0.0, 0.0, 1.0) };

    // `gyro_multiplier` (a device.source calibration knob) is already baked into
    // the gyro signals by `preprocess_dev_sigs`; divide it back out so the
    // orientation stays 1:1 no matter what the user sets it to. The multiplier
    // is stashed per-device under the synthetic pin key "__gyro_mult".
    let dev_gyro_mult = params
        .get("_automap_device_id")
        .and_then(|v| v.as_str())
        .and_then(|dev| dev_sigs.get_pin(&dev, "__gyro_mult"))
        .and_then(|s| if let Signal::Float(f) = s { Some(*f) } else { None })
        .filter(|m| m.abs() > 1e-6)
        .unwrap_or(1.0);

    // ── Which way is down, as JoyShockMapper knows it ─────────────────────
    //
    // Player, World and Orient measure against gravity, and take it from the
    // estimate JSM's motion library keeps (`GravityFusion`): carried by the
    // gyro through every turn, pulled in by the accelerometer. Fed the physical
    // readings; the hold then names which way is "down", as it always has.
    // Every mode keeps it: Steering centres on it, and the preview rolls the
    // pad by it.
    while state.aux_f32.len() < FUSION_AUX + GRAVITY_FUSION_LEN { state.aux_f32.push(0.0); }
    let gyro_dps = glam::Vec3::new(gx_phys, gy, gz) * (GYRO_REF_DPS / dev_gyro_mult);
    let (down_held, pad_q, pad_q_before) = {
        let mut fusion = GravityFusion::load(&state.aux_f32[FUSION_AUX..]);
        let before = fusion.orientation();
        let d = fusion.update(gyro_dps, Some(glam::Vec3::new(ax, ay, az)), dt);
        fusion.store(&mut state.aux_f32[FUSION_AUX..]);
        (d.map(|d| hold.map_or(d, |q| q * d)), fusion.orientation(), before)
    };
    // Orient's pad, in the hold's frame: the orientation itself (the preview's pad).
    let orient_now = (axis == "orient").then(|| nose_angles(pad_q, hold));
    while state.aux_f32.len() < ORIENT_AUX + ORIENT_CAMERA_LEN + 1 { state.aux_f32.push(0.0); }
    // A pad with no accelerometer is taken as held flat.
    let down = down_held.unwrap_or(glam::Vec3::Z);

    // ── Axis selection: decide which gyro components feed X / Y ───────────
    //
    // Player and World are JSM's PLAYER_TURN and WORLD_TURN, by JSM's own
    // maths (the JSM module's, which is JSM's line for line); Orient follows
    // where the nose points. Pitch+Yaw and Pitch+Roll take the gyro rates.
    //
    // Lean is derived separately below from accel tilt (NOT a gyro rate) so
    // that holding a tilted controller still asserts a steady lean signal
    // and rocking back through center doesn't produce a spurious opposite
    // lean. See the lean derivation block after this match.
    let (raw_x, raw_y, _raw_lean_unused) = match axis {
        "pitch_roll" => (hgx, hgy, hgz),
        "player" | "world" => {
            // Into JSM's frame - (right, up, forward), X the pitch axis, Y the
            // yaw axis - from the gyro's (forward, right, down). PLAYER_TURN:
            // the turn about gravity from the yaw and roll axes, relaxed by
            // JSM's 60° buffer but never more than the pad is turning, and the
            // pad's own pitch. WORLD_TURN: the turn about gravity, and the pitch
            // axis laid flat, faded as the pad goes onto its side. JSM's Y counts
            // down the screen; ours up.
            let g = JsmGravity { v: glam::Vec3::new(down.y, -down.z, down.x), known: true, ..Default::default() };
            let r = JsmGyro { x: hgy, y: -hgz, z: hgx };
            let space = if axis == "player" { JsmSpace::PlayerTurn } else { JsmSpace::WorldTurn };
            let (mx, my) = jsm_gravity_space(space, g, r);
            (mx, -my, 0.0)
        }
        "orient" => {
            // A camera kept looking where the nose points (`OrientCamera`): the gyro's own
            // turn of the pad this tick, taken along the camera's up and its level right.
            // Over the top the pitch carries on, upside down with the pad; gravity holds
            // the frame the move is measured in, and while the pad moves brings the
            // camera's pitch back onto the pad's by a share of the move (a turn about the
            // vertical otherwise spirals it down or up), never its heading.
            let mut cam = OrientCamera::load(&state.aux_f32[ORIENT_AUX..]);
            let (d_psi, d_theta, fresh) = cam.step(pad_q_before, pad_q, gyro_dps, dt, hold);
            cam.store(&mut state.aux_f32[ORIENT_AUX..]);
            if fresh {
                // The heading's zero: here, until a Reset.
                state.aux_f32[ORIENT_AUX + ORIENT_CAMERA_LEN] = cam.world_aim().0;
            }
            // In the rates' units: ±1 = GYRO_REF_DPS, with the device's multiplier.
            let per = dev_gyro_mult / GYRO_REF_DPS / dt.max(1e-6);
            (d_psi * per, d_theta * per, 0.0)
        }
        // pitch_yaw: gz=yaw→X (plus the roll share), gy=pitch→Y
        _ => (hgz - hgx * roll_share, hgy, 0.0),
    };

    if axis != "orient" {
        // Back to Orient later, it starts afresh.
        state.aux_f32[ORIENT_AUX..ORIENT_AUX + ORIENT_CAMERA_LEN].fill(0.0);
    }

    // ── Steering integration + auto-recentering ───────────────────────────
    let reset_now = get_b(inputs, 1, false);
    let reset_edge = (reset_now && state.aux_f32[5] < 0.5) || forced_reset;
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

        // X recenter: where gravity can see the wheel, it says where centre is.
        //
        // The wheel turns about the pad's own down in Pitch+Yaw - held upright
        // like a wheel, gravity swings across its face - and about its forward
        // in Pitch+Roll, held flat and rolled. Gravity seen about that axis,
        // against where it was at the last Reset, is how far the wheel is
        // turned, and the turn added up is pulled toward it, at
        // `recenter_strength` a second, scaled by how squarely gravity crosses
        // the axis. In Player, World and Orient the wheel turns about gravity
        // itself, which gravity can't see: only a Reset centres those.
        //
        // (This compared an angle in radians with a turn measured in units 35
        // times smaller, from a reference 90° off for an upright pad: set, it
        // pulled the wheel to full lock and held it there.)
        while state.aux_f32.len() < STEER_REF_AUX + 4 { state.aux_f32.push(0.0); }
        if let Some(g) = down_held {
            if reset_edge || state.aux_f32[STEER_REF_AUX + 3] < 0.5 {
                state.aux_f32[STEER_REF_AUX..STEER_REF_AUX + 4].copy_from_slice(&[g.x, g.y, g.z, 1.0]);
            }
        }
        let wheel_axis = match axis {
            "pitch_yaw" => Some(glam::Vec3::Z),
            "pitch_roll" => Some(glam::Vec3::X),
            _ => None,
        };
        if let (true, Some(w_axis), Some(g)) = (recenter_strength > 0.0, wheel_axis, down_held) {
            let r = glam::Vec3::from_slice(&state.aux_f32[STEER_REF_AUX..STEER_REF_AUX + 3]);
            let (p0, p1) = (r - w_axis * w_axis.dot(r), g - w_axis * w_axis.dot(g));
            let weight = p0.length().min(p1.length());
            if weight > 0.05 {
                // Gravity turns back by the pad's turn.
                let seen = -(w_axis.dot(p0.cross(p1)).atan2(p0.dot(p1))).to_degrees();
                // In the integrator's units: rates are ±1 = GYRO_REF_DPS with
                // the device's multiplier in them, and roll has its invert.
                let seen = if axis == "pitch_roll" { seen * inv("inv_roll") } else { seen };
                let per_deg = dev_gyro_mult / GYRO_REF_DPS;
                let delta = seen - state.aux_f32[0] / per_deg;
                let alpha = (recenter_strength * weight * dt).clamp(0.0, 1.0);
                state.aux_f32[0] += alpha * delta * per_deg;
            }
        }

        // Y recenter is intentionally NOT implemented as an independent atan2 —
        // the per-axis approach couples X and Y badly. The proper fix is a
        // continuous 3DOF pose estimate projecting both axes; pending. Until
        // then, Y centers only via the manual reset (ease_in).
        // Reset edge: start an ease-in toward zero. While ease-in > 0 we
        // blend the steering accumulator toward 0 over `ease_in` seconds.
        if reset_edge { state.aux_f32[6] = 1.0; }
        if state.aux_f32[6] > 0.001 && ease_in > 0.001 {
            let step = (dt / ease_in).clamp(0.0, 1.0);
            // This tick's share of what is left of the ease, so the last tick
            // lands on zero. A fixed `step` of what remained left e⁻¹ - over a
            // third of the way - when the ease ran out.
            let k = if state.aux_f32[6] - step <= 0.001 { 1.0 } else { step / state.aux_f32[6] };
            state.aux_f32[0] *= 1.0 - k;
            state.aux_f32[1] *= 1.0 - k;
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
    // SIDE tilt is canonical `ay`. This read the wrong axis until 2026-07 (first
    // forward tilt, then a legacy adapter), so lean tracked pitch — it fired
    // when the pad was tipped toward or away from the player rather than rolled.
    //
    // This is derived from accel ONLY, not gyro rate, so:
    //   - Holding a tilted controller produces a STEADY non-zero lean.
    //   - Returning to neutral smoothly ramps back to 0 (no spurious
    //     opposite spike like raw gyro rate would give).
    //
    // Every mode reads the SAME axis. `ax` is gravity along the pad's side
    // axis, which measures "tilted sideways" whether the pad is held flat (the
    // player feels a roll) or nose-up (they feel a yaw — grips swinging down).
    // The gesture is the same motion in the pad's own frame either way.
    //
    // What differs per mode is whether the pad is being held the way that mode
    // assumes, judged from the SMOOTHED gravity above so a lean cannot answer
    // its own question:
    //
    //   Pitch+Yaw   assumes flat / pointing forward — gravity through the face
    //   Pitch+Roll  assumes nose-up (handheld, or a pad aimed at the ceiling) —
    //               gravity along the forward axis
    //   Player/World adapt to however the pad is held, so they never gate
    //
    // Gating scales rather than cuts, so a pad drifting out of its assumed
    // orientation loses lean smoothly instead of dropping it at a boundary.
    // Measured from the neutral hold, so a hold rolled to one side leans from
    // there rather than reading as a permanent lean.
    let acc_mag_full = (hax * hax + hay * hay + haz * haz).sqrt().max(1e-3);
    // Lean is the side component: canonical `ay`, + when the right grip drops =
    // + right lean. (Verified against a physical pad and pinned by a test.)
    let lean_side = hay / acc_mag_full;
    // Confidence that the pad is held as the mode assumes, from the smoothed
    // gravity — using the forward (x) and vertical (z) components and ignoring
    // side (y), which is the axis the lean gesture itself moves:
    //   Pitch+Yaw  assumes flat    → gravity through the vertical axis (z)
    //   Pitch+Roll assumes nose-up → gravity along the forward axis   (x)
    let hold_ref = (g_hat.x * g_hat.x + g_hat.z * g_hat.z).sqrt().max(1e-3);
    let hold_conf = match axis {
        "pitch_yaw" => (g_hat.z / hold_ref).abs(),
        "pitch_roll" => (g_hat.x / hold_ref).abs(),
        _ => 1.0,
    };
    let lean_val = (lean_side * hold_conf).clamp(-1.0, 1.0);
    let lean_threshold = pf("lean_threshold", 0.3).clamp(0.01, 4.0);
    let lean_active = lean_val.abs() >= lean_threshold;

    // ── Quaternion orientation integration (3DOF pose estimate) ───────────
    //
    // Maintains a continuous orientation estimate by integrating angular
    // velocity over time. Uses aux_f32[7..10] for quaternion x,y,z,w.
    // Gravity-based drift correction can be added later if needed.
    let q_reset_now = get_b(inputs, 1, false);
    let q_reset_edge = (q_reset_now && state.aux_f32[11] < 0.5) || forced_reset;
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
    // Whether the pose is being eased home this tick (its last tick included).
    let pose_easing = state.aux_f32[12] > 0.001;
    if state.aux_f32[12] > 0.001 && q_ease_in > 0.001 {
        let step = (dt / q_ease_in).clamp(0.0, 1.0);
        let cur_q = glam::Quat::from_xyzw(
            state.aux_f32[7],
            state.aux_f32[8],
            state.aux_f32[9],
            state.aux_f32[10],
        );
        // Blend toward identity (0,0,0,1), this tick's share of what is left
        // of the ease, so the last tick lands on it. A fixed `step` of what
        // remained left e⁻¹ of the turn - a pose Reset from 90° stopped at 33°.
        let k = if state.aux_f32[12] - step <= 0.001 { 1.0 } else { step / state.aux_f32[12] };
        let blend_q = cur_q.slerp(glam::Quat::IDENTITY, k);
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
    // The accel goes into the model body frame through `accel_to_model`, NOT
    // the gyro's axis map: the two sensors' tuples are mirrored in y and z
    // (see where they are read). Mapping accel like the gyro made a still pad
    // held 30° nose-up drift to 30° nose-DOWN within seconds — the correction
    // pulled the pose to the mirror image of the truth.
    // OFF by default: gravity can't distinguish tilt from linear acceleration,
    // so translation (side-to-side / up-down swings) reads as false rotation
    // even behind the steadiness gates. Auto re-center covers rest drift
    // without that failure mode; this stays as an explicit opt-in.
    let drift_corr = pf("orient_drift", 0.0).clamp(0.0, 1.0);
    if drift_corr > 0.0 && dt > 0.0 {
        let a_model = accel_to_model(ax, ay, az);
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
            let a_model = accel_to_model(ax, ay, az);
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

    // ⛔ **Published in the CANONICAL frame, not the one it was integrated in.**
    //
    // The integration above works in the model/renderer frame (X=pitch, Y=yaw,
    // Z=roll, Y-up), and so does the accel drift correction beneath it. That is
    // fine internally, but a `Vec4` orientation PIN is canonical by contract —
    // see `flexinput_core::frames` — because a device that fuses its own
    // absolute orientation publishes on the same kind of pin, and the Joy-Con 2
    // publishes canonical.
    //
    // ❗ Emitting the model frame here made the renderer convert a quaternion
    // that was already converted, which permutes the axes: yaw drew as pitch,
    // pitch as roll, roll as yaw. It only showed on rate-only pads — DualSense,
    // DS4, Switch Pro, XInput — because those are the ones whose orientation
    // comes from this module rather than from the device.
    //
    // Converting HERE rather than changing the integration keeps the drift
    // correction's captured gravity reference in the frame it was captured in.
    // ── Set Neutral Forward (a card sent it for this module's pad) ─────────
    //
    // The engine has already re-zeroed the device's OWN orientation pin, so a
    // Reset reference captured against the old one would now be off by the
    // same turn: drop it. The computed pose is this module's own, so it turns
    // round to forward here — yaw only, tilt kept — and the drift fix's
    // captured gravity turns with it, or it would pull the tilt back to where
    // the old heading had it.
    while state.aux_f32.len() < 30 { state.aux_f32.push(0.0); }
    let neutral_now = params
        .get("_automap_device_id")
        .and_then(|v| v.as_str())
        .and_then(|d| dev_sigs.get_pin(&d, &ORIENT_NEUTRAL_DEV_PIN))
        .is_some_and(|s| s.as_bool());
    if neutral_now {
        state.aux_f32[17..21].fill(0.0);
        let q_model = glam::Quat::from_xyzw(
            state.aux_f32[7], state.aux_f32[8], state.aux_f32[9], state.aux_f32[10]);
        let q_c = flexinput_core::frames::viewer_to_canonical_quat(q_model);
        let n_c = flexinput_core::frames::yaw_neutralizer(q_c);
        let q_model = flexinput_core::frames::canonical_to_viewer_quat((n_c * q_c).normalize());
        state.aux_f32[7..11].copy_from_slice(&[q_model.x, q_model.y, q_model.z, q_model.w]);
        let n_model = flexinput_core::frames::canonical_to_viewer_quat(n_c);
        let u_ref = glam::Vec3::new(state.aux_f32[13], state.aux_f32[14], state.aux_f32[15]);
        if u_ref.length_squared() > 0.5 {
            let u = n_model * u_ref;
            state.aux_f32[13..16].copy_from_slice(&[u.x, u.y, u.z]);
        }
    }
    let q_canonical = flexinput_core::frames::viewer_to_canonical_quat(glam::Quat::from_xyzw(
        state.aux_f32[7],
        state.aux_f32[8],
        state.aux_f32[9],
        state.aux_f32[10],
    ));
    // Device / Auto: pass through the orientation the DEVICE fuses itself (a
    // Joy-Con 2 publishes one — roll and pitch held by gravity, yaw integrated
    // with no compass, relative to the calibration window's baseline), instead of the
    // integration above. Reset re-zeroes it against the pose at the press.
    // Auto falls back to the computed one when the bus carries none; Device
    // then publishes nothing, so a pad without one shows it plainly.
    while state.aux_f32.len() < 30 { state.aux_f32.push(0.0); }
    let source = gyro_orient_source(params);
    let device_q = match source {
        OrientSource::Computed => None,
        OrientSource::Device | OrientSource::Auto => device_orientation(params, dev_sigs, collector_sigs),
    };
    let pose = match device_q {
        Some(q) => {
            if q_reset_edge {
                state.aux_f32[17..21].copy_from_slice(&[q.x, q.y, q.z, q.w]);
            }
            let r = glam::Quat::from_xyzw(
                state.aux_f32[17], state.aux_f32[18], state.aux_f32[19], state.aux_f32[20]);
            Some(if r.length_squared() > 0.5 { (r.normalize().inverse() * q).normalize() } else { q })
        }
        None if source == OrientSource::Device => None,
        None => Some(q_canonical),
    };
    let orientation_signal =
        pose.map(|q| Signal::Vec4(glam::Vec4::new(q.x, q.y, q.z, q.w)));

    // ── Where the output has got to, for the preview ──────────────────────
    //
    // X/Y are turn RATES, and a game adds them up. That sum is not where the
    // pad points, and can't be: two numbers added tick by tick don't hold a 3D
    // orientation, and the same turns taken in another order end somewhere
    // else. Roll 90°, pitch 90° about the pad's own axis, roll back: the pad
    // lies flat turned 90° to the side, and Pitch+Yaw has the camera looking
    // at the ceiling (World turns it 90° to the side, as the pad). So the
    // preview draws the sum as this module adds it - this tick's projection,
    // hold, gravity, roll share and inverts and all - rather than working it
    // out again from the pose at the screen's rate. In degrees, in the pose's
    // frame (a computed pose's 3D rot × on it too); Edge turn's own turn is
    // left out, the preview shows that as the world turning. It starts, and
    // starts over, where the pose's nose points as the outputs read it: at a
    // Reset (and while it or Auto re-center eases the pose home), a Set
    // Neutral Forward, or a change of pose source or of mode.
    while state.aux_f32.len() < AIM_AUX + 5 { state.aux_f32.push(0.0); }
    let kind = match (pose, device_q) {
        (None, _) => 3.0,
        (Some(_), Some(_)) => 2.0,
        (Some(_), None) => 1.0,
    };
    let mode = match axis { "pitch_roll" => 2.0, "player" => 3.0, "world" => 4.0, "orient" => 5.0, _ => 1.0 };
    let inv3 = [inv("inv_yaw"), inv("inv_pitch"), inv("inv_roll")];
    // Steering puts out an angle of its own - added up, centred, eased home by
    // a Reset - so the preview shows that, in degrees, as it goes out.
    // Orient's aim is its camera: heading from where a Reset put it, the pitch as it is
    // (gravity says where level is; past ±90 over the top), in real degrees. A Reset puts
    // the camera back where the nose points.
    let orient_aim = orient_now.map(|_| {
        let mut cam = OrientCamera::load(&state.aux_f32[ORIENT_AUX..]);
        if q_reset_edge || neutral_now {
            cam.aim_at(pad_q, hold);
            cam.store(&mut state.aux_f32[ORIENT_AUX..]);
            state.aux_f32[ORIENT_AUX + ORIENT_CAMERA_LEN] = cam.world_aim().0;
        } else if pb("orient_auto_recenter", false) && state.aux_f32[16] >= 3.0 && dt > 0.0 {
            // Auto re-center, the pad still a while: the camera eases onto its nose and
            // the heading back to forward (τ ≈ 1 s, as the Orientation output eases
            // home), sending nothing - a slow Reset.
            let step = 1.0 - (-dt / 1.0_f32).exp();
            cam.ease_onto(pad_q, hold, step);
            cam.store(&mut state.aux_f32[ORIENT_AUX..]);
            let r = &mut state.aux_f32[ORIENT_AUX + ORIENT_CAMERA_LEN];
            *r += step * wrap180(cam.world_aim().0 - *r);
        }
        let (psi, theta) = cam.world_aim();
        (wrap180(psi - state.aux_f32[ORIENT_AUX + ORIENT_CAMERA_LEN]) * inv3[0], theta * inv3[1])
    });
    let steer = (family == "steering").then(|| {
        let k = GYRO_REF_DPS / dev_gyro_mult * if device_q.is_some() { 1.0 } else { orient_disp_scale };
        let x = state.aux_f32[0] * inv3[0] * k;
        let y = if pb("steering_exclude_y", false) { 0.0 } else { state.aux_f32[1] * inv3[1] * k };
        (x, y)
    });
    // A Reset eases the pose home (`reset_ease_in`), and so does Auto
    // re-center once the pad has been still a while: the aim rides it there.
    let easing = pose_easing || (pb("orient_auto_recenter", false) && state.aux_f32[16] >= 3.0);
    let a = &mut state.aux_f32[AIM_AUX..AIM_AUX + 5];
    if let Some((x, y)) = steer.or(orient_aim) {
        a.copy_from_slice(&[x, y, x, kind, mode]);
    } else if q_reset_edge || easing || neutral_now || a[3] != kind || a[4] != mode {
        let (x, y, base) = pose.map_or((0.0, 0.0, 0.0), |q| aim_start(q, axis, roll_share, inv3, device_q.is_some(), hold));
        a.copy_from_slice(&[x, y, base, kind, mode]);
    } else {
        // Degrees this tick: the rates are ±1 = GYRO_REF_DPS with the device's
        // multiplier in them. A device's pose is physical; a computed one turns
        // by the 3D rot ×.
        let k = GYRO_REF_DPS / dev_gyro_mult * dt * if device_q.is_some() { 1.0 } else { orient_disp_scale };
        let base_rate = match axis { "pitch_roll" => 0.0, "player" | "world" | "orient" => raw_x, _ => hgz };
        let wrap = |d: f32| (d + 180.0).rem_euclid(360.0) - 180.0;
        // Over the poles too: past straight up is over the top, where the nose went.
        a[0] = wrap(a[0] + raw_x * inv3[0] * k);
        a[1] = wrap(a[1] + raw_y * inv3[1] * k);
        a[2] = wrap(a[2] + base_rate * inv3[0] * k);
    }
    let aim = glam::Vec4::new(a[0], a[1], a[2], kind);

    // ── The pad as the output has it, for the preview ─────────────────────
    //
    // Its nose where the aim is, rolled as gravity says: the model the
    // preview draws, so the pad and the pointer it drives agree in every mode.
    // A camera has a heading and a pitch and nothing else, and those are the
    // output's - whatever path the pad took to them, and wherever the
    // Orientation output, integrated on its own, has wandered. In the hold's
    // frame, canonical (forward, left, up).
    // Orient's is the pad's own orientation: no angles rebuilt into one, so nothing
    // spins as the nose goes over a pole.
    let roll = down_held.map_or(0.0, |d| d.y.atan2(d.z));
    let view = {
        let frd = match orient_now {
            Some((_, _, v)) if family != "steering" => {
                glam::Quat::from_rotation_z(-state.aux_f32[ORIENT_AUX + ORIENT_CAMERA_LEN].to_radians()) * v
            }
            _ => {
                glam::Quat::from_rotation_z(aim.x.to_radians())
                    * glam::Quat::from_rotation_y(aim.y.to_radians())
                    * glam::Quat::from_rotation_x(roll)
            }
        };
        let flip = glam::Quat::from_rotation_x(std::f32::consts::PI);
        (flip * frd * flip).normalize()
    };

    // ── Edge turn (Pointer only) ──────────────────────────────────────────
    //
    // Past `edge_zone_deg` from the neutral pose (the Orientation output's
    // identity — where Reset last put it), the camera keeps turning on its
    // own, the way a cursor scrolls a view at the screen edge. The pose is
    // measured as a stick-like deflection: X is how far the pad is yawed (rolled
    // in Pitch+Roll), Y how far it is pitched, in the SAME sign convention as
    // the X/Y outputs, so the turn always continues the way the gyro was
    // already turning the camera. See `edge_turn` for the two ways it turns.
    let mut final_x = final_x;
    let mut final_y = final_y;
    let edge = edge_cfg(params);
    let deflection = match (orient_now, pose) {
        // Orient: where the pad's nose is against the reference - right of the heading a
        // Reset made zero, up from level - as the preview's pad and pointer show it. It
        // went by the Orientation output, which measures from the pad as it was at the
        // Reset: where a Reset wasn't level, or the accelerometer read level wrong, the
        // edge came at a pitch the preview didn't show it at.
        (Some((heading, elevation, _)), _) => Some(glam::Vec2::new(
            wrap180(heading - state.aux_f32[ORIENT_AUX + ORIENT_CAMERA_LEN]) * inv("inv_yaw"),
            elevation * inv("inv_pitch"),
        )),
        (None, Some(q)) => {
            let a = pose_angles(q);
            // The computed pose already integrates with the inv_* toggles (and
            // the 3D rot × scale); a device's is physical, so it takes them here.
            Some(if device_q.is_some() {
                let x = if axis == "pitch_roll" { a.z * inv("inv_roll") } else { a.x };
                glam::Vec2::new(x * inv("inv_yaw"), a.y * inv("inv_pitch"))
            } else {
                let x = if axis == "pitch_roll" { a.z * inv("inv_yaw") } else { a.x };
                glam::Vec2::new(x, a.y) / orient_disp_scale.abs().max(1e-3)
            })
        }
        (None, None) => None,
    };
    match (deflection, &edge) {
        (Some(d), Some(cfg)) if family == "pointer" => {
            let e = edge_turn(&mut state.aux_f32[21..30], d, dt, cfg);
            // Out in the X/Y outputs' units: ±1 == ±GYRO_REF_DPS, with the
            // device's gyro multiplier on top, as the gyro itself carries it.
            let k = dev_gyro_mult / GYRO_REF_DPS;
            final_x = final_x * e.gyro.x + e.add_dps.x * k;
            final_y = final_y * e.gyro.y + e.add_dps.y * k;
        }
        _ => state.aux_f32[21..30].fill(0.0),
    }

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
        // Not pins: the preview's aim, at `GYRO_AIM_OUT`, and the pad as the
        // output has it, at `GYRO_VIEW_OUT`.
        Some(Signal::Vec4(aim)),
        Some(Signal::Vec4(glam::Vec4::new(view.x, view.y, view.z, view.w))),
    ]
}

/// Index in a 3DOF node's `last_out` of the display-only trailing output with
/// where the X/Y outputs have got to, added up (see `compute_gyro_3dof`):
/// `Vec4(x°, y°, x° without roll's share, pose kind)` - kind 1 a computed pose
/// (already turned by the inverts), 2 the device's own (physical), 3 none.
pub const GYRO_AIM_OUT: usize = 7;

/// Index in a 3DOF node's `last_out` of the display-only trailing output with
/// the pad as the output has it: a quaternion, canonical (forward, left, up), in
/// the hold's frame - its nose on the aim, rolled as gravity says. The preview
/// draws the pad from it (the hold turned by it).
pub const GYRO_VIEW_OUT: usize = 8;

/// Where in `aux_f32` the aim is kept: x, y, base x, then the pose kind and
/// the mode it was started for.
const AIM_AUX: usize = 30;

/// Where in `aux_f32` JSM's gravity estimate is kept (`GravityFusion`).
const FUSION_AUX: usize = 35;

/// Where in `aux_f32` Steering keeps gravity as it was at the last Reset (its
/// centre), and whether it has one.
const STEER_REF_AUX: usize = 46;

/// Where in `aux_f32` Orient keeps its camera (`OrientCamera`), then the heading
/// a Reset made zero.
const ORIENT_AUX: usize = 50;

/// A difference of angles, the short way round: -180..180.
fn wrap180(d: f32) -> f32 {
    (d + 180.0).rem_euclid(360.0) - 180.0
}

/// Where the nose of the pad at `q` points - body to world in (forward, right,
/// down), the world's down along +z (`GravityFusion::orientation`) - measured from
/// the neutral hold: its heading right of the world's forward and its elevation,
/// in degrees, and the hold's orientation.
fn nose_angles(q: glam::Quat, hold: Option<glam::Quat>) -> (f32, f32, glam::Quat) {
    let v = hold.map_or(q, |h| (q * h.inverse()).normalize());
    let n = v * glam::Vec3::X;
    (n.y.atan2(n.x).to_degrees(), (-n.z).clamp(-1.0, 1.0).asin().to_degrees(), v)
}

/// Panic mode: every 3DOF module in `graph`, sub-patches too, resets at its next
/// tick - its Orientation output, Steering's wheel, the aim - as its Reset input
/// would. Keys are the engine's (`namespaced_uid` inside a sub-patch).
pub fn request_gyro_reset(graph: &crate::graph::ProcessingGraph, state: &mut HashMap<usize, NodeState>) {
    fn walk(g: &crate::graph::ProcessingGraph, outer: Option<usize>, state: &mut HashMap<usize, NodeState>) {
        for snap in &g.nodes {
            let uid = outer.map_or(snap.node_uid, |o| crate::eval::namespaced_uid(o, snap.node_uid));
            if snap.module_id == "processing.gyro_3dof" {
                state.entry(uid).or_default().reset_request = true;
            }
            if let Some(sub) = &snap.inline_subgraph {
                walk(&sub.graph, Some(uid), state);
            }
        }
    }
    walk(graph, None, state);
}

/// Where pose `q`'s nose points as the X/Y outputs read it, in degrees: (x, y,
/// x without roll's share) - where the aim starts. `inv` is the inverts
/// (yaw, pitch, roll); `device` says the pose is the device's own, which is
/// physical, where a computed one already turns with the inverts. Pitch+Yaw
/// and Pitch+Roll measure the pad about its neutral hold.
fn aim_start(q: glam::Quat, axis: &str, roll_share: f32, inv: [f32; 3], device: bool, hold: Option<glam::Quat>) -> (f32, f32, f32) {
    let q = match hold {
        Some(h) if !matches!(axis, "player" | "world") => {
            // The hold is written in the gyro's (forward, right, down) basis: a
            // half turn about forward from the pose's (forward, left, up).
            let flip = glam::Quat::from_rotation_x(std::f32::consts::PI);
            let h = flip * h * flip;
            (h * q * h.inverse()).normalize()
        }
        _ => q,
    };
    let a = pose_angles(q);
    let [iy, ip, ir] = inv;
    let (yaw, pitch, roll) = if device { (a.x * iy, a.y * ip, a.z * ir * iy) } else { (a.x, a.y, a.z * iy) };
    match axis {
        "pitch_roll" => (roll, pitch, 0.0),
        "player" | "world" | "orient" => (yaw, pitch, yaw),
        _ => (yaw - roll * roll_share, pitch, yaw),
    }
}

// ── Curve helpers ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod hold_offset_tests {
    use super::compute_gyro_3dof;
    use crate::NodeState;
    use flexinput_core::{Signal, BusMap};
    use serde_json::{json, Value};
    use std::collections::HashMap;

    /// Feed a constant canonical gyro + accel reading for a second and return
    /// the module's (X, Y, Lean, Orientation).
    fn run(params: &[(&str, Value)], gyro: [f32; 3], accel: [f32; 3]) -> (f32, f32, f32, glam::Vec4) {
        let mut state = NodeState::default();
        let mut p: HashMap<String, Value> = HashMap::new();
        for (k, v) in params {
            p.insert((*k).into(), v.clone());
        }
        let (dev, coll) = (BusMap::default(), BusMap::default());
        let inputs: Vec<Option<Signal>> = vec![
            None,
            None,
            Some(Signal::Float(gyro[0])),
            Some(Signal::Float(gyro[1])),
            Some(Signal::Float(gyro[2])),
            Some(Signal::Float(accel[0])),
            Some(Signal::Float(accel[1])),
            Some(Signal::Float(accel[2])),
        ];
        // Gravity settles with the pad still, then one tick of the turn: the
        // projection as it reads a turn. Held for a second, a constant gyro
        // with a constant accel is a pad turning while gravity doesn't move in
        // its frame - only true of a turn about gravity itself - and Player and
        // World now carry gravity with the gyro.
        let still: Vec<Option<Signal>> = inputs
            .iter()
            .enumerate()
            .map(|(i, s)| if (2..5).contains(&i) { Some(Signal::Float(0.0)) } else { *s })
            .collect();
        for _ in 0..250 {
            compute_gyro_3dof(&still, &mut state, &p, &dev, &coll, 1.0 / 250.0);
        }
        let out = compute_gyro_3dof(&inputs, &mut state, &p, &dev, &coll, 1.0 / 250.0);
        let f = |i: usize| match out[i] {
            Some(Signal::Float(v)) => v,
            ref other => panic!("slot {i}: {other:?}"),
        };
        let q = match out[5] {
            Some(Signal::Vec4(v)) => v,
            ref other => panic!("orientation: {other:?}"),
        };
        (f(1), f(2), f(3), q)
    }

    /// A pad held `deg` nose-up, turning right about the WORLD's up at `w`:
    /// canonical gyro + accel. Gyro is (forward, right, down), so the turn
    /// splits into yaw `w·cos` and a roll of `-w·sin`; accel reads up in
    /// (forward, left, up).
    fn nose_up_turn(deg: f32, w: f32) -> ([f32; 3], [f32; 3]) {
        let (s, c) = deg.to_radians().sin_cos();
        ([-w * s, 0.0, w * c], [s, 0.0, c])
    }

    #[test]
    fn a_pitch_offset_recovers_the_whole_turn_of_a_tilted_hold() {
        let w = 0.1;
        let (g, a) = nose_up_turn(30.0, w);
        let (x0, _, _, _) = run(&[], g, a);
        assert!((x0 - w * 30f32.to_radians().cos()).abs() < 1e-4, "stock reads yaw only: {x0}");
        let (x, y, _, _) = run(&[("hold_pitch", json!(30.0))], g, a);
        assert!((x - w).abs() < 1e-4, "the turn about the tilted hold's up, whole: {x}");
        assert!(y.abs() < 1e-4, "and none of it leaks into pitch: {y}");
    }

    #[test]
    fn roll_contribution_adds_the_lost_share_with_the_jsm_sign() {
        // Positive is the share a far-edge-raised hold loses into roll, so it
        // must ADD to a body turn there, exactly as `YAW_PLUS_ROLL` does.
        let w = 0.1;
        let (g, a) = nose_up_turn(30.0, w);
        let (x, _, _, _) = run(&[("roll_contribution", json!(50.0))], g, a);
        let (s, c) = 30f32.to_radians().sin_cos();
        assert!((x - w * (c + 0.5 * s)).abs() < 1e-4, "{x}");
        // Pitch+Yaw only: Pitch+Roll's X is roll itself.
        let (xr, _, _, _) =
            run(&[("axis", json!("pitch_roll")), ("family", json!("pointer")), ("roll_contribution", json!(50.0))], g, a);
        assert!((xr - g[0]).abs() < 1e-4, "{xr}");
    }

    #[test]
    fn a_roll_offset_makes_a_sideways_hold_neutral_for_lean_and_player() {
        // Lying on your right side, pad held normally in front of you: the pad
        // is rolled 90° right (right grip down → canonical accel +y), and the
        // turns you make about your own up are the pad's own yaw.
        let w = 0.1;
        let player = [("family", json!("pointer")), ("axis", json!("player"))];
        let (x0, _, lean0, _) = run(&player, [0.0, 0.0, w], [0.0, 1.0, 0.0]);
        assert!(lean0 > 0.99, "stock reads a full right lean: {lean0}");
        assert!(x0.abs() < 1e-4, "and a turn about your up as nothing: {x0}");
        let mut p = player.to_vec();
        p.push(("hold_roll", json!(90.0)));
        let (x, _, lean, _) = run(&p, [0.0, 0.0, w], [0.0, 1.0, 0.0]);
        assert!(lean.abs() < 1e-3, "lean measured from the hold: {lean}");
        assert!((x - w).abs() < 1e-4, "the turn reads again: {x}");
    }

    #[test]
    fn player_rotates_gravity_only_and_pitch_turns_the_right_way() {
        // Pad 30° nose-up, offset naming that hold: in Player the offset moves
        // "down" to the pad's own face normal, so a pure pad-local yaw reads
        // whole. Rotating the gyro too would cancel out (w·cos 30°), and
        // rotating the accel tuple without its basis flip would tilt gravity to
        // 60° instead of flat (w·cos 60°).
        let w = 0.1;
        let (s, c) = 30f32.to_radians().sin_cos();
        let p = [("family", json!("pointer")), ("axis", json!("player")), ("hold_pitch", json!(30.0))];
        let (x, _, _, _) = run(&p, [0.0, 0.0, w], [s, 0.0, c]);
        assert!((x - w).abs() < 1e-4, "{x}");
    }

    #[test]
    fn player_reads_a_whole_turn_about_gravity_on_a_tilted_pad() {
        // ⛔ The regression: Player dotted the gyro with the accel tuple as if
        // they shared a basis, so a real turn read as `cos 2θ` of itself —
        // nothing at 45°, reversed past it. Flat it was right, which hid it.
        let w = 0.1;
        let player = [("family", json!("pointer")), ("axis", json!("player"))];
        for deg in [0.0, 30.0, 45.0, 60.0] {
            let (g, a) = nose_up_turn(deg, w);
            let (x, y, _, _) = run(&player, g, a);
            assert!((x - w).abs() < 1e-4, "{deg}° nose-up: a turn about gravity read {x}");
            assert!(y.abs() < 1e-4, "{deg}° nose-up: and leaked {y} into pitch");
        }
    }

    #[test]
    fn hold_offsets_leave_the_orientation_output_physical() {
        let (g, a) = nose_up_turn(30.0, 0.1);
        let (_, _, _, q0) = run(&[], g, a);
        let (_, _, _, q) =
            run(&[("hold_pitch", json!(30.0)), ("hold_yaw", json!(20.0)), ("hold_roll", json!(-15.0))], g, a);
        assert!((q - q0).length() < 1e-6, "{q:?} vs {q0:?}");
    }
}

#[cfg(test)]
mod edge_turn_tests {
    use super::{compute_gyro_3dof, edge_cfg, edge_turn};
    use crate::NodeState;
    use flexinput_core::{Signal, BusMap};
    use serde_json::{json, Value};
    use std::collections::HashMap;

    const DT: f32 = 1.0 / 250.0;
    /// Normalised gyro for `dps` degrees per second.
    fn rate(dps: f32) -> f32 { dps / 2000.0 }

    struct Rig {
        st: NodeState,
        p: HashMap<String, Value>,
        dev: BusMap,
    }

    impl Rig {
        fn new(extra: &[(&str, Value)]) -> Self {
            let mut p: HashMap<String, Value> = HashMap::new();
            p.insert("family".into(), json!("pointer"));
            p.insert("axis".into(), json!("pitch_yaw"));
            for (k, v) in extra {
                p.insert((*k).into(), v.clone());
            }
            Rig { st: NodeState::default(), p, dev: BusMap::default() }
        }
        /// Run `secs` at a constant gyro (°/s, canonical x/y/z) and return the
        /// last (X, Y, Orientation).
        fn spin(&mut self, dps: [f32; 3], secs: f32) -> (f32, f32, Option<glam::Vec4>) {
            let inputs: Vec<Option<Signal>> = vec![
                None,
                None,
                Some(Signal::Float(rate(dps[0]))),
                Some(Signal::Float(rate(dps[1]))),
                Some(Signal::Float(rate(dps[2]))),
                Some(Signal::Float(0.0)),
                Some(Signal::Float(0.0)),
                Some(Signal::Float(0.125)),
            ];
            let coll = BusMap::default();
            let mut out = Vec::new();
            for _ in 0..((secs / DT).round() as usize).max(1) {
                out = compute_gyro_3dof(&inputs, &mut self.st, &self.p, &self.dev, &coll, DT);
            }
            let f = |i: usize| match out[i] {
                Some(Signal::Float(v)) => v,
                ref o => panic!("{o:?}"),
            };
            let q = match out[5] {
                Some(Signal::Vec4(v)) => Some(v),
                None => None,
                ref o => panic!("{o:?}"),
            };
            (f(1), f(2), q)
        }
    }

    fn ramp(zone: f32, width: f32, max: f32) -> Vec<(&'static str, Value)> {
        vec![
            ("edge_mode", json!("ramp")),
            ("edge_zone_deg", json!(zone)),
            ("edge_ramp_deg", json!(width)),
            ("edge_max_dps", json!(max)),
        ]
    }

    #[test]
    fn inside_the_zone_nothing_changes() {
        let mut r = Rig::new(&ramp(30.0, 30.0, 200.0));
        r.spin([0.0, 0.0, 100.0], 0.2); // 20° right
        let (x, y, _) = r.spin([0.0, 0.0, 0.0], 0.5);
        assert_eq!((x, y), (0.0, 0.0));
    }

    #[test]
    fn ramp_turns_on_past_the_zone_the_way_the_gyro_turned() {
        // 45° right with a 30° zone and 30° ramp: halfway → 100 °/s, held.
        let mut r = Rig::new(&ramp(30.0, 30.0, 200.0));
        r.spin([0.0, 0.0, 100.0], 0.45);
        let (x, y, _) = r.spin([0.0, 0.0, 0.0], 1.0);
        assert!((x - rate(100.0)).abs() < rate(2.0), "yaw right keeps turning right: {x}");
        assert!(y.abs() < 1e-6, "{y}");
        // Pitch up the same way → Y, with the gyro's sign.
        let mut r = Rig::new(&ramp(30.0, 30.0, 200.0));
        r.spin([0.0, 100.0, 0.0], 0.45);
        let (x, y, _) = r.spin([0.0, 0.0, 0.0], 1.0);
        assert!((y - rate(100.0)).abs() < rate(2.0), "nose up keeps turning up: {y}");
        assert!(x.abs() < 1e-6, "{x}");
        // Pitch+Roll: X is roll.
        let mut p = ramp(30.0, 30.0, 200.0);
        p.push(("axis", json!("pitch_roll")));
        let mut r = Rig::new(&p);
        r.spin([100.0, 0.0, 0.0], 0.45);
        let (x, _, _) = r.spin([0.0, 0.0, 0.0], 1.0);
        assert!((x - rate(100.0)).abs() < rate(2.0), "rolled right keeps turning right: {x}");
    }

    #[test]
    fn an_inverted_axis_turns_with_the_gyro_not_against_it() {
        for src in ["computed", "auto"] {
            let mut p = ramp(30.0, 30.0, 200.0);
            p.push(("inv_yaw", json!(true)));
            p.push(("orient_source", json!(src)));
            let mut r = Rig::new(&p);
            let (moving, _, _) = r.spin([0.0, 0.0, 100.0], 0.45);
            assert!(moving < 0.0, "inverted yaw: the gyro turns X negative ({moving})");
            let (x, _, _) = r.spin([0.0, 0.0, 0.0], 0.5);
            assert!(x < -rate(90.0), "{src}: and so does the edge turn ({x})");
        }
    }

    #[test]
    fn a_device_pose_is_used_and_takes_the_inversions() {
        // Device orientation 45° nose-right (a negative turn about canonical
        // up), gyro still. Auto and Device take it; Computed ignores it.
        let q = glam::Quat::from_rotation_z((-45f32).to_radians());
        for (src, inv, want) in [("auto", false, 100.0), ("device", true, -100.0), ("computed", false, 0.0)] {
            let mut p = ramp(30.0, 30.0, 200.0);
            p.push(("orient_source", json!(src)));
            p.push(("inv_yaw", json!(inv)));
            p.push(("_automap_device_id", json!("d")));
            let mut r = Rig::new(&p);
            r.dev.insert(("d".into(), "orientation".into()), Signal::Vec4(glam::Vec4::new(q.x, q.y, q.z, q.w)));
            let (x, _, _) = r.spin([0.0; 3], 0.2);
            assert!((x - rate(want)).abs() < rate(2.0), "{src}: {x}");
        }
    }

    #[test]
    fn set_neutral_turns_the_computed_pose_to_forward() {
        let mut r = Rig::new(&[("_automap_device_id", json!("pad"))]);
        r.spin([0.0, 100.0, 0.0], 0.2); // 20° nose up
        r.spin([0.0, 0.0, 100.0], 0.4); // then 40° right
        r.dev.insert(("pad".into(), super::ORIENT_NEUTRAL_DEV_PIN.into()), Signal::Bool(true));
        let (_, _, q) = r.spin([0.0; 3], DT);
        let q = q.unwrap();
        let q = glam::Quat::from_xyzw(q.x, q.y, q.z, q.w);
        let a = super::pose_angles(q);
        assert!(a.x.abs() < 0.1, "yaw back to forward: {a:?}");
        assert!(a.y > 15.0, "tilt kept: {a:?}");
    }

    #[test]
    fn device_is_strict_and_auto_falls_back() {
        let mut d = Rig::new(&[("orient_source", json!("device"))]);
        assert_eq!(d.spin([0.0, 0.0, 100.0], 0.2).2, None, "Device with no device pose: nothing");
        let mut a = Rig::new(&[]);
        let q = a.spin([0.0, 0.0, 100.0], 0.2).2.expect("Auto (the default) falls back to Computed");
        assert!(q.w < 0.999, "and it's the integrated pose: {q:?}");
    }

    fn momentum(zone: f32, blend: f32) -> Vec<(&'static str, Value)> {
        vec![
            ("edge_mode", json!("momentum")),
            ("edge_zone_deg", json!(zone)),
            ("edge_blend_deg", json!(blend)),
            ("edge_momentum", json!(1.0)),
        ]
    }

    #[test]
    fn momentum_carries_the_exit_speed_until_back_inside() {
        let mut r = Rig::new(&momentum(20.0, 5.0));
        r.spin([0.0, 0.0, 300.0], 0.1); // 30°: out at 300 °/s
        let (x, _, _) = r.spin([0.0, 0.0, 0.0], 1.0);
        assert!((x - rate(300.0)).abs() < rate(15.0), "holds the flick's speed: {x}");
        // A slow exit carries a slow turn.
        let mut r = Rig::new(&momentum(20.0, 5.0));
        r.spin([0.0, 0.0, 40.0], 0.75);
        let (slow, _, _) = r.spin([0.0, 0.0, 0.0], 1.0);
        assert!((slow - rate(40.0)).abs() < rate(5.0), "{slow}");
        // Back inside: stops.
        r.spin([0.0, 0.0, -40.0], 0.5);
        let (x, _, _) = r.spin([0.0, 0.0, 0.0], 0.2);
        assert_eq!(x, 0.0);
    }

    #[test]
    fn momentum_keeps_the_speed_continuous_instead_of_adding_to_it() {
        // ⛔ The first version added the exit speed ON TOP of the gyro, so a
        // pad still turning as it left the zone doubled the camera's speed at
        // the boundary. Turning steadily straight through it, the camera must
        // simply keep the gyro's speed — every tick, not just at the end.
        let mut r = Rig::new(&momentum(20.0, 15.0));
        for _ in 0..40 {
            let (x, _, _) = r.spin([0.0, 0.0, 300.0], 0.005); // to 60°
            assert!((x - rate(300.0)).abs() < rate(10.0), "{x}");
        }
        // Easing off, the edge makes up the difference.
        let (x, _, _) = r.spin([0.0, 0.0, 100.0], 0.1);
        assert!((x - rate(300.0)).abs() < rate(10.0), "slowing down keeps the camera's speed: {x}");
    }

    #[test]
    fn pulling_back_brakes_momentum() {
        let mut r = Rig::new(&momentum(20.0, 5.0));
        r.spin([0.0, 0.0, 300.0], 0.15); // 45°
        r.spin([0.0, 0.0, 0.0], 0.2);
        let (x, _, _) = r.spin([0.0, 0.0, -100.0], 0.05);
        assert!(x < rate(250.0) && x > 0.0, "the gyro turns against the carry: {x}");
    }

    #[test]
    fn the_blend_eases_momentum_in_past_the_zone() {
        // Stopped just past the zone, only part of the carry has eased in.
        let mut r = Rig::new(&momentum(20.0, 20.0));
        r.spin([0.0, 0.0, 300.0], 0.1); // 30°: halfway through the blend
        let (x, _, _) = r.spin([0.0, 0.0, 0.0], 0.2);
        assert!((x - rate(150.0)).abs() < rate(10.0), "smoothstep(0.5) of 300: {x}");
    }

    #[test]
    fn gyro_keep_hands_the_camera_over_to_the_edge() {
        let mut p = ramp(30.0, 30.0, 200.0);
        p.push(("edge_blend_deg", json!(10.0)));
        p.push(("edge_gyro_keep", json!(0.0)));
        let mut r = Rig::new(&p);
        let (x, _, _) = r.spin([0.0, 0.0, 100.0], 0.7); // still turning, at 70°
        assert!((x - rate(200.0)).abs() < rate(3.0), "the ramp alone, not the ramp plus the gyro: {x}");
        let (inside, _, _) = Rig::new(&p).spin([0.0, 0.0, 100.0], 0.2); // 20°
        assert!((inside - rate(100.0)).abs() < 1e-4, "inside, the gyro as usual: {inside}");
    }

    #[test]
    fn momentum_decay_fades_it() {
        let cfg = edge_cfg(&HashMap::from([
            ("edge_mode".to_string(), json!("momentum")),
            ("edge_zone_deg".to_string(), json!(10.0)),
            ("edge_decay_s".to_string(), json!(0.5)),
        ]))
        .unwrap();
        let mut st = [0.0f32; 9];
        let mut d = glam::Vec2::ZERO;
        for _ in 0..50 {
            d.x += 200.0 * DT; // 200 °/s out to 40°
            edge_turn(&mut st, d, DT, &cfg);
        }
        let mut out = edge_turn(&mut st, d, DT, &cfg).add_dps;
        for _ in 0..50 {
            out = edge_turn(&mut st, d, DT, &cfg).add_dps; // let the speed estimate settle at 0
        }
        // The fade runs from the stop (0.2 s ago by now): 200·e^-0.4 ≈ 134.
        let at_stop = out.x;
        for _ in 0..125 {
            out = edge_turn(&mut st, d, DT, &cfg).add_dps; // 0.5 s still
        }
        assert!((at_stop - 134.0).abs() < 5.0, "{at_stop}");
        assert!((out.x / at_stop - (-1.0f32).exp()).abs() < 0.05, "one time constant: {}", out.x / at_stop);
    }

    #[test]
    fn x_only_ignores_pitch() {
        let mut p = ramp(30.0, 30.0, 200.0);
        p.push(("edge_x_only", json!(true)));
        let mut r = Rig::new(&p);
        r.spin([0.0, 100.0, 0.0], 0.6); // 60° nose up
        let (x, y, _) = r.spin([0.0, 0.0, 0.0], 0.5);
        assert_eq!((x, y), (0.0, 0.0));
    }

    #[test]
    fn steering_ignores_it() {
        let mut p = ramp(10.0, 0.0, 200.0);
        p[0] = ("edge_mode", json!("both"));
        let mut r = Rig::new(&p);
        r.p.insert("family".into(), json!("steering"));
        let (x0, _, _) = r.spin([0.0, 0.0, 100.0], 0.45);
        let (x1, _, _) = r.spin([0.0, 0.0, 0.0], 0.5);
        assert!((x1 - x0).abs() < 1e-3, "steering holds its angle: {x0} → {x1}");
    }
}

#[cfg(test)]
mod drift_correction_tests {
    use super::compute_gyro_3dof;
    use crate::NodeState;
    use flexinput_core::{Signal, BusMap};
    use serde_json::{json, Value};
    use std::collections::HashMap;

    const ONE_G: f32 = 0.125; // accel pins: ±1 == ±8 g

    /// Feed `secs` of a constant reading with drift correction on; return the
    /// published (canonical) orientation.
    fn step(state: &mut NodeState, gyro: [f32; 3], accel: [f32; 3], secs: f32) -> glam::Quat {
        let mut p: HashMap<String, Value> = HashMap::new();
        p.insert("orient_drift".into(), json!(0.5));
        let (dev, coll) = (BusMap::default(), BusMap::default());
        let inputs: Vec<Option<Signal>> = [None, None]
            .into_iter()
            .chain(gyro.iter().chain(accel.iter()).map(|v| Some(Signal::Float(*v))))
            .collect();
        let dt = 1.0 / 250.0;
        let mut out = Vec::new();
        for _ in 0..(secs / dt) as usize {
            out = compute_gyro_3dof(&inputs, state, &p, &dev, &coll, dt);
        }
        match out[5] {
            Some(Signal::Vec4(v)) => glam::Quat::from_xyzw(v.x, v.y, v.z, v.w),
            ref other => panic!("{other:?}"),
        }
    }

    /// Nose-up angle of a canonical orientation, in degrees: how far the pad's
    /// forward axis points above the horizon.
    fn nose_up_deg(q: glam::Quat) -> f32 {
        (q * glam::Vec3::X).z.asin().to_degrees()
    }

    #[test]
    fn a_still_tilted_pad_stays_where_it_is() {
        // ⛔ The regression: accel went into the model frame through the gyro's
        // axis map, mirrored about roll, so a pad held still 30° nose-up was
        // pulled to 30° nose-DOWN in about three seconds.
        let mut st = NodeState::default();
        step(&mut st, [0.0; 3], [0.0, 0.0, ONE_G], 2.0); // flat: reference
        let q = step(&mut st, [0.0, 0.03, 0.0], [0.0, 0.0, ONE_G], 0.5); // 60 °/s × 0.5 s
        assert!((nose_up_deg(q) - 30.0).abs() < 0.5, "gyro alone: {}", nose_up_deg(q));
        let (s, c) = 30f32.to_radians().sin_cos();
        let q = step(&mut st, [0.0; 3], [s * ONE_G, 0.0, c * ONE_G], 10.0);
        assert!((nose_up_deg(q) - 30.0).abs() < 0.5, "held still: {}", nose_up_deg(q));
    }

    #[test]
    fn tilt_drift_is_pulled_back_out() {
        // The gyro says the pad pitched 30° and rolled 20°; the accel says it
        // never left flat. Held still, the correction must take the pose back
        // to flat on both axes.
        let mut st = NodeState::default();
        step(&mut st, [0.0; 3], [0.0, 0.0, ONE_G], 2.0);
        step(&mut st, [0.0, 0.03, 0.0], [0.0, 0.0, ONE_G], 0.5);
        step(&mut st, [0.02, 0.0, 0.0], [0.0, 0.0, ONE_G], 0.5);
        let q = step(&mut st, [0.0; 3], [0.0, 0.0, ONE_G], 10.0);
        let up = q * glam::Vec3::Z;
        assert!(up.z > 0.9995, "back to flat, got the face pointing {up:?}");
    }
}

#[cfg(test)]
mod orientation_frame_tests {
    use super::compute_gyro_3dof;
    use crate::NodeState;
    use flexinput_core::{Signal, BusMap};
    use serde_json::{json, Value};
    use std::collections::HashMap;

    /// Spin the module for `secs` at a constant normalised gyro reading and
    /// return the orientation it publishes.
    fn run(gx: f32, gy: f32, gz: f32, secs: f32) -> glam::Quat {
        let mut state = NodeState::default();
        let mut params: HashMap<String, Value> = HashMap::new();
        // Off, so the captured gravity reference cannot nudge the pose and
        // blur what this is measuring.
        params.insert("orient_drift".into(), json!(0.0));
        let dev = BusMap::default();
        let coll = BusMap::default();
        let dt = 1.0 / 250.0;
        let mut out = Vec::new();
        let steps = (secs / dt) as usize;
        for _ in 0..steps {
            // Gyro X/Y/Z are direct pin overrides at inputs 2, 3 and 4.
            let inputs = vec![
                None,
                None,
                Some(Signal::Float(gx)),
                Some(Signal::Float(gy)),
                Some(Signal::Float(gz)),
            ];
            out = compute_gyro_3dof(&inputs, &mut state, &params, &dev, &coll, dt);
        }
        match out.get(5).and_then(|s| s.as_ref()) {
            Some(Signal::Vec4(v)) => glam::Quat::from_xyzw(v.x, v.y, v.z, v.w),
            other => panic!("expected an orientation quaternion, got {other:?}"),
        }
    }

    /// The internal integration frame, recovered from what was published.
    fn as_rendered(q: glam::Quat) -> glam::Quat {
        flexinput_core::frames::canonical_to_viewer_quat(q)
    }

    #[test]
    fn the_published_orientation_is_canonical_not_the_render_frame() {
        // ⛔ The regression. This module integrates in the renderer's frame,
        // and used to publish that frame straight onto the pin — so the
        // renderer, which converts canonical to render frame, converted an
        // already-converted quaternion. The axes came out permuted: yaw drew
        // as pitch, pitch as roll, roll as yaw.
        //
        // A pure yaw must therefore arrive on the pin as a rotation about
        // canonical +z (up), NOT about the renderer's +y.
        let q = run(0.0, 0.0, 0.25, 0.5);
        let (axis, angle) = q.to_axis_angle();
        assert!(angle > 0.05, "the pad should have turned somewhere: {angle}");
        let up = axis.dot(glam::Vec3::Z).abs();
        assert!(up > 0.99, "a yaw must be about canonical up, got axis {axis:?}");
    }

    #[test]
    fn a_roll_and_a_pitch_land_on_their_own_canonical_axes() {
        // ❗ Guards the whole 3-cycle, not just one third of it: the bug moved
        // every axis, so testing yaw alone would pass on a map that still had
        // roll and pitch swapped.
        let (roll_axis, _) = run(0.25, 0.0, 0.0, 0.5).to_axis_angle();
        assert!(
            roll_axis.dot(glam::Vec3::X).abs() > 0.99,
            "a roll must be about canonical forward, got {roll_axis:?}"
        );
        let (pitch_axis, _) = run(0.0, 0.25, 0.0, 0.5).to_axis_angle();
        assert!(
            pitch_axis.dot(glam::Vec3::Y).abs() > 0.99,
            "a pitch must be about canonical side, got {pitch_axis:?}"
        );
    }

    #[test]
    fn rendering_the_published_quaternion_reproduces_the_integration_frame() {
        // The round trip the renderer performs: what it draws must be a
        // rotation about the renderer's up axis for a yaw, which is the
        // property that was broken in the other direction before.
        let (axis, _) = as_rendered(run(0.0, 0.0, 0.25, 0.5)).to_axis_angle();
        assert!(
            axis.dot(glam::Vec3::Y).abs() > 0.99,
            "a yaw must render about the viewer's up axis, got {axis:?}"
        );
    }
}

#[cfg(test)]
mod aim_tests {
    use super::{compute_gyro_3dof, pose_angles, GYRO_AIM_OUT, GYRO_VIEW_OUT};
    use crate::NodeState;
    use flexinput_core::{BusMap, Signal};
    use glam::{Quat, Vec3};
    use serde_json::{json, Value};
    use std::collections::HashMap;

    const DT: f32 = 1.0 / 1000.0;
    // Turns about the pad's own canonical (forward, left, up) axes, + the way named.
    const ROLL_RIGHT: Vec3 = Vec3::X;
    const NOSE_UP: Vec3 = Vec3::NEG_Y;
    const YAW_RIGHT: Vec3 = Vec3::NEG_Z;

    /// A pad turned through the world for real: its true pose (canonical body
    /// to world) and the gyro and accel it reads on the way, consistent with
    /// each other as a real pad's are.
    struct Pad {
        q: Quat,
        st: NodeState,
        p: HashMap<String, Value>,
        out: Vec<Option<Signal>>,
        /// What a real pad's sensors get wrong, none by default: added to the
        /// accelerometer (canonical, in g), and the gyro's axes turned against the
        /// accelerometer's (canonical).
        accel_bias: Vec3,
        gyro_skew: Quat,
    }

    impl Pad {
        fn new(params: &[(&str, Value)]) -> Self {
            let mut p: HashMap<String, Value> = HashMap::new();
            p.insert("family".into(), json!("pointer"));
            for (k, v) in params {
                p.insert((*k).into(), v.clone());
            }
            let mut pad = Pad {
                q: Quat::IDENTITY,
                st: NodeState::default(),
                p,
                out: Vec::new(),
                accel_bias: Vec3::ZERO,
                gyro_skew: Quat::IDENTITY,
            };
            pad.hold_still(0.5);
            pad
        }

        fn tick(&mut self, w_body_dps: Vec3, reset: bool) {
            // Gyro: (forward, right, down), ±1 = 2000 °/s. Accel: up, (forward, left, up).
            let w_body_dps = self.gyro_skew * w_body_dps;
            let g = Vec3::new(w_body_dps.x, -w_body_dps.y, -w_body_dps.z) / 2000.0;
            let up = self.q.inverse() * Vec3::Z + self.accel_bias;
            let f = |v: f32| Some(Signal::Float(v));
            let inputs = vec![None, Some(Signal::Bool(reset)), f(g.x), f(g.y), f(g.z), f(up.x), f(up.y), f(up.z)];
            self.out = compute_gyro_3dof(&inputs, &mut self.st, &self.p, &BusMap::default(), &BusMap::default(), DT);
        }

        fn hold_still(&mut self, secs: f32) {
            for _ in 0..(secs / DT) as usize {
                self.tick(Vec3::ZERO, false);
            }
        }

        /// Turn `deg` about the pad's own axis `axis` at `dps`.
        fn turn(&mut self, axis: Vec3, deg: f32, dps: f32) {
            let n = ((deg.abs() / dps) / DT).ceil().max(1.0) as usize;
            let step = deg / n as f32;
            for _ in 0..n {
                self.q = (self.q * Quat::from_axis_angle(axis, step.to_radians())).normalize();
                self.tick(axis * (step / DT), false);
            }
        }

        /// Turn `deg` right about the world's up at `dps`.
        fn turn_world(&mut self, deg: f32, dps: f32) {
            let axis = self.q.inverse() * Vec3::NEG_Z;
            self.turn(axis, deg, dps);
        }

        fn set(&mut self, k: &str, v: Value) {
            self.p.insert(k.into(), v);
            self.tick(Vec3::ZERO, false);
        }

        fn aim(&self) -> (f32, f32) {
            match self.out[GYRO_AIM_OUT] {
                Some(Signal::Vec4(v)) => (v.x, v.y),
                ref other => panic!("aim: {other:?}"),
            }
        }

        /// The pad as the output has it (`GYRO_VIEW_OUT`), canonical.
        fn view(&self) -> Quat {
            match self.out[GYRO_VIEW_OUT] {
                Some(Signal::Vec4(v)) => Quat::from_xyzw(v.x, v.y, v.z, v.w),
                ref other => panic!("view: {other:?}"),
            }
        }

        /// Where the pad's nose really points: (right, up) in degrees.
        fn nose(&self) -> (f32, f32) {
            let a = pose_angles(self.q);
            (a.x, a.y)
        }
    }

    fn near(a: (f32, f32), b: (f32, f32), tol: f32) -> bool {
        (a.0 - b.0).abs() < tol && (a.1 - b.1).abs() < tol
    }

    // Just connected and lying flat, a pad's gyro reads a turn it isn't making until
    // its calibration settles: the camera follows the gyro, gravity holds the pad
    // level, and the pointer ends up off the pad's nose. Auto re-center, once the pad
    // has lain still a while, puts the camera back on the nose and the heading back
    // to forward, as a Reset would - without sending the move.
    #[test]
    fn orient_auto_recenter_puts_the_pointer_back_on_the_pad() {
        for recenter in [false, true] {
            let mut pad = Pad::new(&[("axis", json!("orient")), ("orient_auto_recenter", json!(recenter))]);
            // Two seconds of a phantom 6°/s nose-up and 4°/s right, the pad flat.
            for _ in 0..(2.0 / DT) as usize {
                pad.tick(NOSE_UP * 6.0 + YAW_RIGHT * 4.0, false);
            }
            let drifted = pad.aim();
            assert!(drifted.1.abs() > 3.0, "the camera followed the phantom turn: {drifted:?}");
            // Calibrated now: still.
            let mut sent = (0.0f32, 0.0f32);
            for _ in 0..(8.0 / DT) as usize {
                pad.tick(Vec3::ZERO, false);
                if let (Some(Signal::Float(x)), Some(Signal::Float(y))) = (&pad.out[1], &pad.out[2]) {
                    sent = (sent.0 + x.abs(), sent.1 + y.abs());
                }
            }
            let rested = pad.aim();
            if recenter {
                assert!(near(rested, (0.0, 0.0), 1.0), "re-centred, the pointer is still off the pad: {rested:?}");
                assert!(sent.0 < 1e-6 && sent.1 < 1e-6, "re-centring sent a move: {sent:?}");
            } else {
                assert!(near(rested, drifted, 0.5), "without it, nothing moves the pointer: {drifted:?} -> {rested:?}");
            }
        }
    }

    // Yawed while tipped up, the nose sweeps a slanted circle - and Pitch+Yaw's
    // output goes straight across: the aim adds up what the outputs say, not
    // where the nose is.
    #[test]
    fn the_aim_adds_up_the_outputs_not_the_nose() {
        let mut pad = Pad::new(&[("axis", json!("pitch_yaw"))]);
        pad.turn(NOSE_UP, 40.0, 200.0);
        pad.turn(YAW_RIGHT, 30.0, 200.0);
        assert!(near(pad.aim(), (30.0, 40.0), 0.5), "{:?}", pad.aim());
        assert!(!near(pad.nose(), (30.0, 40.0), 3.0), "the nose went elsewhere: {:?}", pad.nose());
    }

    // Roll right 90°, nose up 90° about the pad's own axis, roll back: the pad
    // lies flat turned 90° right. Pitch+Yaw read the middle turn as pitch - the
    // camera looks at the ceiling. World reads it as the turn about gravity it
    // was, and ends where the pad points. Neither is a fault: two added-up
    // numbers can't follow every path of a 3D turn, and this is how each mode
    // splits one.
    #[test]
    fn the_same_turns_end_where_each_mode_puts_them() {
        let tumble = |axis: &str| {
            let mut pad = Pad::new(&[("axis", json!(axis))]);
            pad.turn(ROLL_RIGHT, 90.0, 300.0);
            pad.turn(NOSE_UP, 90.0, 300.0);
            pad.turn(ROLL_RIGHT, -90.0, 300.0);
            assert!(near(pad.nose(), (90.0, 0.0), 0.5), "the pad: {:?}", pad.nose());
            pad.aim()
        };
        let py = tumble("pitch_yaw");
        assert!(near(py, (0.0, 90.0), 1.0), "Pitch+Yaw: {py:?}");
        let world = tumble("world");
        assert!(near(world, (90.0, 0.0), 1.5), "World: {world:?}");
    }

    // Tip the nose up 60° in a tenth of a second and turn at once: World's
    // "down" is carried by the gyro, so the turn reads whole and stays out of
    // Y. Gravity low-passed alone lagged three seconds behind, and read the
    // turn at about half.
    #[test]
    fn player_and_world_keep_up_with_a_quick_tilt() {
        for axis in ["player", "world"] {
            let mut pad = Pad::new(&[("axis", json!(axis))]);
            pad.turn(NOSE_UP, 60.0, 600.0);
            pad.turn_world(45.0, 180.0);
            assert!(near(pad.aim(), (45.0, 60.0), 1.5), "{axis}: {:?}", pad.aim());
        }
    }

    // With the inverts on, a computed pose turns with them and so does the
    // output: the aim added up and the aim started afresh from the pose agree.
    #[test]
    fn the_aim_and_where_it_starts_agree_with_the_inverts() {
        for (iy, ip) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut pad = Pad::new(&[("axis", json!("pitch_yaw")), ("inv_yaw", json!(iy)), ("inv_pitch", json!(ip))]);
            pad.turn(YAW_RIGHT, 30.0, 200.0);
            pad.turn(NOSE_UP, 20.0, 200.0);
            let added = pad.aim();
            let want = (if iy { -30.0 } else { 30.0 }, if ip { -20.0 } else { 20.0 });
            assert!(near(added, want, 0.5), "inv {iy}/{ip}: {added:?}");
            // A change of mode starts it over at the nose; back again, the same.
            pad.set("axis", json!("world"));
            pad.set("axis", json!("pitch_yaw"));
            assert!(near(pad.aim(), want, 0.5), "inv {iy}/{ip}: started at {:?}", pad.aim());
        }
    }

    // A Reset eases the pose back to neutral, and the aim rides it home.
    #[test]
    fn a_reset_starts_the_aim_over() {
        let mut pad = Pad::new(&[("axis", json!("pitch_yaw"))]);
        pad.turn(ROLL_RIGHT, 90.0, 300.0);
        pad.turn(NOSE_UP, 90.0, 300.0);
        assert!(!near(pad.aim(), (0.0, 0.0), 10.0));
        pad.tick(Vec3::ZERO, true);
        pad.hold_still(0.4);
        assert!(near(pad.aim(), (0.0, 0.0), 0.01), "{:?}", pad.aim());
        // And from there it adds up again.
        pad.turn(YAW_RIGHT, 20.0, 200.0);
        assert!(near(pad.aim(), (20.0, 0.0), 0.5), "{:?}", pad.aim());
    }

    // Orient keeps the output on the nose whatever path the pad takes, short of
    // pointing straight up or down: rolled, pitched about its own tilted axes,
    // yawed, back and forth - and ends where the nose points. Pitch+Yaw, on the
    // same path, doesn't.
    #[test]
    fn orient_ends_where_the_nose_points_whatever_the_path() {
        let path = |pad: &mut Pad| {
            pad.turn(ROLL_RIGHT, 70.0, 250.0);
            pad.turn(NOSE_UP, 40.0, 250.0);
            pad.turn(YAW_RIGHT, 40.0, 250.0);
            pad.turn(ROLL_RIGHT, -120.0, 250.0);
            pad.turn(NOSE_UP, -25.0, 250.0);
            pad.turn(YAW_RIGHT, -70.0, 250.0);
            pad.turn(ROLL_RIGHT, 50.0, 250.0);
        };
        let mut pad = Pad::new(&[("axis", json!("orient"))]);
        path(&mut pad);
        let nose = pad.nose();
        assert!(near(pad.aim(), nose, 1.0), "Orient {:?}, the nose {nose:?}", pad.aim());
        let mut py = Pad::new(&[("axis", json!("pitch_yaw"))]);
        path(&mut py);
        assert!(!near(py.aim(), nose, 5.0), "Pitch+Yaw went elsewhere: {:?}", py.aim());
    }

    // The nose 60° up, twisted a quarter turn about itself: it points where it
    // did. World turns the camera by the twist's share about gravity (sin 60°),
    // Player - JSM's PLAYER_TURN, relaxed - by all of it; Orient not at all.
    #[test]
    fn a_twist_about_a_raised_nose_turns_world_and_player_but_not_orient() {
        let twist = |axis: &str| {
            let mut pad = Pad::new(&[("axis", json!(axis))]);
            pad.turn(NOSE_UP, 60.0, 300.0);
            let before = pad.aim().0;
            pad.turn(ROLL_RIGHT, 90.0, 300.0);
            (pad.aim().0 - before).abs()
        };
        assert!(twist("orient") < 1.0, "Orient: {}", twist("orient"));
        assert!((twist("world") - 90.0 * 60f32.to_radians().sin()).abs() < 1.5, "World: {}", twist("world"));
        assert!((twist("player") - 90.0).abs() < 1.5, "Player: {}", twist("player"));
    }

    // Over the top - the nose up past straight up and down the far side - Orient
    // carries on pitching, upside down with the pad, as a game with no cap on its
    // pitch does: the heading doesn't turn round. The aim is still where the nose
    // points, and the preview's pad is the pad as it is.
    #[test]
    fn orient_carries_on_over_the_top() {
        let mut pad = Pad::new(&[("axis", json!("orient"))]);
        pad.turn(YAW_RIGHT, 30.0, 200.0);
        pad.turn(NOSE_UP, 130.0, 300.0);
        pad.turn(ROLL_RIGHT, 25.0, 200.0);
        let (x, y) = pad.aim();
        assert!((x - 30.0).abs() < 1.0 && (y - 130.0).abs() < 1.0, "the heading turned round: {x}, {y}");
        let dir = Vec3::new(x.to_radians().cos() * y.to_radians().cos(), -x.to_radians().sin() * y.to_radians().cos(), y.to_radians().sin());
        let nose = pad.q * Vec3::X;
        assert!(dir.angle_between(nose).to_degrees() < 1.0, "aim {x}, {y} vs the nose {nose:?}");
        let off = pad.view().angle_between(pad.q).to_degrees();
        assert!(off < 1.0, "the preview's pad is {off}° off the real one");
    }

    // Over the top, the pad's up is still up: turned right about its own up and
    // pitched up about its own right, the outputs go right and up - not mirrored -
    // and the aim stays on the nose.
    #[test]
    fn orient_keeps_the_pads_up_over_the_top() {
        let mut pad = Pad::new(&[("axis", json!("orient"))]);
        pad.turn(NOSE_UP, 150.0, 300.0);
        let xy = |pad: &Pad| match (&pad.out[1], &pad.out[2]) {
            (Some(Signal::Float(x)), Some(Signal::Float(y))) => (*x, *y),
            other => panic!("{other:?}"),
        };
        pad.turn(YAW_RIGHT, 20.0, 200.0);
        let (x, y) = xy(&pad);
        assert!(x > 0.0 && y.abs() < x * 0.2, "turned right about its own up: {x}, {y}");
        pad.turn(NOSE_UP, 10.0, 200.0);
        let (x, y) = xy(&pad);
        assert!(y > 0.0 && x.abs() < y * 0.2, "pitched up about its own right: {x}, {y}");
        let (ax, ay) = pad.aim();
        let dir = Vec3::new(ax.to_radians().cos() * ay.to_radians().cos(), -ax.to_radians().sin() * ay.to_radians().cos(), ay.to_radians().sin());
        let nose = pad.q * Vec3::X;
        assert!(dir.angle_between(nose).to_degrees() < 1.5, "aim {ax}, {ay} vs the nose {nose:?}");
    }

    // Over the top, then rolled about the nose until level again and turned back to
    // face forward - the tumble that left the camera upside down against a level pad
    // (x +178, y +178 at neutral) and the outputs mirrored. The camera keeps the pad's
    // up through the twist: level and forward again, a turn right reads right.
    #[test]
    fn orient_keeps_the_pads_up_through_a_twist() {
        let mut pad = Pad::new(&[("axis", json!("orient"))]);
        pad.turn(NOSE_UP, 150.0, 300.0);
        pad.turn(ROLL_RIGHT, 180.0, 300.0);
        pad.turn(NOSE_UP, -30.0, 300.0);
        pad.turn(YAW_RIGHT, 180.0, 300.0);
        // The pad's nose: wherever the tumble left it, level and with its up up.
        let up = pad.q * Vec3::Z;
        assert!(up.z > 0.9, "the pad came back the right way up: {up:?}");
        pad.turn(YAW_RIGHT, 20.0, 200.0);
        let x = match &pad.out[1] {
            Some(Signal::Float(x)) => *x,
            other => panic!("{other:?}"),
        };
        assert!(x > 0.0, "a turn right about the pad's own up read {x}");
        let (ax, ay) = pad.aim();
        assert!(ay.abs() < 90.0, "the camera is upside down against a level pad: {ax}, {ay}");
    }

    // Round a whole loop and back to where it started, the heading never jumps, and
    // the aim comes home with the pad.
    #[test]
    fn orient_loops() {
        let mut pad = Pad::new(&[("axis", json!("orient"))]);
        let mut worst = 0.0_f32;
        let mut last = pad.aim().0;
        for _ in 0..360 {
            pad.turn(NOSE_UP, 1.0, 360.0);
            let x = pad.aim().0;
            worst = worst.max((x - last).abs());
            last = x;
        }
        assert!(worst < 0.5, "the heading jumped {worst}° somewhere round the loop");
        assert!(near(pad.aim(), (0.0, 0.0), 1.0), "{:?}", pad.aim());
    }

    // The gyro moves the camera and gravity never does: a pad whose gyro drifted, held
    // still while gravity brings its orientation back, keeps its pointer where it is.
    #[test]
    fn orient_ignores_the_drift_fix() {
        let mut pad = Pad::new(&[("axis", json!("orient"))]);
        // Half a second of a roll the gyro reports and the pad isn't making, nose up
        // a little so the tilt it leaves shows in the pitch.
        pad.turn(NOSE_UP, 20.0, 200.0);
        for _ in 0..500 {
            pad.tick(ROLL_RIGHT * 40.0, false);
        }
        let at = pad.aim();
        pad.hold_still(3.0);
        assert!(near(pad.aim(), at, 0.01), "a still pad's pointer crept: {at:?} -> {:?}", pad.aim());
    }

    // Up past the heading's cap (75°), turned a quarter about the world's up and back down:
    // as the pad comes down the camera gives back what the cap held back, and ends where
    // the nose is - it came down 6° short from 80° up and 12° from 85°. Only while the pad
    // moves: held still after, the pointer stays put.
    #[test]
    fn orient_gives_back_what_the_cap_held_near_the_top() {
        for up in [80.0_f32, 85.0] {
            let mut pad = Pad::new(&[("axis", json!("orient"))]);
            pad.turn(NOSE_UP, up, 200.0);
            pad.turn_world(90.0, 200.0);
            pad.turn(NOSE_UP, -up, 200.0);
            let nose = pad.nose();
            assert!(near(pad.aim(), nose, 1.5), "{up}° up: the aim {:?}, the nose {nose:?}", pad.aim());
            let at = pad.aim();
            pad.hold_still(2.0);
            assert!(near(pad.aim(), at, 0.01), "a still pad's pointer moved: {at:?} -> {:?}", pad.aim());
        }
    }

    // Orient's Edge turn goes by the nose as the preview shows it - up from level, right of
    // the heading a Reset made zero - not by the Orientation output, which measures from the
    // pad as it was at the Reset. Reset pointing 20° down, the nose 20° up is inside a 30°
    // zone (the output had it 40° past the Reset, and turned), 40° up past it.
    #[test]
    fn orient_edge_turn_goes_by_the_nose_as_shown() {
        let mut pad = Pad::new(&[
            ("axis", json!("orient")),
            ("edge_mode", json!("ramp")),
            ("edge_zone_deg", json!(30.0)),
            ("edge_blend_deg", json!(0.0)),
            ("edge_ramp_deg", json!(10.0)),
        ]);
        let y = |pad: &Pad| match pad.out[2] {
            Some(Signal::Float(v)) => v,
            ref other => panic!("y: {other:?}"),
        };
        pad.turn(NOSE_UP, -20.0, 200.0);
        pad.tick(Vec3::ZERO, true);
        pad.hold_still(0.3);
        pad.turn(NOSE_UP, 40.0, 200.0);
        pad.hold_still(0.3);
        assert!(y(&pad).abs() < 1e-4, "20° up is inside the zone: {}", y(&pad));
        pad.turn(NOSE_UP, 20.0, 200.0);
        pad.hold_still(0.3);
        assert!(y(&pad) > 1e-3, "40° up is past it: {}", y(&pad));
    }

    // Held upright like a wheel and turned, Pitch+Yaw steering knows where
    // centre is from gravity: a gyro that drifts while the wheel is held still
    // doesn't carry the wheel off. (The old centring pulled an upright wheel to
    // full lock.) Without centring, the drift is the wheel's.
    #[test]
    fn steering_centres_on_gravity_where_it_can_see_the_wheel() {
        let wheel = |ctr: f32| {
            let mut pad = Pad::new(&[
                ("family", json!("steering")),
                ("axis", json!("pitch_yaw")),
                ("recenter_strength", json!(ctr)),
            ]);
            pad.turn(NOSE_UP, 90.0, 300.0);
            pad.tick(Vec3::ZERO, true); // centre here
            pad.hold_still(0.5);
            assert!(pad.aim().0.abs() < 1.0, "upright at centre, it stays there: {:?}", pad.aim());
            pad.turn(YAW_RIGHT, 30.0, 200.0);
            // Four seconds of a gyro reading 5°/s of turn the wheel isn't making.
            for _ in 0..4000 {
                pad.tick(YAW_RIGHT * 5.0, false);
            }
            pad.aim().0
        };
        let held = wheel(2.0);
        assert!((held - 30.0).abs() < 8.0, "centred on gravity: {held}");
        let drifted = wheel(0.0);
        assert!((drifted - 30.0).abs() > 15.0, "without, it drifts: {drifted}");
    }

    // The preview's pad has its nose on the aim and is rolled as it is held: a
    // pad rolled 30° right and yawed about its own tilted axes in Pitch+Yaw
    // points where the output went, not where the nose did, still rolled.
    #[test]
    fn the_preview_pad_points_where_the_output_does_rolled_as_held() {
        let mut pad = Pad::new(&[("axis", json!("pitch_yaw"))]);
        pad.turn(ROLL_RIGHT, 30.0, 200.0);
        pad.turn(YAW_RIGHT, 40.0, 200.0);
        let (x, y) = pad.aim();
        let v = pad.view();
        let nose = v * Vec3::X;
        let want = Vec3::new(x.to_radians().cos() * y.to_radians().cos(), -x.to_radians().sin() * y.to_radians().cos(), y.to_radians().sin());
        assert!(nose.angle_between(want).to_degrees() < 0.5, "{nose:?} vs the aim {x}, {y}");
        // Rolled as the pad really is about its nose, against the vertical:
        // down in the pad's (forward, right, down) is (-up.F, up.L, up.U).
        let roll = |q: Quat| {
            let up = q.inverse() * Vec3::Z;
            up.y.atan2(up.z).to_degrees()
        };
        let (shown, real) = (roll(v), roll(pad.q));
        assert!(real > 15.0, "the pad is rolled: {real}");
        assert!((shown - real).abs() < 1.0, "shown {shown}, really {real}");
    }

    // A turn about the world's up that the sensors don't quite agree on - an accelerometer
    // reading 0.02 g to the side, or a gyro set a degree askew against it - reads as part
    // pitch: a half turn took Orient's pointer 2.6-3.6° down (up, turned the other way),
    // as it takes World's, while the pad's own orientation, held to gravity, stayed level.
    // Orient's pitch now comes back onto it as the pad turns; still after, it stays put.
    #[test]
    fn orient_keeps_level_through_a_turn_the_sensors_disagree_on() {
        let skewed = [
            (Vec3::new(0.0, 0.02, 0.0), Quat::IDENTITY),
            (Vec3::ZERO, Quat::from_rotation_x(1f32.to_radians())),
        ];
        for (bias, skew) in skewed {
            for deg in [180.0_f32, -180.0] {
                let turned = |axis: &str| {
                    let mut pad = Pad::new(&[("axis", json!(axis))]);
                    pad.accel_bias = bias;
                    pad.gyro_skew = skew;
                    pad.turn(NOSE_UP, 25.0, 200.0);
                    pad.hold_still(1.5);
                    let y0 = pad.aim().1;
                    pad.turn_world(deg, 90.0);
                    (pad.aim().1 - y0, pad)
                };
                let (world, _) = turned("world");
                assert!(world.abs() > 2.0, "World keeps JSM's maths and the leak: {world} ({bias:?}, {deg}°)");
                let (orient, mut pad) = turned("orient");
                assert!(orient.abs() < 0.3, "Orient's pitch moved {orient}° on a {deg}° turn ({bias:?})");
                let at = pad.aim();
                pad.hold_still(3.0);
                assert!(near(pad.aim(), at, 0.01), "a still pad's pointer moved: {at:?} -> {:?}", pad.aim());
            }
        }
    }

    // Panic mode's reset (`request_gyro_reset` sets `reset_request`) is a Reset:
    // a wound-up wheel comes back to centre and the pose to neutral.
    #[test]
    fn a_reset_asked_for_from_outside_recentres_the_wheel() {
        let mut pad = Pad::new(&[("family", json!("steering")), ("axis", json!("world"))]);
        pad.turn(YAW_RIGHT, 70.0, 200.0);
        assert!(pad.aim().0 > 60.0, "{:?}", pad.aim());
        pad.st.reset_request = true;
        pad.hold_still(0.4);
        assert!(pad.aim().0.abs() < 1e-3, "{:?}", pad.aim());
        assert!(!pad.st.reset_request, "taken");
    }

    // Steering's Reset lands on zero too: its ease took a fixed share of what
    // was left each tick and stopped with over a third of it still there.
    #[test]
    fn a_steering_reset_lands_on_zero() {
        let mut pad = Pad::new(&[("family", json!("steering")), ("axis", json!("pitch_yaw"))]);
        pad.turn(YAW_RIGHT, 40.0, 200.0);
        let x = |pad: &Pad| match pad.out[1] {
            Some(Signal::Float(v)) => v,
            ref other => panic!("x: {other:?}"),
        };
        assert!(x(&pad).abs() > 0.01, "{}", x(&pad));
        pad.tick(Vec3::ZERO, true);
        pad.hold_still(0.4);
        assert!(x(&pad).abs() < 1e-6, "{}", x(&pad));
    }
}
