//! Which way is down, the way JoyShockMapper knows it.
//!
//! JSM takes gravity from its motion library, GamepadMotionHelpers (Julian "Jibb"
//! Smart, MIT - `licenses/GamepadMotionHelpers.txt`), and this is that estimate,
//! step for step (`Motion::Update`), so everything here that measures against
//! gravity - the JSM module's gyro spaces, motion stick, lean and steering, the
//! 3DOF module's Player, World and Orient, and Gyro to Stick Rotation's World -
//! reads the same "down" JSM does.
//!
//! Gravity stays put in the world while the pad turns under it, so in the pad's
//! frame it turns back by exactly the gyro's turn, every sample: that carries it
//! through any turn however fast. The accelerometer only pulls it in - quickly
//! while the pad is still, slowly while it is being shaken (a shake is
//! acceleration, not tilt), and barely at all once it is close, unless the pad is
//! turning - which takes out the gyro's drift without letting accelerometer noise
//! wobble it. (Low-passing the accelerometer alone, as this codebase did before,
//! lags every tilt by its whole time constant.)
//!
//! It keeps the library's orientation as well (`GetOrientation`): the gyro's turns
//! added up, its tilt held to that gravity every sample - the pad as it really is,
//! but for a heading that only the gyro knows. Orient reads where the nose points
//! off it.
//!
//! Units are the library's: gyro in degrees per second, accelerometer in g. It is
//! worked in the gyro's basis, (forward, right, down), which is right-handed; the
//! accelerometer arrives in (forward, left, up) and is turned into it here.

use glam::{Quat, Vec3};

/// `ShortSteadinessHalfTime`: how quickly the shake measure forgets, seconds.
const SHAKE_HALF_LIFE_S: f32 = 0.25;
/// `GravityCorrectionShakinessMinThreshold` / `MaxThreshold`, in g: below the
/// first the pad is still, above the second it is being shaken.
const SHAKE_MIN: f32 = 0.01;
const SHAKE_MAX: f32 = 0.4;
/// `GravityCorrectionStillSpeed` / `ShakySpeed`: how fast the accelerometer pulls
/// gravity in, in g per second (about radians per second for a small error).
const STILL_SPEED: f32 = 1.0;
const SHAKY_SPEED: f32 = 0.1;
/// `GravityCorrectionGyroFactor` / `MinimumSpeed`: the pull is capped at this
/// share of the turn rate (rad/s), or the minimum...
const GYRO_FACTOR: f32 = 0.1;
const MIN_SPEED: f32 = 0.01;
/// ...`GravityCorrectionGyroMinThreshold` / `MaxThreshold`: ...once gravity is
/// within this far of the accelerometer (g); further out the cap lifts.
const CLOSE_MIN: f32 = 0.05;
const CLOSE_MAX: f32 = 0.25;

/// The estimate's running state.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GravityFusion {
    /// Gravity, in g, in the gyro's basis. Starts at nothing and grows toward the
    /// accelerometer, as the library's does; only its direction is read.
    grav: Vec3,
    /// The accelerometer smoothed, turned with the pad like gravity, for the shake
    /// measure.
    smooth_accel: Vec3,
    /// How hard the pad is being shaken, in g: peak-held, then forgotten.
    shakiness: f32,
    /// The pad's orientation, body to world, both in (forward, right, down) - the
    /// world's down along +z - its tilt held to `grav`. Identity at the start.
    quat: Quat,
}

/// How many `f32`s [`GravityFusion::store`] writes.
pub const GRAVITY_FUSION_LEN: usize = 11;

impl GravityFusion {
    /// One sample. `gyro_dps` is the bus's gyro - (roll, pitch, yaw), which is
    /// (forward, right, down) - in degrees per second, as it physically turns;
    /// `accel` the bus's accelerometer in g, (forward, left, up), `None` when the
    /// pad has none. Returns which way is down, in the gyro's basis, or `None`
    /// while nothing is known.
    pub fn update(&mut self, gyro_dps: Vec3, accel: Option<Vec3>, dt: f32) -> Option<Vec3> {
        let a = accel.map(|a| Vec3::new(a.x, -a.y, -a.z));
        // The library skips a sample whose six readings are all zero: a pad that
        // reports nothing yet.
        if gyro_dps == Vec3::ZERO && a.is_none_or(|a| a == Vec3::ZERO) {
            return self.down();
        }
        let angle_speed = gyro_dps.length().to_radians();
        // What is fixed in the world turns back by the pad's turn.
        let back = if angle_speed > 0.0 && dt > 0.0 {
            Quat::from_axis_angle(gyro_dps / gyro_dps.length(), -angle_speed * dt)
        } else {
            Quat::IDENTITY
        };
        self.grav = back * self.grav;
        // The orientation turns with the pad: a turn in its own frame, so on the right.
        if angle_speed > 0.0 && dt > 0.0 {
            self.quat = (self.quat * back.inverse()).normalize();
        }
        let Some(a) = a.filter(|a| a.length_squared() > 0.0) else {
            return self.down();
        };
        // Shakiness: how far the reading is from its own recent average, peak-held
        // and forgotten with a quarter-second half-life.
        self.smooth_accel = back * self.smooth_accel;
        let forget = 2f32.powf(-dt.max(0.0) / SHAKE_HALF_LIFE_S);
        self.shakiness = (self.shakiness * forget).max((a - self.smooth_accel).length());
        self.smooth_accel = a + (self.smooth_accel - a) * forget;

        // Close the gap to where the accelerometer says down is (gravity's length
        // taken as 1 g, the library's figure with no calibration samples).
        let target = -a / a.length();
        let gap = target - self.grav;
        let dist = gap.length();
        let shaken = ((self.shakiness - SHAKE_MIN) / (SHAKE_MAX - SHAKE_MIN)).clamp(0.0, 1.0);
        let mut speed = STILL_SPEED + (SHAKY_SPEED - STILL_SPEED) * shaken;
        let gyro_cap = (angle_speed * GYRO_FACTOR).max(MIN_SPEED);
        if speed > gyro_cap {
            let far = ((dist - CLOSE_MIN) / (CLOSE_MAX - CLOSE_MIN)).clamp(0.0, 1.0);
            speed = gyro_cap + (speed - gyro_cap) * far;
        }
        let step = speed * dt.max(0.0);
        if dist > 0.0 && step < dist {
            self.grav += gap / dist * step;
        } else {
            self.grav = target;
        }
        // And the orientation's tilt is gravity's: turned, in the world, so the
        // gravity it carries points straight down.
        if let Some(d) = self.down() {
            let world = self.quat * d;
            self.quat = (Quat::from_rotation_arc(world, Vec3::Z) * self.quat).normalize();
        }
        self.down()
    }

    /// The pad's orientation, body to world, in (forward, right, down) with the
    /// world's down along +z: tilt from gravity, heading from the gyro alone.
    pub fn orientation(&self) -> Quat {
        self.quat
    }

    /// Which way is down, in the gyro's (forward, right, down) basis.
    pub fn down(&self) -> Option<Vec3> {
        (self.grav.length_squared() > 1e-12).then(|| self.grav.normalize())
    }

    /// Kept in a node's `aux_f32` (the 3DOF module's state is a flat array).
    pub fn load(s: &[f32]) -> Self {
        let q = Quat::from_xyzw(s[7], s[8], s[9], s[10]);
        GravityFusion {
            grav: Vec3::new(s[0], s[1], s[2]),
            smooth_accel: Vec3::new(s[3], s[4], s[5]),
            shakiness: s[6],
            // Zeros are a state never written: start level.
            quat: if q.length_squared() > 0.5 { q.normalize() } else { Quat::IDENTITY },
        }
    }

    pub fn store(&self, s: &mut [f32]) {
        s[..GRAVITY_FUSION_LEN].copy_from_slice(&[
            self.grav.x,
            self.grav.y,
            self.grav.z,
            self.smooth_accel.x,
            self.smooth_accel.y,
            self.smooth_accel.z,
            self.shakiness,
            self.quat.x,
            self.quat.y,
            self.quat.z,
            self.quat.w,
        ]);
    }
}

/// How steep the camera can look before Orient stops turning its heading in full,
/// as the cosine of the pitch: 75°. Near straight up a nose moved sideways is a
/// big change of heading; past here the camera takes the move a little at a time
/// instead of spinning on the spot.
const ORIENT_COS_CAP: f32 = 0.259;

/// How much of what the cap held back the camera gives back, as a share of how far the
/// nose moves: each degree the nose moves below the cap brings the camera this many degrees
/// nearer where it would be without the cap. Only while the pad moves, so a still pad's
/// pointer stays still.
const ORIENT_PAYBACK: f32 = 0.3;

/// Owed more than this, degrees, the camera and the look without the cap have parted some
/// other way than by the cap (they shouldn't): the look without the cap starts over on the
/// camera's rather than pull it across.
const ORIENT_PAYBACK_GIVE_UP: f32 = 60.0;

/// How much of the way onto the level the camera's pitch comes, as a share of how far the
/// nose moves: each degree the nose moves brings the camera's pitch up to this many degrees
/// nearer the pitch the pad's orientation has (held to gravity). The gyro's turn axis and
/// the accelerometer's down are never quite the same - an accelerometer that reads 0.02 g
/// to the side, or a gyro set a degree askew on the board - and a turn about the real
/// vertical reads, against gravity, as part pitch: about 3° on a half turn, more on every
/// turn after. Bigger than that leak, so it never builds up; small, so it moves the pointer
/// only a little past where the gyro took it, and only while the pad moves.
const ORIENT_LEVEL_SHARE: f32 = 0.1;

/// How many `f32`s [`OrientCamera::store`] writes.
pub(crate) const ORIENT_CAMERA_LEN: usize = 10;

/// `Orient` (3DOF to 2D) and `GYRO_SPACE = ORIENT` (the JSM module): a camera with
/// a heading and a pitch, kept looking where the pad's nose points, its up the
/// pad's up.
///
/// Each tick the nose's own move - the gyro's turn of the pad, nothing else - is
/// taken along the camera's up (pitch) and its level right (heading). Below the
/// poles that is exactly where the nose goes, whatever path it takes, and a twist
/// about the nose moves nothing.
///
/// The heading turns about a vertical the camera carries with it (`frame`): the
/// world's, until the camera goes over the top. There the frame turns over with it
/// - half a turn about the camera's own right - so the camera stays upright in it:
/// its up is still the pad's up, its right the pad's right, and a game with no cap
/// on its pitch - a spacecraft - loops with the pad, turning the way the pad turns.
/// (Kept upside down in the world's frame instead, the heading came out mirrored
/// past the pole; measured from the world's vertical alone, it turned round.)
///
/// A twist about the nose moves the look nowhere, so it can't move the camera -
/// but it turns the pad's up away from the camera's. Once they are more than
/// about 107° apart the camera turns over about its own look, which sends
/// nothing (the look stays where it is) and gives it the pad's up again: a pad
/// rolled upside down and back level never leaves the camera upside down
/// against it, mirroring the outputs.
///
/// The gyro moves the camera, gravity never does: the orientation the move is
/// measured in is held to gravity (`GravityFusion::orientation`), but bringing it
/// back after the gyro drifted turns the frame, not the camera, so a still pad's
/// pointer stays still.
///
/// Only the pitch is brought back, and only while the pad moves: a turn about the real
/// vertical, which the gyro and the accelerometer never quite agree on, reads as part
/// pitch, and the camera added it up - a few degrees down (or up) on every half turn,
/// where the pad's own orientation, held to gravity, stayed level. Each move now brings
/// the camera's pitch a share of the way onto the orientation's ([`ORIENT_LEVEL_SHARE`]).
///
/// What the heading's cap near a pole holds back is given back below it: the camera
/// carries along the look it would have without the cap (`trail`, turned by the nose's
/// own moves and nothing else), and once it's under the cap again it closes on that look
/// by a share of each move the nose makes. Up past 75°, turned and back down, it comes
/// down where the nose is rather than short of it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct OrientCamera {
    /// Heading in `frame`, degrees, right positive.
    psi: f32,
    /// Pitch in `frame`, up from its level, degrees: within ±90.
    theta: f32,
    valid: bool,
    /// The frame the heading and pitch are in, to the world - both (forward,
    /// right, down). The identity until the camera first goes over a pole.
    frame: Quat,
    /// Where the camera would look without the heading's cap, in the world: carried by the
    /// nose's moves. Zero until the camera starts.
    trail: Vec3,
}

/// The look of a camera at heading `psi` and pitch `theta` (degrees), in its frame.
fn look(psi: f32, theta: f32) -> Vec3 {
    let (st, ct) = theta.to_radians().sin_cos();
    let (sp, cp) = psi.to_radians().sin_cos();
    Vec3::new(ct * cp, ct * sp, -st)
}

impl OrientCamera {
    pub fn load(s: &[f32]) -> Self {
        let f = Quat::from_xyzw(s[3], s[4], s[5], s[6]);
        OrientCamera {
            psi: s[0],
            theta: s[1],
            valid: s[2] > 0.5,
            frame: if f.length_squared() > 0.5 { f.normalize() } else { Quat::IDENTITY },
            trail: Vec3::new(s[7], s[8], s[9]),
        }
    }

    pub fn store(&self, s: &mut [f32]) {
        s[..ORIENT_CAMERA_LEN].copy_from_slice(&[
            self.psi,
            self.theta,
            if self.valid { 1.0 } else { 0.0 },
            self.frame.x,
            self.frame.y,
            self.frame.z,
            self.frame.w,
            self.trail.x,
            self.trail.y,
            self.trail.z,
        ]);
    }

    /// Where the camera looks, in the world.
    fn world_look(&self) -> Vec3 {
        self.frame * look(self.psi, self.theta)
    }

    /// Where the camera looks in the world, as a heading and a pitch in degrees -
    /// past ±90 over the top while it is upside down there, so the two run on
    /// without a jump as it loops.
    pub fn world_aim(&self) -> (f32, f32) {
        let l = self.frame * look(self.psi, self.theta);
        let heading = l.y.atan2(l.x).to_degrees();
        let pitch = (-l.z).clamp(-1.0, 1.0).asin().to_degrees();
        let wrap = |d: f32| (d + 180.0).rem_euclid(360.0) - 180.0;
        // Upside down in the world: the frame's up points down.
        if (self.frame * Vec3::NEG_Z).z > 0.0 {
            (wrap(heading + 180.0), wrap(180.0 - pitch))
        } else {
            (heading, pitch)
        }
    }

    /// Start over looking where the nose of `q` (body to world, (forward, right,
    /// down), the world's down +z) points, measured from the neutral `hold`, level
    /// in the world.
    pub fn aim_at(&mut self, q: Quat, hold: Option<Quat>) {
        let n = nose(q, hold);
        self.psi = n.y.atan2(n.x).to_degrees();
        self.theta = (-n.z).clamp(-1.0, 1.0).asin().to_degrees();
        self.frame = Quat::IDENTITY;
        self.valid = true;
        self.trail = self.world_look();
    }

    /// A share `t` of the way from where the camera looks onto where the nose of `q`
    /// points, level in the world: Auto re-center's pull while the pad lies still. It
    /// moves no output - the camera, like a Reset's, starts over on the nose - so the
    /// pointer and the pad come back together after the gyro read a turn the pad
    /// didn't make (a pad just connected, its calibration still settling), which
    /// gravity takes out of the pad but never out of the camera.
    pub fn ease_onto(&mut self, q: Quat, hold: Option<Quat>, t: f32) {
        let target = nose(q, hold);
        let cur = self.frame * look(self.psi, self.theta);
        let l = (Quat::IDENTITY.slerp(Quat::from_rotation_arc(cur, target), t.clamp(0.0, 1.0)) * cur).normalize();
        self.psi = l.y.atan2(l.x).to_degrees();
        self.theta = (-l.z).clamp(-1.0, 1.0).asin().to_degrees();
        self.frame = Quat::IDENTITY;
        self.valid = true;
        self.trail = self.world_look();
    }

    /// One tick: the pad at `q_prev` (as it was before this tick's reading) turned
    /// by `gyro_dps` for `dt`, and `q_held` as it is after the reading, held to
    /// gravity (the level the pitch is brought back onto). Returns how far the heading
    /// and the pitch moved, degrees, and whether the camera has just started (and moved
    /// nowhere).
    pub fn step(&mut self, q_prev: Quat, q_held: Quat, gyro_dps: Vec3, dt: f32, hold: Option<Quat>) -> (f32, f32, bool) {
        if !self.valid {
            self.aim_at(q_prev, hold);
            return (0.0, 0.0, true);
        }
        let speed = gyro_dps.length().to_radians();
        if speed <= 0.0 || dt <= 0.0 {
            return (0.0, 0.0, false);
        }
        // Up with the pad before its move is measured against the camera's axes.
        self.keep_up(hold.map_or(q_prev, |h| q_prev * h.inverse()) * Vec3::NEG_Z);
        let q_now = q_prev * Quat::from_axis_angle(gyro_dps / gyro_dps.length(), speed * dt);
        let (n_prev, n_now) = (nose(q_prev, hold).normalize(), nose(q_now, hold).normalize());
        // The look without the cap, turned as the nose turned.
        let trail = if self.trail.length_squared() > 0.5 { self.trail } else { self.world_look() };
        let trail = (arc(n_prev, n_now) * trail).normalize();
        // The nose's move, in the camera's frame.
        let moved = self.frame.inverse() * (n_now - n_prev);
        let (st, ct) = self.theta.to_radians().sin_cos();
        let (sp, cp) = self.psi.to_radians().sin_cos();
        // The camera's level right, and its up: which way more pitch moves the look.
        let right = Vec3::new(-sp, cp, 0.0);
        let up = Vec3::new(-st * cp, -st * sp, -ct);
        let d_theta = moved.dot(up).to_degrees();
        let d_psi = (moved.dot(right) / ct.max(ORIENT_COS_CAP)).to_degrees();
        self.psi = ((self.psi + d_psi) + 180.0).rem_euclid(360.0) - 180.0;
        self.theta += d_theta;
        if self.theta.abs() > 90.0 {
            self.turn_over();
        }
        let moved = moved.length().to_degrees();
        let (pay_psi, pay_theta) = self.pay_back(trail, moved);
        let lvl_theta = self.level(nose(q_held, hold), moved);
        (d_psi + pay_psi, d_theta + pay_theta + lvl_theta, false)
    }

    /// Below the cap, the pitch a share of the nose's move (`moved`, degrees) of the way
    /// onto the pitch of `held` - the nose of the orientation held to gravity, in the
    /// world - measured along the camera's own up. The look without the cap is tilted
    /// with it, so the cap's payback doesn't pull the pitch back. Returns how far the
    /// pitch moved.
    fn level(&mut self, held: Vec3, moved: f32) -> f32 {
        let (st, ct) = self.theta.to_radians().sin_cos();
        if ct <= ORIENT_COS_CAP || moved <= 0.0 {
            return 0.0;
        }
        let (sp, cp) = self.psi.to_radians().sin_cos();
        let l = self.frame * look(self.psi, self.theta);
        let up = self.frame * Vec3::new(-st * cp, -st * sp, -ct);
        let ahead = held.dot(l);
        if ahead <= 0.0 {
            return 0.0;
        }
        let gap = held.dot(up).atan2(ahead).to_degrees();
        let cap = ORIENT_LEVEL_SHARE * moved;
        let d = gap.clamp(-cap, cap);
        if d == 0.0 {
            return 0.0;
        }
        self.theta += d;
        // More pitch is a turn about the camera's level right.
        let right = self.frame * Vec3::new(-sp, cp, 0.0);
        if self.trail.length_squared() > 0.5 {
            self.trail = (Quat::from_axis_angle(right, d.to_radians()) * self.trail).normalize();
        }
        d
    }

    /// Below the cap, a share of the nose's move (`moved`, degrees) of the way onto the look
    /// without the cap (`trail`, in the world), which it then keeps. Returns how far the
    /// heading and the pitch moved.
    fn pay_back(&mut self, trail: Vec3, moved: f32) -> (f32, f32) {
        self.trail = trail;
        let ct = self.theta.to_radians().cos();
        if ct <= ORIENT_COS_CAP {
            return (0.0, 0.0);
        }
        let l = self.frame.inverse() * trail;
        let theta_t = (-l.z).clamp(-1.0, 1.0).asin().to_degrees();
        if theta_t.to_radians().cos() <= ORIENT_COS_CAP {
            return (0.0, 0.0);
        }
        let owed_psi = (l.y.atan2(l.x).to_degrees() - self.psi + 180.0).rem_euclid(360.0) - 180.0;
        let owed_theta = theta_t - self.theta;
        // How far apart the two looks are, degrees on the sphere.
        let apart = (owed_psi * ct).hypot(owed_theta);
        if apart < 1e-4 {
            return (0.0, 0.0);
        }
        if apart > ORIENT_PAYBACK_GIVE_UP {
            self.trail = self.world_look();
            return (0.0, 0.0);
        }
        let share = (ORIENT_PAYBACK * moved / apart).min(1.0);
        let (d_psi, d_theta) = (owed_psi * share, owed_theta * share);
        self.psi = ((self.psi + d_psi) + 180.0).rem_euclid(360.0) - 180.0;
        self.theta += d_theta;
        (d_psi, d_theta)
    }

    /// The camera's up, kept on the pad's (`pad_up`, in the world): more than ~107°
    /// apart - the pad twisted round its nose - the frame turns half a turn about the
    /// look. The heading and pitch in the frame stay as they are, so the look does,
    /// and the up and the right come round.
    fn keep_up(&mut self, pad_up: Vec3) {
        let (st, ct) = self.theta.to_radians().sin_cos();
        let (sp, cp) = self.psi.to_radians().sin_cos();
        let up = self.frame * Vec3::new(-st * cp, -st * sp, -ct);
        if up.dot(pad_up) < -0.3 {
            let l = self.frame * look(self.psi, self.theta);
            self.frame = (Quat::from_axis_angle(l, std::f32::consts::PI) * self.frame).normalize();
        }
    }

    /// Past straight up or down: the frame turns over with the camera - half a turn
    /// about the camera's right - so the camera is upright in it again, looking the
    /// same way with the same right.
    fn turn_over(&mut self) {
        let (sp, cp) = self.psi.to_radians().sin_cos();
        let right = self.frame * Vec3::new(-sp, cp, 0.0);
        let l = self.frame * look(self.psi, self.theta);
        self.frame = (Quat::from_axis_angle(right, std::f32::consts::PI) * self.frame).normalize();
        let back = self.frame.inverse();
        let (r, l) = (back * right, back * l);
        self.psi = (-r.x).atan2(r.y).to_degrees();
        let (sp, cp) = self.psi.to_radians().sin_cos();
        self.theta = (-l.z).atan2(l.x * cp + l.y * sp).to_degrees();
    }
}

/// The shortest turn from unit `from` to unit `to`, however small: `Quat::from_rotation_arc`
/// calls a turn of under a few hundredths of a degree none, which is a tick's move of a nose
/// near a pole at 1 kHz.
fn arc(from: Vec3, to: Vec3) -> Quat {
    let c = from.cross(to);
    let w = 1.0 + from.dot(to);
    if w < 1e-6 {
        return Quat::from_rotation_arc(from, to);
    }
    Quat::from_xyzw(c.x, c.y, c.z, w).normalize()
}

/// The nose of `q` measured from the neutral `hold`, in the world.
fn nose(q: Quat, hold: Option<Quat>) -> Vec3 {
    hold.map_or(q, |h| q * h.inverse()) * Vec3::X
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 1000.0;

    /// A pad's true pose (canonical body to world) and what it reads: the gyro in
    /// (forward, right, down) deg/s, the accelerometer's up in (forward, left, up).
    fn reading(q: Quat, w_canon_dps: Vec3) -> (Vec3, Vec3) {
        let up = q.inverse() * Vec3::Z;
        (Vec3::new(w_canon_dps.x, -w_canon_dps.y, -w_canon_dps.z), up)
    }

    /// The true down, in the gyro's basis.
    fn true_down(q: Quat) -> Vec3 {
        let up = q.inverse() * Vec3::Z;
        Vec3::new(-up.x, up.y, up.z)
    }

    fn settle(f: &mut GravityFusion, q: Quat, secs: f32) {
        for _ in 0..(secs / DT) as usize {
            let (g, a) = reading(q, Vec3::ZERO);
            f.update(g, Some(a), DT);
        }
    }

    // Down is known from the first reading: it starts at nothing and grows, but
    // points the accelerometer's way at once.
    #[test]
    fn the_first_reading_says_which_way_is_down() {
        let mut f = GravityFusion::default();
        assert_eq!(f.down(), None);
        let d = f.update(Vec3::ZERO, Some(Vec3::Z), DT).unwrap();
        assert!((d - Vec3::Z).length() < 1e-5, "{d:?}");
    }

    // Tipped 60° in a tenth of a second, down is still where it is: the gyro
    // carried it. A low-passed accelerometer would still be most of the way flat.
    #[test]
    fn the_gyro_carries_down_through_a_quick_tilt() {
        let mut f = GravityFusion::default();
        let mut q = Quat::IDENTITY;
        settle(&mut f, q, 3.0);
        let axis = Vec3::NEG_Y; // nose up, about the pad's own left
        let n = 100;
        for _ in 0..n {
            q = (q * Quat::from_axis_angle(axis, (60.0f32 / n as f32).to_radians())).normalize();
            let (g, a) = reading(q, axis * (60.0 / (n as f32 * DT)));
            f.update(g, Some(a), DT);
        }
        let err = f.down().unwrap().angle_between(true_down(q)).to_degrees();
        assert!(err < 0.5, "down is {err}° off after the tilt");
    }

    // Shaken while held still - the accelerometer swinging a quarter g side to
    // side - down barely moves: a shake isn't a tilt.
    #[test]
    fn a_shake_is_not_a_tilt() {
        let mut f = GravityFusion::default();
        settle(&mut f, Quat::IDENTITY, 3.0);
        for k in 0..1000 {
            let side = 0.25 * (k as f32 * DT * std::f32::consts::TAU * 6.0).sin();
            f.update(Vec3::ZERO, Some(Vec3::new(0.0, side, 1.0)), DT);
        }
        let err = f.down().unwrap().angle_between(Vec3::Z).to_degrees();
        assert!(err < 2.0, "a second's shake moved down {err}°");
    }

    // The gyro's own error is pulled back out: told the pad turned when it didn't,
    // down drifts with it, and the still accelerometer brings it home.
    #[test]
    fn the_accelerometer_takes_out_the_gyros_drift() {
        let mut f = GravityFusion::default();
        settle(&mut f, Quat::IDENTITY, 3.0);
        // Half a second of a phantom 40°/s roll: 20° of error.
        for _ in 0..500 {
            f.update(Vec3::new(40.0, 0.0, 0.0), Some(Vec3::Z), DT);
        }
        let off = f.down().unwrap().angle_between(Vec3::Z).to_degrees();
        assert!(off > 5.0, "the drift is the gyro's: {off}°");
        settle(&mut f, Quat::IDENTITY, 3.0);
        let err = f.down().unwrap().angle_between(Vec3::Z).to_degrees();
        assert!(err < 3.0, "{err}° left after three still seconds");
    }

    // The orientation it keeps is the pad's, tilt and all, through a turn that
    // goes over the top: its nose where the pad's is.
    #[test]
    fn its_orientation_is_the_pads_over_the_top() {
        let mut f = GravityFusion::default();
        let mut q = Quat::IDENTITY;
        settle(&mut f, q, 1.0);
        let axis = Vec3::NEG_Y; // nose up
        for _ in 0..400 {
            q = (q * Quat::from_axis_angle(axis, 0.3f32.to_radians())).normalize();
            let (g, a) = reading(q, axis * (0.3 / DT));
            f.update(g, Some(a), DT);
        }
        // 120° up: over the top, pointing back and up. The library's world is
        // (forward, right, down); the pad's canonical (forward, left, up).
        let nose = f.orientation() * Vec3::X;
        let real = q * Vec3::X;
        let real = Vec3::new(real.x, -real.y, -real.z);
        assert!(nose.angle_between(real).to_degrees() < 1.0, "{nose:?} vs {real:?}");
    }

    // Below the heading's cap nothing is held back, so nothing is given back: through a
    // wobble that never goes near 75° up, the look without the cap stays on the camera's,
    // and the camera moves as it did without it.
    #[test]
    fn below_the_cap_the_camera_owes_nothing() {
        let mut f = GravityFusion::default();
        let mut q = Quat::IDENTITY;
        settle(&mut f, q, 0.5);
        let mut cam = OrientCamera::default();
        for k in 0..4000 {
            let t = k as f32 * DT;
            let w = Vec3::new(60.0 * (t * 2.3).sin(), 50.0 * (t * 1.7).cos(), 70.0 * (t * 3.1).sin());
            q = (q * Quat::from_scaled_axis(w * DT.to_radians())).normalize();
            let (g, a) = reading(q, w);
            let before = f.orientation();
            f.update(g, Some(a), DT);
            cam.step(before, f.orientation(), g, DT, None);
            assert!(cam.theta.abs() < 70.0, "the wobble stays under the cap: {}", cam.theta);
        }
        let apart = cam.trail.angle_between(cam.world_look()).to_degrees();
        assert!(apart < 0.05, "the look without the cap parted from the camera's by {apart}°");
    }

    #[test]
    fn it_keeps_in_a_flat_array() {
        let mut f = GravityFusion::default();
        f.update(Vec3::new(10.0, -5.0, 3.0), Some(Vec3::new(0.1, 0.2, 0.9)), DT);
        let mut s = [0.0; GRAVITY_FUSION_LEN];
        f.store(&mut s);
        assert_eq!(GravityFusion::load(&s), f);
    }
}
