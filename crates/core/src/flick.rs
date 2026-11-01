//! Bound flicks: a mapping output that turns the camera by a set number of
//! degrees, once per press.
//!
//! A flick stick's turn is worked out by RWS Aim, which knows the game's scale,
//! so a mapping card (Remapper, Lean, Touch Zones, Menu, Area Mapper) doesn't do
//! the turning itself: it asserts one of these pins, and whichever RWS Aim has
//! its `flick_target` ticked starts the turn on the pin's rising edge. Holding
//! the card's input does nothing further.
//!
//! The pin id carries everything about the turn — `flick:mouse:90`, or
//! `flick:stick:-45:120` for one with its own time in ms — so a list of outputs
//! stays a list of pins, and a card can hold several different turns. It routes
//! like a Macro Output port: through the reserved macro namespace, never onto
//! the bus.

/// `node.params` key on RWS Aim: this module takes the flicks cards send.
pub const TARGET_PARAM: &str = "flick_target";

const PREFIX: &str = "flick:";

/// The turn a freshly picked flick starts at, in degrees.
pub const DEFAULT_DEG: i32 = 90;
/// The furthest one flick turns either way, in degrees.
pub const MAX_DEG: i32 = 360;
/// One coarse step, in degrees. Coarse steps land on its multiples.
pub const COARSE_DEG: i32 = 45;
/// The longest a flick can be set to take, in ms.
pub const MAX_MS: u32 = 5000;
/// The time a flick's first own-time step starts at, in ms.
pub const DEFAULT_MS: u32 = 100;

/// How the turn reaches the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlickVia {
    Mouse,
    Stick,
}

/// One bound flick, as its pin id spells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlickPin {
    pub via: FlickVia,
    /// Degrees of turn: positive is to the right. Never 0.
    pub deg: i32,
    /// How long it takes, in ms. `None` = the target module's flick smoothing.
    pub ms: Option<u32>,
}

impl FlickPin {
    /// A freshly picked flick.
    pub fn new(via: FlickVia) -> Self {
        FlickPin { via, deg: DEFAULT_DEG, ms: None }
    }

    /// Read a pin id, or `None` if it isn't one (or isn't a turn of any size).
    pub fn parse(pin: &str) -> Option<Self> {
        let mut parts = pin.strip_prefix(PREFIX)?.split(':');
        let via = match parts.next()? {
            "mouse" => FlickVia::Mouse,
            "stick" => FlickVia::Stick,
            _ => return None,
        };
        let deg: i32 = parts.next()?.parse().ok()?;
        if deg == 0 {
            return None;
        }
        let ms = match parts.next() {
            Some(m) => Some(m.parse().ok()?),
            None => None,
        };
        parts.next().is_none().then_some(FlickPin {
            via,
            deg: deg.clamp(-MAX_DEG, MAX_DEG),
            ms: ms.map(|m: u32| m.min(MAX_MS)),
        })
    }

    pub fn to_id(self) -> String {
        let via = match self.via {
            FlickVia::Mouse => "mouse",
            FlickVia::Stick => "stick",
        };
        match self.ms {
            Some(ms) => format!("{PREFIX}{via}:{}:{ms}", self.deg),
            None => format!("{PREFIX}{via}:{}", self.deg),
        }
    }

    /// This flick turning `deg` instead. A turn of 0 is no flick, so it comes out
    /// a degree to the side `deg` came from `self.deg` towards.
    pub fn with_deg(self, deg: i32) -> Self {
        let deg = deg.clamp(-MAX_DEG, MAX_DEG);
        let deg = if deg != 0 { deg } else if self.deg < 0 { 1 } else { -1 };
        FlickPin { deg, ..self }
    }

    /// One step of the turn. Fine moves by a degree; coarse goes to the next
    /// multiple of [`COARSE_DEG`] that way, so an odd angle snaps back onto the
    /// grid. Either way a step never rests on 0: it carries on past it.
    pub fn step_deg(self, dir: i32, fine: bool) -> Self {
        let dir = dir.signum();
        if dir == 0 {
            return self;
        }
        let next = |from: i32| {
            if fine {
                from + dir
            } else if dir > 0 {
                (from.div_euclid(COARSE_DEG) + 1) * COARSE_DEG
            } else {
                (from + COARSE_DEG - 1).div_euclid(COARSE_DEG) * COARSE_DEG - COARSE_DEG
            }
        };
        let mut deg = next(self.deg);
        if deg == 0 {
            deg = next(0);
        }
        FlickPin { deg: deg.clamp(-MAX_DEG, MAX_DEG), ..self }
    }

    /// One step of its time, in `step_ms`: up from the target's own time to
    /// [`DEFAULT_MS`] and on, back down past the smallest step to the target's
    /// again — as a pick's own duration steps.
    pub fn step_ms(self, dir: i32, step_ms: u32) -> Self {
        let ms = match self.ms {
            None if dir > 0 => Some(DEFAULT_MS),
            None => None,
            Some(m) if dir > 0 => Some((m + step_ms).min(MAX_MS)),
            Some(m) if m < step_ms * 2 => None,
            Some(m) => Some(m - step_ms),
        };
        FlickPin { ms, ..self }
    }

    /// What it's called on a chip: the way it goes reads off the arrow.
    pub fn short_label(self) -> String {
        let via = match self.via {
            FlickVia::Mouse => "Flick",
            FlickVia::Stick => "Stick flick",
        };
        let arrow = if self.deg < 0 { '←' } else { '→' };
        match self.ms {
            Some(ms) => format!("{via} {arrow}{}° {ms}ms", self.deg.abs()),
            None => format!("{via} {arrow}{}°", self.deg.abs()),
        }
    }

    /// What it's called in full, for a tooltip.
    pub fn display_name(self) -> String {
        let via = match self.via {
            FlickVia::Mouse => "Mouse flick",
            FlickVia::Stick => "Stick flick",
        };
        let side = if self.deg < 0 { "left" } else { "right" };
        match self.ms {
            Some(ms) => format!("{via} {}° {side} over {ms} ms", self.deg.abs()),
            None => format!("{via} {}° {side}", self.deg.abs()),
        }
    }
}

/// Is this pin a bound flick? Cheap: the engine asks about every output pin.
pub fn is_flick_pin(pin: &str) -> bool {
    pin.starts_with(PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(deg: i32) -> FlickPin {
        FlickPin { via: FlickVia::Mouse, deg, ms: None }
    }

    #[test]
    fn a_pin_id_round_trips() {
        for p in [
            FlickPin::new(FlickVia::Mouse),
            FlickPin { via: FlickVia::Stick, deg: -45, ms: Some(120) },
        ] {
            assert_eq!(FlickPin::parse(&p.to_id()), Some(p));
        }
        assert_eq!(FlickPin::new(FlickVia::Stick).to_id(), "flick:stick:90");
    }

    // A card's list comes from a saved patch: anything else stays not-a-flick.
    #[test]
    fn what_isnt_a_flick_doesnt_read_as_one() {
        for bad in ["key_a", "flick:", "flick:mouse", "flick:wheel:90", "flick:mouse:ninety",
                    "flick:mouse:0", "flick:mouse:90:soon", "flick:mouse:90:10:10"] {
            assert_eq!(FlickPin::parse(bad), None, "{bad}");
        }
        assert_eq!(FlickPin::parse("flick:mouse:9000").map(|p| p.deg), Some(MAX_DEG), "out of range is clamped");
    }

    // Coarse goes along the 45° grid, snapping an odd angle onto it, and past 0.
    #[test]
    fn coarse_steps_land_on_the_grid_and_skip_zero() {
        let up = |d| pin(d).step_deg(1, false).deg;
        let down = |d| pin(d).step_deg(-1, false).deg;
        assert_eq!(up(90), 135);
        assert_eq!(down(90), 45);
        assert_eq!(up(91), 135, "an odd angle snaps up onto the grid");
        assert_eq!(down(91), 90, "and down");
        assert_eq!(down(-46), -90);
        assert_eq!(up(-46), -45);
        assert_eq!(down(45), -45, "not to 0");
        assert_eq!(up(-45), 45, "nor back to it");
        assert_eq!(down(10), -45);
        assert_eq!(up(360), 360, "stops at the end");
        assert_eq!(down(-360), -360);
    }

    #[test]
    fn fine_steps_are_a_degree_and_skip_zero() {
        assert_eq!(pin(90).step_deg(1, true).deg, 91);
        assert_eq!(pin(1).step_deg(-1, true).deg, -1);
        assert_eq!(pin(-1).step_deg(1, true).deg, 1);
    }

    // A dragged or typed 0 moves through to the other side of it.
    #[test]
    fn setting_zero_passes_through_it() {
        assert_eq!(pin(1).with_deg(0).deg, -1);
        assert_eq!(pin(-1).with_deg(0).deg, 1);
        assert_eq!(pin(1).with_deg(400).deg, MAX_DEG);
    }

    #[test]
    fn time_steps_up_from_the_targets_and_back_to_it() {
        let p = pin(90);
        assert_eq!(p.step_ms(-1, 10).ms, None);
        assert_eq!(p.step_ms(1, 10).ms, Some(DEFAULT_MS));
        assert_eq!(p.step_ms(1, 10).step_ms(1, 10).ms, Some(DEFAULT_MS + 10));
        let quick = FlickPin { ms: Some(10), ..p };
        assert_eq!(quick.step_ms(-1, 10).ms, None, "below the smallest step: the target's again");
        assert_eq!(FlickPin { ms: Some(2), ..p }.step_ms(-1, 1).ms, Some(1));
        assert_eq!(FlickPin { ms: Some(1), ..p }.step_ms(-1, 1).ms, None);
    }
}
