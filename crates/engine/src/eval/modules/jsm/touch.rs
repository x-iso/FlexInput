//! The touchpad: a grid of buttons, a pair of relative sticks, or a mouse.
//!
//! ## Units
//!
//! JSM works in touchpad *points* — `TOUCH_STICK_RADIUS` defaults to 300 and
//! `TOUCHPAD_SENS` counts mouse movement per point — and those defaults were
//! chosen against a DualShock 4 / DualSense touchpad, which reports 1920 × 1080.
//! Our bus normalises a finger to -1..1 on each axis, so the two are bridged by
//! that same nominal size: a config carrying JSM's defaults feels the way it did
//! there. A pad with a differently sized touchpad is scaled to the same -1..1, so
//! a sweep across it is a sweep across this one — the honest choice, since nothing
//! on the bus says how many millimetres that was.
//!
//! The bus's touch y counts *up* the touchpad (top edge is +1), as a stick's and
//! the mouse's do — every pad's backend flips its sensor's downward count to get
//! there — so a drag carries straight across; only the grid, whose row 1 is the
//! top row, counts down from the top edge.

use super::analog::StickCfg;

/// The touchpad size JSM's own defaults were tuned against.
const NOMINAL_W: f32 = 1920.0;
const NOMINAL_H: f32 = 1080.0;

/// `TOUCHPAD_MODE`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    /// The touchpad is a grid of buttons plus a relative stick per finger.
    #[default]
    GridAndStick,
    /// A finger drags the mouse pointer.
    Mouse,
    /// The touchpad is handed to a virtual pad as a touchpad.
    PsTouchpad,
    /// `MIDI`: the first finger's place on the pad goes out as MIDI values, to
    /// `TOUCH_MIDI_X` / `TOUCH_MIDI_Y` — an XY pad for a synth.
    Midi,
}

/// What the touch side of a config is configured with.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Settings {
    pub mode: Mode,
    /// `GRID_SIZE`: columns then rows. Their product is 1..=25, because the grid
    /// buttons are named `T1`…`T25`.
    pub grid: (u8, u8),
    /// `TOUCHPAD_SENS`: mouse counts per touchpad point.
    pub sens: (f32, f32),
    /// `TOUCH_STICK_RADIUS`: how far a finger travels, in points, for the touch
    /// stick to read full deflection.
    pub stick_radius: f32,
}

impl Default for Settings {
    fn default() -> Self {
        // JSM's defaults: a 2x1 grid, no mouse scaling, 300-point stick radius.
        Settings { mode: Mode::default(), grid: (2, 1), sens: (1.0, 1.0), stick_radius: 300.0 }
    }
}

/// One finger as the bus reports it: -1..1 on each axis, y up.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Finger {
    pub active: bool,
    pub x: f32,
    pub y: f32,
}

/// What the touchpad produced this tick.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Out {
    /// The grid cell each finger is in, 1-based to match `T1`…`T25`.
    pub cells: [Option<u8>; 2],
    /// Each finger's touch stick, as a stick position (-1..1, y up).
    pub sticks: [(f32, f32); 2],
    /// Mouse movement for `TOUCHPAD_MODE = MOUSE`, in the bus's pixels (y up).
    pub mouse: glam::Vec2,
}

/// One finger's running state.
#[derive(Default, Clone, Copy)]
struct Run {
    was_down: bool,
    prev: (f32, f32),
    /// Where the touch stick has been dragged to, in touchpad points, y up.
    loc: (f32, f32),
}

/// The touch side's running state.
#[derive(Default)]
pub struct Touch {
    fingers: [Run; 2],
}

impl Touch {
    pub fn tick(&mut self, s: &Settings, fingers: [Finger; 2], touch_cfg: StickCfg) -> Out {
        let mut out = Out::default();
        // Columns times rows, kept inside what `T1`…`T25` can name. A config that
        // asks for more is told so when it compiles; this is the belt and braces.
        let cols = s.grid.0.clamp(1, 25) as f32;
        let rows = s.grid.1.clamp(1, 25) as f32;

        for i in 0..2 {
            let f = fingers[i];
            let run = &mut self.fingers[i];

            if !f.active {
                // Lifting resets the stick to centre, so the next touch starts
                // wherever it lands rather than from where the last one ended.
                if run.was_down {
                    run.loc = (0.0, 0.0);
                }
                run.was_down = false;
                continue;
            }

            // How far the finger moved since last tick, in touchpad points, y up.
            let (dx, dy) = if run.was_down {
                (
                    (f.x - run.prev.0) * NOMINAL_W / 2.0,
                    (f.y - run.prev.1) * NOMINAL_H / 2.0,
                )
            } else {
                // The tick a finger lands is not a drag — a first frame counted as
                // movement would jump the stick or the pointer by however far the
                // finger is from where the last one left off.
                (0.0, 0.0)
            };
            run.prev = (f.x, f.y);
            run.was_down = true;

            match s.mode {
                Mode::GridAndStick => {
                    // The grid, from where the finger is: row 1 at the top, so
                    // rows count down from the top edge.
                    let px = ((f.x + 1.0) / 2.0).clamp(0.0, 1.0);
                    let py = ((1.0 - f.y) / 2.0).clamp(0.0, 1.0);
                    let col = ((px * cols).ceil() - 1.0).clamp(0.0, cols - 1.0);
                    let row = ((py * rows).ceil() - 1.0).clamp(0.0, rows - 1.0);
                    out.cells[i] = Some((row * cols + col) as u8 + 1);

                    // The stick, from how far the finger has been dragged.
                    run.loc.0 += dx;
                    run.loc.1 += dy;
                    let r = s.stick_radius.max(1e-6);
                    let mut sx = (run.loc.0 / r).clamp(-1.0, 1.0);
                    let mut sy = (run.loc.1 / r).clamp(-1.0, 1.0);
                    if touch_cfg.invert_x { sx = -sx; }
                    if touch_cfg.invert_y { sy = -sy; }
                    out.sticks[i] = (sx, sy);
                }
                Mode::Mouse => {
                    // JSM takes the first finger down and ignores the second, so
                    // resting a second finger doesn't double the pointer speed.
                    if i == 0 || !fingers[0].active {
                        out.mouse = glam::Vec2::new(dx * s.sens.0, dy * s.sens.1);
                    }
                }
                // The pad's own touchpad passes through to a virtual pad, which is
                // what the bus already does with `touch1_*` — nothing to do here.
                // The finger's position is read straight off the bus by the
                // publish side; nothing here builds on it.
                Mode::PsTouchpad | Mode::Midi => {}
            }
        }
        out
    }
}
