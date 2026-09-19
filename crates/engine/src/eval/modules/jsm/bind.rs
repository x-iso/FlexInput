//! The button side of a JSM config at run time: which binding a press picks,
//! which of its keys fire when, and what ends up held.
//!
//! JSM's press model, as its README documents it and its graph draws it:
//!
//! ```text
//!       hold           turbo period                a tap is shorter
//!        v                  v                      than the hold time
//! ___|---|---|---|---|---|---|--|________|---|_______________
//!    a   b   c   c   c   c   c  d        a   d
//!                               \_ hold release   \_ tap fires here, 40 ms long
//! a start press \   b hold press _   c turbo +   d release press /
//! ```
//!
//! On top of that a press has to decide WHICH binding it belongs to: the top of
//! the chord stack, a double press, a simultaneous or diagonal pair, or the
//! button's own binding. That decision happens once, when the button goes down
//! (except for a simultaneous press, which waits out its window first).

use std::collections::{HashMap, HashSet};

use super::names::{Btn, GyroAction, Out};
use super::parse::{ActionMod, Binding, Compiled, EventMod, Trigger};

/// How long a tap's key stays down (JSM's `MAGIC_TAP_DURATION`), and how long an
/// instant press stays down (`MAGIC_INSTANT_DURATION`). Gyro actions and
/// calibration hold far longer (`MAGIC_EXTENDED_TAP_DURATION`).
const TAP_HOLD: f32 = 0.040;
const INSTANT_HOLD: f32 = 0.040;
const EXTENDED_TAP_HOLD: f32 = 0.500;

/// What one press of a button is doing.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Role {
    Idle,
    /// Down, waiting to see whether a simultaneous partner joins in time.
    WaitSim { until: f32 },
    /// Down and driving press `usize`.
    Active(usize),
    /// Down, but another button's pair press (simultaneous or diagonal) owns it.
    Follower(usize),
}

impl Default for Role {
    fn default() -> Self { Role::Idle }
}

#[derive(Default)]
struct BtnRun {
    down: bool,
    role: Role,
    /// When this button last went down — the double-press window measures from it.
    last_down: f32,
    /// A tap held back to see whether a second press turns it into a double
    /// press: (binding, when to give up waiting).
    pending_tap: Option<(usize, f32)>,
}

/// An active press: a binding, and how far through it we are.
struct Press {
    binding: usize,
    owner: Btn,
    partner: Option<Btn>,
    start: f32,
    hold_fired: bool,
    turbo_next: f32,
    /// Pins this press is holding down, so they release when it ends.
    held: Vec<String>,
    /// Released already: its keys are up and it fires nothing more.
    ended: bool,
}

/// A pin driven for a while rather than held: a tap, an instant press, a turbo
/// pulse, a scroll notch.
struct Timed {
    pin: String,
    until: f32,
}

/// Everything the config asks for right now.
#[derive(Default, PartialEq, Debug)]
pub struct Outputs {
    /// Pins to drive true this tick.
    pub pins: HashSet<String>,
    /// Gyro actions asserted this tick (phase 3 acts on them).
    pub gyro: HashSet<GyroAction>,
    /// Console commands fired this tick (phase 8 runs them).
    pub commands: Vec<String>,
    /// Rumble asked for by a binding, as (strong, weak) — phase 7 sends it.
    pub rumble: Option<(f32, f32)>,
}

/// The run-time state of one JSM Config node.
pub struct Runtime {
    t: f32,
    buttons: HashMap<Btn, BtnRun>,
    presses: Vec<Press>,
    /// Chord buttons currently held, oldest first — the latest chord wins.
    chords: Vec<Btn>,
    /// Pins held by a toggle (`^`), until toggled off again.
    toggles: HashSet<String>,
    timed: Vec<Timed>,
    /// Pins held by active presses, ref-counted so two bindings on one pin don't
    /// release it early.
    held: HashMap<String, u32>,
    out: Outputs,
}

impl Default for Runtime {
    fn default() -> Self {
        Runtime {
            t: 0.0,
            buttons: HashMap::new(),
            presses: Vec::new(),
            chords: Vec::new(),
            toggles: HashSet::new(),
            timed: Vec::new(),
            held: HashMap::new(),
            out: Outputs::default(),
        }
    }
}

impl Runtime {
    /// Advance one tick. `down` answers "is this JSM button pressed right now?".
    pub fn tick(&mut self, cfg: &Compiled, dt: f32, down: &dyn Fn(Btn) -> bool) -> &Outputs {
        self.t += dt;
        self.out = Outputs::default();
        self.timed.retain(|t| t.until > self.t);

        // Buttons the config cares about, in a stable order so a tie between two
        // buttons pressed in the same tick resolves the same way every time.
        let mut watched: Vec<Btn> = cfg.mentioned.iter().copied().collect();
        watched.sort_by_key(|b| b.name());

        // ── releases first, so a button freed this tick can't also start a pair
        for &b in &watched {
            let now = down(b);
            let was = self.buttons.entry(b).or_default().down;
            if was && !now {
                self.release(b, cfg);
            }
        }
        // ── then presses
        for &b in &watched {
            let now = down(b);
            let was = self.buttons.entry(b).or_default().down;
            if !was && now {
                self.press(b, cfg);
            }
            let run = self.buttons.entry(b).or_default();
            run.down = now;
        }
        // ── a simultaneous press that ran out of patience resolves on its own
        for &b in &watched {
            let expired = matches!(self.buttons[&b].role, Role::WaitSim { until } if until <= self.t);
            if expired {
                self.resolve(b, cfg);
            }
        }
        // ── a tap held back for a double press that never came
        for &b in &watched {
            let due = match self.buttons[&b].pending_tap {
                Some((binding, at)) if at <= self.t => Some(binding),
                _ => None,
            };
            if let Some(binding) = due {
                self.buttons.entry(b).or_default().pending_tap = None;
                self.fire(binding, EventMod::Tap, cfg, None);
            }
        }

        self.advance_presses(cfg);
        self.compose();
        &self.out
    }

    // ── press lifecycle ──────────────────────────────────────────────────────

    fn press(&mut self, b: Btn, cfg: &Compiled) {
        {
            let run = self.buttons.entry(b).or_default();
            run.down = true;
        }
        if is_chord_button(cfg, b) && !self.chords.contains(&b) {
            self.chords.push(b);
        }

        // A second press inside the window turns into the double-press binding.
        let last_down = self.buttons[&b].last_down;
        let repeat = self.buttons[&b].last_down > 0.0 && self.t - last_down <= cfg.timings.double;
        self.buttons.entry(b).or_default().last_down = self.t;
        if repeat {
            if let Some(i) = find(cfg, |t| matches!(t, Trigger::Double(x) if *x == b)) {
                self.buttons.entry(b).or_default().pending_tap = None;
                self.start(i, b, None, cfg);
                return;
            }
        }

        // A simultaneous pair needs both buttons inside the window: if the
        // partner is already waiting, they pair up now; otherwise wait for it.
        if let Some((i, partner)) = sim_partner(cfg, b) {
            let ready = matches!(self.buttons.get(&partner).map(|r| r.role), Some(Role::WaitSim { .. }));
            if ready {
                self.take_over(partner);
                self.start(i, b, Some(partner), cfg);
                return;
            }
            self.buttons.entry(b).or_default().role = Role::WaitSim { until: self.t + cfg.timings.sim };
            return;
        }
        self.resolve(b, cfg);
    }

    /// Pick the binding for a button that is down and not waiting on a partner.
    fn resolve(&mut self, b: Btn, cfg: &Compiled) {
        // A diagonal partner already pressing releases its own binding and the
        // pair takes over — no window, either order.
        if let Some((i, partner)) = diag_partner(cfg, b) {
            if self.buttons.get(&partner).is_some_and(|r| matches!(r.role, Role::Active(_))) {
                self.take_over(partner);
                self.start(i, b, Some(partner), cfg);
                return;
            }
        }
        // The top of the chord stack wins, then the button's own binding.
        for &chord in self.chords.iter().rev() {
            if chord == b { continue; }
            if let Some(i) = find(cfg, |t| matches!(t, Trigger::Chord { chord: c, btn }
                if *c == chord && *btn == b))
            {
                self.start(i, b, None, cfg);
                return;
            }
        }
        if let Some(i) = find(cfg, |t| matches!(t, Trigger::Simple(x) if *x == b)) {
            self.start(i, b, None, cfg);
            return;
        }
        self.buttons.entry(b).or_default().role = Role::Idle;
    }

    fn start(&mut self, binding: usize, owner: Btn, partner: Option<Btn>, cfg: &Compiled) {
        let idx = self.presses.len();
        self.presses.push(Press {
            binding,
            owner,
            partner,
            start: self.t,
            hold_fired: false,
            turbo_next: self.t + cfg.timings.hold,
            held: Vec::new(),
            ended: false,
        });
        self.buttons.entry(owner).or_default().role = Role::Active(idx);
        if let Some(p) = partner {
            self.buttons.entry(p).or_default().role = Role::Follower(idx);
        }
        self.fire(binding, EventMod::Start, cfg, Some(idx));
    }

    /// End whatever press a button owns without firing its tap — used when a
    /// pair press takes the button over.
    fn take_over(&mut self, b: Btn) {
        if let Some(Role::Active(i)) = self.buttons.get(&b).map(|r| r.role) {
            self.end_press(i);
        }
        self.buttons.entry(b).or_default().pending_tap = None;
    }

    fn release(&mut self, b: Btn, cfg: &Compiled) {
        self.chords.retain(|&c| c != b);
        let role = self.buttons.entry(b).or_default().role;
        self.buttons.entry(b).or_default().role = Role::Idle;
        self.buttons.entry(b).or_default().down = false;
        let (idx, pair_partner) = match role {
            Role::Active(i) => (i, self.presses.get(i).and_then(|p| p.partner)),
            Role::Follower(i) => (i, self.presses.get(i).map(|p| p.owner)),
            // Released while still waiting for a partner: the button's own
            // binding never started, so resolve it now and let it end at once
            // (a quick tap of a button that has a simultaneous binding).
            Role::WaitSim { .. } => {
                self.resolve(b, cfg);
                let Some(Role::Active(i)) = self.buttons.get(&b).map(|r| r.role) else { return; };
                self.buttons.entry(b).or_default().role = Role::Idle;
                self.finish_press(i, b, cfg);
                return;
            }
            Role::Idle => return,
        };
        self.finish_press(idx, b, cfg);
        // Releasing one half of a pair hands the other half its own binding,
        // the way JSM's diagonal presses do.
        if let Some(other) = pair_partner {
            let still_down = self.buttons.get(&other).is_some_and(|r| r.down);
            let was_diag = self.presses.get(idx).is_some_and(|p|
                matches!(cfg.bindings.get(p.binding).map(|b| b.trigger), Some(Trigger::Diag(..))));
            self.buttons.entry(other).or_default().role = Role::Idle;
            if still_down && was_diag {
                self.resolve(other, cfg);
            }
        }
    }

    /// Fire a press's release (and tap, when it was short) and drop its holds.
    fn finish_press(&mut self, idx: usize, b: Btn, cfg: &Compiled) {
        let Some(press) = self.presses.get(idx) else { return; };
        if press.ended { return; }
        let (binding, held_long, start) = (press.binding, press.hold_fired, press.start);
        self.fire(binding, EventMod::Release, cfg, None);
        if !held_long && self.t - start < cfg.timings.hold {
            // A tap waits out the double-press window when the button has a
            // double-press binding, so the two don't both fire.
            let has_double = find(cfg, |t| matches!(t, Trigger::Double(x) if *x == b)).is_some();
            if has_double {
                self.buttons.entry(b).or_default().pending_tap =
                    Some((binding, self.t + cfg.timings.double));
            } else {
                self.fire(binding, EventMod::Tap, cfg, None);
            }
        }
        self.end_press(idx);
    }

    /// Release everything a press holds and mark it done.
    fn end_press(&mut self, idx: usize) {
        let Some(press) = self.presses.get_mut(idx) else { return; };
        if press.ended { return; }
        press.ended = true;
        let held = std::mem::take(&mut press.held);
        for pin in held {
            if let Some(n) = self.held.get_mut(&pin) {
                *n = n.saturating_sub(1);
                if *n == 0 { self.held.remove(&pin); }
            }
        }
    }

    fn advance_presses(&mut self, cfg: &Compiled) {
        let live: Vec<usize> = self.presses.iter().enumerate()
            .filter(|(_, p)| !p.ended)
            .map(|(i, _)| i)
            .collect();
        for i in live {
            let (binding, start, hold_fired, turbo_next) = {
                let p = &self.presses[i];
                (p.binding, p.start, p.hold_fired, p.turbo_next)
            };
            if !hold_fired && self.t - start >= cfg.timings.hold {
                self.presses[i].hold_fired = true;
                self.fire(binding, EventMod::Hold, cfg, Some(i));
            }
            // Turbo pulses only once the button has been held: `turbo_next`
            // starts one hold time in, then moves on by a turbo period each time.
            if self.t >= turbo_next {
                self.presses[i].turbo_next = self.t + cfg.timings.turbo;
                self.fire(binding, EventMod::Turbo, cfg, None);
            }
        }
        // Drop finished presses once nothing points at them any more.
        if self.presses.iter().all(|p| p.ended) && !self.presses.is_empty() {
            let owners: Vec<Btn> = self.buttons.iter()
                .filter(|(_, r)| matches!(r.role, Role::Active(_) | Role::Follower(_)))
                .map(|(b, _)| *b)
                .collect();
            if owners.is_empty() { self.presses.clear(); }
        }
    }

    // ── firing one event of a binding ────────────────────────────────────────

    fn fire(&mut self, binding: usize, event: EventMod, cfg: &Compiled, press: Option<usize>) {
        let Some(b) = cfg.bindings.get(binding) else { return; };
        for step in b.steps.iter().filter(|s| s.event == event) {
            self.act(step.action, &step.out, event, press, b);
        }
    }

    fn act(&mut self, action: ActionMod, out: &Out, event: EventMod, press: Option<usize>, binding: &Binding) {
        let _ = binding;
        match out {
            Out::None | Out::Unsupported { .. } => {}
            Out::Gyro(g) => { self.out.gyro.insert(*g); }
            Out::Calibrate => {}
            Out::Command(c) => self.out.commands.push(c.clone()),
            Out::Rumble { strong, weak } => self.out.rumble = Some((*strong, *weak)),
            Out::Pulse(pin) => self.timed.push(Timed { pin: pin.clone(), until: self.t + TAP_HOLD }),
            Out::Pin(pin) => match action {
                ActionMod::Toggle => {
                    if !self.toggles.remove(pin) { self.toggles.insert(pin.clone()); }
                }
                ActionMod::Instant => {
                    self.timed.push(Timed { pin: pin.clone(), until: self.t + INSTANT_HOLD });
                }
                ActionMod::Release => {
                    self.toggles.remove(pin);
                    self.held.remove(pin);
                    self.timed.retain(|t| &t.pin != pin);
                }
                ActionMod::None => match event {
                    // Held until the button lets go.
                    EventMod::Start | EventMod::Hold => {
                        *self.held.entry(pin.clone()).or_insert(0) += 1;
                        if let Some(i) = press {
                            if let Some(p) = self.presses.get_mut(i) { p.held.push(pin.clone()); }
                        }
                    }
                    // A tap is a short press of its own; gyro actions and
                    // calibration hold much longer, which is JSM's own rule.
                    EventMod::Tap | EventMod::Turbo => {
                        let hold = if matches!(out, Out::Gyro(_) | Out::Calibrate) {
                            EXTENDED_TAP_HOLD
                        } else {
                            TAP_HOLD
                        };
                        self.timed.push(Timed { pin: pin.clone(), until: self.t + hold });
                    }
                    EventMod::Release => {}
                },
            },
        }
    }

    fn compose(&mut self) {
        for pin in self.held.keys() { self.out.pins.insert(pin.clone()); }
        for t in &self.timed { self.out.pins.insert(t.pin.clone()); }
        for pin in &self.toggles { self.out.pins.insert(pin.clone()); }
    }
}

// ── lookups over the compiled config ────────────────────────────────────────

fn find(cfg: &Compiled, pred: impl Fn(&Trigger) -> bool) -> Option<usize> {
    cfg.bindings.iter().position(|b| pred(&b.trigger))
}

fn is_chord_button(cfg: &Compiled, b: Btn) -> bool {
    cfg.bindings.iter().any(|x| matches!(x.trigger, Trigger::Chord { chord, .. } if chord == b))
}

fn sim_partner(cfg: &Compiled, b: Btn) -> Option<(usize, Btn)> {
    cfg.bindings.iter().enumerate().find_map(|(i, x)| match x.trigger {
        Trigger::Sim(a, c) if a == b => Some((i, c)),
        Trigger::Sim(a, c) if c == b => Some((i, a)),
        _ => None,
    })
}

fn diag_partner(cfg: &Compiled, b: Btn) -> Option<(usize, Btn)> {
    cfg.bindings.iter().enumerate().find_map(|(i, x)| match x.trigger {
        Trigger::Diag(a, c) if a == b => Some((i, c)),
        Trigger::Diag(a, c) if c == b => Some((i, a)),
        _ => None,
    })
}
