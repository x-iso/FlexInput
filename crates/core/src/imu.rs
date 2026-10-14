//! The pad's motion as mapping-card INPUTS: gyro rotation, acceleration with
//! gravity taken out, and tilt.
//!
//! A Remapper card can take `imu_gyro_yaw_right` as an input the way it takes
//! `left_stick_up`: an analog magnitude, shaped by the card's curve and gated
//! by its threshold. These pins are derived inside the evaluator from the
//! canonical `gyro_*` / `accel_*` pins and never published — a card reading
//! them claims nothing, so the same gyro still reaches a gyro-to-mouse module
//! further down the same AutoMap wire.
//!
//! Three families, each in its own unit:
//!
//! * **Gyro** — rotation speed, °/s. Pitch (nose up/down), yaw (turn
//!   left/right), roll (lean left/right).
//! * **Accel** — linear acceleration, g, with gravity REMOVED: pushing,
//!   shaking or thrusting the pad, not how it is held. Resting flat reads 0.
//! * **Tilt** — the pad's attitude from gravity, degrees: how far the nose is
//!   raised and how far the right grip is dropped, whatever the pose.
//!
//! Each comes as a two-sided AXIS pin (signed: one card can steer both ways)
//! and as one-sided DIRECTION pins (0 at rest, rising one way only).
//!
//! ## Frames
//!
//! The bus carries gyro in basis (F, R, −U) and accel in (F, −R, U) — F the
//! nose, R the pad's right, U its face — scaled so ±1.0 is ±[`BUS_GYRO_DPS`]
//! and ±[`BUS_ACCEL_G`]. Everything here works in the GYRO basis, the accel
//! converted in by `diag(1, −1, −1)`. Mixing the two bases unconverted is the
//! bug class gyro3dof's Player/World and drift fix both had.

use glam::Vec3;

/// ±1.0 on a `gyro_*` pin, in °/s (`flexinput_devices::gyro::GYRO_REF_DPS`).
pub const BUS_GYRO_DPS: f32 = 2000.0;
/// ±1.0 on an `accel_*` pin, in g (`flexinput_devices::gyro::ACCEL_REF_G`).
pub const BUS_ACCEL_G: f32 = 8.0;

/// Card param holding the Log/Exp factor (−1..1) applied after the range.
pub const IMU_LOG_KEY: &str = "imu_log";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImuKind {
    Gyro,
    Accel,
    Tilt,
}

impl ImuKind {
    pub const ALL: [ImuKind; 3] = [ImuKind::Gyro, ImuKind::Accel, ImuKind::Tilt];

    /// What 100% on the card's curve means when the card doesn't say.
    pub fn default_range(self) -> f32 {
        match self {
            ImuKind::Gyro => 360.0,
            ImuKind::Accel => 1.0,
            ImuKind::Tilt => 45.0,
        }
    }

    /// The range field's limits — wide enough for any pad's sensor.
    pub fn range_limits(self) -> (f32, f32) {
        match self {
            ImuKind::Gyro => (10.0, 2000.0),
            ImuKind::Accel => (0.05, 8.0),
            ImuKind::Tilt => (1.0, 90.0),
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            ImuKind::Gyro => "°/s",
            ImuKind::Accel => "g",
            ImuKind::Tilt => "°",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ImuKind::Gyro => "Gyro",
            ImuKind::Accel => "Accel",
            ImuKind::Tilt => "Tilt",
        }
    }

    /// Card param holding this family's range.
    pub fn range_key(self) -> &'static str {
        match self {
            ImuKind::Gyro => "imu_range_gyro",
            ImuKind::Accel => "imu_range_accel",
            ImuKind::Tilt => "imu_range_tilt",
        }
    }
}

/// One signed physical quantity [`ImuEstimator`] measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImuAxis {
    /// °/s, + nose up.
    GyroPitch,
    /// °/s, + turning right.
    GyroYaw,
    /// °/s, + right grip going down.
    GyroRoll,
    /// g, + upward (out of the face).
    AccelUp,
    /// g, + to the pad's right.
    AccelRight,
    /// g, + toward the nose.
    AccelForward,
    /// °, + nose raised.
    TiltPitch,
    /// °, + right grip lowered.
    TiltRoll,
}

const N_AXES: usize = 8;

pub struct ImuPin {
    pub id: &'static str,
    /// Full name, for chips and tooltips.
    pub name: &'static str,
    /// A few characters, for the picker's cell.
    pub short: &'static str,
    pub kind: ImuKind,
    pub axis: ImuAxis,
    /// `None` = the two-sided axis; `Some(±1)` = only that direction, 0..
    pub sign: Option<f32>,
}

const fn pin(
    id: &'static str,
    name: &'static str,
    short: &'static str,
    kind: ImuKind,
    axis: ImuAxis,
    sign: Option<f32>,
) -> ImuPin {
    ImuPin { id, name, short, kind, axis, sign }
}

use ImuAxis as A;
use ImuKind as K;

/// Every IMU input pin, in picker order: per family, per axis, the two-sided
/// pin then its two directions.
pub static IMU_PINS: &[ImuPin] = &[
    pin("imu_gyro_pitch", "Gyro Pitch ↕", "P↕", K::Gyro, A::GyroPitch, None),
    pin("imu_gyro_pitch_up", "Gyro Pitch Up", "P↑", K::Gyro, A::GyroPitch, Some(1.0)),
    pin("imu_gyro_pitch_down", "Gyro Pitch Down", "P↓", K::Gyro, A::GyroPitch, Some(-1.0)),
    pin("imu_gyro_yaw", "Gyro Yaw ↔", "Y↔", K::Gyro, A::GyroYaw, None),
    pin("imu_gyro_yaw_left", "Gyro Yaw Left", "Y←", K::Gyro, A::GyroYaw, Some(-1.0)),
    pin("imu_gyro_yaw_right", "Gyro Yaw Right", "Y→", K::Gyro, A::GyroYaw, Some(1.0)),
    pin("imu_gyro_roll", "Gyro Roll ↔", "R↔", K::Gyro, A::GyroRoll, None),
    pin("imu_gyro_roll_left", "Gyro Roll Left", "R←", K::Gyro, A::GyroRoll, Some(-1.0)),
    pin("imu_gyro_roll_right", "Gyro Roll Right", "R→", K::Gyro, A::GyroRoll, Some(1.0)),
    pin("imu_accel_ud", "Accel Up/Down", "↕", K::Accel, A::AccelUp, None),
    pin("imu_accel_up", "Accel Up", "↑", K::Accel, A::AccelUp, Some(1.0)),
    pin("imu_accel_down", "Accel Down", "↓", K::Accel, A::AccelUp, Some(-1.0)),
    pin("imu_accel_lr", "Accel Left/Right", "↔", K::Accel, A::AccelRight, None),
    pin("imu_accel_left", "Accel Left", "←", K::Accel, A::AccelRight, Some(-1.0)),
    pin("imu_accel_right", "Accel Right", "→", K::Accel, A::AccelRight, Some(1.0)),
    pin("imu_accel_fb", "Accel Forward/Backward", "F/B", K::Accel, A::AccelForward, None),
    pin("imu_accel_forward", "Accel Forward", "Fwd", K::Accel, A::AccelForward, Some(1.0)),
    pin("imu_accel_backward", "Accel Backward", "Back", K::Accel, A::AccelForward, Some(-1.0)),
    pin("imu_tilt_pitch", "Tilt Nose ↕", "N↕", K::Tilt, A::TiltPitch, None),
    pin("imu_tilt_up", "Tilt Nose Up", "N↑", K::Tilt, A::TiltPitch, Some(1.0)),
    pin("imu_tilt_down", "Tilt Nose Down", "N↓", K::Tilt, A::TiltPitch, Some(-1.0)),
    pin("imu_tilt_roll", "Tilt Left/Right", "T↔", K::Tilt, A::TiltRoll, None),
    pin("imu_tilt_left", "Tilt Left", "T←", K::Tilt, A::TiltRoll, Some(-1.0)),
    pin("imu_tilt_right", "Tilt Right", "T→", K::Tilt, A::TiltRoll, Some(1.0)),
];

pub fn imu_pin(id: &str) -> Option<&'static ImuPin> {
    if !id.starts_with("imu_") {
        return None;
    }
    IMU_PINS.iter().find(|p| p.id == id)
}

pub fn is_imu_pin(id: &str) -> bool {
    imu_pin(id).is_some()
}

/// **Set Neutral Forward** — a mapping-card OUTPUT, never an input. While a card
/// sends it, the pad(s) feeding that card take the way they point now as
/// forward: their orientation's yaw is re-zeroed, roll and pitch stay on
/// gravity. Let go and the new forward holds — so it doubles as a clutch, and
/// as the cure for a heading that has crept. The engine applies it to a pad's
/// own orientation pin (a Joy-Con 2's) and the 3DOF module to the one it
/// computes.
pub const ORIENT_NEUTRAL_PIN: &str = "orient_neutral";
pub const ORIENT_NEUTRAL_NAME: &str = "Set Neutral Forward";

/// The families a card's inputs read, in [`ImuKind::ALL`] order — one range
/// field each on the card.
pub fn kinds_read<'a>(pins: impl IntoIterator<Item = &'a str>) -> Vec<ImuKind> {
    let mut seen = [false; 3];
    for p in pins {
        if let Some(p) = imu_pin(p) {
            seen[p.kind as usize] = true;
        }
    }
    ImuKind::ALL.into_iter().filter(|k| seen[*k as usize]).collect()
}

impl ImuPin {
    /// This pin's physical value in `r`: signed for an axis, ≥ 0 for a
    /// direction.
    pub fn physical(&self, r: &ImuReading) -> f32 {
        let v = r.0[self.axis as usize];
        match self.sign {
            None => v,
            Some(s) => (v * s).max(0.0),
        }
    }

    /// The value the evaluator carries on its pin map: the physical value over
    /// the family's DEFAULT range. A card rescales it to its own range with
    /// [`ImuScale::normalize`]; anything reading the pin without a card reads
    /// it as a 0..1-ish value, so "≥ 0.5" means half the default range.
    pub fn bus_value(&self, r: &ImuReading) -> f32 {
        self.physical(r) / self.kind.default_range()
    }
}

/// One tick's physical IMU quantities, indexed by [`ImuAxis`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ImuReading(pub [f32; N_AXES]);

impl ImuReading {
    pub fn get(&self, axis: ImuAxis) -> f32 {
        self.0[axis as usize]
    }
}

/// The Log/Exp curve of the Response Curve module, on a 0..1 magnitude: t = 0
/// is linear, t < 0 log-like (raises small values), t > 0 exp-like.
pub fn log_exp(x: f32, t: f32) -> f32 {
    if t.abs() < 1e-4 {
        return x;
    }
    x.clamp(0.0, 1.0).powf(2.0f32.powf(t.clamp(-1.0, 1.0) * 3.0))
}

/// A card's range per family and its Log/Exp factor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImuScale {
    pub ranges: [f32; 3],
    pub log: f32,
}

impl Default for ImuScale {
    fn default() -> Self {
        ImuScale {
            ranges: ImuKind::ALL.map(ImuKind::default_range),
            log: 0.0,
        }
    }
}

impl ImuScale {
    pub fn from_card(card: &serde_json::Value) -> Self {
        let mut s = ImuScale::default();
        for (i, k) in ImuKind::ALL.into_iter().enumerate() {
            if let Some(v) = card.get(k.range_key()).and_then(|v| v.as_f64()) {
                let (lo, hi) = k.range_limits();
                s.ranges[i] = (v as f32).clamp(lo, hi);
            }
        }
        if let Some(v) = card.get(IMU_LOG_KEY).and_then(|v| v.as_f64()) {
            s.log = (v as f32).clamp(-1.0, 1.0);
        }
        s
    }

    pub fn range(&self, kind: ImuKind) -> f32 {
        self.ranges[kind as usize]
    }

    /// A pin's evaluator value ([`ImuPin::bus_value`]) as this card sees it:
    /// over the card's range, clamped to 1, through Log/Exp. Keeps the sign of
    /// a two-sided axis.
    pub fn normalize(&self, pin: &ImuPin, bus_value: f32) -> f32 {
        let physical = bus_value * pin.kind.default_range();
        let m = (physical.abs() / self.range(pin.kind)).clamp(0.0, 1.0);
        physical.signum() * log_exp(m, self.log)
    }
}

/// How quickly the gravity estimate gives way to the accelerometer. The gyro
/// carries it through a turn, so this only has to correct drift — long enough
/// that a half-second thrust isn't taken for a change in which way is down.
const GRAVITY_TAU: f32 = 0.5;

/// Gyro rates and the gravity estimate that takes gravity out of the
/// accelerometer and says how the pad is tilted.
///
/// Gravity (the accelerometer's at-rest reading, pointing UP in the pad) is
/// carried through rotations by the gyro and eased toward the accelerometer, so
/// turning the pad doesn't read as acceleration the way a plain low-pass would.
#[derive(Clone, Debug, Default)]
pub struct ImuEstimator {
    /// Gravity reaction in the gyro basis, g.
    up: Vec3,
    settled: bool,
}

impl ImuEstimator {
    /// Advance by `dt` seconds with the bus's `gyro_*` and `accel_*` values
    /// (`accel` `None` on a pad without one: tilt and accel then read 0).
    pub fn tick(&mut self, gyro_bus: Vec3, accel_bus: Option<Vec3>, dt: f32) -> ImuReading {
        let dps = gyro_bus * BUS_GYRO_DPS;
        let mut r = [0.0f32; N_AXES];
        r[A::GyroPitch as usize] = dps.y;
        r[A::GyroYaw as usize] = dps.z;
        r[A::GyroRoll as usize] = dps.x;

        let Some(accel_bus) = accel_bus else { return ImuReading(r) };
        // Accel into the gyro basis, in g.
        let a = Vec3::new(accel_bus.x, -accel_bus.y, -accel_bus.z) * BUS_ACCEL_G;
        if !self.settled {
            if a.length() < 0.1 {
                return ImuReading(r);
            }
            // Start on the first reading, not by easing up from zero.
            self.up = a;
            self.settled = true;
        }
        // A world-fixed vector seen from the turning pad: dv/dt = −ω × v.
        let omega = dps * (std::f32::consts::PI / 180.0);
        self.up -= omega.cross(self.up) * dt;
        self.up += (a - self.up) * (1.0 - (-dt / GRAVITY_TAU).exp());

        let lin = a - self.up;
        r[A::AccelUp as usize] = -lin.z;
        r[A::AccelRight as usize] = lin.y;
        r[A::AccelForward as usize] = lin.x;

        if let Some(u) = self.up.try_normalize() {
            // Elevation of the nose / of the right grip above the horizon.
            r[A::TiltPitch as usize] = u.x.clamp(-1.0, 1.0).asin().to_degrees();
            r[A::TiltRoll as usize] = -u.y.clamp(-1.0, 1.0).asin().to_degrees();
        }
        ImuReading(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 250.0;

    /// What the bus reads for a pad at rest whose world-up, in the gyro
    /// basis, is `up` — the inverse of the conversion in `tick`.
    fn accel_bus_for(up_gyro: Vec3) -> Vec3 {
        Vec3::new(up_gyro.x, -up_gyro.y, -up_gyro.z) / BUS_ACCEL_G
    }

    fn settle(e: &mut ImuEstimator, accel: Vec3) -> ImuReading {
        let mut r = ImuReading::default();
        for _ in 0..1000 {
            r = e.tick(Vec3::ZERO, Some(accel), DT);
        }
        r
    }

    #[test]
    fn every_pin_is_unique_and_found_by_its_id() {
        for (i, p) in IMU_PINS.iter().enumerate() {
            assert!(p.id.starts_with("imu_"), "{}", p.id);
            assert!(IMU_PINS.iter().skip(i + 1).all(|q| q.id != p.id), "{} twice", p.id);
            assert!(std::ptr::eq(imu_pin(p.id).unwrap(), p));
        }
        assert!(!is_imu_pin("gyro_x"), "the raw bus pins are not card inputs");
        assert!(!is_imu_pin("imu_nonsense"));
    }

    /// The bus's flat-on-a-table reading (accel_z = +1 g) is the reference.
    #[test]
    fn flat_and_still_reads_nothing() {
        let mut e = ImuEstimator::default();
        let r = settle(&mut e, Vec3::new(0.0, 0.0, 1.0 / BUS_ACCEL_G));
        for (i, v) in r.0.iter().enumerate() {
            assert!(v.abs() < 1e-3, "axis {i} reads {v}");
        }
    }

    /// Canonical frame: accel_x > 0 with the nose up, accel_y > 0 with the
    /// right grip dropped. Tilt reads both in degrees, with those signs.
    #[test]
    fn tilt_reads_the_pose_in_degrees() {
        let mut e = ImuEstimator::default();
        let nose_up_30 = Vec3::new(0.5, 0.0, 0.866) / BUS_ACCEL_G;
        let r = settle(&mut e, nose_up_30);
        assert!((r.get(A::TiltPitch) - 30.0).abs() < 0.5, "{}", r.get(A::TiltPitch));
        assert!(r.get(A::TiltRoll).abs() < 0.5);

        let mut e = ImuEstimator::default();
        let right_down_20 = Vec3::new(0.0, 0.342, 0.940) / BUS_ACCEL_G;
        let r = settle(&mut e, right_down_20);
        assert!((r.get(A::TiltRoll) - 20.0).abs() < 0.5, "{}", r.get(A::TiltRoll));
        // Holding still in a pose is not acceleration.
        assert!(r.get(A::AccelRight).abs() < 1e-3);
        assert!(r.get(A::AccelUp).abs() < 1e-3);
    }

    /// Gravity is carried through a turn by the gyro, so tilting the pad
    /// doesn't read as being shoved — the failure a plain low-pass has.
    #[test]
    fn turning_the_pad_is_not_acceleration() {
        let mut e = ImuEstimator::default();
        settle(&mut e, Vec3::new(0.0, 0.0, 1.0 / BUS_ACCEL_G));
        let rate_dps: f32 = 180.0;
        let mut theta = 0.0f32;
        let mut worst = 0.0f32;
        let mut last = ImuReading::default();
        // Nose up through 90° in half a second. Positive gyro_y is nose-up.
        for _ in 0..125 {
            theta += rate_dps.to_radians() * DT;
            let up = Vec3::new(theta.sin(), 0.0, -theta.cos());
            last = e.tick(Vec3::new(0.0, rate_dps / BUS_GYRO_DPS, 0.0), Some(accel_bus_for(up)), DT);
            let lin = Vec3::new(last.get(A::AccelForward), last.get(A::AccelRight), last.get(A::AccelUp));
            worst = worst.max(lin.length());
        }
        assert!(worst < 0.05, "turning read as {worst} g");
        assert!((last.get(A::GyroPitch) - rate_dps).abs() < 1e-2);
        assert!((last.get(A::TiltPitch) - 90.0).abs() < 2.0, "{}", last.get(A::TiltPitch));
    }

    /// A shove to the pad's right is Accel Right; the accel basis has +y to the
    /// LEFT, so the bus reads it negative.
    #[test]
    fn a_shove_reads_in_the_direction_it_went() {
        let mut e = ImuEstimator::default();
        let flat = Vec3::new(0.0, 0.0, 1.0 / BUS_ACCEL_G);
        settle(&mut e, flat);
        let shove_right = flat + Vec3::new(0.0, -0.5, 0.0) / BUS_ACCEL_G;
        let r = e.tick(Vec3::ZERO, Some(shove_right), DT);
        assert!((r.get(A::AccelRight) - 0.5).abs() < 0.02, "{}", r.get(A::AccelRight));
        let right = imu_pin("imu_accel_right").unwrap();
        let left = imu_pin("imu_accel_left").unwrap();
        assert!(right.physical(&r) > 0.45);
        assert_eq!(left.physical(&r), 0.0, "a direction pin is one-sided");
        assert!(imu_pin("imu_accel_lr").unwrap().physical(&r) > 0.45);
    }

    #[test]
    fn gyro_directions_follow_the_canonical_signs() {
        let mut e = ImuEstimator::default();
        // gyro_z > 0 is a right turn, gyro_x > 0 the right grip going down.
        let r = e.tick(Vec3::new(90.0, 0.0, 180.0) / BUS_GYRO_DPS, None, DT);
        assert!((imu_pin("imu_gyro_yaw_right").unwrap().physical(&r) - 180.0).abs() < 1e-2);
        assert_eq!(imu_pin("imu_gyro_yaw_left").unwrap().physical(&r), 0.0);
        assert!((imu_pin("imu_gyro_roll_right").unwrap().physical(&r) - 90.0).abs() < 1e-2);
        assert!((imu_pin("imu_gyro_yaw").unwrap().physical(&r) - 180.0).abs() < 1e-2);
    }

    /// The card's range sets 100%, Log/Exp bends what's under it, and a
    /// two-sided axis keeps its sign.
    #[test]
    fn a_card_scales_by_its_range_then_log_exp() {
        let yaw = imu_pin("imu_gyro_yaw").unwrap();
        let default = ImuScale::default();
        let bus = |dps: f32| dps / ImuKind::Gyro.default_range();
        assert!((default.normalize(yaw, bus(180.0)) - 0.5).abs() < 1e-5, "360 °/s by default");
        assert!((default.normalize(yaw, bus(-180.0)) + 0.5).abs() < 1e-5);
        assert_eq!(default.normalize(yaw, bus(720.0)), 1.0, "clamped at the range");

        let card = serde_json::json!({ "imu_range_gyro": 90.0, "imu_log": -1.0 });
        let s = ImuScale::from_card(&card);
        assert_eq!(s.range(ImuKind::Gyro), 90.0);
        assert_eq!(s.range(ImuKind::Tilt), 45.0, "other families keep their default");
        // 45 of 90 °/s is 0.5, then t = -1: 0.5^(1/8).
        assert!((s.normalize(yaw, bus(45.0)) - 0.5f32.powf(0.125)).abs() < 1e-4);

        let silly = serde_json::json!({ "imu_range_gyro": 0.0 });
        assert_eq!(ImuScale::from_card(&silly).range(ImuKind::Gyro), 10.0, "never a zero range");
    }
}
