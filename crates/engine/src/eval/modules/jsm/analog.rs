//! The analog half of a JSM config: a trigger's soft and full pull, and a
//! stick's directions, ring and scroll wheel.
//!
//! JSM turns analog inputs into the same buttons everything else is built on, so
//! this runs before the press machinery and answers "is ZL down?", "is LUP
//! down?" for it. The rules are JSM's own (`src/JoyShock.cpp`,
//! `processTriggerPress` and `processStick`), including the dual-stage trigger
//! state machine and its skip modes.
//!
//! What a later phase owns is deliberately absent: a stick in a mouse or pad
//! mode ([`StickMode::Elsewhere`]) drives no directions here, and its ring still
//! does — which is how JSM behaves.

use std::collections::HashSet;

use super::names::{Btn, Dir, StickId};

/// A stick counts as ringing past this much of its travel (JSM's constant).
const RING_EDGE: f32 = 0.7;
/// How long a scroll notch holds its direction button (JSM's `MAGIC_TAP_DURATION`).
const SCROLL_TAP: f32 = 0.040;
/// The smallest soft-pull threshold we use. JSM's default is 0 — "the slightest
/// press" — which on a pad whose trigger rests a hair above zero would sit on
/// permanently, so a resting trigger has to clear this much noise first.
const NOISE_FLOOR: f32 = 0.02;

/// JSM's dual-stage trigger modes (`ZL_MODE` / `ZR_MODE`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TriggerMode {
    /// The full pull is ignored: `ZLF` / `ZRF` never fire (JSM's default).
    #[default]
    NoFull,
    /// Full pull fires on top of the soft pull.
    NoSkip,
    /// Full pull replaces the soft pull while it lasts.
    NoSkipExclusive,
    /// Only a quick full pull fires, and it skips the soft pull.
    MustSkip,
    /// A quick full pull skips the soft pull; a later one fires on top of it.
    MaySkip,
    /// `MUST_SKIP`, but the soft pull presses at once and is taken back if the
    /// full pull arrives quickly.
    MustSkipR,
    /// `MAY_SKIP`, responsive in the same way.
    MaySkipR,
}

impl TriggerMode {
    /// Does this mode ever fire the full pull?
    pub fn has_full(self) -> bool { self != TriggerMode::NoFull }
}

/// The stick modes this module runs, plus one standing for the rest.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum StickMode {
    /// Directions are buttons (JSM's default).
    #[default]
    NoMouse,
    /// Turning the stick works a scroll wheel through its left / right buttons.
    ScrollWheel,
    /// The stick aims the mouse.
    Aim,
    /// Flick stick: push to face that way, turn to trace the camera.
    Flick,
    /// Flick without the turning.
    FlickOnly,
    /// Turning without the flick.
    RotateOnly,
    /// The stick moves the pointer by how far it is pushed.
    MouseArea,
    /// A mode a later phase owns: nothing comes from the stick here.
    Elsewhere,
}

impl StickMode {
    /// Does this mode make the stick's directions into buttons?
    pub fn is_digital(self) -> bool { matches!(self, StickMode::NoMouse | StickMode::ScrollWheel) }

    /// Does this mode drive the mouse?
    pub fn aims(self) -> bool {
        matches!(self, StickMode::Aim | StickMode::Flick | StickMode::FlickOnly
            | StickMode::RotateOnly | StickMode::MouseArea)
    }

    /// Is the stick this module's to read at all?
    pub fn runs_here(self) -> bool { self.is_digital() || self.aims() }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RingMode {
    Inner,
    #[default]
    Outer,
}

/// Which way the controller is being held (`CONTROLLER_ORIENTATION`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Orientation {
    #[default]
    Forward,
    Left,
    Right,
    Backward,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct StickCfg {
    pub mode: StickMode,
    pub ring: RingMode,
    pub inner_dz: f32,
    /// JSM's own sense: how much of the OUTER edge is full deflection, so the
    /// stick saturates at `1 - outer_dz`.
    pub outer_dz: f32,
    pub invert_x: bool,
    pub invert_y: bool,
    /// Degrees of turn per scroll notch.
    pub scroll_sens: f32,
}

impl Default for StickCfg {
    fn default() -> Self {
        // JSM's defaults: NO_MOUSE, ring OUTER, deadzones 0.15 / 0.1, SCROLL_SENS 30.
        StickCfg {
            mode: StickMode::default(),
            ring: RingMode::default(),
            inner_dz: 0.15,
            outer_dz: 0.1,
            invert_x: false,
            invert_y: false,
            scroll_sens: 30.0,
        }
    }
}

/// Everything the analog side reads out of a config.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Settings {
    /// `TRIGGER_THRESHOLD`: below zero means hair trigger.
    pub threshold: f32,
    /// `TRIGGER_SKIP_DELAY`, in seconds.
    pub skip_delay: f32,
    pub zl: TriggerMode,
    pub zr: TriggerMode,
    pub left: StickCfg,
    pub right: StickCfg,
    pub orientation: Orientation,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            threshold: 0.0,
            skip_delay: 0.150,
            zl: TriggerMode::default(),
            zr: TriggerMode::default(),
            left: StickCfg::default(),
            right: StickCfg::default(),
            orientation: Orientation::default(),
        }
    }
}

/// What the pad is reporting this tick, in JSM's terms.
#[derive(Clone, Copy, Default, Debug)]
pub struct Pad {
    /// Trigger positions, 0..1 — `None` where the pad has no analog trigger.
    pub triggers: [Option<f32>; 2],
    /// The digital trigger button some pads report instead.
    pub trigger_digital: [bool; 2],
    /// Stick positions, -1..1, y up.
    pub sticks: [(f32, f32); 2],
}

/// Where a trigger is in JSM's dual-stage state machine.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Dst {
    #[default]
    NoPress,
    /// Down, waiting to see whether a full pull follows quickly.
    PressStart,
    /// The same, with the soft pull already pressed (the responsive modes).
    PressStartResp,
    /// Let go before the wait was over: the soft pull fires for a moment.
    QuickSoftTap,
    QuickFullPress,
    QuickFullRelease,
    SoftPress,
    /// Full pull on top of a soft pull that stays.
    DelayFullPress,
    /// Full pull instead of the soft pull.
    ExclFullPress,
}

#[derive(Default)]
struct TriggerRun {
    state: Dst,
    /// When the current soft press started (the skip delay measures from it).
    since: f32,
    /// The last few positions, for the hair trigger's rolling averages.
    hist: [f32; 5],
    hair_down: bool,
}

#[derive(Default)]
struct StickRun {
    /// Where the stick is now, and was last tick, after the deadzones.
    last: (f32, f32),
    prev: (f32, f32),
    /// The same two, before the deadzones — `MOUSE_AREA` works off raw movement.
    raw: (f32, f32),
    prev_raw: (f32, f32),
    len: f32,
    /// Pushed past the outer deadzone.
    pegged: bool,
    /// Doing something this tick, in whatever its mode is: what `GYRO_OFF =
    /// LEFT_STICK` watches for.
    active: bool,
    mode: StickMode,
    /// Degrees of turn not yet spent on a notch.
    leftovers: f32,
    /// The direction a notch is holding, and until when.
    notch: Option<(Btn, f32)>,
}

/// One stick as the aiming side needs to see it.
#[derive(Clone, Copy, Debug)]
pub struct StickOut {
    pub mode: StickMode,
    /// After the deadzones, this tick and last.
    pub x: f32,
    pub y: f32,
    pub prev_x: f32,
    pub prev_y: f32,
    /// Before them.
    pub raw: (f32, f32),
    pub prev_raw: (f32, f32),
    pub len: f32,
    pub pegged: bool,
}

/// The analog state of one JSM Config node.
#[derive(Default)]
pub struct Analog {
    t: f32,
    triggers: [TriggerRun; 2],
    sticks: [StickRun; 2],
    down: HashSet<Btn>,
}

impl Analog {
    /// Advance one tick and work out which analog-derived buttons are down.
    pub fn tick(&mut self, s: &Settings, dt: f32, pad: &Pad) {
        self.t += dt;
        self.down.clear();
        for side in 0..2 {
            self.trigger(s, side, pad);
            self.stick(s, side, pad);
        }
    }

    /// Is this analog-derived button down right now?
    pub fn down(&self, b: Btn) -> bool { self.down.contains(&b) }

    // ── triggers ─────────────────────────────────────────────────────────────

    fn trigger(&mut self, s: &Settings, side: usize, pad: &Pad) {
        let (soft_btn, full_btn) = if side == 0 { (Btn::Zl, Btn::Zlf) } else { (Btn::Zr, Btn::Zrf) };
        let analog = pad.triggers[side];
        let digital = pad.trigger_digital[side];
        // A pad with no analog trigger is all or nothing, and JSM forces NO_FULL
        // on those rather than firing the full pull off a button press.
        let mode = match analog {
            Some(_) => if side == 0 { s.zl } else { s.zr },
            None => TriggerMode::NoFull,
        };
        let position = analog.unwrap_or(0.0).max(if digital { 1.0 } else { 0.0 }).clamp(0.0, 1.0);
        let full = position >= 1.0;
        let soft_pull = self.soft_pull(s, side, position);

        let run = &mut self.triggers[side];
        let (mut soft_on, mut full_on) = (false, false);
        match run.state {
            Dst::NoPress => {
                if soft_pull {
                    match mode {
                        TriggerMode::MaySkip | TriggerMode::MustSkip => {
                            run.state = Dst::PressStart;
                            run.since = self.t;
                        }
                        TriggerMode::MaySkipR | TriggerMode::MustSkipR => {
                            run.state = Dst::PressStartResp;
                            run.since = self.t;
                            soft_on = true;
                        }
                        _ => {
                            run.state = Dst::SoftPress;
                            soft_on = true;
                        }
                    }
                }
            }
            Dst::PressStart => {
                if !soft_pull {
                    // Tapped and let go before we knew: the soft pull fires now.
                    run.state = Dst::QuickSoftTap;
                    soft_on = true;
                } else if full {
                    run.state = Dst::QuickFullPress;
                    full_on = true;
                } else if self.t - run.since >= s.skip_delay {
                    run.state = Dst::SoftPress;
                    run.since = self.t;
                    soft_on = true;
                }
            }
            Dst::PressStartResp => {
                if !soft_pull {
                    run.state = Dst::NoPress;
                } else if full {
                    // The soft pull was already pressed: take it back.
                    run.state = Dst::QuickFullPress;
                    full_on = true;
                } else {
                    if self.t - run.since >= s.skip_delay {
                        run.state = Dst::SoftPress;
                    }
                    soft_on = true;
                }
            }
            Dst::QuickSoftTap => run.state = Dst::NoPress,
            Dst::QuickFullPress => {
                if full {
                    full_on = true;
                } else {
                    run.state = Dst::QuickFullRelease;
                }
            }
            Dst::QuickFullRelease => {
                // Wait out the rest of the pull before anything can fire again.
                if !soft_pull {
                    run.state = Dst::NoPress;
                } else if full {
                    run.state = Dst::QuickFullPress;
                    full_on = true;
                }
            }
            Dst::SoftPress => {
                if !soft_pull {
                    run.state = Dst::NoPress;
                } else {
                    match mode {
                        TriggerMode::NoSkip | TriggerMode::MaySkip | TriggerMode::MaySkipR => {
                            soft_on = true;
                            if full {
                                run.state = Dst::DelayFullPress;
                                full_on = true;
                            }
                        }
                        TriggerMode::NoSkipExclusive => {
                            if full {
                                run.state = Dst::ExclFullPress;
                                full_on = true;
                            } else {
                                soft_on = true;
                            }
                        }
                        _ => soft_on = true,
                    }
                }
            }
            Dst::DelayFullPress => {
                soft_on = true;
                if full {
                    full_on = true;
                } else {
                    run.state = Dst::SoftPress;
                }
            }
            Dst::ExclFullPress => {
                if full {
                    full_on = true;
                } else {
                    run.state = Dst::SoftPress;
                    soft_on = true;
                }
            }
        }
        if soft_on { self.down.insert(soft_btn); }
        // No guard on the mode here: NO_FULL never reaches a state that fires the
        // full pull, which is how JSM leaves it too.
        if full_on { self.down.insert(full_btn); }
    }

    /// Is the trigger past its soft-pull point? Either a plain threshold, or —
    /// when the threshold is negative — JSM's hair trigger, which presses while
    /// the trigger is still moving down and releases while it is coming back up.
    fn soft_pull(&mut self, s: &Settings, side: usize, position: f32) -> bool {
        if s.threshold >= 0.0 {
            let run = &mut self.triggers[side];
            run.hist.rotate_left(1);
            run.hist[4] = position;
            return position > s.threshold.max(NOISE_FLOOR);
        }
        let run = &mut self.triggers[side];
        let h = run.hist;
        // Four three-sample averages ending at this one, as JSM smooths them.
        let avg = |a: f32, b: f32, c: f32| (a + b + c) / 3.0;
        let (t3, t2, t1, t0) = (
            avg(h[0], h[1], h[2]),
            avg(h[1], h[2], h[3]),
            avg(h[2], h[3], h[4]),
            avg(h[3], h[4], position),
        );
        if t0 > t1 && t1 > t2 && t2 > t3 {
            run.hair_down = true;
        } else if t0 < t1 && t1 < t2 && t2 < t3 {
            run.hair_down = false;
        }
        run.hist.rotate_left(1);
        run.hist[4] = position;
        run.hair_down
    }

    // ── sticks ───────────────────────────────────────────────────────────────

    fn stick(&mut self, s: &Settings, side: usize, pad: &Pad) {
        let (cfg, id) = if side == 0 {
            (s.left, StickId::Left)
        } else {
            (s.right, StickId::Right)
        };
        let (mut x, mut y) = pad.sticks[side];
        if cfg.invert_x { x = -x; }
        if cfg.invert_y { y = -y; }
        let (raw_x, raw_y) = orient(s.orientation, x, y);
        let raw_len = (raw_x * raw_x + raw_y * raw_y).sqrt();
        let outer = 1.0 - cfg.outer_dz;
        let (x, y) = dead_zones(raw_x, raw_y, cfg.inner_dz, outer);
        // Past the outer deadzone the stick reads full, and it has to read
        // *exactly* full: normalising leaves a length of 0.99999994, and a flick
        // needs the stick at 1.0 before it will start.
        let pegged = raw_len >= outer;
        let len = if pegged { 1.0 } else { (x * x + y * y).sqrt() };

        // The ring fires whatever the stick mode is — JSM checks it first.
        let ringing = match cfg.ring {
            RingMode::Inner => len > 0.0 && len < RING_EDGE,
            RingMode::Outer => len > RING_EDGE,
        };
        if ringing { self.down.insert(btn_for(id, Dir::Ring)); }

        let mut active = false;
        match cfg.mode {
            StickMode::NoMouse => {
                // JSM's eight sectors: a diagonal lights both of its directions.
                let (ax, ay) = (x.abs(), y.abs());
                for (on, dir) in [
                    (x < -0.5 * ay, Dir::Left),
                    (x > 0.5 * ay, Dir::Right),
                    (y > 0.5 * ax, Dir::Up),
                    (y < -0.5 * ax, Dir::Down),
                ] {
                    if on {
                        self.down.insert(btn_for(id, dir));
                        // The ring doesn't count as the stick being used.
                        active = true;
                    }
                }
            }
            StickMode::ScrollWheel => self.scroll(side, id, cfg, x, y),
            // What counts as "being used" depends on what the stick is for:
            // aiming starts inside the deadzone, a flick only at full push.
            StickMode::Aim | StickMode::MouseArea => active = raw_len > cfg.inner_dz,
            StickMode::Flick | StickMode::FlickOnly | StickMode::RotateOnly =>
                active = raw_len > outer,
            StickMode::Elsewhere => {}
        }

        let run = &mut self.sticks[side];
        run.prev = run.last;
        run.last = (x, y);
        run.prev_raw = run.raw;
        run.raw = (raw_x, raw_y);
        run.len = len;
        run.pegged = pegged;
        run.active = active;
        run.mode = cfg.mode;
    }

    /// One stick as the aiming side needs to see it.
    pub fn stick_out(&self, side: usize) -> StickOut {
        let run = &self.sticks[side];
        StickOut {
            mode: run.mode,
            x: run.last.0,
            y: run.last.1,
            prev_x: run.prev.0,
            prev_y: run.prev.1,
            raw: run.raw,
            prev_raw: run.prev_raw,
            len: run.len,
            pegged: run.pegged,
        }
    }

    /// Is this stick doing something this tick? `GYRO_OFF = LEFT_STICK` turns the
    /// gyro off while it is.
    pub fn stick_active(&self, side: usize) -> bool { self.sticks[side].active }

    /// Turning the stick spends degrees on scroll notches, each of which presses
    /// the stick's left or right button for a moment.
    fn scroll(&mut self, side: usize, id: StickId, cfg: StickCfg, x: f32, y: f32) {
        let t = self.t;
        let run = &mut self.sticks[side];
        if x == 0.0 && y == 0.0 {
            // Back to centre: forget the turn so far rather than banking it.
            run.leftovers = 0.0;
            run.notch = None;
            return;
        }
        let (lx, ly) = run.last;
        if lx != 0.0 || ly != 0.0 {
            let last_angle = ly.atan2(lx).to_degrees();
            let angle = y.atan2(x).to_degrees();
            let mut last_angle = last_angle;
            // Crossing the ±180° seam is a small turn, not a full circle back.
            if (last_angle > 0.0) != (angle > 0.0) && (angle - last_angle).abs() > 270.0 {
                last_angle += if last_angle > 0.0 { -360.0 } else { 360.0 };
            }
            run.leftovers += angle - last_angle;
        }
        // A notch holds its button briefly, and nothing else fires until it ends.
        if let Some((btn, until)) = run.notch {
            if t < until {
                self.down.insert(btn);
                return;
            }
            run.notch = None;
            return;
        }
        let sens = cfg.scroll_sens.max(1.0);
        if run.leftovers.abs() > sens {
            let dir = if run.leftovers > 0.0 { Dir::Left } else { Dir::Right };
            run.leftovers -= sens * run.leftovers.signum();
            let btn = btn_for(id, dir);
            run.notch = Some((btn, t + SCROLL_TAP));
            self.down.insert(btn);
        }
    }
}

/// JSM's stick deadzones: nothing inside `inner`, full deflection past `outer`,
/// and the span between them rescaled to 0..1.
fn dead_zones(x: f32, y: f32, inner: f32, outer: f32) -> (f32, f32) {
    let len = (x * x + y * y).sqrt();
    if len <= inner { return (0.0, 0.0); }
    if len >= outer { return (x / len, y / len); }
    let scaled = (len - inner) / (outer - inner);
    let rescale = scaled / len;
    (x * rescale, y * rescale)
}

/// Turn the stick with the controller (`CONTROLLER_ORIENTATION`).
fn orient(o: Orientation, x: f32, y: f32) -> (f32, f32) {
    match o {
        Orientation::Forward => (x, y),
        Orientation::Left => (-y, x),
        Orientation::Right => (y, -x),
        Orientation::Backward => (-x, -y),
    }
}

fn btn_for(stick: StickId, dir: Dir) -> Btn {
    match (stick, dir) {
        (StickId::Left, Dir::Up) => Btn::Lup,
        (StickId::Left, Dir::Down) => Btn::Ldown,
        (StickId::Left, Dir::Left) => Btn::Lleft,
        (StickId::Left, Dir::Right) => Btn::Lright,
        (StickId::Left, Dir::Ring) => Btn::Lring,
        (StickId::Right, Dir::Up) => Btn::Rup,
        (StickId::Right, Dir::Down) => Btn::Rdown,
        (StickId::Right, Dir::Left) => Btn::Rleft,
        (StickId::Right, Dir::Right) => Btn::Rright,
        (StickId::Right, Dir::Ring) => Btn::Rring,
    }
}
