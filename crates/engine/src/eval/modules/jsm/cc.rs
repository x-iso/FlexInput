//! The `JSM_custom_curve` fork's additions to the gyro pipeline.
//!
//! Surveyed from <https://github.com/evan1mclean/JSM_custom_curve> at commit
//! `0ace2da`, diffed against upstream 3.6.2. Everything the fork adds lives in the
//! gyro path, which is why it all fits here rather than being scattered: five
//! acceleration curves, a decay smoother, the one-euro filter, axial angle snapping,
//! a deceleration brake, and a gyro space that mixes roll into the turn.
//!
//! Kept in its own file because it is a *fork's* vocabulary. A config written for
//! stock JSM never touches any of it, and someone comparing this against upstream
//! should be able to see at a glance what is and isn't Electronicks'.

use glam::Vec3;

/// `ACCEL_CURVE`: the shape of the climb from `MIN_GYRO_SENS` to `MAX_GYRO_SENS`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Curve {
    /// A straight line, which is what stock JSM does.
    #[default]
    Linear,
    Natural,
    Power,
    Quadratic,
    Sigmoid,
    Jump,
}

impl Curve {
    /// Does this curve consult `MAX_GYRO_THRESHOLD` at all?
    ///
    /// Three of the six don't: they take their shape from their own parameters, in
    /// absolute degrees per second. Setting a threshold pair and then switching
    /// curve silently stops the top one mattering, so the editor says so on the
    /// `MAX_GYRO_THRESHOLD` line — this is the question it asks.
    pub fn uses_max_threshold(self) -> bool {
        matches!(self, Curve::Linear | Curve::Quadratic | Curve::Jump)
    }
}

/// The fork's settings.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Settings {
    pub curve: Curve,
    /// `ACCEL_NATURAL_VHALF`: deg/s at which `NATURAL` sits halfway up.
    pub natural_vhalf: f32,
    pub power_vref: f32,
    pub power_exponent: f32,
    pub sigmoid_mid: f32,
    pub sigmoid_width: f32,
    pub jump_tau: f32,
    /// `GYRO_SMOOTHING_DECAY`: swap the rolling-average smoother for a decaying one.
    pub decay_smoothing: bool,
    /// `ONE_EURO_MIN_CUTOFF` / `ONE_EURO_SPEED_COEFF`. The filter itself is switched
    /// on by the `ONE_EURO_FILTER` *command*, not by a setting — see `enabled`.
    pub one_euro_min_cutoff: f32,
    pub one_euro_speed_coeff: f32,
    /// True once the `ONE_EURO_FILTER` command has been seen. It is a command in the
    /// fork too, which matters: it is global and sticky until `RESET_MAPPINGS`, so
    /// unlike the two numbers above it **cannot be chorded**. Reproducing that
    /// asymmetry is deliberate — a config relying on it being sticky would otherwise
    /// behave differently here.
    pub one_euro_enabled: bool,
    /// `GYRO_ANGLE_SNAP`: snap to the nearest axis within this many degrees.
    pub angle_snap: f32,
    pub angle_snap_ease: bool,
    /// `DECEL_BRAKE_STRENGTH`, 0..1.
    pub brake_strength: f32,
    /// `DECEL_BRAKE_THRESHOLD`: deg/s of slowing over 10 ms before braking starts.
    pub brake_threshold: f32,
    /// `ROLL_CONTRIBUTION`: percent of roll mixed into the turn, for `YAW_PLUS_ROLL`.
    pub roll_contribution: f32,
}

impl Default for Settings {
    fn default() -> Self {
        // The fork's own defaults, from its command registry.
        Settings {
            curve: Curve::Linear,
            natural_vhalf: 200.0,
            power_vref: 0.01,
            power_exponent: 0.5,
            sigmoid_mid: 20.0,
            sigmoid_width: 8.0,
            jump_tau: 1.5,
            decay_smoothing: false,
            one_euro_min_cutoff: 6.0,
            one_euro_speed_coeff: 0.3,
            one_euro_enabled: false,
            angle_snap: 0.0,
            angle_snap_ease: false,
            brake_strength: 0.0,
            brake_threshold: 25.0,
            roll_contribution: 0.0,
        }
    }
}

/// The sensitivity this curve gives at speed `omega`, between `lo` and `hi`.
///
/// `omega` is degrees per second **already reduced by `MIN_GYRO_THRESHOLD`**, and
/// `cap` is `MAX_GYRO_THRESHOLD` — used only by the three curves that consult it.
/// `t` is the linear position between the thresholds, which only `LINEAR` wants.
pub fn sensitivity(s: &Settings, omega: f32, t: f32, cap: f32, lo: f32, hi: f32) -> f32 {
    match s.curve {
        Curve::Linear => lo * (1.0 - t) + hi * t,
        // Approaches `hi` without reaching it, halfway at `vhalf`.
        Curve::Natural => {
            if s.natural_vhalf <= 0.0 {
                return hi;
            }
            let k = std::f32::consts::LN_2 / s.natural_vhalf;
            hi - (hi - lo) * (-k * omega).exp()
        }
        Curve::Power => {
            if s.power_vref <= 0.0 {
                return hi;
            }
            if s.power_exponent <= 0.0 || omega <= 0.0 {
                return lo;
            }
            let u = (omega / s.power_vref).powf(s.power_exponent);
            let t = (1.0 - (-u).exp()).clamp(0.0, 1.0);
            lo + (hi - lo) * t
        }
        Curve::Quadratic => {
            if cap <= 0.0 || omega >= cap {
                return hi;
            }
            let t = omega / cap;
            lo + (hi - lo) * t * t
        }
        // Logistic, rescaled so a standing-still pad reads exactly `lo`.
        Curve::Sigmoid => {
            let w = if s.sigmoid_width > 0.0 { s.sigmoid_width } else { 1e-6 };
            let raw = |x: f32| 1.0 / (1.0 + (-((x - s.sigmoid_mid) / w)).exp());
            let at_rest = raw(0.0);
            let denom = 1.0 - at_rest;
            let t = if denom > 0.0 {
                ((raw(omega) - at_rest) / denom).clamp(0.0, 1.0)
            } else {
                0.0
            };
            lo + (hi - lo) * t
        }
        // Rises towards a step at the cap, rescaled the same way.
        Curve::Jump => {
            if s.jump_tau <= 0.0 {
                return if omega < cap { lo } else { hi };
            }
            let raw = |x: f32| {
                if x >= cap {
                    1.0
                } else {
                    ((x - cap) / s.jump_tau).exp()
                }
            };
            let at_rest = raw(0.0);
            let denom = 1.0 - at_rest;
            let t = if denom > 0.0 {
                ((raw(omega) - at_rest) / denom).clamp(0.0, 1.0)
            } else {
                0.0
            };
            lo + (hi - lo) * t
        }
    }
}

/// A first-order low-pass, the building block of the one-euro filter.
#[derive(Default, Clone, Copy)]
struct LowPass {
    prev: f32,
    started: bool,
}

impl LowPass {
    fn filter(&mut self, x: f32, alpha: f32) -> f32 {
        if !self.started {
            self.prev = x;
            self.started = true;
        }
        self.prev = alpha * x + (1.0 - alpha) * self.prev;
        self.prev
    }
}

/// The one-euro filter: a low-pass whose cutoff opens up with how fast the signal is
/// moving, so it is smooth at rest and barely delays a flick. One per axis.
#[derive(Default, Clone, Copy)]
pub struct OneEuro {
    value: LowPass,
    derivative: LowPass,
    prev: f32,
    started: bool,
}

/// The cutoff the derivative itself is filtered at, in Hz (the fork's constant).
const D_CUTOFF: f32 = 1.0;

fn alpha(cutoff: f32, dt: f32) -> f32 {
    let tau = 1.0 / (std::f32::consts::TAU * cutoff.max(1e-6));
    1.0 / (1.0 + tau / dt.max(1e-6))
}

impl OneEuro {
    pub fn filter(&mut self, x: f32, dt: f32, min_cutoff: f32, beta: f32) -> f32 {
        let dx = if self.started { (x - self.prev) / dt.max(1e-6) } else { 0.0 };
        self.prev = x;
        self.started = true;
        let smoothed_dx = self.derivative.filter(dx, alpha(D_CUTOFF, dt));
        let cutoff = min_cutoff + beta * smoothed_dx.abs();
        self.value.filter(x, alpha(cutoff, dt))
    }

    pub fn reset(&mut self) {
        *self = OneEuro::default();
    }
}

/// `GYRO_SMOOTHING_DECAY`'s smoother: an exponential one whose time constant shrinks
/// as the pad speeds up, so slow drift is smoothed hard and a fast turn is not
/// smoothed at all.
#[derive(Default)]
pub struct Decay {
    x: f32,
    y: f32,
    started: bool,
}

impl Decay {
    pub fn smooth(&mut self, x: f32, y: f32, dt: f32, time: f32, threshold: f32) -> (f32, f32) {
        if time <= 0.0 || threshold <= 0.0 {
            self.started = false;
            return (x, y);
        }
        let speed = (x * x + y * y).sqrt();
        // How much of the signal to smooth: all of it at rest, none of it past the
        // threshold, squared so the handover is gentle.
        let fast = (speed / threshold).clamp(0.0, 1.0);
        let smoothed_share = (1.0 - fast) * (1.0 - fast);
        let immediate_share = 1.0 - smoothed_share;
        let target = (x * smoothed_share, y * smoothed_share);
        if !self.started {
            self.x = target.0;
            self.y = target.1;
            self.started = true;
        }
        let effective = time * smoothed_share;
        let a = if effective <= 1e-6 { 1.0 } else { 1.0 - (-dt / effective).exp() };
        self.x += a * (target.0 - self.x);
        self.y += a * (target.1 - self.y);
        (
            self.x + x * immediate_share,
            self.y + y * immediate_share,
        )
    }

    pub fn reset(&mut self) {
        self.started = false;
    }
}

/// `GYRO_ANGLE_SNAP`: within this many degrees of an axis, snap to it — so a turn
/// meant to be level comes out level. The magnitude is preserved, which is the point:
/// snapping should straighten a movement, not slow it.
///
/// **A departure from the fork, deliberately.** Its eased branch sets the surviving
/// axis to the full vector magnitude the moment the direction enters the snap zone,
/// while only fading the other — so right at the edge, where the ease blend is still
/// zero, the output already gains magnitude. That reads as an oversight rather than
/// an intent, so here the surviving axis is blended in over the same curve. At full
/// snap the two agree exactly; in between, this one is smooth.
pub fn angle_snap(s: &Settings, x: f32, y: f32) -> (f32, f32) {
    if s.angle_snap <= 0.0 || (x == 0.0 && y == 0.0) {
        return (x, y);
    }
    let mag = (x * x + y * y).sqrt();
    if mag <= 0.0 {
        return (x, y);
    }
    let snap = s.angle_snap.to_radians();
    // The angle off the horizontal, in the first quadrant.
    let off_horizontal = if x == 0.0 {
        std::f32::consts::FRAC_PI_2
    } else {
        (y / x).abs().atan()
    };
    let smoothstep = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        v * v * (3.0 - 2.0 * v)
    };
    let blend = |how_far_in: f32| {
        if s.angle_snap_ease {
            smoothstep(how_far_in)
        } else {
            1.0
        }
    };
    if off_horizontal > std::f32::consts::FRAC_PI_2 - snap {
        // Nearly vertical: fade x out and y up to the full magnitude.
        let k = blend(1.0 - (std::f32::consts::FRAC_PI_2 - off_horizontal) / snap);
        (x * (1.0 - k), y * (1.0 - k) + mag.copysign(y) * k)
    } else if off_horizontal < snap {
        let k = blend(1.0 - off_horizontal / snap);
        (x * (1.0 - k) + mag.copysign(x) * k, y * (1.0 - k))
    } else {
        (x, y)
    }
}

/// How long a window the brake measures slowing over, and how far back it looks.
const BRAKE_WINDOW: f32 = 0.010;
/// The speed band the brake works in, deg/s — too slow to matter, too fast to be a
/// stop.
const BRAKE_SPEED_MIN: f32 = 2.0;
const BRAKE_SPEED_MAX: f32 = 60.0;
/// How quickly the brake comes on and goes off, in seconds.
const BRAKE_ENGAGE: f32 = 0.010;
const BRAKE_RELEASE: f32 = 0.006;
/// Deceleration at which the brake is fully on, as a multiple of the threshold's.
const BRAKE_FULL: f32 = 3.5;

/// `DECEL_BRAKE_STRENGTH`: damp the tail of a fast flick so the camera doesn't
/// overshoot when the hands stop. Watches how sharply the pad is *slowing*.
#[derive(Default)]
pub struct Brake {
    /// Speeds seen over the last 50 ms, oldest first, with how long ago each was.
    history: std::collections::VecDeque<(f32, f32)>,
    /// How far on the brake is, 0..1.
    engagement: f32,
}

impl Brake {
    /// `speed_now` is the post-smoothing, pre-cutoff gyro speed the fork records for
    /// this; `omega` is the speed the sensitivity will be taken at. Returns the
    /// multiplier to apply.
    pub fn tick(&mut self, s: &Settings, dt: f32, speed_now: f32, omega: f32) -> f32 {
        let strength = s.brake_strength.clamp(0.0, 1.0);
        if strength <= 0.0 {
            self.engagement = 0.0;
            self.history.clear();
            return 1.0;
        }
        // Age everything, drop what has fallen out of the window, and record now.
        for (_, age) in self.history.iter_mut() {
            *age += dt;
        }
        while self.history.front().is_some_and(|(_, age)| *age > 0.050) {
            self.history.pop_front();
        }
        self.history.push_back((speed_now, 0.0));

        // The speed one window ago — the newest sample at least that old, or the
        // current one when nothing is (the first few ticks).
        let was = self
            .history
            .iter()
            .rev()
            .find(|(_, age)| *age >= BRAKE_WINDOW)
            .map(|(v, _)| *v)
            .unwrap_or(speed_now);
        let slope = (speed_now - was) / BRAKE_WINDOW;
        let slowing = (-slope).max(0.0);
        // The threshold, as a deceleration.
        let onset = s.brake_threshold.max(0.0) / BRAKE_WINDOW;
        let full = onset * BRAKE_FULL;
        let span = if full > onset { full - onset } else { 1e-6 };

        let in_band = (BRAKE_SPEED_MIN..=BRAKE_SPEED_MAX).contains(&omega);
        let want = if in_band { ((slowing - onset) / span).clamp(0.0, 1.0) } else { 0.0 };
        if in_band && want > 0.0 {
            self.engagement += (dt / BRAKE_ENGAGE) * want;
        } else {
            self.engagement -= dt / BRAKE_RELEASE;
        }
        self.engagement = self.engagement.clamp(0.0, 1.0);
        1.0 - strength * self.engagement
    }

    pub fn reset(&mut self) {
        self.history.clear();
        self.engagement = 0.0;
    }
}

/// `GYRO_SPACE = YAW_PLUS_ROLL`: pitch on the vertical axis, and yaw with a share of
/// roll mixed in, for players who turn partly by tilting. It is default `LOCAL` plus
/// a roll term, and uses `LOCAL`'s own signs — confirmed against upstream, where
/// X-from-Y and X-from-Z are both negative.
pub fn yaw_plus_roll(s: &Settings, gyro: Vec3) -> (f32, f32) {
    let share = s.roll_contribution / 100.0;
    // gyro is (pitch about right, yaw about up, roll about forward) in JSM's frame.
    (-gyro.y - gyro.z * share, -gyro.x)
}
