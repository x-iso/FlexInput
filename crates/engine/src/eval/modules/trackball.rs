//! A finger's motion on a touchpad as a velocity, the flick it lifts with, the
//! trackball that flick lets go, and the haptic ticks of their travel: Touch
//! Zones' Touchpad and Trackball modes, and the Area Mapper's.
//!
//! The motion is a leaky integrator of the finger's movement: each touch report
//! moves it, and it falls back toward zero between them, so it follows the
//! finger's speed and settles when the finger stops (robust to touch reports
//! arriving a few ms apart, each a jump). It goes by time, not by engine tick,
//! so it reads the same at any processing rate.
//!
//! The flick: how fast the finger was going as it lifted, from where it was a few
//! tens of ms before to where it lifted. Not the motion, which a single late touch
//! report can kick hard. A lift slower than a flick, or a finger that had stopped
//! before lifting, lets no ball go: aiming, and touching the pad to stop a ball,
//! set nothing rolling.
//!
//! The ball rolls on at the flick's speed and slows down by the friction, more
//! while it's fast: a gentle flick stops in a moment, a hard one rolls on for a
//! second or two, and every ball comes to a stop.

/// The speed a ball loses each second, in pad widths a second (more while it's
/// fast, see [`BALL_DRAG_WIDTHS_S`]). Never zero, so a ball always stops.
pub const TRACKBALL_FRICTION_DEFAULT: f32 = 3.0;
pub const TRACKBALL_FRICTION_MIN: f32 = 0.5;
pub const TRACKBALL_FRICTION_MAX: f32 = 20.0;
/// How many haptic ticks the finger or the ball gives going the pad's width.
pub const TRACKBALL_TICK_DENSITY_DEFAULT: f32 = 20.0;
pub const TRACKBALL_TICK_DENSITY_MIN: f32 = 4.0;
pub const TRACKBALL_TICK_DENSITY_MAX: f32 = 50.0;

/// How long the motion takes to fall back by `1/e` (s): 0.82 a tick at 500 Hz,
/// the rate Touchpad mode was tuned at.
const MOTION_SECS: f32 = 0.010_078;
/// The steady motion of a finger moving one pad width a second.
const MOTION_PER_WIDTH_S: f32 = 0.035_556;
/// The flick is the finger's move from where it was between this long and twice
/// this long (s) before it last moved, to where it lifted.
const FLICK_SECS: f32 = 0.02;
/// A finger that hadn't moved for this long (s) as it lifted had stopped.
const FLICK_STILL_SECS: f32 = 0.03;
/// A lift slower than this (pad widths a second) lets no ball go; from twice this
/// the ball goes at the flick's full speed, and in between at a share of it that
/// grows from none, so there's no jump at the threshold.
const FLICK_MIN_WIDTHS_S: f32 = 1.5;
/// The fastest a ball goes (pad widths a second).
const BALL_MAX_WIDTHS_S: f32 = 30.0;
/// The drag: at this speed (pad widths a second) a ball slows twice as fast as at
/// a crawl, so a hard flick doesn't send it off for ever.
const BALL_DRAG_WIDTHS_S: f32 = 6.0;

/// [`flick_follow`]'s state: `[older mark: age, x, y, newer mark: age, x, y, time
/// since the finger last moved, where it last was: x, y]`.
pub(crate) const FLICK_SLOTS: usize = 9;
/// A ball: `[vx, vy, rolling]`, pad widths a second.
pub(crate) const BALL_SLOTS: usize = 3;

/// One tick of the motion. `acc` is its `[x, y]`; `delta` the finger's move since
/// the last tick, in pad widths, +Y up. Returns the motion, each axis within ±1.
pub(crate) fn pad_motion(acc: &mut [f32], delta: (f32, f32), dt: f32) -> (f32, f32) {
    let keep = (-dt.max(0.0) / MOTION_SECS).exp();
    let gain = if dt > 0.0 { MOTION_PER_WIDTH_S * (1.0 - keep) / dt } else { 0.0 };
    acc[0] = (acc[0] * keep + delta.0 * gain).clamp(-1.5, 1.5);
    acc[1] = (acc[1] * keep + delta.1 * gain).clamp(-1.5, 1.5);
    (acc[0].clamp(-1.0, 1.0), acc[1].clamp(-1.0, 1.0))
}

/// One tick of a finger down at `pos` (pad widths, +Y up), for the flick it may
/// lift with. `landed`: its first tick down. `st` is [`FLICK_SLOTS`] long.
pub(crate) fn flick_follow(st: &mut [f32], pos: (f32, f32), landed: bool, dt: f32) {
    if landed {
        st[..FLICK_SLOTS].copy_from_slice(&[0.0, pos.0, pos.1, 0.0, pos.0, pos.1, 0.0, pos.0, pos.1]);
        return;
    }
    let dt = dt.max(0.0);
    st[0] += dt;
    st[3] += dt;
    if pos != (st[7], st[8]) {
        st[6] = 0.0;
        st[7] = pos.0;
        st[8] = pos.1;
    } else {
        st[6] = (st[6] + dt).min(1.0);
    }
    // Every FLICK_SECS the newer mark becomes the older and a new one is set
    // here, so the older is between one and two of them back. A mark is as old as
    // the report that put the finger where it is, not the tick that set it.
    if st[3] >= FLICK_SECS {
        let since = st[6];
        st.copy_within(3..6, 0);
        st[3..6].copy_from_slice(&[since, pos.0, pos.1]);
    }
}

/// The flick a finger lifted with (pad widths a second, +Y up), from
/// [`flick_follow`]'s state; None for one too slow, a finger that had stopped,
/// or a touch too short to tell.
pub(crate) fn flick_of(st: &[f32]) -> Option<(f32, f32)> {
    let (age, still) = (st[0], st[6]);
    let span = age - still;
    if still >= FLICK_STILL_SECS || span < FLICK_SECS * 0.5 {
        return None;
    }
    let v = ((st[7] - st[1]) / span, (st[8] - st[2]) / span);
    let speed = v.0.hypot(v.1);
    let share = ((speed - FLICK_MIN_WIDTHS_S) / FLICK_MIN_WIDTHS_S).clamp(0.0, 1.0);
    if share <= 0.0 {
        return None;
    }
    let k = share * speed.min(BALL_MAX_WIDTHS_S) / speed;
    Some((v.0 * k, v.1 * k))
}

/// Let a ball (`[vx, vy, rolling]`) go at the flick a finger lifted with
/// (`flick` is [`flick_follow`]'s state); none for no flick.
pub(crate) fn trackball_let_go(ball: &mut [f32], flick: &[f32]) {
    match flick_of(flick) {
        Some((x, y)) => ball[..BALL_SLOTS].copy_from_slice(&[x, y, 1.0]),
        None => ball[..BALL_SLOTS].fill(0.0),
    }
}

/// One tick of a rolling ball: it slows down by `friction` (clamped to its
/// range), more while it's fast, and stops. Its velocity while it rolls, on the
/// motion's scale ([`pad_motion`]), so it carries on where the finger left off.
pub(crate) fn trackball_roll(ball: &mut [f32], friction: f32, dt: f32) -> Option<(f32, f32)> {
    if ball[2] == 0.0 {
        return None;
    }
    let f = friction.clamp(TRACKBALL_FRICTION_MIN, TRACKBALL_FRICTION_MAX);
    let speed = ball[0].hypot(ball[1]);
    // dv/dt = -f (1 + v / drag): exactly, v + drag falls by e^(-f dt / drag).
    let drag = BALL_DRAG_WIDTHS_S;
    let next = (speed + drag) * (-f * dt.max(0.0) / drag).exp() - drag;
    if speed <= 0.0 || next <= 0.0 {
        ball[..BALL_SLOTS].fill(0.0);
        return None;
    }
    ball[0] *= next / speed;
    ball[1] *= next / speed;
    Some((ball[0] * MOTION_PER_WIDTH_S, ball[1] * MOTION_PER_WIDTH_S))
}

/// How far (pad widths) a motion on [`pad_motion`]'s scale goes in `dt`: a
/// rolling ball's travel, for its ticks.
pub(crate) fn motion_travel(motion: (f32, f32), dt: f32) -> f32 {
    motion.0.hypot(motion.1) / MOTION_PER_WIDTH_S * dt.max(0.0)
}

/// Count `moved` (pad widths) toward the next haptic tick: true once the finger
/// or the ball has gone the pad's width over `density` (clamped to its range)
/// since the last one. Faster than a tick an engine tick, the extra ones are
/// dropped rather than owed.
pub(crate) fn trackball_tick(travel: &mut f32, moved: f32, density: f32) -> bool {
    let every = 1.0 / density.clamp(TRACKBALL_TICK_DENSITY_MIN, TRACKBALL_TICK_DENSITY_MAX);
    *travel += moved.max(0.0);
    if *travel < every {
        return false;
    }
    *travel = (*travel - every).min(every * 0.5);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A finger moving steadily reads the same motion at any processing rate,
    /// and the one Touchpad mode was tuned to at 500 Hz.
    #[test]
    fn the_motion_reads_the_same_at_any_rate() {
        let steady = |rate: f32| {
            let dt = 1.0 / rate;
            let mut acc = [0.0; 2];
            let mut sum = 0.0;
            // A report every 4 ms (or every tick, if they're longer), one pad width
            // a second.
            let ticks = (rate * 0.5) as usize;
            let report = ((rate * 0.004).round() as usize).max(1);
            for k in 0..ticks {
                let d = if k % report == 0 { report as f32 / rate } else { 0.0 };
                let (x, _) = pad_motion(&mut acc, (d, 0.0), dt);
                if k >= ticks / 2 {
                    sum += x;
                }
            }
            sum / (ticks - ticks / 2) as f32
        };
        let at_500 = steady(500.0);
        assert!((at_500 - MOTION_PER_WIDTH_S).abs() < MOTION_PER_WIDTH_S * 0.05, "{at_500}");
        for rate in [250.0, 1000.0, 2000.0, 8000.0] {
            let v = steady(rate);
            assert!((v - at_500).abs() < at_500 * 0.1, "{rate} Hz: {v} vs {at_500}");
        }
        // The tick at 500 Hz keeps what it always did.
        let mut acc = [1.0, 0.0];
        pad_motion(&mut acc, (0.0, 0.0), 0.002);
        assert!((acc[0] - 0.82).abs() < 1e-3, "{}", acc[0]);
    }

    /// A finger at `speed` pad widths a second for `secs`, a touch report every
    /// `report` s, the engine at `rate`, then `still` s without moving; the flick
    /// it lifts with.
    fn flick(speed: f32, secs: f32, still: f32, report: f32, rate: f32) -> Option<(f32, f32)> {
        let dt = 1.0 / rate;
        let mut st = [0.0; FLICK_SLOTS];
        let (mut t, mut next, mut x) = (0.0, 0.0, 0.0);
        flick_follow(&mut st, (0.0, 0.0), true, dt);
        while t < secs + still {
            t += dt;
            if t >= next {
                next += report;
                x = speed * t.min(secs);
            }
            flick_follow(&mut st, (x, 0.0), false, dt);
        }
        flick_of(&st)
    }

    /// A flick reads at the finger's speed whatever the touch report and engine
    /// rates; slower than a flick, or stopped before the lift, it lets nothing go.
    #[test]
    fn a_flick_reads_the_fingers_speed_as_it_lifted() {
        for (report, rate) in [(0.004, 500.0), (0.008, 288.0), (0.001, 1000.0), (0.004, 8000.0)] {
            let (x, y) = flick(5.0, 0.1, 0.0, report, rate).expect("a flick");
            assert!((x - 5.0).abs() < 0.75 && y == 0.0, "{report}s reports at {rate} Hz: {x}");
            assert_eq!(flick(0.5, 0.3, 0.0, report, rate), None, "aiming: too slow");
            assert_eq!(flick(5.0, 0.1, 0.05, report, rate), None, "stopped before lifting");
        }
        // Just over the threshold, a share of the speed: no jump from nothing.
        let barely = flick(FLICK_MIN_WIDTHS_S * 1.1, 0.1, 0.0, 0.001, 1000.0).map(|v| v.0).unwrap_or(0.0);
        assert!(barely < FLICK_MIN_WIDTHS_S * 0.3, "{barely}");
        assert_eq!(flick(5.0, 0.005, 0.0, 0.001, 1000.0), None, "a touch too short to tell");
    }

    /// A finger resting on the pad, its reading jittering, then a last report
    /// off by a little as it lifts: no flick.
    #[test]
    fn a_tap_with_a_jittery_lift_lets_nothing_go() {
        let dt = 0.002;
        let mut st = [0.0; FLICK_SLOTS];
        flick_follow(&mut st, (0.5, 0.5), true, dt);
        for k in 1..40 {
            let wobble = if k % 2 == 0 { 0.004 } else { -0.004 };
            let last = if k == 39 { 0.012 } else { 0.0 };
            flick_follow(&mut st, (0.5 + wobble + last, 0.5), false, dt);
        }
        assert_eq!(flick_of(&st), None);
    }

    /// A ball let go at `speed`: how long it rolls and how far (pad widths).
    fn roll(speed: f32, friction: f32) -> (f32, f32) {
        let mut ball = [speed, 0.0, 1.0];
        let (mut t, mut far) = (0.0, 0.0);
        while let Some(m) = trackball_roll(&mut ball, friction, 0.001) {
            t += 0.001;
            far += motion_travel(m, 0.001);
            assert!(t < 60.0, "never stopped");
        }
        (t, far)
    }

    /// A harder flick rolls longer and much further; more friction stops it
    /// sooner; every ball stops, at the lowest friction too.
    #[test]
    fn a_ball_rolls_by_its_speed_and_friction() {
        let (slow_t, slow_far) = roll(2.0, 3.0);
        let (fast_t, fast_far) = roll(8.0, 3.0);
        assert!(slow_t < 0.7 && fast_t > 1.5, "slow {slow_t}s, fast {fast_t}s");
        assert!(fast_far > slow_far * 8.0, "slow {slow_far}, fast {fast_far}");
        assert!(roll(8.0, 10.0).0 < fast_t / 2.0);
        assert!(roll(BALL_MAX_WIDTHS_S, 0.0).0 < 30.0, "friction never below its minimum");
        let mut ball = [0.0; BALL_SLOTS];
        trackball_let_go(&mut ball, &[0.03, 0.0, 0.0, 0.01, 0.1, 0.0, 0.0, 0.2, 0.0]);
        assert_eq!(ball[2], 1.0, "a flick of 6.7 widths a second rolls");
        assert!((ball[0] - 0.2 / 0.03).abs() < 1e-3, "{ball:?}");
    }

    /// `density` ticks a pad width of travel, and no more than one at a time owed.
    #[test]
    fn ticks_follow_the_travel() {
        for density in [TRACKBALL_TICK_DENSITY_MIN, 20.0, TRACKBALL_TICK_DENSITY_MAX] {
            let mut travel = 0.0;
            let n = (0..1000).filter(|_| trackball_tick(&mut travel, 0.001_000_1, density)).count();
            assert_eq!(n, density as usize, "{density} a pad width");
        }
        let mut travel = 0.0;
        assert!(trackball_tick(&mut travel, 1.0, 20.0));
        assert!(travel < 0.05, "the rest dropped: {travel}");
    }
}
