//! Gravity, and everything JSM measures against it: the lean buttons, the motion
//! stick, and the gyro spaces that separate turning from leaning.
//!
//! ## Where gravity comes from
//!
//! JSM asks its motion library for a fused gravity estimate. Our bus carries only
//! raw accelerometer and gyro, so gravity is estimated the way the Gyro 3DOF
//! module already does it: low-pass the accelerometer direction. Gravity is the
//! only *sustained* acceleration a hand-held pad sees, so a slow filter settles
//! onto "down" in the pad's own frame while a shake or a flick averages out. The
//! lag is the point — it answers "how is this being held?", not "what is it doing
//! this instant?".
//!
//! ## Which way round the axes go
//!
//! This is the one place in the module where accelerometer and gyro readings are
//! combined, and on our bus **they are not in the same basis**. Written against
//! the pad's own body axes — F forward out of the USB end, R the player's right, U
//! out of the face:
//!
//! * our **accel** components are in `(F, -R, U)`;
//! * our **gyro** components are in `(F, R, -U)`.
//!
//! Both are right-handed, so nothing looks wrong until something combines them —
//! which is exactly what the gravity gyro spaces do. So both are converted here
//! into JSM's own frame and the maths is done there, rather than trusting the two
//! to agree. JSM's frame is `(R, U, F)`: X is the pitch axis, Y the yaw axis, Z
//! the roll axis, which is why `MOUSE_X_FROM_GYRO_AXIS` defaults to `Y`.
//!
//! An accelerometer at rest reads the direction that is *up* (the reaction to
//! gravity), so gravity is the negative of it. Putting the two together:
//!
//! | JSM | from our accel | from our gyro |
//! | --- | --- | --- |
//! | X (right / pitch) | `accel_y` | `gyro_y` |
//! | Y (up / yaw) | `-accel_z` | `-gyro_z` |
//! | Z (forward / roll) | `-accel_x` | `gyro_x` |
//!
//! Checks: flat and face up our accel is `(0, 0, 1)`, giving JSM gravity
//! `(0, -1, 0)` — straight down its Y, which is what its motion stick reads as
//! centred. Nose up 90° gives `(1, 0, 0)` → `(0, 0, -1)`, gravity along -Z, so its
//! forward axis points at the sky. Right grip down gives `accel_y = 1` → gravity
//! along +X, the pad's right. The gyro column is the same mapping phase 3 derived
//! independently from what JSM's *defaults* have to do, which is a reassuring
//! place for two derivations to meet.

use glam::Vec3;

use super::analog::{Orientation, StickCfg};

/// How long the gravity estimate takes to settle, in seconds. The Gyro 3DOF
/// module uses 1-3 s depending on how world-anchored its mode wants to feel; the
/// motion stick wants to follow a deliberate tilt without chasing a flick, and
/// 0.5 s is the compromise JSM's own fused estimate feels closest to.
const GRAVITY_TAU: f32 = 0.5;

/// JSM's gyro spaces. The first is phase 3's; the rest are measured against
/// gravity and are what this file adds.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Space {
    /// `LOCAL`: the pad's own axes, picked by the two axis masks.
    #[default]
    Local,
    /// Turning the pad about gravity turns the camera.
    PlayerTurn,
    /// Leaning the pad about gravity turns the camera.
    PlayerLean,
    /// As `PLAYER_TURN`, but pitch is measured against the world too.
    WorldTurn,
    WorldLean,
    /// `YAW_PLUS_ROLL`, from the custom-curve fork: pitch on the vertical and yaw
    /// with a share of roll mixed in. Needs no gravity — see `cc.rs`.
    YawPlusRoll,
}

impl Space {
    /// Does this space need to know which way is down?
    pub fn needs_gravity(self) -> bool {
        !matches!(self, Space::Local | Space::YawPlusRoll)
    }
}

/// What the motion side of a config is configured with.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Settings {
    pub space: Space,
    /// `LEAN_THRESHOLD`, in degrees of side tilt.
    pub lean_threshold: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { space: Space::default(), lean_threshold: 15.0 }
    }
}

/// The motion side's running state.
#[derive(Default)]
pub struct Motion {
    /// Low-passed accelerometer direction, in our bus's accel basis.
    smoothed: Vec3,
    /// Set once the filter has something to say.
    settled: bool,
    /// The orientation `SET_MOTION_STICK_NEUTRAL` captured, as a rotation that
    /// takes the captured "down" back to straight down in JSM's frame.
    neutral: Option<glam::Quat>,
}

/// Gravity for one tick, in JSM's frame, already normalised.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Gravity {
    pub v: Vec3,
    /// False until the filter has seen any accelerometer reading at all, so a pad
    /// with no accelerometer reads as "no idea which way is down" rather than as
    /// "perfectly flat" — the difference between a motion stick that does nothing
    /// and one that pushes itself to an edge.
    pub known: bool,
}

impl Motion {
    /// Advance the gravity estimate. `accel` is the bus's raw accelerometer in its
    /// own basis; `None` when the pad reports none.
    pub fn tick(&mut self, accel: Option<Vec3>, dt: f32) -> Gravity {
        if let Some(a) = accel {
            let len = a.length();
            if len > 0.01 {
                let alpha = 1.0 - (-dt / GRAVITY_TAU).exp();
                let norm = a / len;
                if self.settled {
                    self.smoothed += alpha * (norm - self.smoothed);
                } else {
                    // Start on the first reading rather than easing up from zero,
                    // or the first half-second of every session reads as a pad
                    // held at some angle it never was.
                    self.smoothed = norm;
                    self.settled = true;
                }
            }
        }
        if !self.settled {
            return Gravity::default();
        }
        let len = self.smoothed.length();
        if len < 0.01 {
            return Gravity::default();
        }
        let up = self.smoothed / len;
        // Accel reads "up"; gravity is the other way. Then into JSM's (R, U, F).
        let mut v = Vec3::new(up.y, -up.z, -up.x);
        if let Some(q) = self.neutral {
            v = q * v;
        }
        Gravity { v, known: true }
    }

    /// `SET_MOTION_STICK_NEUTRAL`: take however the pad is being held right now as
    /// the motion stick's centre. Stored as the rotation that carries the captured
    /// "down" back to straight down, so everything after is measured from there.
    pub fn set_neutral(&mut self, g: Gravity) {
        self.neutral = None;
        if !g.known {
            return;
        }
        let down = Vec3::new(0.0, -1.0, 0.0);
        // `from_rotation_arc` is undefined for exactly opposite vectors, and a pad
        // held perfectly upside down is a real thing to do.
        let q = if g.v.dot(down) < -0.999_9 {
            glam::Quat::from_axis_angle(Vec3::X, std::f32::consts::PI)
        } else {
            glam::Quat::from_rotation_arc(g.v.normalize(), down)
        };
        self.neutral = Some(q);
    }

    pub fn has_neutral(&self) -> bool { self.neutral.is_some() }
}

/// The motion stick, as a stick position. JSM builds it from how far gravity has
/// swung away from straight down: tilt the pad and the stick pushes the way it was
/// tilted, with a half-turn of tilt reaching full deflection.
pub fn motion_stick(g: Gravity, cfg: StickCfg) -> (f32, f32) {
    if !g.known {
        return (0.0, 0.0);
    }
    let (mut x, mut y) = (g.v.x, -g.v.z);
    // How far off straight down, as a fraction of a half turn: 0 flat, 0.5 on its
    // side, 1 upside down.
    let flat_len = (g.v.x * g.v.x + g.v.z * g.v.z).sqrt();
    let deflection = flat_len.atan2(-g.v.y) / std::f32::consts::PI;
    if flat_len > 0.0 {
        x *= deflection / flat_len;
        y *= deflection / flat_len;
    }
    if cfg.invert_x { x = -x; }
    if cfg.invert_y { y = -y; }
    (x, y)
}

/// The lean buttons: `LEAN_LEFT` and `LEAN_RIGHT` fire once the pad is tilted past
/// `LEAN_THRESHOLD` degrees to one side. Which body axis counts as "to the side"
/// depends on how the pad is being held, so `CONTROLLER_ORIENTATION` picks it — the
/// same setting the thumbsticks use.
pub fn lean(g: Gravity, s: &Settings, orientation: Orientation) -> (bool, bool) {
    if !g.known {
        return (false, false);
    }
    let len = g.v.length();
    if len <= 0.0 {
        return (false, false);
    }
    let side = match orientation {
        Orientation::Forward => g.v.x,
        Orientation::Left => g.v.z,
        Orientation::Right => -g.v.z,
        Orientation::Backward => -g.v.x,
    };
    let dir = side / len;
    let threshold = (s.lean_threshold * std::f32::consts::PI / 180.0).sin();
    (dir < -threshold, dir > threshold)
}

/// One gyro reading in JSM's frame, as the spaces below want it.
#[derive(Clone, Copy, Debug, Default)]
pub struct JsmGyro {
    /// About the pad's right: pitch.
    pub x: f32,
    /// About the pad's up: yaw.
    pub y: f32,
    /// About the pad's forward: roll.
    pub z: f32,
}

/// Turn a gyro reading into the two mouse axes, for a space measured against
/// gravity. Returns `(mouse_x, mouse_y)` in degrees per second, JSM's own sign
/// convention (y counts down the screen).
///
/// The shape of all four is the same: work out which way "turning" or "leaning"
/// points right now given where down is, and take the gyro's component along it.
/// That is what makes them hold up when the pad is not being held flat.
pub fn gravity_space(space: Space, g: Gravity, r: JsmGyro) -> (f32, f32) {
    let grav = if g.known && g.v.length() > 0.0 { g.v.normalize() } else { Vec3::ZERO };

    // How much to trust a pitch axis worked out from gravity. Held on its side —
    // neither flat nor upright — the estimate is nearly meaningless, so JSM fades
    // these outputs out rather than letting them flail.
    let flatness = grav.y.abs();
    let upness = grav.z.abs();
    let trust = ((flatness.max(upness) - 0.125) / 0.125).clamp(0.0, 1.0);

    // The pad's own pitch axis (JSM's X) with the part along gravity taken out:
    // what is left is "pitch as the world sees it".
    let along = grav.x;
    let pitch_axis = Vec3::new(1.0 - grav.x * along, -grav.y * along, -grav.z * along);
    let pitch_axis = if pitch_axis.length_squared() > 0.0 {
        Some(pitch_axis.normalize())
    } else {
        None
    };
    // World roll is what is left over: perpendicular to both gravity and pitch.
    let roll_axis = pitch_axis.and_then(|p| {
        let r = p.cross(grav);
        (r.length_squared() > 0.0).then(|| r.normalize())
    });
    let gyro = Vec3::new(r.x, r.y, r.z);
    // The amount of turn the pad could possibly be doing about anything but its
    // own pitch axis — the ceiling JSM clamps a relaxed estimate to.
    let off_pitch = (r.y * r.y + r.z * r.z).sqrt();

    let (mut mx, mut my) = (0.0, 0.0);
    match space {
        // Handled in `aim.rs`; nothing here.
        Space::Local | Space::YawPlusRoll => {}
        Space::PlayerTurn | Space::PlayerLean => {
            // Player spaces keep pitch as the pad's own, which is what makes them
            // feel direct: only the horizontal half is re-referenced.
            my = -r.x;
            if space == Space::PlayerTurn {
                // Turning about gravity, from the two axes that can contribute.
                let yaw = grav.y * r.y + grav.z * r.z;
                // Relaxed by a 60 degree buffer, so a turn still counts when the
                // pad is not held squarely — but never more than it could be.
                mx = yaw.signum() * (yaw.abs() * 2.0).min(off_pitch);
            } else if let Some(roll) = roll_axis {
                let world_roll = roll.y * r.y + roll.z * r.z;
                // A 45 degree buffer here: a lean is a smaller gesture.
                mx = world_roll.signum() * (world_roll.abs() * 1.41).min(off_pitch);
                mx *= trust;
            }
        }
        Space::WorldTurn | Space::WorldLean => {
            if let Some(pitch) = pitch_axis {
                my = -pitch.dot(gyro) * trust;
            }
            if space == Space::WorldTurn {
                mx = grav.dot(gyro);
            } else if let Some(roll) = roll_axis {
                mx = roll.dot(gyro) * trust;
            }
        }
    }
    (mx, my)
}

/// `LEFT_STEER_X` / `RIGHT_STEER_X`: how far the pad is leaned becomes a push
/// along a virtual stick's X, so tilting it steers like a wheel. Returns the sign
/// and a 0..1 reach for `pad.rs` to place in the game's live band; `None` when
/// which way is down isn't known yet.
///
/// Its range is a whole half-turn, not the 90 degrees `ANGLE_TO_AXIS` uses, and
/// `MOTION_DEADZONE_OUTER`'s default of 135 degrees is what makes that usable: it
/// reaches full lock well before the pad is upside down. Past straight up the
/// angle is folded back (`180 - angle`), so carrying on past vertical keeps
/// steering the same way instead of unwinding.
pub fn steer(
    g: Gravity,
    orientation: Orientation,
    inner_dz_deg: f32,
    outer_dz_deg: f32,
    stick_power: f32,
) -> Option<(f32, f32)> {
    if !g.known {
        return None;
    }
    let len = g.v.length();
    if len <= 0.0 {
        return None;
    }
    let side = match orientation {
        Orientation::Forward => g.v.x,
        Orientation::Left => g.v.z,
        Orientation::Right => -g.v.z,
        Orientation::Backward => -g.v.x,
    };
    let dir = (side / len).clamp(-1.0, 1.0);
    let angle = dir.asin().to_degrees();
    let sign = if angle < 0.0 { -1.0 } else { 1.0 };
    let mut abs = angle.abs();
    // Gravity pointing up the pad's own up-axis means it has gone past vertical.
    if g.v.y > 0.0 {
        abs = 180.0 - abs;
    }
    let span = 180.0 - outer_dz_deg - inner_dz_deg;
    let reach = if span > 0.0 {
        ((abs - inner_dz_deg) / span).clamp(0.0, 1.0)
    } else if abs > inner_dz_deg {
        1.0
    } else {
        0.0
    };
    Some((sign, reach.powf(stick_power)))
}
