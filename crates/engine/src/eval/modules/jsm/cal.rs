//! Real-world calibration sweep: the user turns the camera a known amount, and
//! the constant that would have made it a 1:1 physical rotation is solved from
//! how far the pad actually turned.
//!
//! The same measurement RWS Aim makes, on JSM's constants. While a sweep runs
//! the config's own aiming is set aside and the game is driven from the raw pad
//! rotation at the config's CURRENT calibration — the whole config (its
//! sensitivity curve, smoothing, acceleration, cutoff, the sticks) would
//! otherwise be folded into the answer, and a curve makes that unsolvable rather
//! than merely wrong. Driving at the current calibration, rather than some fixed
//! constant, is what keeps the physical turn about one rotation instead of
//! however many a coarse in-game sensitivity would demand.
//!
//! The rotation is integrated HERE, at tick rate with the real `dt`, on the one
//! copy of the node the engine evaluates — the UI may render the widget in
//! several windows at different repaint rates, and every one of them has to see
//! the same number. It leaves as a display-only trailing output, the way RWS
//! publishes its own.

use glam::Vec2;

/// Which known turn is being performed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axis {
    /// Camera fully down → fully up: 180°.
    Pitch,
    /// One full turn on the spot: 360°.
    Yaw,
}

impl Axis {
    fn code(self) -> u8 {
        match self {
            Axis::Pitch => 1,
            Axis::Yaw => 2,
        }
    }
}

/// A sweep in progress: which turn, and which output it is solving for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sweep {
    pub axis: Axis,
    /// Solving `VIRTUAL_STICK_CALIBRATION` rather than `REAL_WORLD_CALIBRATION`.
    pub stick: bool,
}

impl Sweep {
    /// Read the node's calibration params, or `None` when no sweep is running.
    ///
    /// Which output is being calibrated is the user's explicit choice
    /// (`cal_output`), not a guess from `GYRO_OUTPUT`: a config can point the
    /// gyro at one and still be tuned for the other, and guessing would drive
    /// the game through the output the answer isn't for.
    pub fn read(params: &std::collections::HashMap<String, serde_json::Value>) -> Option<Sweep> {
        let axis = match params.get("cal_measure").and_then(|v| v.as_str()) {
            Some("pitch") => Axis::Pitch,
            Some("yaw") => Axis::Yaw,
            _ => return None,
        };
        let stick = params.get("cal_output").and_then(|v| v.as_str()) == Some("stick");
        Some(Sweep { axis, stick })
    }
}

/// The running measurement, kept on the node's own state.
#[derive(Default)]
pub struct Measure {
    /// The sweep this total belongs to (0 = none), so switching method restarts
    /// rather than adding one turn to another.
    axis: u8,
    /// Degrees turned so far. SIGNED, so turning back — repositioning, or
    /// correcting an overshoot — subtracts instead of inflating the total.
    pub deg: f32,
    /// The furthest the virtual stick was pushed during the sweep, UNCLAMPED.
    /// Past full deflection the game turns slower than the pad does and a stick
    /// back-solve comes out wrong, so the UI refuses the result instead.
    pub peak: f32,
}

impl Measure {
    /// Integrate one tick. `rate_dps` is the pad's rotation on the measured axis
    /// and `defl` the stick deflection it is producing (zero when calibrating the
    /// mouse). The total is held, not cleared, once the sweep stops — a Finish
    /// reads it on the tick after the button.
    pub fn tick(&mut self, sweep: Option<Sweep>, rate_dps: f32, defl: f32, dt: f32) {
        let Some(sweep) = sweep else {
            self.axis = 0;
            return;
        };
        if self.axis != sweep.axis.code() {
            self.axis = sweep.axis.code();
            self.deg = 0.0;
            self.peak = 0.0;
        }
        self.deg += rate_dps * dt;
        self.peak = self.peak.max(defl.abs());
    }
}

/// What a sweep drives the game with this tick.
///
/// Only the axis being measured moves — the off-axis is blocked, as it is in
/// RWS, so a turn that drifts up or down doesn't smear the measurement into the
/// other axis's constant.
pub struct Drive {
    /// Mouse displacement in bus pixels, when the mouse is what's calibrated.
    pub mouse: Option<Vec2>,
    /// Virtual stick deflection, when the stick is. Already clamped to the unit
    /// range; `Measure::peak` keeps the unclamped magnitude.
    pub stick: Option<Vec2>,
}

/// Work out the sweep's output for this tick.
///
/// `counts_per_deg` is `REAL_WORLD_CALIBRATION / IN_GAME_SENS` — JSM divides the
/// calibration by the in-game sensitivity everywhere it aims, so the sweep has
/// to drive the game through the same number or the answer would be that factor
/// out. `stick_dps` is `VIRTUAL_STICK_CALIBRATION`.
pub fn drive(sweep: Sweep, yaw_dps: f32, pitch_dps: f32, counts_per_deg: f32, stick_dps: f32) -> (Drive, f32, f32) {
    // Mouse axes as `aim.rs` lands them on the bus with JSM's default spaces:
    // x from the pad's yaw, y from its pitch, both counting up.
    let (x_dps, y_dps) = match sweep.axis {
        Axis::Yaw => (yaw_dps, 0.0),
        Axis::Pitch => (0.0, pitch_dps),
    };
    let rate = match sweep.axis {
        Axis::Yaw => yaw_dps,
        Axis::Pitch => pitch_dps,
    };
    if sweep.stick {
        let k = stick_dps.max(1.0);
        let raw = Vec2::new(x_dps / k, y_dps / k);
        let defl = raw.x.abs().max(raw.y.abs());
        let stick = Vec2::new(raw.x.clamp(-1.0, 1.0), raw.y.clamp(-1.0, 1.0));
        (Drive { mouse: Some(Vec2::ZERO), stick: Some(stick) }, rate, defl)
    } else {
        // `dt` is applied by the caller, which owns the tick's length.
        (
            Drive { mouse: Some(Vec2::new(x_dps * counts_per_deg, y_dps * counts_per_deg)), stick: None },
            rate,
            0.0,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(measure: &str, output: &str) -> std::collections::HashMap<String, serde_json::Value> {
        let mut p = std::collections::HashMap::new();
        p.insert("cal_measure".to_string(), serde_json::json!(measure));
        p.insert("cal_output".to_string(), serde_json::json!(output));
        p
    }

    #[test]
    fn a_sweep_is_read_from_the_two_params() {
        assert_eq!(
            Sweep::read(&params("yaw", "mouse")),
            Some(Sweep { axis: Axis::Yaw, stick: false })
        );
        assert_eq!(
            Sweep::read(&params("pitch", "stick")),
            Some(Sweep { axis: Axis::Pitch, stick: true })
        );
        assert_eq!(Sweep::read(&params("off", "mouse")), None);
        assert_eq!(Sweep::read(&std::collections::HashMap::new()), None);
    }

    #[test]
    fn only_the_measured_axis_drives_the_game() {
        // A yaw sweep that drifts 20°/s vertically must not move the camera up:
        // the off-axis is blocked so it can't smear into the other constant.
        let (d, rate, _) = drive(Sweep { axis: Axis::Yaw, stick: false }, 90.0, 20.0, 2.0, 360.0);
        let m = d.mouse.unwrap();
        assert_eq!(m.x, 180.0);
        assert_eq!(m.y, 0.0);
        assert_eq!(rate, 90.0);
        assert!(d.stick.is_none());
    }

    #[test]
    fn a_stick_sweep_reports_deflection_past_full() {
        // Turning twice as fast as the stick can express pins it at 1.0, and the
        // unclamped 2.0 is what tells the UI to refuse the back-solve.
        let (d, _, defl) = drive(Sweep { axis: Axis::Yaw, stick: true }, 720.0, 0.0, 2.0, 360.0);
        assert_eq!(defl, 2.0);
        assert_eq!(d.stick.unwrap().x, 1.0);
        // The mouse is held at zero so a config wired to both can't move through it.
        assert_eq!(d.mouse, Some(Vec2::ZERO));
    }

    #[test]
    fn turning_back_subtracts_and_a_method_change_restarts() {
        let mut m = Measure::default();
        let yaw = Sweep { axis: Axis::Yaw, stick: false };
        m.tick(Some(yaw), 90.0, 0.0, 1.0);
        m.tick(Some(yaw), 90.0, 0.0, 1.0);
        assert_eq!(m.deg, 180.0);
        // Overshooting and coming back takes it off the total again.
        m.tick(Some(yaw), -45.0, 0.0, 1.0);
        assert_eq!(m.deg, 135.0);
        // Switching to the other method starts a fresh sweep.
        m.tick(Some(Sweep { axis: Axis::Pitch, stick: false }), 10.0, 0.0, 1.0);
        assert_eq!(m.deg, 10.0);
    }

    #[test]
    fn the_total_survives_the_tick_the_sweep_stops() {
        // Finish arrives a tick after the button, and has to find the number
        // still there.
        let mut m = Measure::default();
        m.tick(Some(Sweep { axis: Axis::Yaw, stick: false }), 360.0, 0.0, 1.0);
        m.tick(None, 0.0, 0.0, 1.0);
        assert_eq!(m.deg, 360.0);
    }

    #[test]
    fn the_peak_keeps_the_worst_moment_not_the_last() {
        let mut m = Measure::default();
        let s = Sweep { axis: Axis::Yaw, stick: true };
        m.tick(Some(s), 100.0, 1.8, 0.1);
        m.tick(Some(s), 100.0, 0.2, 0.1);
        assert_eq!(m.peak, 1.8);
    }
}
