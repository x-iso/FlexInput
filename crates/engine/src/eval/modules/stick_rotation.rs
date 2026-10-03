//! Gyro to Stick Rotation: turning the pad turns a stick that is already pushed.
//!
//! The stick picks a direction; the gyro fine-tunes it. While the stick is out
//! past the inner deadzone, the pad's turn rate winds up an OFFSET ANGLE, and the
//! stick goes out rotated by it, its length untouched. Let the stick back inside
//! the deadzone and the offset is dropped: the next push starts from the thumb's
//! own direction again. This is aiming for twin-stick games, where the stick IS
//! the aim direction, so a thumb is coarse and a wrist is fine.
//!
//! Turning right (clockwise, seen from above) turns the stick clockwise.
//!
//! Two modules run this: the AutoMap module `module.stick_rotation` (below), and
//! the JSM Config module's `GYRO_OUTPUT = RIGHT_STICK_ROTATION`, which feeds it
//! the turn rate its own gyro pipeline produced (`GYRO_SPACE`, `GYRO_SENS`,
//! `GYRO_OFF` and the rest all apply there). [`StickRotation`] is the part the
//! two share.
//!
//! ## Stabilising
//!
//! A thumb that can't hold an angle still shakes it back and forth a few times a
//! second. That is filtered out of the stick's DIRECTION (never its length, and
//! never the gyro's offset, which is the precise part) with a one-euro filter: a
//! low-pass whose cutoff opens up with how fast the angle is moving. The speed it
//! opens on is itself low-passed at 1 Hz, and that is what makes it right for
//! tremor: a tremor reverses many times a second, so its average speed is near
//! zero and the filter stays shut, while a deliberate sweep keeps going one way
//! and opens it. (A plain speed threshold gets this backwards — tremor is FAST,
//! and the slow careful aiming is what it would smooth.)

use super::*;
use glam::Vec3;
use std::f32::consts::{PI, TAU};

/// Stable module id.
pub const STICK_ROT_ID: &str = "module.stick_rotation";
/// The stick to rotate: `left_stick` or `right_stick` (the default).
pub const STICK_ROT_STICK_PARAM: &str = "rot_stick";
/// What the turn is measured about: `yaw` (the pad's own vertical, the default)
/// or `world` (gravity, so a pad pitched up turns by rolling).
pub const STICK_ROT_MODE_PARAM: &str = "rot_mode";
/// Degrees of stick rotation per degree the pad turns.
pub const STICK_ROT_SENS_PARAM: &str = "rot_sens";
/// Turn the other way.
pub const STICK_ROT_INVERT_PARAM: &str = "rot_invert";
/// The inner deadzone, a fraction of stick travel 0..1.
pub const STICK_ROT_DEADZONE_PARAM: &str = "rot_deadzone";
/// Stabilising, in milliseconds (0 = off) — see the module docs.
pub const STICK_ROT_SMOOTH_PARAM: &str = "rot_smooth_ms";
/// Relative mode: the offset follows the pad's turning and folds back to the
/// thumb once it stops, rather than holding (absolute, the default).
pub const STICK_ROT_RELATIVE_PARAM: &str = "rot_relative";
/// Relative mode's return time constant, in milliseconds.
pub const STICK_ROT_RETURN_PARAM: &str = "rot_return_ms";

pub const STICK_ROT_DEADZONE_DEFAULT: f32 = 0.2;
pub const STICK_ROT_SENS_DEFAULT: f32 = 1.0;
/// Relative mode's default return time, ms: a flick of the wrist registers
/// nearly whole, and the stick is back on the thumb well inside a second.
pub const STICK_ROT_RETURN_DEFAULT_MS: f32 = 250.0;

/// Where the node's live values sit in its `last_out`, for the body's circle:
/// the stick as it came in, the stick as it goes out, the offset in degrees
/// (positive = clockwise), and whether the offset is applying at all.
pub const STICK_ROT_OUT_RAW: usize = 0;
pub const STICK_ROT_OUT_ROTATED: usize = 1;
pub const STICK_ROT_OUT_OFFSET: usize = 2;
pub const STICK_ROT_OUT_ENGAGED: usize = 3;

/// How fast the stabiliser lets go of a deliberate sweep: its cutoff in Hz rises
/// by this much per radian per second the direction is turning. At heavy
/// stabilising (200 ms, a 0.8 Hz floor) a steady 90°/s sweep opens it to ~4 Hz,
/// a lag of a few degrees; a tremor barely moves it.
const STABILISE_BETA: f32 = 2.0;
/// The cutoff the turning speed is itself filtered at (the one-euro default).
const STABILISE_D_CUTOFF: f32 = 1.0;
/// Gravity settles over this long — the JSM module's figure, for the same reason:
/// follow a change of grip, not a flick.
const GRAVITY_TAU: f32 = 0.5;

/// How one stick is turned — the knobs both modules expose.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RotationSettings {
    /// The inner deadzone, a fraction of stick travel. Inside it the stick passes
    /// through and the offset is dropped. 0 means there is none: the offset is
    /// never dropped and keeps applying, a centred stick included.
    pub deadzone: f32,
    /// The stabiliser's time constant, seconds; 0 for none.
    pub smooth_s: f32,
    /// Relative mode: the offset folds back to the thumb's own direction with
    /// this time constant, seconds. `None` is absolute — the offset holds.
    pub return_s: Option<f32>,
}

impl Default for RotationSettings {
    fn default() -> Self {
        RotationSettings { deadzone: STICK_ROT_DEADZONE_DEFAULT, smooth_s: 0.0, return_s: None }
    }
}

/// One tick's result.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rotated {
    /// The stick to send.
    pub out: Vec2,
    /// The offset applied, in degrees, clockwise positive. Within ±180 in
    /// absolute mode; in relative mode it is at most the pad's turn rate times
    /// the return time, and isn't wrapped (a wrap would send it home the long way).
    pub offset_deg: f32,
    /// Whether the offset applies: the stick is out past the deadzone, or there
    /// is no deadzone.
    pub engaged: bool,
}

/// Below this length a stick's direction is noise — a deadzone-less stick
/// passing through the centre, or resting there — so the stabiliser lets go and
/// picks the direction up afresh, rather than easing round from wherever the
/// thumb last pointed.
const DIRECTION_FLOOR: f32 = 0.02;

/// The shared running state: the offset, and the stabiliser on the stick's
/// direction.
#[derive(Clone, Debug, Default)]
pub struct StickRotation {
    /// Radians, clockwise positive.
    offset: f32,
    /// Whether the stick's direction is being followed (it is out past the
    /// floor), so the stabiliser has a previous direction to go on.
    tracking: bool,
    /// The stick's direction, unwrapped so a stick swept round and round doesn't
    /// jump by a full turn at the bottom, and its reading last tick.
    unwrapped: f32,
    last_raw: f32,
    filter: AngleFilter,
}

impl StickRotation {
    /// Advance one tick. `turn_dps` is how fast the stick should turn, in degrees
    /// per second clockwise — the pad's turn rate with any sensitivity already in
    /// it.
    pub fn tick(&mut self, stick: Vec2, turn_dps: f32, dt: f32, s: &RotationSettings) -> Rotated {
        let len = stick.length();
        // Inside the deadzone: drop everything, pass the stick through untouched.
        if s.deadzone > 0.0 && len <= s.deadzone {
            *self = StickRotation::default();
            return Rotated { out: stick, offset_deg: 0.0, engaged: false };
        }

        // The offset. Absolute: the turn, added up — where the pad points now
        // against where it pointed when the stick went out. Relative: the same
        // sum leaking away with time constant `return_s`, so a quick turn
        // registers almost 1:1, a steady one holds an offset of its rate times
        // the return time, and once the pad stops the stick folds back to the
        // thumb. (Integrated exactly, so the result doesn't depend on the tick.)
        let rate = turn_dps.to_radians();
        self.offset = match s.return_s {
            None => wrap_pi(self.offset + rate * dt),
            Some(tau) if tau > 0.0 => {
                let k = (-dt / tau).exp();
                self.offset * k + rate * tau * (1.0 - k)
            }
            // Folding back instantly: nothing is ever held.
            Some(_) => 0.0,
        };

        if len < DIRECTION_FLOOR {
            // Only without a deadzone: too little stick to have a direction worth
            // stabilising, but the offset still turns what there is. The
            // stabiliser starts again from the next real direction.
            self.filter = AngleFilter::default();
            let out = if len > 0.0 {
                let bearing = stick.x.atan2(stick.y) + self.offset;
                Vec2::new(bearing.sin(), bearing.cos()) * len
            } else {
                stick
            };
            return Rotated { out, offset_deg: self.offset.to_degrees(), engaged: true };
        }

        // The stick's direction as a compass bearing: 0 up, clockwise positive.
        let raw = stick.x.atan2(stick.y);
        if self.tracking {
            self.unwrapped += wrap_pi(raw - self.last_raw);
        } else {
            // Just pushed out: start at the thumb's own direction.
            self.tracking = true;
            self.unwrapped = raw;
        }
        self.last_raw = raw;
        // Many turns of a stick swept round and round would cost the angle its
        // precision, so take whole turns back off — the filter with it.
        if self.unwrapped.abs() > 64.0 * TAU {
            let turns = (self.unwrapped / TAU).trunc() * TAU;
            self.unwrapped -= turns;
            self.filter.shift(-turns);
        }

        let direction = if s.smooth_s > 0.0 {
            let min_cutoff = 1.0 / (TAU * s.smooth_s);
            self.filter.filter(self.unwrapped, dt, min_cutoff)
        } else {
            self.filter = AngleFilter::default();
            self.unwrapped
        };

        let bearing = direction + self.offset;
        Rotated {
            out: Vec2::new(bearing.sin(), bearing.cos()) * len,
            offset_deg: self.offset.to_degrees(),
            engaged: true,
        }
    }
}

/// Into (-π, π].
fn wrap_pi(a: f32) -> f32 {
    let w = (a + PI).rem_euclid(TAU) - PI;
    if w <= -PI { w + TAU } else { w }
}

/// A one-euro filter on an angle (see the module docs), its own rather than the
/// JSM module's because it has to be shiftable by whole turns.
#[derive(Clone, Copy, Debug, Default)]
struct AngleFilter {
    value: f32,
    speed: f32,
    prev: f32,
    started: bool,
}

impl AngleFilter {
    fn filter(&mut self, x: f32, dt: f32, min_cutoff: f32) -> f32 {
        if !self.started {
            *self = AngleFilter { value: x, speed: 0.0, prev: x, started: true };
            return x;
        }
        let dt = dt.max(1e-6);
        let speed = (x - self.prev) / dt;
        self.prev = x;
        self.speed += alpha(STABILISE_D_CUTOFF, dt) * (speed - self.speed);
        let cutoff = min_cutoff + STABILISE_BETA * self.speed.abs();
        self.value += alpha(cutoff, dt) * (x - self.value);
        self.value
    }

    fn shift(&mut self, by: f32) {
        self.value += by;
        self.prev += by;
    }
}

/// A first-order low-pass's smoothing factor for one tick of `dt`.
fn alpha(cutoff: f32, dt: f32) -> f32 {
    let tau = 1.0 / (TAU * cutoff.max(1e-6));
    1.0 / (1.0 + tau / dt)
}

/// Which way is down, from the accelerometer, low-passed.
#[derive(Clone, Copy, Debug, Default)]
pub struct GravityLp {
    /// The accelerometer's direction, in its own basis.
    up: Vec3,
    settled: bool,
}

impl GravityLp {
    /// Advance with this tick's accelerometer (`None` when the pad has none) and
    /// say which way is DOWN, in the GYRO's basis — the two differ on our bus
    /// (accel is `(F, -R, U)`, gyro `(F, R, -U)`), so a dot product of the gyro
    /// with this is a true turn rate about gravity. `None` while nothing is known.
    pub fn tick(&mut self, accel: Option<Vec3>, dt: f32) -> Option<Vec3> {
        if let Some(a) = accel {
            let len = a.length();
            if len > 0.01 {
                let n = a / len;
                if self.settled {
                    self.up += (1.0 - (-dt / GRAVITY_TAU).exp()) * (n - self.up);
                } else {
                    // Start on the first reading, not by easing up from zero.
                    self.up = n;
                    self.settled = true;
                }
            }
        }
        if !self.settled || self.up.length() < 0.01 {
            return None;
        }
        let up = self.up.normalize();
        // Down, the accel's up negated, with the basis flip diag(1, -1, -1).
        Some(Vec3::new(-up.x, up.y, up.z))
    }
}

/// The pad's turn rate in deg/s, clockwise seen from above positive. `gyro` is
/// the bus's (roll, pitch, yaw) in deg/s; `down` what [`GravityLp`] says, used
/// only by `world` — which falls back to the pad's own yaw while it is unknown.
pub fn turn_rate(gyro: Vec3, down: Option<Vec3>, world: bool) -> f32 {
    match (world, down) {
        (true, Some(d)) => gyro.dot(d),
        _ => gyro.z,
    }
}

/// The AutoMap module's running state.
#[derive(Clone, Debug, Default)]
pub struct StickRotationState {
    rot: StickRotation,
    gravity: GravityLp,
}

/// Evaluate one Gyro to Stick Rotation node: hand the bus on under
/// `collector:{uid}` with the picked stick rotated. Returns the live values for
/// the body's circle (see `STICK_ROT_OUT_*`).
pub(crate) fn eval_stick_rotation_node(
    snap: &NodeSnap,
    uid: usize,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
    state: &mut HashMap<usize, NodeState>,
    dt: f32,
) -> Vec<Option<Signal>> {
    republish_bus_as_collector(snap, uid, dev_sigs, collector_sigs);
    let key = format!("collector:{uid}");
    let p = &snap.params;
    let num = |k: &str, d: f32| p.get(k).and_then(|v| v.as_f64()).map(|v| v as f32).unwrap_or(d);
    let stick_pin = match p.get(STICK_ROT_STICK_PARAM).and_then(|v| v.as_str()) {
        Some("left_stick") => "left_stick",
        _ => "right_stick",
    };
    let world = p.get(STICK_ROT_MODE_PARAM).and_then(|v| v.as_str()) == Some("world");
    let invert = p.get(STICK_ROT_INVERT_PARAM).and_then(|v| v.as_bool()).unwrap_or(false);
    let sens = num(STICK_ROT_SENS_PARAM, STICK_ROT_SENS_DEFAULT);
    let relative = p.get(STICK_ROT_RELATIVE_PARAM).and_then(|v| v.as_bool()).unwrap_or(false);
    let settings = RotationSettings {
        deadzone: num(STICK_ROT_DEADZONE_PARAM, STICK_ROT_DEADZONE_DEFAULT).clamp(0.0, 1.0),
        smooth_s: num(STICK_ROT_SMOOTH_PARAM, 0.0).max(0.0) / 1000.0,
        return_s: relative.then(|| num(STICK_ROT_RETURN_PARAM, STICK_ROT_RETURN_DEFAULT_MS).max(0.0) / 1000.0),
    };

    let bus = |pin: &str| collector_sigs.get(&(key.clone(), pin.to_string())).copied();
    let f = |pin: &str| bus(pin).map(|s| s.as_float());
    let stick = match bus(stick_pin) {
        Some(Signal::Vec2(v)) => v,
        _ => Vec2::new(
            f(&format!("{stick_pin}_x")).unwrap_or(0.0),
            f(&format!("{stick_pin}_y")).unwrap_or(0.0),
        ),
    };
    let gyro = Vec3::new(
        f("gyro_x").unwrap_or(0.0),
        f("gyro_y").unwrap_or(0.0),
        f("gyro_z").unwrap_or(0.0),
    ) * GYRO_REF_DPS;
    let accel = ["accel_x", "accel_y", "accel_z"].iter().any(|p| bus(p).is_some()).then(|| {
        Vec3::new(f("accel_x").unwrap_or(0.0), f("accel_y").unwrap_or(0.0), f("accel_z").unwrap_or(0.0))
    });

    let st = state.entry(uid).or_default().stick_rotation.get_or_insert_with(Default::default);
    let down = st.gravity.tick(accel, dt);
    let sign = if invert { -1.0 } else { 1.0 };
    let turn = turn_rate(gyro, down, world) * sens * sign;
    let r = st.rot.tick(stick, turn, dt, &settings);

    if r.engaged {
        // All three forms of the stick, or a reader taking the one left alone
        // gets the thumb's direction instead of the aim.
        collector_sigs.insert((key.clone(), stick_pin.to_string()), Signal::Vec2(r.out));
        collector_sigs.insert((key.clone(), format!("{stick_pin}_x")), Signal::Float(r.out.x));
        collector_sigs.insert((key.clone(), format!("{stick_pin}_y")), Signal::Float(r.out.y));
    }
    vec![
        Some(Signal::Vec2(stick)),
        Some(Signal::Vec2(r.out)),
        Some(Signal::Float(r.offset_deg)),
        Some(Signal::Bool(r.engaged)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 0.001;

    /// Absolute mode with this deadzone and stabilising time.
    fn abs(deadzone: f32, smooth_s: f32) -> RotationSettings {
        RotationSettings { deadzone, smooth_s, return_s: None }
    }

    fn bearing(v: Vec2) -> f32 {
        v.x.atan2(v.y).to_degrees()
    }

    fn run(rot: &mut StickRotation, stick: Vec2, turn: f32, secs: f32, dz: f32, smooth: f32) -> Rotated {
        let mut r = Rotated::default();
        for _ in 0..(secs / DT).round() as usize {
            r = rot.tick(stick, turn, DT, &abs(dz, smooth));
        }
        r
    }

    #[test]
    fn a_right_turn_turns_the_stick_clockwise_and_keeps_its_length() {
        let mut rot = StickRotation::default();
        // Pushed up at 0.8, the pad turning right at 90°/s for a tenth of a second.
        let r = run(&mut rot, Vec2::new(0.0, 0.8), 90.0, 0.1, 0.2, 0.0);
        assert!(r.engaged);
        assert!((r.offset_deg - 9.0).abs() < 0.05, "{}", r.offset_deg);
        assert!((bearing(r.out) - 9.0).abs() < 0.05, "clockwise from up is +x: {:?}", r.out);
        assert!(r.out.x > 0.0);
        assert!((r.out.length() - 0.8).abs() < 1e-4);
    }

    #[test]
    fn a_left_turn_turns_it_anticlockwise() {
        let mut rot = StickRotation::default();
        let r = run(&mut rot, Vec2::new(1.0, 0.0), -45.0, 0.2, 0.2, 0.0);
        // Pointing right (90°), turned 9° anticlockwise.
        assert!((bearing(r.out) - 81.0).abs() < 0.05, "{:?}", r.out);
    }

    #[test]
    fn the_offset_holds_while_the_pad_is_still_and_follows_the_thumb() {
        let mut rot = StickRotation::default();
        run(&mut rot, Vec2::new(0.0, 1.0), 100.0, 0.1, 0.2, 0.0);
        // The pad stops; the thumb moves to the right. The 10° stays on top.
        let r = run(&mut rot, Vec2::new(1.0, 0.0), 0.0, 0.05, 0.2, 0.0);
        assert!((bearing(r.out) - 100.0).abs() < 0.05, "{:?}", r.out);
    }

    #[test]
    fn inside_the_deadzone_the_stick_passes_untouched_and_the_offset_is_dropped() {
        let mut rot = StickRotation::default();
        run(&mut rot, Vec2::new(0.0, 1.0), 100.0, 0.1, 0.3, 0.0);
        let inside = Vec2::new(0.1, 0.2);
        let r = rot.tick(inside, 100.0, DT, &abs(0.3, 0.0));
        assert!(!r.engaged);
        assert_eq!(r.out, inside, "passed through as it came");
        assert_eq!(r.offset_deg, 0.0);
        // Pushed out again: from the thumb's own direction, no leftover offset.
        let r = rot.tick(Vec2::new(0.0, 1.0), 0.0, DT, &abs(0.3, 0.0));
        assert!(r.engaged);
        assert!(bearing(r.out).abs() < 1e-3, "{:?}", r.out);
    }

    #[test]
    fn the_gyro_does_nothing_while_the_stick_is_inside_the_deadzone() {
        let mut rot = StickRotation::default();
        // Turning all the while the stick sits in the deadzone…
        run(&mut rot, Vec2::new(0.0, 0.1), 200.0, 0.5, 0.2, 0.0);
        // …accumulates nothing for the push that follows.
        let r = rot.tick(Vec2::new(0.0, 1.0), 0.0, DT, &abs(0.2, 0.0));
        assert!(bearing(r.out).abs() < 1e-3, "{:?}", r.out);
    }

    #[test]
    fn with_no_deadzone_the_offset_is_never_dropped() {
        let mut rot = StickRotation::default();
        // Turning with the stick centred still turns the aim it will have…
        let r = run(&mut rot, Vec2::ZERO, 100.0, 0.1, 0.0, 0.0);
        assert!(r.engaged);
        assert_eq!(r.out, Vec2::ZERO, "a centred stick stays centred");
        assert!((r.offset_deg - 10.0).abs() < 0.05, "{}", r.offset_deg);
        // …so the push that follows comes out already turned.
        let r = rot.tick(Vec2::new(0.0, 1.0), 0.0, DT, &abs(0.0, 0.0));
        assert!((bearing(r.out) - 10.0).abs() < 0.05, "{:?}", r.out);
        // Back through the centre and out the other way: still on.
        run(&mut rot, Vec2::ZERO, 0.0, 0.05, 0.0, 0.0);
        let r = rot.tick(Vec2::new(0.0, -1.0), 0.0, DT, &abs(0.0, 0.0));
        assert!((bearing(r.out) - (-170.0)).abs() < 0.05, "{:?}", r.out);
        // A barely-pushed stick is turned too, just not stabilised.
        let r = rot.tick(Vec2::new(0.0, 0.01), 0.0, DT, &abs(0.0, 0.2));
        assert!((bearing(r.out) - 10.0).abs() < 0.05, "{:?}", r.out);
        assert!((r.out.length() - 0.01).abs() < 1e-6);
    }

    #[test]
    fn with_no_deadzone_a_stick_through_the_centre_is_not_eased_round() {
        let mut rot = StickRotation::default();
        let up = Vec2::new(0.0, 1.0);
        run(&mut rot, up, 0.0, 1.0, 0.0, 0.2);
        // Flicked through the centre to straight down: the stabiliser starts
        // afresh rather than sweeping 180° round from up.
        rot.tick(Vec2::ZERO, 0.0, DT, &abs(0.0, 0.2));
        let r = rot.tick(Vec2::new(0.0, -1.0), 0.0, DT, &abs(0.0, 0.2));
        assert!(r.out.y < -0.999, "{:?}", r.out);
    }

    fn rel(return_s: f32) -> RotationSettings {
        RotationSettings { deadzone: 0.2, smooth_s: 0.0, return_s: Some(return_s) }
    }

    fn run_rel(rot: &mut StickRotation, turn: f32, secs: f32, return_s: f32) -> Rotated {
        let mut r = Rotated::default();
        for _ in 0..(secs / DT).round() as usize {
            r = rot.tick(Vec2::new(0.0, 1.0), turn, DT, &rel(return_s));
        }
        r
    }

    #[test]
    fn relative_registers_a_quick_turn_nearly_whole() {
        // 10° in 50 ms against a 250 ms return: most of it shows.
        let mut rot = StickRotation::default();
        let r = run_rel(&mut rot, 200.0, 0.05, 0.25);
        assert!(r.offset_deg > 9.0 && r.offset_deg < 10.0, "{}", r.offset_deg);
        assert!((bearing(r.out) - r.offset_deg).abs() < 1e-3);
    }

    #[test]
    fn relative_holds_a_steady_turn_at_its_rate_times_the_return_time() {
        let mut rot = StickRotation::default();
        let r = run_rel(&mut rot, 40.0, 3.0, 0.25);
        assert!((r.offset_deg - 10.0).abs() < 0.05, "40°/s × 0.25 s: {}", r.offset_deg);
    }

    #[test]
    fn relative_folds_back_to_the_thumb_once_the_pad_stops() {
        let mut rot = StickRotation::default();
        run_rel(&mut rot, 200.0, 0.1, 0.25);
        let r = run_rel(&mut rot, 0.0, 0.25, 0.25);
        let peak = 20.0 * (1.0 - (-0.1f32 / 0.25).exp()) / (0.1 / 0.25);
        assert!((r.offset_deg - peak / std::f32::consts::E).abs() < 0.1, "a third left after one return time: {}", r.offset_deg);
        let r = run_rel(&mut rot, 0.0, 2.0, 0.25);
        assert!(r.offset_deg.abs() < 0.01 && bearing(r.out).abs() < 0.01, "home: {:?}", r);
    }

    #[test]
    fn relative_goes_home_the_short_way_even_past_half_a_turn() {
        // 1000°/s × 0.25 s holds 250° — past 180, and must not be wrapped to
        // -110, or it would fold back through the bottom.
        let mut rot = StickRotation::default();
        let r = run_rel(&mut rot, 1000.0, 3.0, 0.25);
        assert!((r.offset_deg - 250.0).abs() < 0.5, "{}", r.offset_deg);
        let mut last = r.offset_deg;
        for _ in 0..50 {
            let r = run_rel(&mut rot, 0.0, 0.02, 0.25);
            assert!(r.offset_deg < last && r.offset_deg >= 0.0, "unwinding the way it came: {} then {}", last, r.offset_deg);
            last = r.offset_deg;
        }
    }

    #[test]
    fn relative_with_no_return_time_holds_nothing() {
        let mut rot = StickRotation::default();
        // Even a turn already built up in absolute mode is let go of at once.
        run(&mut rot, Vec2::new(0.0, 1.0), 100.0, 0.1, 0.2, 0.0);
        let r = run_rel(&mut rot, 500.0, 0.2, 0.0);
        assert_eq!(r.offset_deg, 0.0);
        assert!(bearing(r.out).abs() < 1e-4);
    }

    #[test]
    fn the_offset_wraps_rather_than_growing() {
        let mut rot = StickRotation::default();
        // 200°/s for two seconds is 400° — shown as 40°.
        let r = run(&mut rot, Vec2::new(0.0, 1.0), 200.0, 2.0, 0.2, 0.0);
        assert!((r.offset_deg - 40.0).abs() < 0.2, "{}", r.offset_deg);
        assert!((bearing(r.out) - 40.0).abs() < 0.2);
    }

    /// The stick's bearing under a tremor of `amp` degrees at `hz`, held on 0°.
    fn tremor_spread(smooth: f32, amp: f32, hz: f32) -> f32 {
        let mut rot = StickRotation::default();
        let mut worst: f32 = 0.0;
        for i in 0..2000 {
            let t = i as f32 * DT;
            let a = (amp * (TAU * hz * t).sin()).to_radians();
            let r = rot.tick(Vec2::new(a.sin(), a.cos()), 0.0, DT, &abs(0.2, smooth));
            // Past the first half second, once the filter has settled.
            if t > 0.5 {
                worst = worst.max(bearing(r.out).abs());
            }
        }
        worst
    }

    #[test]
    fn stabilising_holds_back_a_tremor() {
        let raw = tremor_spread(0.0, 4.0, 8.0);
        let smoothed = tremor_spread(0.15, 4.0, 8.0);
        assert!(raw > 3.9, "unfiltered, the tremor is all there: {raw}");
        assert!(smoothed < raw * 0.25, "filtered to a quarter or less: {smoothed} of {raw}");
    }

    #[test]
    fn stabilising_lets_a_deliberate_sweep_through() {
        let mut rot = StickRotation::default();
        // Sweep from up to right at 90°/s, heavily stabilised.
        let mut r = Rotated::default();
        for i in 0..=1000 {
            let a = (90.0 * i as f32 * DT).to_radians();
            r = rot.tick(Vec2::new(a.sin(), a.cos()), 0.0, DT, &abs(0.2, 0.2));
        }
        let lag = 90.0 - bearing(r.out);
        assert!(lag > 0.0 && lag < 8.0, "a few degrees behind a 90°/s sweep: {lag}");
        // And it arrives once the thumb stops.
        let r = run(&mut rot, Vec2::new(1.0, 0.0), 0.0, 2.0, 0.2, 0.2);
        assert!((bearing(r.out) - 90.0).abs() < 0.1, "{:?}", r.out);
    }

    #[test]
    fn stabilising_is_not_fooled_by_the_bottom_of_the_circle() {
        let mut rot = StickRotation::default();
        // Hold just left of straight down, then just right of it: a 2° move
        // across the ±180° seam, not a 358° one.
        let at = |deg: f32| {
            let a = deg.to_radians();
            Vec2::new(a.sin(), a.cos())
        };
        run(&mut rot, at(179.0), 0.0, 1.0, 0.2, 0.2);
        let r = run(&mut rot, at(-179.0), 0.0, 2.0, 0.2, 0.2);
        assert!((bearing(r.out).abs() - 179.0).abs() < 0.1, "{:?}", r.out);
        // It never swung round through the top on the way.
        let mut rot2 = StickRotation::default();
        run(&mut rot2, at(179.0), 0.0, 1.0, 0.2, 0.2);
        for _ in 0..200 {
            let r = rot2.tick(at(-179.0), 0.0, DT, &abs(0.2, 0.2));
            assert!(r.out.y < -0.99, "stays at the bottom: {:?}", r.out);
        }
    }

    #[test]
    fn a_stick_swept_round_many_times_keeps_its_precision() {
        let mut rot = StickRotation::default();
        // 200 turns at 10 turns a second, stabilised.
        let mut r = Rotated::default();
        for i in 0..20_000 {
            let a = (3600.0 * i as f32 * DT).to_radians();
            r = rot.tick(Vec2::new(a.sin(), a.cos()), 0.0, DT, &abs(0.2, 0.05));
        }
        assert!(r.out.length() > 0.999);
        let r = run(&mut rot, Vec2::new(0.0, 1.0), 0.0, 1.0, 0.2, 0.05);
        assert!(bearing(r.out).abs() < 0.05, "{:?}", r.out);
    }

    #[test]
    fn yaw_is_the_pads_own_vertical_and_world_is_about_gravity() {
        let gyro = Vec3::new(30.0, 0.0, 50.0); // rolling and yawing
        // Flat, face up: accel reads +z. Both modes see the yaw.
        let mut g = GravityLp::default();
        let flat = g.tick(Some(Vec3::new(0.0, 0.0, 1.0)), DT);
        assert!((turn_rate(gyro, flat, false) - 50.0).abs() < 1e-3);
        assert!((turn_rate(gyro, flat, true) - 50.0).abs() < 1e-3);
        // Nose up 90°: accel reads +x. A right turn about the sky is now a roll
        // the negative way about the pad's forward (which points at the sky).
        let mut g = GravityLp::default();
        let nose_up = g.tick(Some(Vec3::new(1.0, 0.0, 0.0)), DT);
        assert!((turn_rate(gyro, nose_up, false) - 50.0).abs() < 1e-3, "yaw ignores the hold");
        assert!((turn_rate(gyro, nose_up, true) + 30.0).abs() < 1e-3, "world takes the roll");
        assert!((turn_rate(Vec3::new(-40.0, 0.0, 0.0), nose_up, true) - 40.0).abs() < 1e-3);
    }

    #[test]
    fn world_measures_a_tilted_pad_against_gravity_not_its_face() {
        // Pitched 45° nose up: down in the gyro's basis is (-s, 0, c). A right
        // turn about the world's vertical, ω = 60°/s, shows on the pad as
        // (-s, 0, c)·60 — and must read back as 60.
        let (s, c) = (45f32.to_radians().sin(), 45f32.to_radians().cos());
        let mut g = GravityLp::default();
        let down = g.tick(Some(Vec3::new(s, 0.0, c)), DT);
        let gyro = Vec3::new(-s, 0.0, c) * 60.0;
        assert!((turn_rate(gyro, down, true) - 60.0).abs() < 1e-3);
        // A pure pitch, about the pad's side, is no turn at all.
        assert!(turn_rate(Vec3::new(0.0, 80.0, 0.0), down, true).abs() < 1e-3);
        // Right grip dropped: the accel reads +y, which is the pad's LEFT (up),
        // so down is along the pad's right — the gyro's +y, the pitch axis.
        let mut g = GravityLp::default();
        let side = g.tick(Some(Vec3::new(0.0, 1.0, 0.0)), DT);
        assert!((turn_rate(Vec3::new(0.0, 25.0, 0.0), side, true) - 25.0).abs() < 1e-3);
    }

    // ── the node, through the bus ────────────────────────────────────────────

    const PAD: &str = "pad";

    fn node(params: &[(&str, Value)]) -> NodeSnap {
        let mut p: HashMap<String, Value> = params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        p.insert("_automap_device_id".into(), Value::String(PAD.into()));
        NodeSnap {
            node_uid: 7,
            module_id: STICK_ROT_ID.into(),
            params: p,
            n_outputs: 1,
            input_sources: vec![None],
            device_id: None,
            output_pin_ids: Vec::new(),
            aux_f32_override: None,
            sink_target: None,
            inline_subgraph: None,
        }
    }

    /// The pad's bus: a stick on `stick_pin` (all three forms), a yaw rate, the
    /// pad flat, and a button to see pass through.
    fn pad(stick_pin: &str, stick: Vec2, yaw_dps: f32) -> HashMap<(String, String), Signal> {
        let mut d = HashMap::new();
        let mut put = |pin: &str, s: Signal| { d.insert((PAD.to_string(), pin.to_string()), s); };
        for side in ["left_stick", "right_stick"] {
            let v = if side == stick_pin { stick } else { Vec2::ZERO };
            put(side, Signal::Vec2(v));
            put(&format!("{side}_x"), Signal::Float(v.x));
            put(&format!("{side}_y"), Signal::Float(v.y));
        }
        put("gyro_x", Signal::Float(0.0));
        put("gyro_y", Signal::Float(0.0));
        put("gyro_z", Signal::Float(yaw_dps / GYRO_REF_DPS));
        put("accel_x", Signal::Float(0.0));
        put("accel_y", Signal::Float(0.0));
        put("accel_z", Signal::Float(1.0));
        put("btn_south", Signal::Bool(true));
        d
    }

    /// Run the node for `secs`, a fresh bus every tick as the engine does.
    fn run_node(snap: &NodeSnap, dev: &HashMap<(String, String), Signal>, secs: f32)
        -> (HashMap<(String, String), Signal>, Vec<Option<Signal>>)
    {
        let mut state = HashMap::new();
        let mut last = (HashMap::new(), Vec::new());
        for _ in 0..(secs / DT).round() as usize {
            let mut cs = HashMap::new();
            let out = eval_stick_rotation_node(snap, snap.node_uid, dev, &mut cs, &mut state, DT);
            last = (cs, out);
        }
        last
    }

    fn published(cs: &HashMap<(String, String), Signal>, pin: &str) -> Signal {
        *cs.get(&("collector:7".to_string(), pin.to_string())).unwrap_or_else(|| panic!("{pin} published"))
    }

    #[test]
    fn the_node_turns_the_right_stick_in_all_three_forms_by_default() {
        let (cs, out) = run_node(&node(&[]), &pad("right_stick", Vec2::new(0.0, 0.9), 100.0), 0.1);
        let Signal::Vec2(v) = published(&cs, "right_stick") else { panic!("a Vec2") };
        assert!((bearing(v) - 10.0).abs() < 0.1, "{v:?}");
        assert!((v.length() - 0.9).abs() < 1e-4);
        assert_eq!(published(&cs, "right_stick_x").as_float(), v.x, "the axes agree with the Vec2");
        assert_eq!(published(&cs, "right_stick_y").as_float(), v.y);
        // Everything else passes through.
        assert!(published(&cs, "btn_south").as_bool());
        // And the body sees both sticks and the offset.
        assert_eq!(out[STICK_ROT_OUT_RAW], Some(Signal::Vec2(Vec2::new(0.0, 0.9))));
        assert_eq!(out[STICK_ROT_OUT_ROTATED], Some(Signal::Vec2(v)));
        let Some(Signal::Float(off)) = out[STICK_ROT_OUT_OFFSET] else { panic!("an offset") };
        assert!((off - 10.0).abs() < 0.1);
        assert_eq!(out[STICK_ROT_OUT_ENGAGED], Some(Signal::Bool(true)));
    }

    #[test]
    fn the_node_turns_only_the_stick_it_was_given() {
        let snap = node(&[(STICK_ROT_STICK_PARAM, Value::String("left_stick".into()))]);
        let (cs, _) = run_node(&snap, &pad("left_stick", Vec2::new(0.0, 1.0), 100.0), 0.1);
        let Signal::Vec2(v) = published(&cs, "left_stick") else { panic!() };
        assert!((bearing(v) - 10.0).abs() < 0.1, "{v:?}");
        assert_eq!(published(&cs, "right_stick"), Signal::Vec2(Vec2::ZERO));
        // And a right stick pushed while the left one is picked is left alone.
        let (cs, _) = run_node(&snap, &pad("right_stick", Vec2::new(0.0, 1.0), 100.0), 0.1);
        assert_eq!(published(&cs, "right_stick"), Signal::Vec2(Vec2::new(0.0, 1.0)));
    }

    #[test]
    fn the_node_applies_sensitivity_and_invert() {
        let snap = node(&[
            (STICK_ROT_SENS_PARAM, serde_json::json!(0.5)),
            (STICK_ROT_INVERT_PARAM, Value::Bool(true)),
        ]);
        let (cs, _) = run_node(&snap, &pad("right_stick", Vec2::new(0.0, 1.0), 100.0), 0.1);
        let Signal::Vec2(v) = published(&cs, "right_stick") else { panic!() };
        assert!((bearing(v) + 5.0).abs() < 0.1, "half as far, the other way: {v:?}");
    }

    #[test]
    fn the_node_in_relative_mode_holds_rate_times_return_time() {
        let snap = node(&[
            (STICK_ROT_RELATIVE_PARAM, Value::Bool(true)),
            (STICK_ROT_RETURN_PARAM, serde_json::json!(100.0)),
        ]);
        // 50°/s held for a second against a 100 ms return: 5°, not 50°.
        let (cs, _) = run_node(&snap, &pad("right_stick", Vec2::new(0.0, 1.0), 50.0), 1.0);
        let Signal::Vec2(v) = published(&cs, "right_stick") else { panic!() };
        assert!((bearing(v) - 5.0).abs() < 0.05, "{v:?}");
    }

    #[test]
    fn the_node_with_no_deadzone_keeps_turning_a_centred_stick() {
        let snap = node(&[(STICK_ROT_DEADZONE_PARAM, serde_json::json!(0.0))]);
        let (cs, out) = run_node(&snap, &pad("right_stick", Vec2::ZERO, 100.0), 0.1);
        assert_eq!(published(&cs, "right_stick"), Signal::Vec2(Vec2::ZERO));
        assert_eq!(out[STICK_ROT_OUT_ENGAGED], Some(Signal::Bool(true)));
        let Some(Signal::Float(off)) = out[STICK_ROT_OUT_OFFSET] else { panic!() };
        assert!((off - 10.0).abs() < 0.1, "{off}");
    }

    #[test]
    fn the_node_leaves_a_stick_in_its_deadzone_exactly_as_it_came() {
        let snap = node(&[(STICK_ROT_DEADZONE_PARAM, serde_json::json!(0.5))]);
        let (cs, out) = run_node(&snap, &pad("right_stick", Vec2::new(0.3, 0.3), 300.0), 0.1);
        assert_eq!(published(&cs, "right_stick"), Signal::Vec2(Vec2::new(0.3, 0.3)));
        assert_eq!(published(&cs, "right_stick_x"), Signal::Float(0.3));
        assert_eq!(out[STICK_ROT_OUT_ENGAGED], Some(Signal::Bool(false)));
    }

    #[test]
    fn the_node_in_world_mode_turns_with_a_pitched_up_pads_roll() {
        let snap = node(&[(STICK_ROT_MODE_PARAM, Value::String("world".into()))]);
        let mut dev = pad("right_stick", Vec2::new(0.0, 1.0), 0.0);
        // Nose up: the accel reads +x. Rolling the negative way about the pad's
        // forward (now pointing at the sky) is a right turn.
        dev.insert((PAD.into(), "accel_x".into()), Signal::Float(1.0));
        dev.insert((PAD.into(), "accel_z".into()), Signal::Float(0.0));
        dev.insert((PAD.into(), "gyro_x".into()), Signal::Float(-100.0 / GYRO_REF_DPS));
        let (cs, _) = run_node(&snap, &dev, 0.1);
        let Signal::Vec2(v) = published(&cs, "right_stick") else { panic!() };
        assert!((bearing(v) - 10.0).abs() < 0.1, "{v:?}");
        // The same roll in yaw mode does nothing.
        let (cs, _) = run_node(&node(&[]), &dev, 0.1);
        assert_eq!(published(&cs, "right_stick"), Signal::Vec2(Vec2::new(0.0, 1.0)));
    }

    #[test]
    fn world_without_an_accelerometer_falls_back_to_yaw() {
        let mut g = GravityLp::default();
        let down = g.tick(None, DT);
        assert!(down.is_none());
        assert!((turn_rate(Vec3::new(30.0, 0.0, 50.0), down, true) - 50.0).abs() < 1e-3);
    }
}
