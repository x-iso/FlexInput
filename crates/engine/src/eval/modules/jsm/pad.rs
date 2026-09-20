//! Virtual pad output: the stick modes and gyro outputs that drive a pad
//! downstream instead of the mouse (JSM's `processGyroStick` and the three stick
//! modes beside it).
//!
//! Everything here works in the unit JSM works in: degrees per second of in-game
//! camera turn. A stick push and a gyro rate are both converted into that rate,
//! added together, and only then converted back into a stick position — which is
//! what lets a gyro and a stick share one virtual stick without either one
//! clipping the other. `VIRTUAL_STICK_CALIBRATION` is the rate a fully deflected
//! stick produces in the game being played, so it is the scale for the whole
//! conversion, and a config whose calibration is wrong feels wrong in exactly one
//! direction: too fast or too slow.
//!
//! The `*_UNDEADZONE_*` settings run the *game's* deadzone backwards. A game that
//! ignores the first 20% of stick travel would swallow a gentle gyro nudge
//! entirely, so telling this module about that 20% lets it start its output there
//! instead of at zero. `*_UNPOWER` does the same for a game's response curve.

use std::f32::consts::{PI, TAU};

use glam::Vec2;

use super::analog::{Analog, StickMode};

/// Where the gyro (`GYRO_OUTPUT`) or the flick stick (`FLICK_STICK_OUTPUT`) sends
/// what it produces.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Dest {
    /// The mouse, as in phase 3 (JSM's default).
    #[default]
    Mouse,
    LeftStick,
    RightStick,
    /// `PS_MOTION`: the pad's own motion sensors, forwarded rather than aimed
    /// with. Nothing to do here — see the note in `parse.rs`.
    PsMotion,
}

impl Dest {
    /// Which virtual stick this one feeds, if it feeds one.
    pub fn side(self) -> Option<usize> {
        match self {
            Dest::LeftStick => Some(0),
            Dest::RightStick => Some(1),
            Dest::Mouse | Dest::PsMotion => None,
        }
    }
}

/// How one virtual stick is shaped on the way out.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct OutStick {
    pub undeadzone_inner: f32,
    pub undeadzone_outer: f32,
    /// `0` means "no curve", which is also what `1` means.
    pub unpower: f32,
    pub virtual_scale: f32,
}

impl Default for OutStick {
    fn default() -> Self {
        // JSM's defaults: no undeadzone at either end, no curve, full scale.
        OutStick { undeadzone_inner: 0.0, undeadzone_outer: 0.0, unpower: 0.0, virtual_scale: 1.0 }
    }
}

/// Everything the pad side of a config is configured with.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Settings {
    /// `VIRTUAL_STICK_CALIBRATION`: deg/s of camera turn at full stick.
    pub calibration: f32,
    /// Indexed by virtual stick, 0 left and 1 right.
    pub out: [OutStick; 2],
    pub gyro_dest: Dest,
    pub flick_dest: Dest,
    /// `ANGLE_TO_AXIS_DEADZONE_INNER` / `_OUTER`, in degrees off the axis.
    pub angle_dz_inner: f32,
    pub angle_dz_outer: f32,
    /// `WIND_STICK_RANGE`: degrees of winding for a full push.
    pub wind_range: f32,
    pub wind_power: f32,
    /// `UNWIND_RATE`: degrees per second the winding unwinds at a centred stick.
    pub unwind_rate: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            calibration: 360.0,
            out: [OutStick::default(); 2],
            gyro_dest: Dest::default(),
            flick_dest: Dest::default(),
            angle_dz_inner: 0.0,
            angle_dz_outer: 10.0,
            wind_range: 900.0,
            wind_power: 1.0,
            unwind_rate: 1800.0,
        }
    }
}

/// What a virtual pad should read this tick. `None` for a stick nothing here
/// drives, so the pad's own stick passes through untouched.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Out {
    pub sticks: [Option<Vec2>; 2],
}

/// The pad side's running state.
#[derive(Default)]
pub struct Pad {
    /// Degrees wound up on each virtual stick by `*_WIND_X`.
    winding: [f32; 2],
}

impl Pad {
    /// Work out what the virtual sticks should read. `gyro` and `flick` are camera
    /// rates in deg/s — what phase 3 computed before it turned them into mouse
    /// pixels — and are already zero when their destination is the mouse.
    pub fn tick(
        &mut self,
        s: &Settings,
        dt: f32,
        analog: &Analog,
        stick_power: f32,
        gyro: Vec2,
        flick: Vec2,
    ) -> Out {
        // What camera rate each virtual stick is owed before any stick is read.
        let mut owed = [Vec2::ZERO; 2];
        if let Some(side) = s.gyro_dest.side() {
            owed[side] += gyro;
        }
        if let Some(side) = s.flick_dest.side() {
            owed[side] += flick;
        }

        let mut out = Out::default();
        // A stick in a virtual-stick mode carries whatever rate is owed to the
        // side it drives — that is how a gyro and a stick end up sharing one
        // stick rather than fighting over it. JSM tracks this with a
        // `processed_gyro_stick` flag; the same thing said plainly is: the rate is
        // owed to a side, and the side pays it out once.
        let mut carried = [false; 2];
        for phys in 0..2 {
            let st = analog.stick_out(phys);
            let Some(side) = st.mode.pad_side() else { continue };
            // JSM reads the *previous* tick's direction here with the current
            // tick's length (it deadzones `lastX`/`lastY` in place and hands those
            // to `processGyroStick`). It is one tick of lag on direction only,
            // imperceptible at any rate we run, and reproducing it keeps a config
            // feeling the way it does in JSM — so it is deliberate, not an
            // oversight. The two modes that measure a *change* in angle use both
            // ticks and need no such choice.
            let prev = Vec2::new(st.prev_x, st.prev_y);
            let now = Vec2::new(st.x, st.y);
            out.sticks[side] = match st.mode {
                StickMode::AngleToAxis(_, is_x) => {
                    angle_to_axis(s, side, is_x, now, st.len, stick_power)
                }
                StickMode::Wind(_) => self.wind(s, side, now, prev, st.len, dt),
                // `VirtualStick`, and the only other thing `pad_side` answers for.
                _ => {
                    carried[side] = true;
                    virtual_stick(s, side, prev, st.len, owed[side])
                }
            };
        }

        // A rate nobody carried drives its stick on its own.
        for side in 0..2 {
            let owes = s.gyro_dest.side() == Some(side) || s.flick_dest.side() == Some(side);
            if owes && !carried[side] {
                out.sticks[side] = virtual_stick(s, side, Vec2::ZERO, 0.0, owed[side]);
            }
        }
        out
    }

    /// `LEFT_WIND_X` / `RIGHT_WIND_X`: turning the stick winds a value up like a
    /// spring, and letting the stick go lets it unwind. Pushed hard and turned,
    /// it winds fast; released, it runs back to zero at `UNWIND_RATE`.
    fn wind(
        &mut self,
        s: &Settings,
        side: usize,
        now: Vec2,
        prev: Vec2,
        len: f32,
        dt: f32,
    ) -> Option<Vec2> {
        let wound = &mut self.winding[side];
        // Both ticks have to be off-centre for there to be an angle between them.
        if len > 0.0 && prev.x != 0.0 && prev.y != 0.0 {
            let angle = (-now.x).atan2(now.y);
            let was = (-prev.x).atan2(prev.y);
            // The shortest way round, so passing 180° doesn't unwind everything.
            let change = (angle - was + PI).rem_euclid(TAU) - PI;
            *wound -= change * len * 180.0 / PI;
        }
        if len < 1.0 {
            let unwind = s.unwind_rate * (1.0 - len) * dt;
            if wound.abs() <= unwind {
                *wound = 0.0;
            } else {
                *wound -= unwind * wound.signum();
            }
        }

        let sign = if *wound < 0.0 { -1.0 } else { 1.0 };
        let power = if s.wind_power == 0.0 { 1.0 } else { s.wind_power };
        // Half the range each way, so `WIND_STICK_RANGE` is the full sweep.
        let reach = if s.wind_range > 0.0 {
            (wound.abs() / s.wind_range * 2.0).powf(power).min(1.0)
        } else {
            // A range of zero would divide by it: any winding at all is full.
            if *wound == 0.0 { 0.0 } else { 1.0 }
        };
        Some(Vec2::new(sign * undeadzone(reach, &s.out[side])?, 0.0))
    }
}

/// JSM's `processGyroStick`: a stick push and a camera rate, combined in deg/s
/// and converted back into a stick position.
fn virtual_stick(s: &Settings, side: usize, push: Vec2, len: f32, rate: Vec2) -> Option<Vec2> {
    let o = &s.out[side];
    let livezone = 1.0 - o.undeadzone_outer - o.undeadzone_inner;
    if livezone <= 0.0 || s.calibration <= 0.0 {
        // No band to put anything in, or no idea how fast full stick is.
        return None;
    }
    let unpower = if o.unpower == 0.0 { 1.0 } else { o.unpower };

    // The push, as the camera rate it is asking for.
    let mut want = Vec2::ZERO;
    if len > 0.0 {
        let reach = ((len - o.undeadzone_inner) / livezone).clamp(0.0, 1.0);
        let speed = reach.powf(unpower) * s.calibration * o.virtual_scale;
        if speed > 0.0 {
            // JSM counts a stick's y up and a camera's y down.
            want = Vec2::new(push.x / len * speed, -push.y / len * speed);
        }
    }
    want += rate;

    let target = want.length();
    let mut strength = (target / s.calibration).min(1.0);
    strength = strength.powf(1.0 / unpower);
    let mut stick = Vec2::ZERO;
    // Below a hundredth of full there is nothing worth sending, and dividing by
    // `target` at zero would not end well.
    if strength > 0.01 {
        stick = want / target * (o.undeadzone_inner + strength * livezone);
    }

    // JSM's own calibration aid, kept: with the stick inside the undeadzone and
    // no rate at all, park the virtual stick exactly on the edge of the game's
    // deadzone, so a player can find that edge by watching the game react. At the
    // default undeadzone of zero this is just a centred stick.
    if len <= o.undeadzone_inner && target == 0.0 {
        return Some(Vec2::new(o.undeadzone_inner, 0.0));
    }
    // Back to the bus's y-up.
    Some(Vec2::new(stick.x, -stick.y))
}

/// `LEFT_ANGLE_TO_X` and its three siblings: how far the stick is turned away from
/// one axis becomes a push along that axis, the way a wheel steers — so a stick
/// held forward and rolled left reads as "left" without any sideways travel.
fn angle_to_axis(
    s: &Settings,
    side: usize,
    is_x: bool,
    now: Vec2,
    len: f32,
    stick_power: f32,
) -> Option<Vec2> {
    // The angle off the axis this mode drives, so pointing straight along the
    // other axis is 90° and reads as full.
    let angle = if is_x {
        now.x.atan2(now.y.abs())
    } else {
        now.y.atan2(now.x.abs())
    };
    let degrees = angle.to_degrees().abs();
    let sign = if angle < 0.0 { -1.0 } else { 1.0 };

    let span = 90.0 - s.angle_dz_outer - s.angle_dz_inner;
    // Deadzones that meet or cross leave no span to scale across; anything past
    // the inner one is then full deflection. (JSM divides by the span regardless,
    // which at zero is a division by zero — this says what that was reaching for.)
    let mut reach = if span > 0.0 {
        ((degrees - s.angle_dz_inner) / span).clamp(0.0, 1.0)
    } else if degrees > s.angle_dz_inner {
        1.0
    } else {
        0.0
    };
    // How far the stick is pushed still counts, through `STICK_POWER`.
    reach *= len.powf(stick_power);

    let value = sign * undeadzone(reach, &s.out[side])?;
    Some(if is_x { Vec2::new(value, 0.0) } else { Vec2::new(0.0, value) })
}

/// Put a 0..1 strength into the band of stick travel the game actually responds
/// to, through the unpower curve. `None` when the settings leave no band at all.
fn undeadzone(strength: f32, o: &OutStick) -> Option<f32> {
    let livezone = 1.0 - o.undeadzone_outer - o.undeadzone_inner;
    if livezone <= 0.0 {
        return None;
    }
    let mut v = strength;
    if o.unpower != 0.0 {
        v = v.powf(1.0 / o.unpower);
    }
    // Full deflection stays full — the outer undeadzone is what the game clips
    // anyway, so pulling it in would only cost the top of the range.
    if v < 1.0 {
        v = o.undeadzone_inner + v * livezone;
    }
    Some(v)
}
