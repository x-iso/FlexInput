//! Aiming: the gyro as a mouse, a stick as a mouse, and flick stick.
//!
//! JSM's own pipeline (`src/main.cpp` around the IMU callback, and
//! `JoyShock::handleFlickStick`), in order: put the gyro in the space the config
//! asked for, smooth it, apply the cutoff, let the gyro buttons block it, run the
//! trackball, apply the sensitivity ramp, then add whatever the sticks are
//! contributing and turn the lot into one mouse displacement for this tick.
//!
//! ## Which way round the axes go
//!
//! JSM names its gyro axes in its own frame and ours are not the same, so the two
//! that matter are pinned to what JSM's *defaults* have to do — its default
//! config aims right when you turn right, and up when you tilt up:
//!
//! * `MOUSE_X_FROM_GYRO_AXIS = Y` does `gyroX -= inGyroY`, so turning right must
//!   make JSM's Y negative. Our `gyro_z` is positive turning right, so
//!   **JSM Y = -gyro_z**.
//! * `MOUSE_Y_FROM_GYRO_AXIS = X` does `gyroY -= inGyroX` against a screen whose
//!   y counts downward, so tilting up must make JSM's X positive. Our `gyro_y` is
//!   positive tilting up, so **JSM X = +gyro_y**.
//! * Nothing in JSM's defaults uses its Z (roll), so its sign can't be pinned the
//!   same way: **JSM Z = +gyro_x**, and a config that puts Z in a mouse axis mask
//!   may want `GYRO_AXIS_X` / `GYRO_AXIS_Y` inverted. It is the one axis here
//!   that wants checking on a real pad.
//!
//! Our bus counts mouse y upward (the sink negates it), JSM's counts downward, so
//! the vertical half is negated on the way out.

use std::collections::HashSet;

use glam::Vec2;

use super::analog::{Analog, StickMode};
use super::names::{Btn, GyroAction};
use super::pad::{self, Dest};

/// JSM's own buffer sizes for the two smoothers and the trackball.
const GYRO_SAMPLES: usize = 256;
const FLICK_SAMPLES: usize = 256;
const TRACKBALL_SAMPLES: usize = 100;

/// Which gyro axes feed one mouse axis (`MOUSE_X_FROM_GYRO_AXIS`). JSM takes a
/// set, so `X+Y` is a legal value.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct AxisMask {
    pub x: bool,
    pub y: bool,
    pub z: bool,
}

impl AxisMask {
    pub const X: AxisMask = AxisMask { x: true, y: false, z: false };
    pub const Y: AxisMask = AxisMask { x: false, y: true, z: false };
    pub fn none(self) -> bool { !self.x && !self.y && !self.z }
}

/// `FLICK_SNAP_MODE`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SnapMode {
    #[default]
    None,
    /// Quarter turns.
    Four,
    /// Eighth turns.
    Eight,
}

/// What turns the gyro off (or on): `GYRO_OFF` / `GYRO_ON`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GyroButton {
    pub source: GyroSource,
    /// `GYRO_ON = …`: off until the button is held, rather than the other way up.
    pub always_off: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GyroSource {
    Button(Btn),
    LeftStick,
    RightStick,
    /// `GYRO_ON = NONE` — never on.
    Never,
}

/// How much of a rotation at `length` deg/s survives the cutoff, 0..1.
///
/// This is the gyro's deadzone: below `GYRO_CUTOFF_SPEED` a slow drift is thrown
/// away entirely, and a `GYRO_CUTOFF_RECOVERY` above it fades the rotation back
/// in across that band instead of switching it on. It scales the VELOCITY, before
/// the sensitivity ramp reads it — so a config with a cutoff aims at nothing
/// below that speed however high its sensitivity is.
///
/// Shared with the editor's curve preview, which would otherwise draw a config's
/// deadzone as if it weren't there.
pub(crate) fn cutoff_factor(s: &Settings, length: f32) -> f32 {
    if s.cutoff_recovery > s.cutoff_speed {
        ((length - s.cutoff_speed) / (s.cutoff_recovery - s.cutoff_speed)).clamp(0.0, 1.0)
    } else if s.cutoff_speed > 0.0 && length < s.cutoff_speed {
        0.0
    } else {
        1.0
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Settings {
    pub min_sens: (f32, f32),
    pub max_sens: (f32, f32),
    pub min_threshold: f32,
    pub max_threshold: f32,
    /// `GYRO_AXIS_X` / `GYRO_AXIS_Y`: 1.0 or -1.0.
    pub axis_x: f32,
    pub axis_y: f32,
    /// `STICK_AXIS_X` / `STICK_AXIS_Y`: the same inversion for the mouse a stick
    /// in `AIM` drives. Separate from the gyro's, as in JSM — plenty of people
    /// want one inverted and not the other.
    pub stick_axis_x: f32,
    pub stick_axis_y: f32,
    pub mouse_x_from: AxisMask,
    pub mouse_y_from: AxisMask,
    pub cutoff_speed: f32,
    pub cutoff_recovery: f32,
    pub smooth_threshold: f32,
    pub smooth_time: f32,
    pub trackball_decay: f32,
    pub real_world_calibration: f32,
    pub in_game_sens: f32,
    pub gyro_button: Option<GyroButton>,
    pub stick_sens: (f32, f32),
    pub stick_power: f32,
    pub stick_accel_rate: f32,
    pub stick_accel_cap: f32,
    pub flick_time: f32,
    pub flick_time_exponent: f32,
    pub flick_snap: SnapMode,
    pub flick_snap_strength: f32,
    pub flick_deadzone_angle: f32,
    pub rotate_smooth_override: f32,
    /// `MOUSE_RING_RADIUS`, which `MOUSE_AREA` scales its movement by.
    pub mouse_ring_radius: f32,
}

impl Default for Settings {
    fn default() -> Self {
        // JSM's defaults, from its own command registry.
        Settings {
            min_sens: (0.0, 0.0),
            max_sens: (0.0, 0.0),
            min_threshold: 0.0,
            max_threshold: 0.0,
            axis_x: 1.0,
            axis_y: 1.0,
            stick_axis_x: 1.0,
            stick_axis_y: 1.0,
            mouse_x_from: AxisMask::Y,
            mouse_y_from: AxisMask::X,
            cutoff_speed: 0.0,
            cutoff_recovery: 0.0,
            smooth_threshold: 0.0,
            smooth_time: 0.125,
            trackball_decay: 1.0,
            real_world_calibration: 40.0,
            in_game_sens: 1.0,
            gyro_button: None,
            stick_sens: (360.0, 360.0),
            stick_power: 1.0,
            stick_accel_rate: 0.0,
            stick_accel_cap: 1_000_000.0,
            flick_time: 0.1,
            flick_time_exponent: 0.0,
            flick_snap: SnapMode::None,
            flick_snap_strength: 1.0,
            flick_deadzone_angle: 0.0,
            rotate_smooth_override: -1.0,
            mouse_ring_radius: 128.0,
        }
    }
}

/// What one tick of aiming produced.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Aimed {
    /// The mouse displacement for this tick, in the bus's pixels.
    pub mouse: Vec2,
    /// The gyro's camera rate in deg/s, for a virtual stick to carry. Zero unless
    /// `GYRO_OUTPUT` names a stick — the mouse gets it in `mouse` instead.
    pub gyro_dps: Vec2,
    /// The flick stick's camera rate in deg/s, on the same terms.
    pub flick_dps: Vec2,
}

/// The pad's rotation this tick, in degrees per second, on our bus's axes.
#[derive(Clone, Copy, Default, Debug)]
pub struct Gyro {
    pub roll: f32,
    pub pitch: f32,
    pub yaw: f32,
}

/// One stick's flick state.
struct Flick {
    flicking: bool,
    /// The angle this flick is turning through, in radians.
    delta: f32,
    /// How far through the flick's easing we are, 0..1.
    done: f32,
    /// A flick is still being paid out. JSM asks `flick_percent_done < 1`, which
    /// is also true for a stick that has never flicked at all (it starts at
    /// zero) — so the fact of a flick being underway is tracked outright.
    paying_out: bool,
    since: f32,
    samples: Box<[f32; FLICK_SAMPLES]>,
    front: usize,
}

impl Default for Flick {
    fn default() -> Self {
        Flick {
            flicking: false,
            delta: 0.0,
            done: 0.0,
            paying_out: false,
            since: 0.0,
            samples: Box::new([0.0; FLICK_SAMPLES]),
            front: 0,
        }
    }
}

#[derive(Default)]
struct StickAim {
    /// `STICK_ACCELERATION_RATE`'s running multiplier.
    acceleration: f32,
    flick: Flick,
}

/// The aiming state of one JSM Config node.
pub struct Aim {
    t: f32,
    gyro_samples: Box<[(f32, f32); GYRO_SAMPLES]>,
    gyro_front: usize,
    trackball_x: Box<[f32; TRACKBALL_SAMPLES]>,
    trackball_y: Box<[f32; TRACKBALL_SAMPLES]>,
    trackball_ix: usize,
    trackball_iy: usize,
    last_abs_x: f32,
    last_abs_y: f32,
    sticks: [StickAim; 2],
    /// The custom-curve fork's own running state.
    decay: super::cc::Decay,
    one_euro_x: super::cc::OneEuro,
    one_euro_y: super::cc::OneEuro,
    brake: super::cc::Brake,
}

impl Default for Aim {
    fn default() -> Self {
        Aim {
            t: 0.0,
            gyro_samples: Box::new([(0.0, 0.0); GYRO_SAMPLES]),
            gyro_front: 0,
            trackball_x: Box::new([0.0; TRACKBALL_SAMPLES]),
            trackball_y: Box::new([0.0; TRACKBALL_SAMPLES]),
            trackball_ix: 0,
            trackball_iy: 0,
            last_abs_x: 0.0,
            last_abs_y: 0.0,
            sticks: [StickAim::default(), StickAim::default()],
            decay: super::cc::Decay::default(),
            one_euro_x: super::cc::OneEuro::default(),
            one_euro_y: super::cc::OneEuro::default(),
            brake: super::cc::Brake::default(),
        }
    }
}

impl Aim {
    /// Is this stick's flick still being paid out? A modeshift can't take the
    /// stick out of flick mode mid-turn: JSM degrades it to `FLICK_ONLY` so the
    /// turn finishes instead of stopping halfway round.
    pub fn flick_unfinished(&self, side: usize) -> bool {
        self.sticks[side].flick.paying_out
    }

    /// One tick of aiming. Returns the mouse displacement for this tick in our
    /// bus's terms: pixels, y counting upward.
    pub fn tick(
        &mut self,
        s: &Settings,
        p: &pad::Settings,
        m: &super::motion::Settings,
        c: &super::cc::Settings,
        gravity: super::motion::Gravity,
        dt: f32,
        gyro: Gyro,
        analog: &Analog,
        actions: &HashSet<GyroAction>,
        down: &dyn Fn(Btn) -> bool,
    ) -> Aimed {
        self.t += dt;
        // JSM's frame, from the note at the top of this file.
        let (in_x, in_y, in_z) = (gyro.pitch, -gyro.yaw, gyro.roll);

        // ── the gyro, into the two mouse axes ────────────────────────────────
        let pick = |m: AxisMask, x_sign: f32| {
            // `gyroX += inGyroX; gyroX -= inGyroY; gyroX -= inGyroZ` for mouse x,
            // and the signs the other way round for mouse y.
            let mut v = 0.0;
            if m.x { v += x_sign * in_x; }
            if m.y { v += -x_sign * in_y; }
            if m.z { v += -x_sign * in_z; }
            v
        };
        let (mut gx, mut gy) = if m.space == super::motion::Space::YawPlusRoll {
            // The custom-curve fork's space: pitch on the vertical, yaw with a share
            // of roll mixed into the turn. See `cc.rs`.
            super::cc::yaw_plus_roll(c, glam::Vec3::new(in_x, in_y, in_z))
        } else if m.space.needs_gravity() {
            // A space measured against gravity ignores the axis masks entirely —
            // it works out which way "turning" and "leaning" point from where down
            // is, and takes the gyro's component along that. See `motion.rs`.
            super::motion::gravity_space(
                m.space,
                gravity,
                super::motion::JsmGyro { x: in_x, y: in_y, z: in_z },
            )
        } else {
            (pick(s.mouse_x_from, 1.0), pick(s.mouse_y_from, -1.0))
        };

        // ── smoothing ────────────────────────────────────────────────────────
        if c.decay_smoothing {
            // The fork's alternative: one exponential smoother whose time constant
            // shrinks as the pad speeds up, instead of a rolling average.
            let (sx, sy) = self.decay.smooth(gx, gy, dt, s.smooth_time, s.smooth_threshold);
            gx = sx;
            gy = sy;
        } else {
            self.decay.reset();
            let length = (gx * gx + gy * gy).sqrt();
            let samples = ((s.smooth_time / dt.max(1e-6)) as usize).clamp(1, GYRO_SAMPLES);
            let (sx, sy) = self.smooth_gyro(gx, gy, length, s.smooth_threshold / 2.0, s.smooth_threshold, samples);
            gx = sx;
            gy = sy;
        }

        // ── the one-euro filter, when the command has switched it on ──────────
        if c.one_euro_enabled {
            gx = self.one_euro_x.filter(gx, dt, c.one_euro_min_cutoff, c.one_euro_speed_coeff);
            gy = self.one_euro_y.filter(gy, dt, c.one_euro_min_cutoff, c.one_euro_speed_coeff);
        } else {
            self.one_euro_x.reset();
            self.one_euro_y.reset();
        }

        // The speed the deceleration brake measures against: post-smoothing, before
        // the cutoff and before anything synthetic is added. The fork records it
        // exactly here, and the order matters — a cutoff fading the tail of a flick
        // out would read as braking all by itself.
        let brake_speed = (gx * gx + gy * gy).sqrt();

        // ── cutoff: ignore a slow drift, fade back in over the recovery band ──
        let length = (gx * gx + gy * gy).sqrt();
        let factor = cutoff_factor(s, length);
        if factor != 1.0 {
            gx *= factor;
            gy *= factor;
        }

        // ── is the gyro on at all? ───────────────────────────────────────────
        let mut blocked = match s.gyro_button {
            None => false,
            Some(b) => {
                let held = match b.source {
                    GyroSource::Button(btn) => down(btn),
                    GyroSource::LeftStick => analog.stick_active(0),
                    GyroSource::RightStick => analog.stick_active(1),
                    GyroSource::Never => false,
                };
                b.always_off ^ held
            }
        };
        // A binding's gyro action overrides the setting for as long as it lasts.
        if actions.contains(&GyroAction::On) { blocked = false; }
        if actions.contains(&GyroAction::Off) { blocked = true; }

        // ── trackball: let the last of the movement carry on, decaying ───────
        let track_x = actions.contains(&GyroAction::Trackball) || actions.contains(&GyroAction::TrackballX);
        let track_y = actions.contains(&GyroAction::Trackball) || actions.contains(&GyroAction::TrackballY);
        let (tx, ty) = self.trackball(s, dt, gx, gy, track_x, track_y);
        gx = tx;
        gy = ty;

        if blocked {
            gx = 0.0;
            gy = 0.0;
            // The fork resets the one-euro filter here, and rightly: coming back from
            // a blocked gyro should not have to wash the filter's memory of zero out
            // first. The brake goes with it — a blocked gyro reads as a dead stop,
            // which is exactly the shape the brake watches for, so leaving it engaged
            // would damp the first movement after the gyro comes back.
            self.one_euro_x.reset();
            self.one_euro_y.reset();
            self.brake.reset();
        }

        // ── axis signs, and the inversions a binding can ask for ─────────────
        let invert = actions.contains(&GyroAction::Invert);
        let sign_x = if invert || actions.contains(&GyroAction::InvertX) { -s.axis_x } else { s.axis_x };
        let sign_y = if invert || actions.contains(&GyroAction::InvertY) { -s.axis_y } else { s.axis_y };
        let mut vel_x = gx * sign_x;
        let mut vel_y = gy * sign_y;

        // ── the brake, then the snap, then the ramp ──────────────────────────
        //
        // This order is the fork's and is not the obvious one: the brake measures
        // BEFORE the snap, so a snap-induced slowdown is not mistaken for the hands
        // stopping, and the speed is recomputed afterwards for the curve. Implement
        // it the other way round and the two features fight each other.
        let speed_for_brake = (vel_x * vel_x + vel_y * vel_y).sqrt();
        let brake = self.brake.tick(c, dt, brake_speed, speed_for_brake);

        let (sx, sy) = super::cc::angle_snap(c, vel_x, vel_y);
        vel_x = sx;
        vel_y = sy;

        // ── the sensitivity ramp ─────────────────────────────────────────────
        let magnitude = (vel_x * vel_x + vel_y * vel_y).sqrt() - s.min_threshold;
        let magnitude = magnitude.max(0.0);
        let denom = s.max_threshold - s.min_threshold;
        let ramp = if denom <= 0.0 {
            // The thresholds meet: jump to the high sensitivity as soon as there
            // is any movement at all.
            if magnitude > 0.0 { 1.0 } else { 0.0 }
        } else {
            (magnitude / denom).min(1.0)
        };
        // `LINEAR` is the straight line stock JSM draws; the fork's five curves take
        // their shape from their own parameters instead. Note that three of the six
        // never look at `MAX_GYRO_THRESHOLD` at all.
        vel_x *= super::cc::sensitivity(c, magnitude, ramp, s.max_threshold, s.min_sens.0, s.max_sens.0);
        vel_y *= super::cc::sensitivity(c, magnitude, ramp, s.max_threshold, s.min_sens.1, s.max_sens.1);
        if brake < 1.0 {
            vel_x *= brake;
            vel_y *= brake;
        }

        // ── the sticks ───────────────────────────────────────────────────────
        let mut cam = Vec2::ZERO;
        let mut flick_dps = Vec2::ZERO;
        for side in 0..2 {
            let (mouse, dps) = self.stick(s, p, dt, side, analog);
            cam += mouse;
            flick_dps.x += dps;
        }

        // ── where the gyro's rate goes ───────────────────────────────────────
        // A stick carries it in camera terms (deg/s, y down, which is what
        // `pad::virtual_stick` adds it to); the mouse takes it in pixels below.
        let gyro_dps = match p.gyro_dest.side() {
            Some(_) => Vec2::new(vel_x, vel_y),
            None => Vec2::ZERO,
        };

        // ── one displacement, in our bus's terms ─────────────────────────────
        //
        // JSM gates the whole mouse move on `GYRO_OUTPUT` being `MOUSE`, so
        // pointing the gyro at a virtual stick also stops an `AIM` stick from
        // moving the mouse. That reads like an oversight rather than a design, but
        // it is what a config written against JSM was tuned on, so it is
        // reproduced — and the editor says as much on the `GYRO_OUTPUT` line so
        // nobody has to discover it the hard way.
        let mouse = if p.gyro_dest == Dest::Mouse {
            let calibration = s.real_world_calibration / s.in_game_sens.max(1e-6);
            let x = vel_x * calibration * dt + cam.x;
            // JSM counts the screen's y downward and the bus counts it up.
            let y = -(vel_y * calibration * dt) + cam.y;
            Vec2::new(x, y)
        } else {
            Vec2::ZERO
        };
        Aimed { mouse, gyro_dps, flick_dps }
    }

    /// JSM's gyro smoother: what is over the threshold goes straight through,
    /// what is under it is averaged over the window.
    fn smooth_gyro(&mut self, x: f32, y: f32, length: f32, bottom: f32, top: f32, samples: usize) -> (f32, f32) {
        let immediate = if top <= bottom {
            if length < bottom { 0.0 } else { 1.0 }
        } else {
            ((length - bottom) / (top - bottom)).clamp(0.0, 1.0)
        };
        let smooth = 1.0 - immediate;
        self.gyro_front = (self.gyro_front + GYRO_SAMPLES - 1) % GYRO_SAMPLES;
        self.gyro_samples[self.gyro_front] = (x * smooth, y * smooth);
        let mut out = (0.0, 0.0);
        for i in 0..samples {
            let s = self.gyro_samples[(self.gyro_front + i) % GYRO_SAMPLES];
            out.0 += s.0 / samples as f32;
            out.1 += s.1 / samples as f32;
        }
        (out.0 + x * immediate, out.1 + y * immediate)
    }

    /// The trackball: while its button is held the gyro keeps coasting on the
    /// average of what it was doing, decaying as it goes.
    fn trackball(&mut self, s: &Settings, dt: f32, gx: f32, gy: f32, track_x: bool, track_y: bool) -> (f32, f32) {
        let decay = (-dt * s.trackball_decay).exp2();
        let max = (((1.0 / dt.max(1e-6)) * 0.125) as usize).clamp(1, TRACKBALL_SAMPLES);
        if !track_x && !track_y {
            self.last_abs_x = gx.abs();
            self.last_abs_y = gy.abs();
        }
        let mut out = (gx, gy);
        if track_x {
            let mut sum = 0.0;
            for i in 0..max {
                sum += self.trackball_x[i];
                self.trackball_x[i] *= decay;
            }
            sum /= max as f32;
            // Never faster than the movement that started it.
            if sum.abs() > self.last_abs_x && sum.abs() > 0.0 {
                sum *= self.last_abs_x / sum.abs();
            }
            out.0 = sum;
        } else {
            self.trackball_ix = (self.trackball_ix + 1) % max;
            self.trackball_x[self.trackball_ix] = gx;
        }
        if track_y {
            let mut sum = 0.0;
            for i in 0..max {
                sum += self.trackball_y[i];
                self.trackball_y[i] *= decay;
            }
            sum /= max as f32;
            if sum.abs() > self.last_abs_y && sum.abs() > 0.0 {
                sum *= self.last_abs_y / sum.abs();
            }
            out.1 = sum;
        } else {
            self.trackball_iy = (self.trackball_iy + 1) % max;
            self.trackball_y[self.trackball_iy] = gy;
        }
        out
    }

    /// What one stick contributes to the mouse this tick (JSM's `camSpeed`, with
    /// y counting up).
    /// One stick's contribution, as a mouse displacement and — for a flick bound
    /// for a virtual stick — a camera rate in deg/s.
    fn stick(
        &mut self,
        s: &Settings,
        p: &pad::Settings,
        dt: f32,
        side: usize,
        analog: &Analog,
    ) -> (Vec2, f32) {
        let st = analog.stick_out(side);
        let mouse = match st.mode {
            StickMode::Aim => {
                if !st.pegged {
                    self.sticks[side].acceleration = 1.0;
                }
                let len = (st.x * st.x + st.y * st.y).sqrt();
                if len == 0.0 {
                    return (Vec2::ZERO, 0.0);
                }
                if self.sticks[side].acceleration == 0.0 {
                    self.sticks[side].acceleration = 1.0;
                }
                let accel = self.sticks[side].acceleration;
                let warped = len.powf(s.stick_power);
                let cal = s.real_world_calibration / s.in_game_sens.max(1e-6);
                let out = Vec2::new(
                    st.x / len * warped * s.stick_sens.0 * cal * accel * dt * s.stick_axis_x,
                    st.y / len * warped * s.stick_sens.1 * cal * accel * dt * s.stick_axis_y,
                );
                if st.pegged {
                    let a = accel + s.stick_accel_rate * dt;
                    self.sticks[side].acceleration = a.min(s.stick_accel_cap);
                }
                out
            }
            StickMode::Flick | StickMode::FlickOnly | StickMode::RotateOnly => {
                // A flick bound for a virtual stick is a rate, not a displacement,
                // and a wholly different calculation — see `flick`.
                let turn = self.flick(s, p, dt, side, &st);
                if p.flick_dest != Dest::Mouse {
                    return (Vec2::ZERO, turn);
                }
                Vec2::new(turn, 0.0)
            }
            StickMode::MouseArea => {
                // Pushing the stick moves the pointer by how far it moved, which
                // is a displacement already — no dt.
                let r = s.mouse_ring_radius;
                Vec2::new((st.raw.0 - st.prev_raw.0) * r, (st.raw.1 - st.prev_raw.1) * r)
            }
            _ => Vec2::ZERO,
        };
        (mouse, 0.0)
    }

    /// Flick stick: pushing the stick out snaps the camera to face that way, and
    /// turning it while held traces the camera around.
    /// Returns mouse pixels when `FLICK_STICK_OUTPUT` is `MOUSE`, and a camera rate
    /// in deg/s when it names a virtual stick. The two are not the same sum: a
    /// mouse can be moved any distance in one tick, so the turn is eased out over
    /// `FLICK_TIME`, while a stick can only be pushed so far — so there the turn
    /// becomes "hold it right over at `VIRTUAL_STICK_CALIBRATION` deg/s for however
    /// long the angle takes", and `FLICK_TIME` has nothing to say.
    fn flick(
        &mut self,
        s: &Settings,
        p: &pad::Settings,
        dt: f32,
        side: usize,
        st: &super::analog::StickOut,
    ) -> f32 {
        use std::f32::consts::PI;
        let mode = st.mode;
        let to_stick = p.flick_dest.side().is_some();
        let cal = s.real_world_calibration / s.in_game_sens.max(1e-6);
        // JSM works in radians here and its mouse calibration is per degree. Bound
        // for a stick there is no mouse to calibrate against, so the rotation stays
        // in radians per tick and is converted to deg/s at the end.
        let flick_speed_constant = if to_stick { 1.0 } else { cal * (180.0 / PI) };
        let mut cam = 0.0;

        // Full deflection starts a flick; it takes a little less to keep one.
        let threshold = if self.sticks[side].flick.flicking { 0.9 } else { 1.0 };
        if st.len >= threshold {
            let angle = (-st.x).atan2(st.y);
            if !self.sticks[side].flick.flicking {
                self.sticks[side].flick.flicking = true;
                if mode != StickMode::RotateOnly {
                    let mut angle = angle;
                    if s.flick_snap != SnapMode::None {
                        let interval = match s.flick_snap {
                            SnapMode::Four => PI / 2.0,
                            SnapMode::Eight => PI / 4.0,
                            SnapMode::None => PI,
                        };
                        let snapped = (angle / interval).round() * interval;
                        angle = angle * (1.0 - s.flick_snap_strength) + snapped * s.flick_snap_strength;
                    }
                    if angle.abs() * (180.0 / PI) < s.flick_deadzone_angle {
                        angle = 0.0;
                    }
                    let f = &mut self.sticks[side].flick;
                    f.since = self.t;
                    f.delta = angle;
                    f.done = 0.0;
                    f.paying_out = true;
                    f.samples.fill(0.0);
                    f.front = 0;
                }
            } else if mode != StickMode::FlickOnly {
                // Already flicking: the stick's turn drags the camera with it.
                let last_angle = (-st.prev_x).atan2(st.prev_y);
                let mut change = angle - last_angle;
                // The short way round, never the long way.
                change = (change + PI).rem_euclid(2.0 * PI) - PI;
                let speed = -(change * flick_speed_constant);
                let step = 0.01;
                let smooth = if s.rotate_smooth_override < 0.0 {
                    (flick_speed_constant * step * 2.0, flick_speed_constant * step * 4.0)
                } else {
                    (flick_speed_constant * s.rotate_smooth_override,
                     flick_speed_constant * s.rotate_smooth_override * 2.0)
                };
                let samples = FLICK_SAMPLES.min(64);
                cam = self.smooth_rotation(side, speed, smooth.0, smooth.1, samples);
                if to_stick {
                    // Radians this tick into degrees per second.
                    cam *= 180.0 / (PI * dt.max(1e-6));
                }
            }
        } else {
            self.sticks[side].flick.flicking = false;
        }

        let f = &mut self.sticks[side].flick;
        if to_stick {
            // A stick turns at one speed for as long as the angle needs. Its
            // duration is the angle divided by that speed, in radians because
            // `delta` is.
            let speed = p.calibration;
            let flick_time = if speed > 0.0 {
                f.delta.abs() / (speed * PI / 180.0)
            } else {
                0.0
            };
            let paying_out = self.t - f.since <= flick_time && flick_time > 0.0;
            f.paying_out = paying_out;
            if paying_out {
                cam -= if f.delta >= 0.0 { speed } else { -speed };
            }
            return cam;
        }

        // The flick itself, eased over `FLICK_TIME` and paid out a tick at a time.
        let mut percent = (self.t - f.since) / s.flick_time.max(1e-6);
        if f.delta.abs() > 0.0 {
            percent /= (f.delta.abs() / PI).powf(s.flick_time_exponent);
        }
        let percent = percent.min(1.0);
        if percent >= 1.0 {
            f.paying_out = false;
        }
        let ease = |p: f32| 1.0 - (1.0 - p) * (1.0 - p);
        let was = ease(f.done);
        f.done = percent;
        cam += (ease(percent) - was) * f.delta * cal * -(180.0 / PI);
        cam
    }

    /// The same shape as the gyro smoother, for a flick's rotation.
    fn smooth_rotation(&mut self, side: usize, value: f32, bottom: f32, top: f32, samples: usize) -> f32 {
        let f = &mut self.sticks[side].flick;
        f.front = (f.front + FLICK_SAMPLES - 1) % FLICK_SAMPLES;
        let length = value.abs();
        let immediate = if top <= bottom {
            1.0
        } else {
            ((length - bottom) / (top - bottom)).clamp(0.0, 1.0)
        };
        let smooth = 1.0 - immediate;
        f.samples[f.front] = value * smooth;
        let mut result = 0.0;
        for i in 0..samples {
            result += f.samples[(f.front + i) % FLICK_SAMPLES] / samples as f32;
        }
        result + value * immediate
    }
}
