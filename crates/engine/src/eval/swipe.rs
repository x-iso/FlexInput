//! A pad's own swipe gestures, derived for the mapping cards that read them.
//!
//! A touchpad's CLICK and its zones are on the bus, but a swipe is a gesture
//! over time, so nothing publishes it: a card that reads `touch1_swipe_left`
//! gets it from here, out of the field's position pins. A pad with two
//! trackpads of its own (a Steam Controller) has the gesture of each — its
//! fields are its pads — and a pad with one has it per finger.
//!
//! Two readings of the same gesture. The direction, which latches for the rest
//! of the touch: one swipe per finger down, the way the Touch Zones' swipe
//! triggers do, so sweeping a thumb across the pad fires the direction it went
//! rather than every one it passed through. And how far the finger has travelled
//! along each axis, which follows the thumb and is what a card takes in the
//! Analog mode.

use glam::Vec2;

use flexinput_core::{PinSigs, Signal, touchzones as tz};

/// How far a finger has to travel over a pad before it is a swipe and not a
/// thumb settling: 0.18 of the pad's −1..1 span, the same line the Touch Zones'
/// swipe triggers draw, so the gesture means the same thing either way.
const SWIPE_TRAVEL: f32 = 0.18;

/// One touch field's finger: where it landed, and the direction it has since
/// travelled (`-1` = none yet).
#[derive(Clone, Copy, PartialEq)]
struct SwipeFinger {
    start: Vec2,
    down: bool,
    dir: i8,
}

impl Default for SwipeFinger {
    fn default() -> Self {
        Self { start: Vec2::ZERO, down: false, dir: -1 }
    }
}

/// A node's swipe tracking, one slot per touch field. Created on first use, like
/// the node's gravity estimate.
#[derive(Default)]
pub struct PadSwipes {
    fields: [SwipeFinger; 2],
}

/// Whether any card reads a swipe — the direction of one, or how far it went —
/// and the tracking is worth doing at all.
pub(crate) fn mappings_read_swipes(mappings: &[serde_json::Value]) -> bool {
    mappings.iter().any(|m| {
        m.get("in").and_then(|v| v.as_array())
            .is_some_and(|a| a.iter().any(|v|
                v.as_str().is_some_and(|p| tz::is_swipe_pin(p) || tz::is_swipe_axis_pin(p))))
    })
}

impl PadSwipes {
    /// One tick: read each field's finger and write its four direction pins and
    /// its two axes into `upstream`, the direction latched for the touch. A
    /// field whose contact pin the bus doesn't carry is not a touch field at all,
    /// and writes nothing.
    pub fn step(&mut self, upstream: &mut PinSigs) {
        const FINGERS: [(&str, &str, &str); 2] = [
            ("touch1_x", "touch1_y", "touch1_active"),
            ("touch2_x", "touch2_y", "touch2_active"),
        ];
        for (field, (xa, ya, pa)) in FINGERS.into_iter().enumerate() {
            let touched = upstream.get(pa).is_some_and(|s| s.as_bool());
            let f = &mut self.fields[field];
            if !touched {
                // Lifted — or never arrived. The next touch starts clean, and a
                // card reading how far the swipe went reads nothing: no swipe.
                *f = SwipeFinger::default();
                continue;
            }
            let at = |p: &str| upstream.get(p).map(|s| s.as_float()).unwrap_or(0.0);
            let at = Vec2::new(at(xa), at(ya));
            if !f.down {
                *f = SwipeFinger { start: at, down: true, dir: -1 };
            } else if f.dir < 0 {
                f.dir = swipe_direction(at - f.start);
            }
            for (dir, pin) in tz::SWIPE_PINS[field].iter().enumerate() {
                upstream.insert(pin, Signal::Bool(f.dir == dir as i8));
            }
            // How far the finger has come, the other reading of the same
            // gesture: it follows the thumb, and a full pad's travel is 1.0.
            let gone = at - f.start;
            upstream.insert(tz::SWIPE_AXIS_PINS[field][0], Signal::Float(gone.x.clamp(-1.0, 1.0)));
            upstream.insert(tz::SWIPE_AXIS_PINS[field][1], Signal::Float(gone.y.clamp(-1.0, 1.0)));
        }
    }
}

/// Which way a finger went, or −1 while it hasn't gone far enough. The dominant
/// axis decides, so a diagonal picks the axis it travelled further along.
fn swipe_direction(travel: Vec2) -> i8 {
    let (ax, ay) = (travel.x.abs(), travel.y.abs());
    if ax.max(ay) < SWIPE_TRAVEL {
        return -1;
    }
    i8::try_from(if ax >= ay {
        if travel.x > 0.0 { tz::SWIPE_RIGHT } else { tz::SWIPE_LEFT }
    } else if travel.y > 0.0 {
        tz::SWIPE_UP
    } else {
        tz::SWIPE_DOWN
    })
    .unwrap_or(-1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Move a field's finger, and read back its four direction pins.
    fn swipe(st: &mut PadSwipes, field: usize, at: Option<Vec2>) -> [bool; 4] {
        let (xa, ya, pa) = if field == 0 {
            ("touch1_x", "touch1_y", "touch1_active")
        } else {
            ("touch2_x", "touch2_y", "touch2_active")
        };
        let mut up = PinSigs::default();
        match at {
            Some(p) => {
                up.insert(xa, Signal::Float(p.x));
                up.insert(ya, Signal::Float(p.y));
                up.insert(pa, Signal::Bool(true));
            }
            None => up.insert(pa, Signal::Bool(false)),
        }
        st.step(&mut up);
        tz::SWIPE_PINS[field].map(|p| up.get(p).is_some_and(|s| s.as_bool()))
    }

    #[test]
    fn a_swipe_latches_one_direction_for_the_touch() {
        let mut st = PadSwipes::default();
        assert_eq!(swipe(&mut st, 0, Some(Vec2::ZERO)), [false; 4],
            "a finger that arrived has not swiped");
        assert_eq!(swipe(&mut st, 0, Some(Vec2::new(0.05, -0.04))), [false; 4],
            "and a thumb settling is not one either");
        assert_eq!(swipe(&mut st, 0, Some(Vec2::new(0.4, 0.05))), [false, false, false, true],
            "so it went right");
        assert_eq!(swipe(&mut st, 0, Some(Vec2::new(-0.6, 0.0))), [false, false, false, true],
            "and stays that way however the finger moves after");
        assert_eq!(swipe(&mut st, 0, None), [false; 4], "until it lifts");
    }

    #[test]
    fn the_axis_it_travelled_further_along_wins() {
        let mut st = PadSwipes::default();
        swipe(&mut st, 1, Some(Vec2::new(0.2, -0.1)));
        assert_eq!(swipe(&mut st, 1, Some(Vec2::new(0.2, 0.5))), [true, false, false, false],
            "up beats a rightward drift, and only that pin is on");
    }

    /// Each pad of a two-pad controller has its own gesture, and one pad's
    /// finger says nothing about the other's.
    #[test]
    fn each_pad_has_its_own_swipe() {
        let mut up = PinSigs::default();
        up.insert("touch1_x", Signal::Float(-0.6));
        up.insert("touch1_y", Signal::Float(0.0));
        up.insert("touch1_active", Signal::Bool(true));
        up.insert("touch2_x", Signal::Float(0.7));
        up.insert("touch2_y", Signal::Float(0.3));
        up.insert("touch2_active", Signal::Bool(true));
        let mut st = PadSwipes::default();
        st.step(&mut up); // both fingers arrive
        up.insert("touch1_x", Signal::Float(-0.9));
        up.insert("touch2_y", Signal::Float(-0.4));
        st.step(&mut up);
        assert!(up.get("touch1_swipe_left").is_some_and(|s| s.as_bool()), "the left pad went left");
        assert!(up.get("touch2_swipe_down").is_some_and(|s| s.as_bool()), "the right pad went down");
        assert!(!up.get("touch1_swipe_down").is_some_and(|s| s.as_bool()));
        assert!(!up.get("touch2_swipe_left").is_some_and(|s| s.as_bool()));
    }

    /// The same gesture read as how far: it follows the thumb along each axis, a
    /// full pad's width is the most it reads, and a finger that has not moved
    /// yet — or has lifted — says nothing at all.
    #[test]
    fn a_swipes_how_far_follows_the_thumb() {
        let mut st = PadSwipes::default();
        let mut up = PinSigs::default();
        let touch = |up: &mut PinSigs, x: f32, y: f32| {
            up.insert("touch1_x", Signal::Float(x));
            up.insert("touch1_y", Signal::Float(y));
            up.insert("touch1_active", Signal::Bool(true));
        };
        let axes = |up: &PinSigs| [
            up.get("touch1_swipe_x").map(|s| s.as_float()),
            up.get("touch1_swipe_y").map(|s| s.as_float()),
        ];
        touch(&mut up, 0.0, 0.0);
        st.step(&mut up);
        assert_eq!(axes(&up), [Some(0.0), Some(0.0)], "a finger that arrived has not gone anywhere");
        touch(&mut up, 0.4, -0.2);
        st.step(&mut up);
        assert_eq!(axes(&up), [Some(0.4), Some(-0.2)], "it went right and down, that far");
        touch(&mut up, -1.0, -0.2);
        st.step(&mut up);
        assert_eq!(axes(&up), [Some(-1.0), Some(-0.2)], "a pad's width is the most it reads");
        let mut lifted = PinSigs::default();
        lifted.insert("touch1_active", Signal::Bool(false));
        st.step(&mut lifted);
        assert_eq!(axes(&lifted), [None, None], "and nothing at all once the finger lifts");
    }

    /// A pad with no touch at all writes no swipe pins, so a card on one can
    /// never fire.
    #[test]
    fn a_pad_without_touch_fields_says_nothing() {
        let mut up = PinSigs::default();
        up.insert("btn_south", Signal::Bool(true));
        let mut st = PadSwipes::default();
        st.step(&mut up);
        for p in tz::SWIPE_PINS.iter().flatten().chain(tz::SWIPE_AXIS_PINS.iter().flatten()) {
            assert!(up.get(p).is_none(), "{p} was written on a pad with no touch");
        }
    }
}
