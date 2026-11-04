//! Gamepad touchpad → virtual trackpad: the Virtual Keyboard & Mouse sink's
//! "Route gamepad touchpad" toggle (`route_touchpad` param, off by default).
//!
//! Reads the upstream pad's `touch1_*` / `touch2_*` / `btn_touchpad` off the
//! sink's Auto-Map bus and drives the sink the way a laptop trackpad drives a
//! PC: one finger moves the pointer, two fingers scroll (smooth, through the
//! `scroll_move_*` pins), a tap clicks (two-finger tap = right click), and
//! the pad's click is a left click (right with two fingers down).
//!
//! A pad whose two touch fields are two trackpads (a Steam Controller,
//! `automap::TOUCH_PADS_PIN`) splits the work between them instead: the right
//! pad moves the pointer and its click is the left button, the left pad scrolls
//! both ways and its click is the right button, and both clicked together are
//! the middle button. No taps there: a thumb rests on those pads.
//!
//! Touch coordinates are the canonical pin range: [-1, 1] across the pad, +Y
//! at the top (see the SDL / native DualSense backends).

use glam::Vec2;

/// Pointer pixels per touch unit (the pad is 2 units wide), before the sink's
/// mouse sensitivity.
const POINTER_PX_PER_UNIT: f32 = 700.0;
/// Scroll notches per touch unit of two-finger travel. (6 felt about ten times
/// too slow next to a laptop trackpad.)
const SCROLL_NOTCHES_PER_UNIT: f32 = 60.0;
/// A touch that lifts within this long, having moved less than `TAP_SLOP`, is a tap.
const TAP_MAX_S: f64 = 0.18;
/// Total travel (touch units) a tap may wander.
const TAP_SLOP: f32 = 0.05;
/// How long a tap holds its button down.
const TAP_PULSE_S: f64 = 0.03;
/// After routing turns off, how long the buttons keep reading released. The
/// sink bus publishes one tick per wakeup, so a release sent for a single tick
/// can be skipped and leave a button stuck down.
const RELEASE_S: f64 = 0.1;
/// On two pads, how long a pad's click waits for the other's, which together
/// are the middle button. A click let go sooner plays as a pulse.
const CHORD_S: f64 = 0.17;

/// One tick of touchpad input.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TouchFrame {
    pub fingers: [Option<Vec2>; 2],
    pub click: bool,
    /// The fields are two trackpads: `fingers[0]` and `click` the left one,
    /// `fingers[1]` and `click2` the right.
    pub split: bool,
    pub click2: bool,
}

/// What the trackpad asks of the sink this tick.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct TrackpadOut {
    /// Pointer displacement in px, +Y up (the `mouse_move` convention).
    pub pointer: Vec2,
    /// Scroll in notches, +Y up / +X right (the `scroll_move_*` convention).
    pub scroll: Vec2,
    pub left: bool,
    pub right: bool,
    pub middle: bool,
}

/// Which button a held pad click presses.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ClickButton { Left, Right }

/// Two pads' clicks: each pad's own button, or the middle for both.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Chord {
    #[default]
    Up,
    /// A pad clicked, waiting for the other: (the pad, seconds since).
    Waiting(usize, f64),
    /// A pad's own button, until that pad lets go.
    One(usize),
    /// Both: the middle button, until both let go.
    Both,
}

/// Gesture state across ticks. Clocks are f64: they add `dt` for as long as a
/// finger stays down.
#[derive(Debug, Default)]
pub struct TrackpadState {
    /// Each finger slot's position last tick.
    prev: [Option<Vec2>; 2],
    /// A touch session runs from the first finger down until the last one lifts.
    in_session: bool,
    session_s: f64,
    session_travel: f32,
    session_max_fingers: usize,
    /// The pad was clicked during this session, so lifting off isn't a tap.
    session_clicked: bool,
    /// Seconds left on a tap's button pulse.
    tap_left_s: f64,
    tap_right_s: f64,
    click: Option<ClickButton>,
    /// On two pads: their clicks, and seconds left on each pad's click pulse.
    chord: Chord,
    pad_pulse_s: [f64; 2],
    /// Seconds since routing turned off (see `release`).
    off_s: f64,
}

impl TrackpadState {
    /// Routing is off: lift every finger and button. Returns the outputs to
    /// send while the release settles, then `None` once the state can go.
    pub(crate) fn release(&mut self, dt: f32) -> Option<TrackpadOut> {
        let off_s = self.off_s + dt as f64;
        if off_s > RELEASE_S { return None; }
        // Lifting off here is the toggle, not the user: no tap, no pulse.
        self.session_clicked = true;
        self.tap_left_s = 0.0;
        self.tap_right_s = 0.0;
        self.chord = Chord::Up;
        self.pad_pulse_s = [0.0; 2];
        let out = self.step(&TouchFrame::default(), dt, true);
        self.off_s = off_s;
        Some(out)
    }

    /// Advance one tick. `natural` scrolls the content with the fingers (the
    /// Windows trackpad default); off, it scrolls like a wheel turned the same way.
    pub(crate) fn step(&mut self, f: &TouchFrame, dt: f32, natural: bool) -> TrackpadOut {
        self.off_s = 0.0;
        let dt = dt as f64;
        if f.split {
            return self.step_split(f, dt, natural);
        }
        let count = f.fingers.iter().filter(|p| p.is_some()).count();
        let same_fingers = f.fingers.iter().zip(&self.prev).all(|(a, b)| a.is_some() == b.is_some());
        let mut out = TrackpadOut::default();

        // Motion only while the same fingers stay down: a finger landing or
        // lifting moves the centroid (and changes the gesture), not the hand.
        if same_fingers && count > 0 {
            let delta = f.fingers.iter().zip(&self.prev)
                .filter_map(|(a, b)| Some((*a)? - (*b)?))
                .sum::<Vec2>() / count as f32;
            if count == 1 {
                out.pointer = delta * POINTER_PX_PER_UNIT;
            } else {
                let s = delta * SCROLL_NOTCHES_PER_UNIT;
                out.scroll = if natural { -s } else { s };
            }
            if self.in_session {
                self.session_travel += delta.length();
            }
        }
        self.prev = f.fingers;

        // Tap detection over the whole session.
        if count > 0 {
            if !self.in_session {
                self.in_session = true;
                self.session_s = 0.0;
                self.session_travel = 0.0;
                self.session_max_fingers = 0;
                self.session_clicked = false;
            } else {
                self.session_s += dt;
            }
            self.session_max_fingers = self.session_max_fingers.max(count);
        } else if self.in_session {
            self.in_session = false;
            if !self.session_clicked && self.session_s <= TAP_MAX_S && self.session_travel <= TAP_SLOP {
                if self.session_max_fingers >= 2 {
                    self.tap_right_s = TAP_PULSE_S;
                } else {
                    self.tap_left_s = TAP_PULSE_S;
                }
            }
        }

        // The pad's click: chooses its button when it goes down and keeps it
        // until it comes up, however many fingers come and go meanwhile.
        match (f.click, self.click) {
            (true, None) => {
                self.click = Some(if count >= 2 { ClickButton::Right } else { ClickButton::Left });
                self.session_clicked = true;
            }
            (false, Some(_)) => self.click = None,
            _ => {}
        }

        out.left = self.tap_left_s > 0.0 || self.click == Some(ClickButton::Left);
        out.right = self.tap_right_s > 0.0 || self.click == Some(ClickButton::Right);
        self.tap_left_s = (self.tap_left_s - dt).max(0.0);
        self.tap_right_s = (self.tap_right_s - dt).max(0.0);
        out
    }

    /// One tick on two pads (see the module's note): pad 0 the left, which
    /// scrolls and whose click is the right button; pad 1 the right, which
    /// points and whose click is the left button.
    fn step_split(&mut self, f: &TouchFrame, dt: f64, natural: bool) -> TrackpadOut {
        let mut out = TrackpadOut::default();
        // Each pad moves only while its finger stays down: one landing is no motion.
        if let (Some(now), Some(was)) = (f.fingers[0], self.prev[0]) {
            let s = (now - was) * SCROLL_NOTCHES_PER_UNIT;
            out.scroll = if natural { -s } else { s };
        }
        if let (Some(now), Some(was)) = (f.fingers[1], self.prev[1]) {
            out.pointer = (now - was) * POINTER_PX_PER_UNIT;
        }
        self.prev = f.fingers;

        let down = [f.click, f.click2];
        self.chord = match self.chord {
            _ if down == [true, true] && !matches!(self.chord, Chord::One(_)) => Chord::Both,
            Chord::Up => match down.iter().position(|d| *d) {
                Some(pad) => Chord::Waiting(pad, 0.0),
                None => Chord::Up,
            },
            Chord::Waiting(pad, _) if !down[pad] => {
                // Let go before the other came: still a click of its own.
                self.pad_pulse_s[pad] = TAP_PULSE_S;
                Chord::Up
            }
            Chord::Waiting(pad, s) if s + dt >= CHORD_S => Chord::One(pad),
            Chord::Waiting(pad, s) => Chord::Waiting(pad, s + dt),
            Chord::One(pad) if down[pad] => Chord::One(pad),
            Chord::Both if down != [false, false] => Chord::Both,
            _ => Chord::Up,
        };
        let button = |pad: usize| self.chord == Chord::One(pad) || self.pad_pulse_s[pad] > 0.0;
        out.right = button(0);
        out.left = button(1);
        out.middle = self.chord == Chord::Both;
        self.pad_pulse_s = self.pad_pulse_s.map(|s| (s - dt).max(0.0));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 0.001;

    fn one(x: f32, y: f32) -> TouchFrame {
        TouchFrame { fingers: [Some(Vec2::new(x, y)), None], ..Default::default() }
    }
    fn two(x: f32, y: f32) -> TouchFrame {
        TouchFrame { fingers: [Some(Vec2::new(x - 0.2, y)), Some(Vec2::new(x + 0.2, y))], ..Default::default() }
    }
    fn none() -> TouchFrame { TouchFrame::default() }

    /// Run `frames`, returning the summed pointer/scroll and whether each
    /// button was ever down.
    fn run(st: &mut TrackpadState, frames: &[TouchFrame]) -> (Vec2, Vec2, bool, bool) {
        let (mut p, mut s, mut l, mut r) = (Vec2::ZERO, Vec2::ZERO, false, false);
        for f in frames {
            let o = st.step(f, DT, true);
            p += o.pointer;
            s += o.scroll;
            l |= o.left;
            r |= o.right;
        }
        (p, s, l, r)
    }

    #[test]
    fn one_finger_drag_moves_the_pointer() {
        let mut st = TrackpadState::default();
        let frames: Vec<_> = (0..=100).map(|i| one(i as f32 * 0.005, 0.0)).collect();
        let (p, s, l, _) = run(&mut st, &frames);
        assert!((p.x - 0.5 * POINTER_PX_PER_UNIT).abs() < 1e-2, "{p}");
        assert_eq!(p.y, 0.0);
        assert_eq!(s, Vec2::ZERO);
        assert!(!l, "a drag is not a tap");
    }

    /// Natural scrolling: fingers moving up push the content up, i.e. scroll down.
    #[test]
    fn two_finger_drag_scrolls_with_the_content() {
        let mut st = TrackpadState::default();
        let frames: Vec<_> = (0..=100).map(|i| two(0.0, i as f32 * 0.005)).collect();
        let (p, s, ..) = run(&mut st, &frames);
        assert_eq!(p, Vec2::ZERO, "two fingers never move the pointer");
        assert!((s.y + 0.5 * SCROLL_NOTCHES_PER_UNIT).abs() < 1e-3, "{s}");
        let mut st = TrackpadState::default();
        let o = (0..=100).map(|i| st.step(&two(0.0, i as f32 * 0.005), DT, false).scroll).sum::<Vec2>();
        assert!(o.y > 0.0, "reversed: fingers up scroll up");
    }

    /// A second finger landing jumps the centroid; that is not motion.
    #[test]
    fn finger_count_changes_do_not_jump() {
        let mut st = TrackpadState::default();
        let (p, s, ..) = run(&mut st, &[one(0.0, 0.0), one(0.0, 0.0), two(0.5, 0.5), two(0.5, 0.5), one(-0.5, -0.5)]);
        assert_eq!((p, s), (Vec2::ZERO, Vec2::ZERO));
    }

    #[test]
    fn taps_click_left_and_right() {
        let mut st = TrackpadState::default();
        let mut frames = vec![one(0.1, 0.1); 50];
        frames.extend([none(); 5]);
        let (_, _, l, r) = run(&mut st, &frames);
        assert!(l && !r, "one-finger tap = left click");

        let mut st = TrackpadState::default();
        let mut frames = vec![two(0.1, 0.1); 50];
        frames.extend([none(); 5]);
        let (_, _, l, r) = run(&mut st, &frames);
        assert!(r && !l, "two-finger tap = right click");
    }

    #[test]
    fn a_tap_releases_and_a_long_touch_is_no_tap() {
        let mut st = TrackpadState::default();
        let mut frames = vec![one(0.1, 0.1); 50];
        frames.extend([none(); 100]);
        run(&mut st, &frames);
        assert!(!st.step(&none(), DT, true).left, "the tap pulse ends");

        let mut st = TrackpadState::default();
        let mut frames = vec![one(0.1, 0.1); 400];
        frames.push(none());
        let (_, _, l, _) = run(&mut st, &frames);
        assert!(!l, "resting a finger is not a tap");
    }

    /// The pad's click holds its button, picks right with two fingers, and the
    /// lift after it is not also a tap.
    #[test]
    fn pad_click_holds_its_button() {
        let mut st = TrackpadState::default();
        let press = |f: TouchFrame| TouchFrame { click: true, ..f };
        assert!(st.step(&press(one(0.0, 0.0)), DT, true).left);
        assert!(st.step(&press(two(0.0, 0.0)), DT, true).left, "the button chosen at press stays");
        let o = st.step(&one(0.0, 0.0), DT, true);
        assert!(!o.left && !o.right);
        assert!(!st.step(&none(), DT, true).left, "no tap after a click");

        let mut st = TrackpadState::default();
        let o = st.step(&press(two(0.0, 0.0)), DT, true);
        assert!(o.right && !o.left, "two fingers down = right click");
    }

    /// Two pads: (left pad's finger, right pad's finger), and each pad's click.
    fn pads(left: Option<(f32, f32)>, right: Option<(f32, f32)>, clicks: [bool; 2]) -> TouchFrame {
        let at = |p: Option<(f32, f32)>| p.map(|(x, y)| Vec2::new(x, y));
        TouchFrame { fingers: [at(left), at(right)], click: clicks[0], split: true, click2: clicks[1] }
    }

    /// On two pads the right one points and the left one scrolls, both at once,
    /// and neither taps.
    #[test]
    fn two_pads_point_with_the_right_and_scroll_with_the_left() {
        let mut st = TrackpadState::default();
        let frames: Vec<_> = (0..=100)
            .map(|i| pads(Some((0.0, i as f32 * 0.005)), Some((i as f32 * 0.005, 0.0)), [false; 2]))
            .collect();
        let (p, s, ..) = run(&mut st, &frames);
        assert!((p.x - 0.5 * POINTER_PX_PER_UNIT).abs() < 1e-2 && p.y == 0.0, "{p}");
        assert!((s.y + 0.5 * SCROLL_NOTCHES_PER_UNIT).abs() < 1e-3 && s.x == 0.0, "natural: {s}");
        let mut st = TrackpadState::default();
        let mut frames = vec![pads(Some((0.1, 0.1)), Some((0.1, 0.1)), [false; 2]); 50];
        frames.extend([pads(None, None, [false; 2]); 5]);
        let (_, _, l, r) = run(&mut st, &frames);
        assert!(!l && !r, "no taps on two pads");
    }

    /// The right pad's click is the left button and the left pad's the right,
    /// each once it hasn't been joined by the other; both are the middle.
    #[test]
    fn two_pads_click_left_right_and_both_middle() {
        let held = |st: &mut TrackpadState, clicks: [bool; 2], ticks: usize| {
            let mut last = TrackpadOut::default();
            for _ in 0..ticks {
                last = st.step(&pads(None, None, clicks), DT, true);
            }
            last
        };
        let mut st = TrackpadState::default();
        let o = held(&mut st, [false, true], 150);
        assert!(!o.left, "waits for the other pad, 170 ms");
        let o = held(&mut st, [false, true], 30);
        assert!(o.left && !o.right && !o.middle, "then the right pad's is the left button");
        assert!(!held(&mut st, [false; 2], 1).left);

        let mut st = TrackpadState::default();
        held(&mut st, [true, false], 150);
        let o = held(&mut st, [true, true], 1);
        assert!(o.middle && !o.left && !o.right, "both within the wait: middle");
        let o = held(&mut st, [true, false], 50);
        assert!(o.middle, "until both let go");
        assert!(!held(&mut st, [false; 2], 1).middle);

        // A click let go before the wait is over still clicks, as a pulse.
        let mut st = TrackpadState::default();
        held(&mut st, [true, false], 20);
        let o = held(&mut st, [false; 2], 1);
        assert!(o.right && !o.left, "the left pad's click is the right button");
        assert!(!held(&mut st, [false; 2], 100).right, "and the pulse ends");
    }

    /// Turning routing off mid-click releases the button for long enough to
    /// survive the bus, then lets the state go.
    #[test]
    fn release_lets_go_then_ends() {
        let mut st = TrackpadState::default();
        st.step(&TouchFrame { click: true, ..one(0.0, 0.0) }, DT, true);
        let mut ticks = 0;
        while let Some(o) = st.release(DT) {
            assert!(!o.left && !o.right && o.pointer == Vec2::ZERO);
            ticks += 1;
            assert!(ticks < 1000, "release never ends");
        }
        assert!(ticks >= 50, "released for {ticks} ticks only");
    }
}
