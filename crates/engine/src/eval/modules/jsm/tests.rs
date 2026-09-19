//! Parser tests — the grammar and the per-line statuses the editor shows.

use super::bind::*;
use super::names::*;
use super::parse::*;

fn one(line: &str) -> LineInfo {
    let c = compile(line);
    c.lines.into_iter().next().expect("a line")
}

fn steps(line: &str) -> Vec<Step> {
    let c = compile(line);
    assert_eq!(c.lines[0].status, LineStatus::Ok, "{line} should be live: {:?}", c.lines[0]);
    c.bindings.into_iter().next().expect("a binding").steps
}

fn pin(name: &str) -> Out { Out::Pin(name.to_string()) }

// ── the value side ───────────────────────────────────────────────────────────

// The only key of a binding follows the press; a second key is the hold and the
// first becomes the tap (JSM's defaults).
#[test]
fn single_key_follows_the_press_and_a_pair_is_tap_then_hold() {
    let s = steps("S = SPACE");
    assert_eq!(s, vec![Step { out: pin("key_space"), action: ActionMod::None, event: EventMod::Start }]);

    let s = steps("W = R E");
    assert_eq!(s, vec![
        Step { out: pin("key_r"), action: ActionMod::None, event: EventMod::Tap },
        Step { out: pin("key_e"), action: ActionMod::None, event: EventMod::Hold },
    ]);

    // NONE takes the slot without doing anything: tap reloads, hold does nothing.
    let s = steps("W = R NONE");
    assert_eq!(s[1], Step { out: Out::None, action: ActionMod::None, event: EventMod::Hold });
}

// Action modifiers before the key, event modifiers after it.
#[test]
fn modifiers_parse_on_both_sides_of_the_key() {
    // JSM's own example: toggle ADS on tap, release the toggle on hold.
    let s = steps("ZL = ^RMOUSE\\ RMOUSE_");
    assert_eq!(s, vec![
        Step { out: pin("mouse_right"), action: ActionMod::Toggle, event: EventMod::Start },
        Step { out: pin("mouse_right"), action: ActionMod::None, event: EventMod::Hold },
    ]);

    // Instant press and release, turning an in-game toggle into a plain press.
    let s = steps("E = !C\\ !C/");
    assert_eq!(s, vec![
        Step { out: pin("key_c"), action: ActionMod::Instant, event: EventMod::Start },
        Step { out: pin("key_c"), action: ActionMod::Instant, event: EventMod::Release },
    ]);

    // Turbo, and a release modifier that only sends the key up.
    assert_eq!(steps("S = SPACE+")[0].event, EventMod::Turbo);
    assert_eq!(steps("S = -LSHIFT/")[0].action, ActionMod::Release);
}

#[test]
fn a_third_key_must_say_which_event_it_wants() {
    let s = steps("R3 = !1\\ LMOUSE+ !Q/");
    assert_eq!(s.len(), 3);
    assert_eq!(s[1].event, EventMod::Turbo);

    let info = one("R3 = A B C");
    assert!(matches!(info.status, LineStatus::Error(ref e) if e.contains("third key")), "{info:?}");
}

#[test]
fn a_key_on_release_needs_an_action_modifier() {
    let info = one("S = SPACE/");
    assert!(matches!(info.status, LineStatus::Error(ref e) if e.contains("action modifier")), "{info:?}");
}

// A console command in quotes fires once, and can only be instant. The line
// itself waits for the phase that runs commands.
#[test]
fn a_quoted_command_is_instant() {
    let (s, _) = parse_mapping("\"GyroConfigs/driving.txt\"").expect("parses");
    assert_eq!(s[0].action, ActionMod::Instant);
    assert_eq!(s[0].out, Out::Command("GyroConfigs/driving.txt".into()));
    assert!(matches!(one("HOME = \"GyroConfigs/driving.txt\"").status,
        LineStatus::Pending(p) if p.contains("another config")));

    let info = one("HOME = ^\"RESET_MAPPINGS\"");
    assert!(matches!(info.status, LineStatus::Error(ref e) if e.contains("instant")), "{info:?}");
}

// Calibration on a tap or release only makes sense as a toggle, so JSM makes it
// one. The line runs, but says that recalibrating is not ours to do.
#[test]
fn calibrate_on_a_tap_becomes_a_toggle() {
    let c = compile("HOME = CALIBRATE'");
    assert_eq!(c.lines[0].status, LineStatus::Ok, "{:?}", c.lines[0]);
    assert_eq!(c.bindings[0].steps[0].action, ActionMod::Toggle);
    assert!(c.lines[0].notes.iter().any(|n| n.contains("device card")),
        "the line should say who owns calibration: {:?}", c.lines[0].notes);
}

#[test]
fn unknown_names_are_errors() {
    let info = one("S = FLOOMP");
    assert!(matches!(info.status, LineStatus::Error(ref e) if e.contains("FLOOMP")), "{info:?}");
    let info = one("FLOOMP = SPACE");
    assert!(matches!(info.status, LineStatus::Error(ref e) if e.contains("FLOOMP")), "{info:?}");
}

// ── the trigger side ─────────────────────────────────────────────────────────

#[test]
fn combos_parse_into_their_own_triggers() {
    let t = |line: &str| compile(line).bindings.into_iter().next().expect("a binding").trigger;
    assert_eq!(t("L,W = 3"), Trigger::Chord { chord: Btn::L, btn: Btn::W });
    assert_eq!(t("N,N = X"), Trigger::Double(Btn::N));
    assert_eq!(t("L+R = Q"), Trigger::Sim(Btn::L, Btn::R));
    assert_eq!(t("UP*RIGHT = 2"), Trigger::Diag(Btn::Up, Btn::Right));
    // `-` and `+` are button names, so a chord can be built from them.
    assert_eq!(t("-,S = SPACE+"), Trigger::Chord { chord: Btn::Minus, btn: Btn::S });
    assert_eq!(t("+,S = SPACE"), Trigger::Chord { chord: Btn::Plus, btn: Btn::S });
}

#[test]
fn every_button_a_line_names_counts_as_mentioned() {
    let c = compile("L+R = Q\nUP*RIGHT = 2\nZLF,GYRO_SENS = 0.5");
    for b in [Btn::L, Btn::R, Btn::Up, Btn::Right, Btn::Zlf] {
        assert!(c.mentioned.contains(&b), "{} should be mentioned", b.name());
    }
    assert!(!c.mentioned.contains(&Btn::S));
}

// ── settings, commands, comments ─────────────────────────────────────────────

#[test]
fn timings_are_live_and_in_seconds() {
    let c = compile("HOLD_PRESS_TIME = 200\nTURBO_PERIOD = 40\nSIM_PRESS_WINDOW = 60\nDBL_PRESS_WINDOW = 300");
    assert!(c.lines.iter().all(|l| l.status == LineStatus::Ok), "{:?}", c.lines);
    assert_eq!(c.timings.hold, 0.2);
    assert_eq!(c.timings.turbo, 0.04);
    assert_eq!(c.timings.sim, 0.06);
    assert_eq!(c.timings.double, 0.3);

    let info = one("HOLD_PRESS_TIME = soon");
    assert!(matches!(info.status, LineStatus::Error(ref e) if e.contains("milliseconds")), "{info:?}");
}

// A setting we recognise but don't run yet says which phase will run it, rather
// than being dropped or called an error.
#[test]
fn recognised_but_not_live_settings_name_their_phase() {
    for line in ["RIGHT_STICK_MODE = HYBRID_AIM", "GYRO_SPACE = WORLD_TURN", "ZL_MODE = X_LT",
                 "TOUCHPAD_MODE = MOUSE", "RUMBLE = OFF", "GYRO_OUTPUT = RIGHT_STICK"] {
        assert!(matches!(one(line).status, LineStatus::Pending(_)), "{line} should be pending");
    }
    assert!(matches!(one("ZLF,GYRO_SENS = 0.5").status, LineStatus::Pending(p) if p.contains("while a button is held")));
}

// Settings FlexInput owns are ignored on purpose, with the reason.
#[test]
fn settings_flexinput_owns_are_ignored_with_a_reason() {
    for line in ["AUTOLOAD = OFF", "JSM_DIRECTORY = D:\\JSM", "TICK_TIME = 3",
                 "VIRTUAL_CONTROLLER = DS4", "AUTO_CALIBRATE_GYRO = ON", "WHITELIST_ADD"] {
        assert!(matches!(one(line).status, LineStatus::Ignored(_)), "{line} should be ignored");
    }
    assert!(matches!(one("RESTART_GYRO_CALIBRATION").status,
        LineStatus::Ignored(w) if w.contains("device card")));
}

#[test]
fn comments_and_blank_lines_are_blank_and_dont_hide_a_binding() {
    let c = compile("# a comment\n\n   \nS = SPACE # jump\n");
    assert_eq!(c.lines[0].status, LineStatus::Blank);
    assert_eq!(c.lines[1].status, LineStatus::Blank);
    assert_eq!(c.lines[2].status, LineStatus::Blank);
    assert_eq!(c.lines[3].status, LineStatus::Ok);
    assert_eq!(c.bindings.len(), 1);
}

#[test]
fn naming_a_config_file_waits_for_the_layers_phase() {
    assert!(matches!(one("GyroConfigs/xbox.txt").status, LineStatus::Pending(p) if p.contains("another config")));
}

// ── name mapping caveats ─────────────────────────────────────────────────────

// JSM's left/right modifier variants land on our generic pins, and the line says so.
#[test]
fn left_right_modifiers_note_that_they_become_generic() {
    let c = compile("E = LSHIFT");
    assert_eq!(c.lines[0].status, LineStatus::Ok);
    assert_eq!(c.bindings[0].steps[0].out, pin("key_shift"));
    assert!(c.lines[0].notes[0].contains("generic Shift"), "{:?}", c.lines[0].notes);
}

// Keys our sink can't send yet are reported, not silently dropped.
#[test]
fn keys_we_cannot_send_are_reported() {
    let c = compile("E = N5");
    assert!(matches!(c.lines[0].status, LineStatus::Ignored(_)), "{:?}", c.lines[0]);
    assert!(c.lines[0].notes[0].contains("numpad"), "{:?}", c.lines[0].notes);
    assert!(c.bindings.is_empty());

    let c = compile("E = PLAY_PAUSE");
    assert!(c.lines[0].notes[0].contains("media"), "{:?}", c.lines[0].notes);
}

// Pad output names are aliases of each other in JSM and land on our pad pins.
#[test]
fn pad_output_names_map_to_our_pins() {
    assert_eq!(steps("S = X_A")[0].out, pin("btn_south"));
    assert_eq!(steps("S = PS_CROSS")[0].out, pin("btn_south"));
    assert_eq!(steps("ZL = X_LT")[0].out, pin("left_trigger"));
}

// A button we can't read yet compiles, but waits for its phase rather than
// pretending to work.
#[test]
fn inputs_we_cannot_read_yet_wait_for_their_phase() {
    let c = compile("MUP = W\nTOUCH = LMOUSE\nLEAN_LEFT = Q");
    assert!(c.lines.iter().all(|l| matches!(l.status, LineStatus::Pending(_))), "{:?}", c.lines);
    assert!(c.bindings.is_empty());
}

#[test]
fn the_summary_counts_what_the_editor_shows() {
    let c = compile("S = SPACE\nTOUCHPAD_MODE = MOUSE\nAUTOLOAD = OFF\nFLOOMP = A");
    assert_eq!(c.summary(), (1, 1, 1));
}

// ── the press machinery ──────────────────────────────────────────────────────

const DT: f32 = 0.010;

struct Rig {
    cfg: Compiled,
    rt: Runtime,
    down: std::collections::HashSet<Btn>,
}

impl Rig {
    fn new(text: &str) -> Rig {
        let cfg = compile(text);
        let bad: Vec<&LineInfo> = cfg.lines.iter()
            .filter(|l| matches!(l.status, LineStatus::Error(_)))
            .collect();
        assert!(bad.is_empty(), "config should compile: {bad:?}");
        Rig { cfg, rt: Runtime::default(), down: std::collections::HashSet::new() }
    }
    fn set(&mut self, b: Btn, on: bool) {
        if on { self.down.insert(b); } else { self.down.remove(&b); }
    }
    /// One tick; returns the pins driven this tick.
    fn tick(&mut self) -> std::collections::HashSet<String> {
        let down = self.down.clone();
        self.rt.tick(&self.cfg, DT, &|b| down.contains(&b)).pins.clone()
    }
    /// `n` ticks; returns the pins driven on the last one.
    fn ticks(&mut self, n: usize) -> std::collections::HashSet<String> {
        let mut last = std::collections::HashSet::new();
        for _ in 0..n { last = self.tick(); }
        last
    }
    /// Whether a pin is driven at any point over `n` ticks.
    fn seen_within(&mut self, n: usize, pin: &str) -> bool {
        for _ in 0..n {
            if self.tick().contains(pin) { return true; }
        }
        false
    }
}

// A plain binding holds its key while the button is down.
#[test]
fn a_press_holds_its_key_until_release() {
    let mut r = Rig::new("S = SPACE");
    assert!(!r.tick().contains("key_space"));
    r.set(Btn::S, true);
    assert!(r.tick().contains("key_space"));
    assert!(r.ticks(30).contains("key_space"), "still held");
    r.set(Btn::S, false);
    assert!(!r.ticks(6).contains("key_space"));
}

// Tap and hold: a short press fires the first key after release, a long one
// fires the second while held.
#[test]
fn tap_fires_on_release_and_hold_fires_while_held() {
    let mut r = Rig::new("W = R E");
    r.set(Btn::W, true);
    assert!(!r.ticks(5).contains("key_r"), "a tap waits for the release");
    r.set(Btn::W, false);
    assert!(r.seen_within(3, "key_r"), "the tap key fires after release");
    assert!(!r.ticks(10).contains("key_r"), "and lets go on its own");

    let mut r = Rig::new("W = R E");
    r.set(Btn::W, true);
    let pins = r.ticks(20); // past the 150 ms hold
    assert!(pins.contains("key_e"), "hold fires while held");
    assert!(!pins.contains("key_r"));
    r.set(Btn::W, false);
    let pins = r.ticks(2);
    assert!(!pins.contains("key_e"), "and releases with the button");
    assert!(!pins.contains("key_r"), "a held press never taps");
}

// Turbo only starts once the button has been held, then pulses.
#[test]
fn turbo_pulses_after_the_hold_time() {
    let mut r = Rig::new("S = SPACE+");
    r.set(Btn::S, true);
    let mut pulses = 0;
    let mut was = false;
    for _ in 0..60 {
        let on = r.tick().contains("key_space");
        if on && !was { pulses += 1; }
        was = on;
    }
    // 600 ms: 150 ms of hold, then a pulse every 80 ms.
    assert!((4..=6).contains(&pulses), "expected a handful of turbo pulses, got {pulses}");
}

// A toggle stays on after the button is released, and the next press clears it.
#[test]
fn a_toggle_latches_until_pressed_again() {
    let mut r = Rig::new("ZL = ^RMOUSE\\");
    r.set(Btn::Zl, true);
    assert!(r.tick().contains("mouse_right"));
    r.set(Btn::Zl, false);
    assert!(r.ticks(10).contains("mouse_right"), "the toggle holds after release");
    r.set(Btn::Zl, true);
    assert!(!r.tick().contains("mouse_right"), "pressing again clears it");
}

// An instant press lets go by itself while the button is still down.
#[test]
fn an_instant_press_releases_itself() {
    let mut r = Rig::new("E = !C\\");
    r.set(Btn::E, true);
    assert!(r.tick().contains("key_c"));
    assert!(!r.ticks(8).contains("key_c"), "instant presses release themselves");
}

// A release modifier clears a toggle another binding set.
#[test]
fn a_release_modifier_clears_a_toggle() {
    let mut r = Rig::new("S = ^SPACE\\\nE = -SPACE\\");
    r.set(Btn::S, true);
    assert!(r.tick().contains("key_space"));
    r.set(Btn::S, false);
    r.tick();
    r.set(Btn::E, true);
    assert!(!r.tick().contains("key_space"), "the release modifier clears the latch");
}

// While a chord button is held, the chorded binding replaces the button own one —
// and the chord button keeps doing its own job.
#[test]
fn a_chord_replaces_the_buttons_own_binding() {
    let mut r = Rig::new("W = R\nL = Q\nL,W = 3");
    r.set(Btn::L, true);
    assert!(r.tick().contains("key_q"), "the chord button still does its own job");
    r.set(Btn::W, true);
    let pins = r.tick();
    assert!(pins.contains("key_3"), "the chorded binding applies");
    assert!(!pins.contains("key_r"), "not the plain binding");
    // Let the chord go and W gets its own binding back on the next press.
    r.set(Btn::W, false);
    r.set(Btn::L, false);
    r.ticks(2);
    r.set(Btn::W, true);
    assert!(r.tick().contains("key_r"));
}

// With two chords held, the one pressed last wins.
#[test]
fn the_latest_chord_wins() {
    let mut r = Rig::new("W = R\nL,W = 3\nZL,W = 4");
    r.set(Btn::L, true);
    r.tick();
    r.set(Btn::Zl, true);
    r.tick();
    r.set(Btn::W, true);
    let pins = r.tick();
    assert!(pins.contains("key_4"), "the last chord pressed wins: {pins:?}");
    assert!(!pins.contains("key_3"));
}

// A double press inside the window uses its own binding, and the single press
// TAP is held back long enough not to fire as well. (A single-key binding fires
// on the press instead, which JSM applies on the first press either way — hence
// the tap/hold binding here.)
#[test]
fn a_double_press_holds_back_the_single_tap() {
    let cfg = "N = SCROLLDOWN NONE
N,N = X";
    // One press: the tap waits out the double window, then fires.
    let mut r = Rig::new(cfg);
    r.set(Btn::N, true);
    r.ticks(3);
    r.set(Btn::N, false);
    assert!(!r.ticks(3).contains("scroll_down"), "the tap waits for the double window");
    assert!(r.seen_within(25, "scroll_down"), "a lone tap still fires, a little late");

    // Two presses inside the window: the double binding fires, the tap does not.
    // Watched across the WHOLE sequence, so a held-back tap cannot slip past.
    let mut r = Rig::new(cfg);
    let mut saw_x = false;
    let mut saw_scroll = false;
    let mut watch = |r: &mut Rig, n: usize, saw_x: &mut bool, saw_scroll: &mut bool| {
        for _ in 0..n {
            let pins = r.tick();
            *saw_x |= pins.contains("key_x");
            *saw_scroll |= pins.contains("scroll_down");
        }
    };
    r.set(Btn::N, true);
    watch(&mut r, 3, &mut saw_x, &mut saw_scroll);
    r.set(Btn::N, false);
    watch(&mut r, 3, &mut saw_x, &mut saw_scroll);
    r.set(Btn::N, true);
    watch(&mut r, 20, &mut saw_x, &mut saw_scroll);
    assert!(saw_x, "the double press fires");
    assert!(!saw_scroll, "the single press tap must not fire too");
}

// Two buttons pressed together inside the window use the simultaneous binding
// instead of their own.
#[test]
fn a_simultaneous_press_replaces_both_bindings() {
    let mut r = Rig::new("L = LSHIFT\nR = E\nL+R = Q");
    r.set(Btn::L, true);
    let pins = r.tick();
    assert!(!pins.contains("key_shift"), "L waits to see if R joins it");
    r.set(Btn::R, true);
    let pins = r.tick();
    assert!(pins.contains("key_q"), "the pair fires: {pins:?}");
    assert!(!pins.contains("key_shift") && !pins.contains("key_e"));
    r.set(Btn::R, false);
    assert!(!r.ticks(3).contains("key_q"), "letting either go ends it");
}

// Pressed alone, a button that has a simultaneous binding still works — just
// after the window.
#[test]
fn a_lone_press_survives_the_simultaneous_window() {
    let mut r = Rig::new("L = LSHIFT\nR = E\nL+R = Q");
    r.set(Btn::L, true);
    assert!(r.seen_within(10, "key_shift"), "L own binding starts after the window");
}

// A diagonal takes over from the button already pressing, and hands back when
// one of the two is released.
#[test]
fn a_diagonal_takes_over_and_hands_back() {
    let mut r = Rig::new("UP = 1\nUP*RIGHT = 2\nRIGHT = 3");
    r.set(Btn::Up, true);
    assert!(r.tick().contains("key_1"));
    r.set(Btn::Right, true);
    let pins = r.tick();
    assert!(pins.contains("key_2"), "the diagonal binding applies: {pins:?}");
    assert!(!pins.contains("key_1") && !pins.contains("key_3"));
    r.set(Btn::Right, false);
    let pins = r.ticks(2);
    assert!(pins.contains("key_1"), "the still-held button gets its binding back: {pins:?}");
    assert!(!pins.contains("key_2"));
}

// The timings a config sets are the ones used.
#[test]
fn the_configs_own_hold_time_applies() {
    let mut r = Rig::new("HOLD_PRESS_TIME = 50\nW = R E");
    r.set(Btn::W, true);
    assert!(r.ticks(7).contains("key_e"), "50 ms hold should have fired by 70 ms");
}

// ── triggers and sticks ──────────────────────────────────────────────────────

use super::analog::{Analog, Pad, RingMode, StickMode, TriggerMode};

/// A config's analog settings, driven a tick at a time with a pad in hand.
struct Stage {
    s: super::analog::Settings,
    a: Analog,
    pad: Pad,
}

impl Stage {
    fn new(text: &str) -> Stage {
        let c = compile(text);
        let bad: Vec<&LineInfo> = c.lines.iter()
            .filter(|l| matches!(l.status, LineStatus::Error(_)))
            .collect();
        assert!(bad.is_empty(), "config should compile: {bad:?}");
        Stage { s: c.settings, a: Analog::default(), pad: Pad::default() }
    }
    /// Advance one tick on whatever the pad is reporting.
    fn tick(&mut self) -> &Analog {
        self.a.tick(&self.s, DT, &self.pad);
        &self.a
    }
    /// Put the left trigger at `v` and advance one tick.
    fn pull(&mut self, v: f32) -> &Analog {
        self.pad.triggers[0] = Some(v);
        self.a.tick(&self.s, DT, &self.pad);
        &self.a
    }
    fn pull_for(&mut self, v: f32, ticks: usize) -> &Analog {
        for _ in 0..ticks { self.pull(v); }
        &self.a
    }
    /// Put the left stick at (x, y) and advance one tick.
    fn stick(&mut self, x: f32, y: f32) -> &Analog {
        self.pad.sticks[0] = (x, y);
        self.a.tick(&self.s, DT, &self.pad);
        &self.a
    }
}

/// Comfortably past the 150 ms skip delay, in ticks.
const SKIP: usize = 20;

// The threshold decides when the soft pull counts as a press, and JSM's default
// of 0 means the slightest press.
#[test]
fn the_trigger_threshold_decides_when_a_pull_counts() {
    let mut t = Stage::new("TRIGGER_THRESHOLD = 0.5");
    assert!(!t.pull(0.4).down(Btn::Zl), "under the threshold is not a press");
    assert!(t.pull(0.6).down(Btn::Zl), "over it is");
    assert!(!t.pull(0.5).down(Btn::Zl), "exactly at it is not — JSM compares strictly");

    let mut t = Stage::new("S = SPACE");
    assert!(t.pull(0.10).down(Btn::Zl), "the default threshold is the slightest press");
    assert!(!t.pull(0.0).down(Btn::Zl));
}

// A negative threshold is JSM's hair trigger: it presses while the trigger is
// still being pulled and releases while it is coming back, without ever needing
// to cross a level.
#[test]
fn a_hair_trigger_follows_the_movement_not_a_level() {
    let mut t = Stage::new("TRIGGER_THRESHOLD = -1");
    let mut pressed = false;
    for v in [0.05, 0.10, 0.15, 0.20, 0.25] {
        pressed |= t.pull(v).down(Btn::Zl);
    }
    assert!(pressed, "pulling it down presses, well short of any threshold");

    let mut released = false;
    for v in [0.20, 0.15, 0.10, 0.05, 0.0, 0.0] {
        released |= !t.pull(v).down(Btn::Zl);
    }
    assert!(released, "and easing it back releases");
}

// The default mode ignores the full pull altogether, so a config that binds ZLF
// without setting a mode gets nothing — and the line says so.
#[test]
fn no_full_is_the_default_and_says_so() {
    let mut t = Stage::new("ZLF = LMOUSE");
    let a = t.pull_for(1.0, 3);
    assert!(a.down(Btn::Zl), "the soft pull still presses");
    assert!(!a.down(Btn::Zlf), "the full pull is ignored in NO_FULL");

    let c = compile("ZLF = LMOUSE");
    assert!(c.lines[0].notes.iter().any(|n| n.contains("ZL_MODE is NO_FULL")),
        "the line should explain itself: {:?}", c.lines[0].notes);
}

// NO_SKIP puts the full pull on top of the soft one; NO_SKIP_EXCLUSIVE swaps
// one for the other.
#[test]
fn the_full_pull_can_join_the_soft_one_or_replace_it() {
    let mut t = Stage::new("ZL_MODE = NO_SKIP");
    assert!(t.pull(0.5).down(Btn::Zl));
    let a = t.pull(1.0);
    assert!(a.down(Btn::Zl) && a.down(Btn::Zlf), "NO_SKIP keeps both");

    let mut t = Stage::new("ZL_MODE = NO_SKIP_EXCLUSIVE");
    assert!(t.pull(0.5).down(Btn::Zl));
    let a = t.pull(1.0);
    assert!(!a.down(Btn::Zl) && a.down(Btn::Zlf), "EXCLUSIVE trades one for the other");
    let a = t.pull(0.5);
    assert!(a.down(Btn::Zl) && !a.down(Btn::Zlf), "and gives the soft pull back");
}

// MUST_SKIP: a quick full pull fires the full binding only; getting there slowly
// leaves you on the soft binding, which the full pull no longer interrupts.
#[test]
fn must_skip_is_the_quick_pull_only() {
    let mut t = Stage::new("ZL_MODE = MUST_SKIP");
    t.pull(0.5);
    let a = t.pull(1.0);
    assert!(a.down(Btn::Zlf), "a quick full pull fires the full binding");
    assert!(!a.down(Btn::Zl), "and skips the soft one");

    let mut t = Stage::new("ZL_MODE = MUST_SKIP");
    let a = t.pull_for(0.5, SKIP);
    assert!(a.down(Btn::Zl), "waiting out the skip delay presses the soft binding");
    let a = t.pull(1.0);
    assert!(a.down(Btn::Zl) && !a.down(Btn::Zlf),
        "and once firing, reaching the full pull doesn't stop you");
}

// MAY_SKIP is the same quick pull, but a full pull after the soft one lands on
// top instead of being ignored.
#[test]
fn may_skip_allows_the_full_pull_afterwards() {
    let mut t = Stage::new("ZL_MODE = MAY_SKIP");
    t.pull(0.5);
    let a = t.pull(1.0);
    assert!(a.down(Btn::Zlf) && !a.down(Btn::Zl), "quick full pull still skips the soft one");

    let mut t = Stage::new("ZL_MODE = MAY_SKIP");
    t.pull_for(0.5, SKIP);
    let a = t.pull(1.0);
    assert!(a.down(Btn::Zl) && a.down(Btn::Zlf), "a later full pull joins the soft one");
}

// The responsive modes press the soft binding at once and take it back if the
// full pull turns up quickly.
#[test]
fn a_responsive_mode_presses_first_and_takes_it_back() {
    let mut t = Stage::new("ZL_MODE = MUST_SKIP_R");
    assert!(t.pull(0.5).down(Btn::Zl), "the soft binding is on straight away");
    let a = t.pull(1.0);
    assert!(a.down(Btn::Zlf), "the quick full pull fires");
    assert!(!a.down(Btn::Zl), "and the soft binding is taken back");
}

// Letting go before the skip delay is up still sends the soft binding — a quick
// tap of the trigger isn't swallowed by the wait.
#[test]
fn a_quick_tap_of_the_trigger_still_sends_the_soft_binding() {
    let mut t = Stage::new("ZL_MODE = MUST_SKIP");
    t.pull(0.5);
    t.pull(0.5);
    assert!(t.pull(0.0).down(Btn::Zl), "the held-back soft press fires on release");
    assert!(!t.pull(0.0).down(Btn::Zl), "and is over the next tick");
}

// A pad with no analog trigger can't tell a soft pull from a full one, so JSM
// forces NO_FULL on it rather than firing both.
#[test]
fn a_digital_trigger_never_fires_the_full_pull() {
    let mut t = Stage::new("ZL_MODE = NO_SKIP");
    t.pad.trigger_digital[0] = true;
    // Over several ticks, because a full pull would only land on the second one.
    let mut ever_full = false;
    for _ in 0..4 { ever_full |= t.tick().down(Btn::Zlf); }
    assert!(t.a.down(Btn::Zl), "the button still presses the soft binding");
    assert!(!ever_full, "but nothing on a digital trigger reads as a full pull");
}

// A stick in a digital mode is eight sectors, and a diagonal lights both of its
// directions the way JSM's do.
#[test]
fn stick_directions_are_eight_sectors() {
    let mut t = Stage::new("LUP = W");
    let a = t.stick(0.0, 1.0);
    assert!(a.down(Btn::Lup) && !a.down(Btn::Lleft) && !a.down(Btn::Lright));
    let a = t.stick(0.7, 0.7);
    assert!(a.down(Btn::Lup) && a.down(Btn::Lright), "a diagonal is both");
    let a = t.stick(0.3, 1.0);
    assert!(a.down(Btn::Lup) && !a.down(Btn::Lright), "but leaning isn't: a sector is 90° wide");
    let a = t.stick(-1.0, 0.0);
    assert!(a.down(Btn::Lleft) && !a.down(Btn::Lup) && !a.down(Btn::Ldown));
    let a = t.stick(0.05, 0.0);
    for b in [Btn::Lleft, Btn::Lright, Btn::Lup, Btn::Ldown, Btn::Lring] {
        assert!(!a.down(b), "inside the deadzone is nothing at all, and {} fired", b.name());
    }
}

// The ring fires whatever else the stick is doing, and which ring it is decides
// whether it means "moved a little" or "pushed to the edge".
#[test]
fn the_ring_reads_how_far_the_stick_is_pushed() {
    let mut t = Stage::new("LRING = LMOUSE");
    assert!(t.stick(0.0, 1.0).down(Btn::Lring), "OUTER is the default ring");
    assert!(!t.stick(0.0, 0.5).down(Btn::Lring));

    let mut t = Stage::new("LEFT_RING_MODE = INNER\nLRING = LMOUSE");
    assert!(t.stick(0.0, 0.5).down(Btn::Lring), "INNER is the other half of the travel");
    assert!(!t.stick(0.0, 1.0).down(Btn::Lring));
    assert!(!t.stick(0.0, 0.0).down(Btn::Lring), "centred is not a ring press");
}

// Axis inversion and controller orientation both turn the stick before its
// directions are read.
#[test]
fn a_stick_can_be_inverted_or_turned_with_the_controller() {
    let mut t = Stage::new("LEFT_STICK_AXIS = INVERTED\nLDOWN = S");
    assert!(t.stick(0.0, 1.0).down(Btn::Ldown), "INVERTED flips both axes");

    let mut t = Stage::new("LEFT_STICK_AXIS = STANDARD INVERTED\nLDOWN = S");
    let a = t.stick(1.0, 1.0);
    assert!(a.down(Btn::Ldown) && a.down(Btn::Lright), "one sign each, x then y");

    let mut t = Stage::new("CONTROLLER_ORIENTATION = LEFT\nLUP = W");
    assert!(t.stick(1.0, 0.0).down(Btn::Lup), "held sideways, right is up");
}

// Turning the stick in SCROLL_WHEEL mode spends the turn on notches, each one a
// tap of the stick's left or right button.
#[test]
fn the_scroll_wheel_turns_into_notches() {
    let mut t = Stage::new("LEFT_STICK_MODE = SCROLL_WHEEL\nLLEFT = SCROLLUP\nLRIGHT = SCROLLDOWN");
    t.stick(1.0, 0.0);
    // A quarter turn anticlockwise is three 30° notches' worth, but a notch
    // holds its button for a moment, so only one fires at a time.
    let a = t.stick(0.0, 1.0);
    assert!(a.down(Btn::Lleft), "turning one way taps the left button");
    assert!(!a.down(Btn::Lright));

    let mut t = Stage::new("LEFT_STICK_MODE = SCROLL_WHEEL\nLLEFT = SCROLLUP\nLRIGHT = SCROLLDOWN");
    t.stick(0.0, 1.0);
    assert!(t.stick(1.0, 0.0).down(Btn::Lright), "and the other way taps the right one");

    // Back to the middle forgets the turn rather than banking it.
    let mut t = Stage::new("LEFT_STICK_MODE = SCROLL_WHEEL\nLLEFT = SCROLLUP");
    t.stick(1.0, 0.0);
    t.stick(0.0, 0.0);
    assert!(!t.stick(0.0, 1.0).down(Btn::Lleft), "a lap through the centre is not a notch");
}

// A stick that aims the mouse drives no directions, and the line that bound one
// says why rather than looking broken.
#[test]
fn a_stick_that_aims_the_mouse_has_no_directions() {
    let mut t = Stage::new("LEFT_STICK_MODE = AIM\nLUP = W");
    assert!(!t.stick(0.0, 1.0).down(Btn::Lup));

    let c = compile("LEFT_STICK_MODE = AIM\nLUP = W");
    assert_eq!(c.lines[0].status, LineStatus::Ok, "{:?}", c.lines[0]);
    assert!(c.lines[1].notes.iter().any(|n| n.contains("LEFT_STICK_MODE")),
        "the binding should explain itself: {:?}", c.lines[1].notes);

    // A mode a later phase owns is pending, and drives nothing at all.
    let c = compile("LEFT_STICK_MODE = MOUSE_RING\nLUP = W");
    assert!(matches!(c.lines[0].status, LineStatus::Pending(p) if p.contains("absolute mouse")),
        "{:?}", c.lines[0]);
}

// The settings this phase runs are read off the config, not guessed at.
#[test]
fn trigger_and_stick_settings_are_read() {
    let c = compile("ZR_MODE = MAY_SKIP_R\nRIGHT_STICK_MODE = SCROLL_WHEEL\nSCROLL_SENS = 45\n\
        LEFT_STICK_DEADZONE_INNER = 0.2\nSTICK_DEADZONE_OUTER = 0.05\nRIGHT_RING_MODE = INNER");
    assert!(c.lines.iter().all(|l| l.status == LineStatus::Ok), "{:?}", c.lines);
    assert_eq!(c.settings.zr, TriggerMode::MaySkipR);
    assert_eq!(c.settings.right.mode, StickMode::ScrollWheel);
    assert_eq!(c.settings.right.scroll_sens, 45.0);
    assert_eq!(c.settings.left.inner_dz, 0.2);
    assert_eq!(c.settings.right.inner_dz, 0.15, "one stick's deadzone is its own");
    assert_eq!(c.settings.left.outer_dz, 0.05, "the unprefixed one sets both");
    assert_eq!(c.settings.right.outer_dz, 0.05);
    assert_eq!(c.settings.right.ring, RingMode::Inner);
    assert_eq!(c.settings.left.ring, RingMode::Outer);

    // A value JSM knows but we don't run yet names its phase; a typo is an error.
    assert!(matches!(one("LEFT_STICK_MODE = LEFT_STICK").status, LineStatus::Pending(p)
        if p.contains("virtual pad")));
    assert!(matches!(one("ZL_MODE = SORT_OF").status, LineStatus::Error(_)));
    assert!(matches!(one("TRIGGER_THRESHOLD = half").status, LineStatus::Error(_)));
}

// ── aiming: gyro, stick aim, flick ───────────────────────────────────────────

use super::aim::{Aim, AxisMask, Gyro, SnapMode};

/// A config's aiming, driven a tick at a time with a gyro and sticks in hand.
struct Aiming {
    cfg: Compiled,
    a: Aim,
    analog: Analog,
    pad: Pad,
    gyro: Gyro,
    actions: std::collections::HashSet<GyroAction>,
    down: std::collections::HashSet<Btn>,
}

impl Aiming {
    fn new(text: &str) -> Aiming {
        let cfg = compile(text);
        let bad: Vec<&LineInfo> = cfg.lines.iter()
            .filter(|l| matches!(l.status, LineStatus::Error(_)))
            .collect();
        assert!(bad.is_empty(), "config should compile: {bad:?}");
        Aiming {
            cfg,
            a: Aim::default(),
            analog: Analog::default(),
            pad: Pad::default(),
            gyro: Gyro::default(),
            actions: std::collections::HashSet::new(),
            down: std::collections::HashSet::new(),
        }
    }
    /// Turn the pad at this many degrees a second and advance one tick.
    fn turn(&mut self, pitch: f32, yaw: f32) -> glam::Vec2 {
        self.gyro = Gyro { roll: 0.0, pitch, yaw };
        self.tick()
    }
    fn stick(&mut self, side: usize, x: f32, y: f32) -> glam::Vec2 {
        self.pad.sticks[side] = (x, y);
        self.tick()
    }
    fn tick(&mut self) -> glam::Vec2 {
        self.analog.tick(&self.cfg.settings, DT, &self.pad);
        let down = self.down.clone();
        self.a.tick(&self.cfg.aim, DT, self.gyro, &self.analog, &self.actions,
            &|b| down.contains(&b))
    }
    fn ticks(&mut self, n: usize) -> glam::Vec2 {
        let mut last = glam::Vec2::ZERO;
        for _ in 0..n { last = self.tick(); }
        last
    }
}

/// Enough gyro sensitivity to see, and a calibration that keeps the numbers
/// readable: 1 mouse count per degree turned.
const AIM_CFG: &str = "GYRO_SENS = 1\nREAL_WORLD_CALIBRATION = 1\nGYRO_SMOOTH_TIME = 0";

// Turning the pad moves the mouse: right is +x, and up is +y because our bus
// counts the screen upward even though JSM counts it down.
#[test]
fn turning_the_pad_moves_the_mouse() {
    let mut a = Aiming::new(AIM_CFG);
    let right = a.turn(0.0, 90.0);
    assert!(right.x > 0.0, "turning right aims right: {right:?}");
    assert!(right.y.abs() < 1e-6, "and not up or down");

    let left = a.turn(0.0, -90.0);
    assert!(left.x < 0.0, "and turning left aims left: {left:?}");

    let up = a.turn(90.0, 0.0);
    assert!(up.y > 0.0, "tilting up aims up: {up:?}");

    // A degree turned is a mouse count at this calibration, whatever the tick.
    let moved = a.turn(0.0, 100.0).x;
    assert!((moved - 100.0 * DT).abs() < 0.01, "a degree is a count: {moved}");
}

// Sensitivity multiplies the turn, and zero sensitivity — JSM's default — aims
// with nothing at all.
#[test]
fn gyro_sensitivity_scales_the_turn_and_zero_means_off() {
    let mut a = Aiming::new(AIM_CFG);
    let one = a.turn(0.0, 90.0).x;
    let mut b = Aiming::new("GYRO_SENS = 4\nREAL_WORLD_CALIBRATION = 1\nGYRO_SMOOTH_TIME = 0");
    let four = b.turn(0.0, 90.0).x;
    assert!((four - one * 4.0).abs() < 0.01, "four times the sensitivity: {four} vs {one}");

    let mut off = Aiming::new("REAL_WORLD_CALIBRATION = 1");
    assert_eq!(off.turn(0.0, 90.0), glam::Vec2::ZERO, "JSM starts at zero sensitivity");
}

// The ramp: slow turns get the low sensitivity, fast ones the high, and in
// between it interpolates.
#[test]
fn the_sensitivity_ramp_runs_between_its_two_thresholds() {
    let cfg = "MIN_GYRO_SENS = 1\nMAX_GYRO_SENS = 5\nMIN_GYRO_THRESHOLD = 10\n\
        MAX_GYRO_THRESHOLD = 110\nREAL_WORLD_CALIBRATION = 1\nGYRO_SMOOTH_TIME = 0";
    let mut a = Aiming::new(cfg);
    // At the bottom of the ramp the low sensitivity applies.
    let slow = a.turn(0.0, 10.0).x / (10.0 * DT);
    assert!((slow - 1.0).abs() < 0.05, "slow turns stay at MIN_GYRO_SENS: {slow}");
    // Halfway up, halfway between the two.
    let mid = a.turn(0.0, 60.0).x / (60.0 * DT);
    assert!((mid - 3.0).abs() < 0.05, "halfway is halfway: {mid}");
    // At the top, and beyond it, the high sensitivity.
    let fast = a.turn(0.0, 200.0).x / (200.0 * DT);
    assert!((fast - 5.0).abs() < 0.05, "fast turns reach MAX_GYRO_SENS: {fast}");
}

// The cutoff ignores a slow drift, and fades the gyro back in over the recovery
// band rather than snapping it on.
#[test]
fn the_cutoff_ignores_a_slow_drift() {
    let cfg = "GYRO_SENS = 1\nREAL_WORLD_CALIBRATION = 1\nGYRO_SMOOTH_TIME = 0\n\
        GYRO_CUTOFF_SPEED = 10\nGYRO_CUTOFF_RECOVERY = 20";
    let mut a = Aiming::new(cfg);
    assert_eq!(a.turn(0.0, 5.0).x, 0.0, "under the cutoff, nothing moves");
    let half = a.turn(0.0, 15.0).x;
    let full = a.turn(0.0, 40.0).x;
    assert!(half > 0.0 && half < 15.0 * DT, "halfway through recovery it is faded: {half}");
    assert!((full - 40.0 * DT).abs() < 0.01, "past recovery it is all there: {full}");
}

// Which gyro axis drives which mouse axis is the config's to choose, and either
// can be inverted.
#[test]
fn the_gyro_axes_can_be_swapped_and_inverted() {
    let mut a = Aiming::new(&format!("{AIM_CFG}\nMOUSE_X_FROM_GYRO_AXIS = X"));
    assert_eq!(a.cfg.aim.mouse_x_from, AxisMask::X);
    let tilt = a.turn(90.0, 0.0);
    assert!(tilt.x > 0.0, "tilting now aims sideways: {tilt:?}");

    let mut a = Aiming::new(&format!("{AIM_CFG}\nGYRO_AXIS_X = INVERTED"));
    assert!(a.turn(0.0, 90.0).x < 0.0, "inverted, turning right aims left");

    let mut a = Aiming::new(&format!("{AIM_CFG}\nMOUSE_X_FROM_GYRO_AXIS = NONE"));
    assert_eq!(a.turn(0.0, 90.0).x, 0.0, "and NONE means nothing drives it");
}

// A gyro button holds the gyro off (or on) while it is held, and a binding's own
// GYRO_OFF overrides whatever the setting said.
#[test]
fn a_button_can_hold_the_gyro_off_or_on() {
    let mut a = Aiming::new(&format!("{AIM_CFG}\nGYRO_OFF = R3"));
    assert!(a.turn(0.0, 90.0).x > 0.0, "the gyro is on to begin with");
    a.down.insert(Btn::R3);
    assert_eq!(a.turn(0.0, 90.0).x, 0.0, "and off while the button is held");

    let mut a = Aiming::new(&format!("{AIM_CFG}\nGYRO_ON = ZL"));
    assert_eq!(a.turn(0.0, 90.0).x, 0.0, "GYRO_ON is the other way up");
    a.down.insert(Btn::Zl);
    assert!(a.turn(0.0, 90.0).x > 0.0, "on only while held");

    // NO_GYRO_BUTTON takes the button away again.
    let mut a = Aiming::new(&format!("{AIM_CFG}\nGYRO_OFF = R3\nNO_GYRO_BUTTON"));
    a.down.insert(Btn::R3);
    assert!(a.turn(0.0, 90.0).x > 0.0, "with no button, the gyro is simply on");

    // A binding's action wins over the setting while it is asserted.
    let mut a = Aiming::new(AIM_CFG);
    a.actions.insert(GyroAction::Off);
    assert_eq!(a.turn(0.0, 90.0).x, 0.0, "GYRO_OFF as a binding blocks it too");
}

// Aiming with a stick: pushing it moves the mouse, and the deadzone holds it
// still.
#[test]
fn a_stick_can_aim_the_mouse() {
    let cfg = "RIGHT_STICK_MODE = AIM\nSTICK_SENS = 100\nREAL_WORLD_CALIBRATION = 1";
    let mut a = Aiming::new(cfg);
    let out = a.stick(1, 1.0, 0.0);
    assert!(out.x > 0.0 && out.y.abs() < 1e-6, "pushing right aims right: {out:?}");
    let up = a.stick(1, 0.0, 1.0);
    assert!(up.y > 0.0, "and pushing up aims up: {up:?}");
    assert_eq!(a.stick(1, 0.05, 0.0), glam::Vec2::ZERO, "inside the deadzone, nothing");

    // How far it is pushed is shaped by STICK_POWER and only then scaled, so a
    // half push with a squared curve aims at a quarter speed.
    let cfg = "RIGHT_STICK_MODE = AIM\nSTICK_SENS = 100\nREAL_WORLD_CALIBRATION = 1\n\
        STICK_POWER = 2\nSTICK_DEADZONE_INNER = 0\nSTICK_DEADZONE_OUTER = 0";
    let mut a = Aiming::new(cfg);
    let half = a.stick(1, 0.5, 0.0).x;
    let full = a.stick(1, 1.0, 0.0).x;
    assert!((half - 0.25 * 100.0 * DT).abs() < 1e-4, "half push, quarter speed: {half}");
    assert!((full - 100.0 * DT).abs() < 1e-4, "full push, full speed: {full}");
}

// Flick stick: pushing the stick out turns the camera to face that way, paid out
// over FLICK_TIME rather than all at once. At a calibration of one count per
// degree, a flick's whole movement is the angle it turned through.
#[test]
fn a_flick_turns_the_camera_to_face_the_stick() {
    let cfg = "RIGHT_STICK_MODE = FLICK\nREAL_WORLD_CALIBRATION = 1\nFLICK_TIME = 0.1";
    let mut a = Aiming::new(cfg);
    a.stick(1, 0.0, 0.0);
    // Straight back is half a turn.
    a.stick(1, 0.0, -1.0);
    let (mut total, mut biggest) = (0.0f32, 0.0f32);
    for _ in 0..30 {
        let x = a.tick().x;
        total += x;
        biggest = biggest.max(x.abs());
    }
    assert!((total.abs() - 180.0).abs() < 2.0,
        "a flick backwards turns 180 degrees' worth: {total}");
    assert!(biggest < total.abs() * 0.5,
        "and it is eased over the flick time, not dumped in a tick: {biggest}");
}

// Turning the stick while it is held out traces the camera around with it.
#[test]
fn turning_a_held_flick_stick_traces_the_camera() {
    let cfg = "RIGHT_STICK_MODE = FLICK\nREAL_WORLD_CALIBRATION = 1\nROTATE_SMOOTH_OVERRIDE = 0";
    let mut a = Aiming::new(cfg);
    a.stick(1, 0.0, 1.0);
    a.ticks(30); // let the flick itself finish
    // A quarter turn of the stick is a quarter turn of the camera.
    let traced = a.stick(1, 1.0, 0.0).x;
    assert!((traced.abs() - 90.0).abs() < 2.0, "the camera follows the stick round: {traced}");

    // FLICK_ONLY leaves the tracing out.
    let mut b = Aiming::new(
        "RIGHT_STICK_MODE = FLICK_ONLY\nREAL_WORLD_CALIBRATION = 1\nROTATE_SMOOTH_OVERRIDE = 0");
    b.stick(1, 0.0, 1.0);
    b.ticks(30);
    let traced = b.stick(1, 1.0, 0.0).x;
    assert!(traced.abs() < 1.0, "FLICK_ONLY doesn't trace: {traced}");

    // ROTATE_ONLY is the other half: it traces without ever flicking.
    let mut c = Aiming::new(
        "RIGHT_STICK_MODE = ROTATE_ONLY\nREAL_WORLD_CALIBRATION = 1\nROTATE_SMOOTH_OVERRIDE = 0");
    c.stick(1, 0.0, 0.0);
    let flick = c.stick(1, 0.0, -1.0).x + c.ticks(30).x;
    assert!(flick.abs() < 1.0, "ROTATE_ONLY doesn't flick: {flick}");
    assert!((c.stick(1, 1.0, 0.0).x.abs() - 90.0).abs() < 2.0, "but it does trace");
}

// Snapping rounds a flick to the nearest quarter or eighth of a turn.
#[test]
fn a_flick_can_snap_to_quarters() {
    let cfg = "RIGHT_STICK_MODE = FLICK\nREAL_WORLD_CALIBRATION = 1\n\
        FLICK_SNAP_MODE = 4\nFLICK_SNAP_STRENGTH = 1";
    let mut a = Aiming::new(cfg);
    assert_eq!(a.cfg.aim.flick_snap, SnapMode::Four);
    a.stick(1, 0.0, 0.0);
    // Pushed nearly-but-not-quite sideways: it snaps to the quarter turn.
    a.stick(1, 0.95, 0.31);
    let mut total = 0.0;
    for _ in 0..40 { total += a.tick().x; }
    assert!((total.abs() - 90.0).abs() < 2.0, "snapped to a quarter turn: {total}");

    // Without snapping it turns through the angle it was actually given.
    let mut b = Aiming::new("RIGHT_STICK_MODE = FLICK\nREAL_WORLD_CALIBRATION = 1");
    b.stick(1, 0.0, 0.0);
    b.stick(1, 0.95, 0.31);
    let mut total = 0.0;
    for _ in 0..40 { total += b.tick().x; }
    assert!((total.abs() - 72.0).abs() < 2.0, "unsnapped, the angle is its own: {total}");
}

// The settings this phase runs are read off the config.
#[test]
fn aiming_settings_are_read() {
    let c = compile("MIN_GYRO_SENS = 2 3\nGYRO_SMOOTH_TIME = 0.05\nFLICK_TIME = 0.25\n\
        IN_GAME_SENS = 2\nSTICK_POWER = 1.5\nTRACKBALL_DECAY = 3\nGYRO_SPACE = LOCAL");
    assert!(c.lines.iter().all(|l| l.status == LineStatus::Ok), "{:?}", c.lines);
    assert_eq!(c.aim.min_sens, (2.0, 3.0), "a pair setting takes one number per axis");
    assert_eq!(c.aim.smooth_time, 0.05);
    assert_eq!(c.aim.flick_time, 0.25);
    assert_eq!(c.aim.in_game_sens, 2.0);
    assert_eq!(c.aim.stick_power, 1.5);
    assert_eq!(c.aim.trackball_decay, 3.0);

    // A gyro space measured against gravity waits for the phase that has one.
    assert!(matches!(one("GYRO_SPACE = PLAYER_TURN").status,
        LineStatus::Pending(p) if p.contains("gravity")));
    assert!(matches!(one("REAL_WORLD_CALIBRATION = 0").status, LineStatus::Error(_)),
        "a calibration of zero would divide the aim by nothing");
}



// ── the node on the bus ──────────────────────────────────────────────────────

use std::collections::HashMap;
use std::sync::Mutex;

use flexinput_core::Signal;

use crate::graph::NodeSnap;
use crate::state::NodeState;

const PAD: &str = "gilrs:xinput:0";

/// Editor focus is process-wide (the UI sets it for whichever editor has the
/// keyboard), so these tests run one at a time.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn jsm_snap(uid: usize, text: &str, strict: bool) -> NodeSnap {
    let mut params = HashMap::new();
    params.insert("_automap_device_id".to_string(), serde_json::json!(PAD));
    params.insert("jsm_tabs".to_string(), serde_json::json!([{ "name": "main", "text": text }]));
    params.insert("jsm_strict".to_string(), serde_json::json!(strict));
    NodeSnap {
        node_uid: uid,
        module_id: "module.jsm".to_string(),
        params,
        n_outputs: 1,
        input_sources: vec![None],
        device_id: None,
        output_pin_ids: Vec::new(),
        aux_f32_override: None,
        sink_target: None,
        inline_subgraph: None,
    }
}

/// Run the node for `ticks` with the given pad pins held, and return what it
/// published on its own bus.
fn run_node(text: &str, strict: bool, held: &[&str], ticks: usize) -> HashMap<String, Signal> {
    let sigs: Vec<(&str, Signal)> = held.iter().map(|p| (*p, Signal::Bool(true))).collect();
    run_node_with(text, strict, &sigs, ticks)
}

/// The same, with the pad reporting whatever these signals say.
fn run_node_with(
    text: &str,
    strict: bool,
    sigs: &[(&str, Signal)],
    ticks: usize,
) -> HashMap<String, Signal> {
    let uid = 4242;
    let snap = jsm_snap(uid, text, strict);
    let mut dev: HashMap<(String, String), Signal> = HashMap::new();
    for (pin, sig) in sigs {
        dev.insert((PAD.to_string(), pin.to_string()), *sig);
    }
    let mut collector: HashMap<(String, String), Signal> = HashMap::new();
    let mut state: HashMap<usize, NodeState> = HashMap::new();
    for _ in 0..ticks {
        super::eval::jsm_publish(&snap, uid, &dev, &mut collector, &mut state, 0.010);
    }
    let key = format!("collector:{uid}");
    collector.into_iter()
        .filter(|((d, _), _)| *d == key)
        .map(|((_, p), s)| (p, s))
        .collect()
}

fn on(bus: &HashMap<String, Signal>, pin: &str) -> bool {
    bus.get(pin).map(|s| s.as_bool()).unwrap_or(false)
}

/// Run the node over a sequence of pad frames — for what only movement can show
/// — and return every pin it drove along the way.
fn run_frames(
    text: &str,
    strict: bool,
    frames: &[Vec<(&str, Signal)>],
) -> std::collections::HashSet<String> {
    let uid = 4242;
    let snap = jsm_snap(uid, text, strict);
    let key = format!("collector:{uid}");
    let mut state: HashMap<usize, NodeState> = HashMap::new();
    let mut drove = std::collections::HashSet::new();
    for frame in frames {
        let mut dev: HashMap<(String, String), Signal> = HashMap::new();
        for (pin, sig) in frame {
            dev.insert((PAD.to_string(), pin.to_string()), *sig);
        }
        let mut collector: HashMap<(String, String), Signal> = HashMap::new();
        super::eval::jsm_publish(&snap, uid, &dev, &mut collector, &mut state, 0.010);
        for ((d, pin), sig) in collector {
            if d == key && sig.as_bool() { drove.insert(pin); }
        }
    }
    drove
}

// Pass-through mode: the config's key comes out, the button it used is taken
// over, and a button it never mentions carries on to the game.
#[test]
fn a_bound_button_is_taken_over_and_the_rest_passes_through() {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let bus = run_node("S = SPACE", false, &["btn_south", "btn_north"], 2);
    assert!(on(&bus, "key_space"), "the config drives its key");
    assert!(!on(&bus, "btn_south"), "the button it bound is taken over");
    assert!(on(&bus, "btn_north"), "a button it never mentions passes through");
    // Downstream Combiners learn which pins this module consumed.
    assert!(bus.keys().any(|p| p.starts_with("__consumed__") && p.contains("btn_south")),
        "the claimed pin is marked consumed: {:?}", bus.keys().collect::<Vec<_>>());
}

// A stick direction reaches the keyboard through the bus, and the stick it came
// from belongs to the config from then on.
#[test]
fn a_stick_direction_drives_its_key_and_claims_the_stick() {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let up = Signal::Vec2(glam::Vec2::new(0.0, 1.0));
    let stick = |bus: &HashMap<String, Signal>| match bus.get("left_stick") {
        Some(Signal::Vec2(v)) => *v,
        other => panic!("the left stick should be a vector: {other:?}"),
    };
    let bus = run_node_with("LUP = W", false, &[("left_stick", up)], 2);
    assert!(on(&bus, "key_w"), "the direction drives its key");
    assert_eq!(stick(&bus), glam::Vec2::ZERO, "and the stick itself is taken over");
    assert_eq!(bus.get("left_stick_y").map(|s| s.as_float()), Some(0.0));

    // A stick this config never reads as directions is left alone instead.
    let bus = run_node_with("S = SPACE", false, &[("left_stick", up)], 2);
    assert_eq!(stick(&bus), glam::Vec2::new(0.0, 1.0),
        "an untouched stick passes straight through");

    // And so is one whose mode a later phase owns: taking it over would silence
    // the stick for a binding that can't run yet.
    let bus = run_node_with("LEFT_STICK_MODE = MOUSE_RING\nLUP = W", false, &[("left_stick", up)], 2);
    assert_eq!(stick(&bus), glam::Vec2::new(0.0, 1.0),
        "a stick in a mode we don't run keeps passing through");
    assert!(!on(&bus, "key_w"));
}

// The axes on their own are enough: a pad that publishes x and y but no vector
// still drives the directions.
#[test]
fn stick_axes_work_without_the_vector() {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let bus = run_node_with("LRIGHT = D", false, &[("left_stick_x", Signal::Float(1.0))], 2);
    assert!(on(&bus, "key_d"));
}

// The full pull is its own binding once the mode allows it, and it takes the
// analog trigger over rather than passing it on half-pressed.
#[test]
fn a_full_pull_binding_runs_on_the_bus() {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = "ZR_MODE = NO_SKIP\nZR = LMOUSE\nZRF = RMOUSE";
    let bus = run_node_with(cfg, false, &[("right_trigger", Signal::Float(0.5))], 2);
    assert!(on(&bus, "mouse_left"), "the soft pull holds its key");
    assert!(!on(&bus, "mouse_right"));

    let bus = run_node_with(cfg, false, &[("right_trigger", Signal::Float(1.0))], 3);
    assert!(on(&bus, "mouse_left") && on(&bus, "mouse_right"), "NO_SKIP keeps both: {bus:?}");
    assert_eq!(bus.get("right_trigger").map(|s| s.as_float()), Some(0.0),
        "the trigger is the config's now");

    // Without a mode, the full pull can't fire — so the trigger isn't taken over
    // on its account either.
    let bus = run_node_with("ZRF = RMOUSE", false, &[("right_trigger", Signal::Float(1.0))], 2);
    assert!(!on(&bus, "mouse_right"));
    assert_eq!(bus.get("right_trigger").map(|s| s.as_float()), Some(1.0),
        "a trigger nothing live reads keeps passing through");
}

// Aiming reaches the bus as a mouse displacement, and a config that aims with
// the gyro takes the gyro over so it can't also drive a node downstream.
#[test]
fn gyro_aiming_reaches_the_bus_as_mouse_movement() {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    // Half of the bus's full scale is 1000 deg/s of yaw.
    let turn = [("gyro_z", Signal::Float(0.5))];
    let bus = run_node_with("GYRO_SENS = 1\nREAL_WORLD_CALIBRATION = 1", false, &turn, 2);
    match bus.get("mouse_move") {
        Some(Signal::Vec2(v)) => assert!(v.x > 0.0, "turning right aims right: {v:?}"),
        other => panic!("the gyro should move the mouse: {other:?}"),
    }
    assert_eq!(bus.get("gyro_z").map(|s| s.as_float()), Some(0.0),
        "and the gyro belongs to the config now");

    // At JSM's default sensitivity of zero, nothing aims and the gyro passes on.
    let bus = run_node_with("S = SPACE", false, &turn, 2);
    assert!(bus.get("mouse_move").is_none(), "no sensitivity, no movement");
    assert_eq!(bus.get("gyro_z").map(|s| s.as_float()), Some(0.5),
        "a gyro nothing reads keeps passing through");

    // A stick set to aim is the config's too, even with no binding on it.
    let push = [("right_stick", Signal::Vec2(glam::Vec2::new(1.0, 0.0)))];
    let bus = run_node_with("RIGHT_STICK_MODE = AIM\nSTICK_SENS = 100", false, &push, 2);
    match bus.get("mouse_move") {
        Some(Signal::Vec2(v)) => assert!(v.x > 0.0, "the stick aims: {v:?}"),
        other => panic!("the stick should move the mouse: {other:?}"),
    }
    match bus.get("right_stick") {
        Some(Signal::Vec2(v)) => assert_eq!(*v, glam::Vec2::ZERO, "and is taken over"),
        other => panic!("the stick should still be on the bus: {other:?}"),
    }
}

// Strict mode: nothing but the config's own output leaves the module.
#[test]
fn strict_mode_publishes_only_what_the_config_says() {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let bus = run_node("S = SPACE", true, &["btn_south", "btn_north"], 2);
    assert!(on(&bus, "key_space"));
    assert!(!on(&bus, "btn_north"), "strict mode holds back the rest of the pad");
}

// A config can drive a virtual pad rather than the keyboard, and an analog
// destination goes to full.
#[test]
fn a_pad_binding_drives_the_pad_pin() {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let bus = run_node("S = X_LT", false, &["btn_south"], 2);
    assert_eq!(bus.get("left_trigger").map(|s| s.as_float()), Some(1.0));
}

// While an editor has keyboard focus, keys pause but pad output carries on —
// otherwise a binding under test types into the config you are writing.
#[test]
fn keys_pause_while_an_editor_has_focus() {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    crate::eval::set_jsm_editor_focus(true);
    let bus = run_node("S = SPACE\nE = X_A", false, &["btn_south", "btn_east"], 2);
    crate::eval::set_jsm_editor_focus(false);
    assert!(!on(&bus, "key_space"), "keys pause while typing in the editor");
    assert!(on(&bus, "btn_south"), "pad output carries on, so the mapping is still felt");

    let bus = run_node("S = SPACE", false, &["btn_south"], 2);
    assert!(on(&bus, "key_space"), "and keys resume once the editor loses focus");
}

// An analog trigger counts as a press without a digital trigger pin, which is
// how JSM reads ZL/ZR by default.
#[test]
fn an_analog_trigger_presses_its_button() {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let uid = 4243;
    let snap = jsm_snap(uid, "ZR = LMOUSE", false);
    let mut dev: HashMap<(String, String), Signal> = HashMap::new();
    dev.insert((PAD.to_string(), "right_trigger".to_string()), Signal::Float(0.6));
    let mut collector: HashMap<(String, String), Signal> = HashMap::new();
    let mut state: HashMap<usize, NodeState> = HashMap::new();
    for _ in 0..2 {
        super::eval::jsm_publish(&snap, uid, &dev, &mut collector, &mut state, 0.010);
    }
    let key = format!("collector:{uid}");
    assert_eq!(collector.get(&(key.clone(), "mouse_left".to_string())).map(|s| s.as_bool()), Some(true));
    assert_eq!(collector.get(&(key, "right_trigger".to_string())).map(|s| s.as_float()), Some(0.0),
        "and the trigger itself is taken over");
}

// ── whole configs from JoyShockMapper's own GyroConfigs folder ───────────────
//
// The phase-1 acceptance case: a button-only config loads and plays, and every
// line it carries that later phases will run says so instead of erroring.

/// JSM's `GyroConfigs/xbox.txt`, verbatim (JoyShockMapper, MIT).
const XBOX_CONFIG: &str = "\
# This configuration file will map a virtual xbox controller for each connected controller.
# There is a separate configuration file if you want to use joycons sideways as a xbox controller

VIRTUAL_CONTROLLER = XBOX

UP = X_UP
DOWN = X_DOWN
LEFT = X_LEFT
RIGHT = X_RIGHT
L = X_LB
R = X_RB
W = X_X
S = X_A
N = X_Y
E = X_B
L3 = X_LS
R3 = X_RS
- = X_BACK
+ = X_START
HOME = X_GUIDE
ZL_MODE = X_LT
ZR_MODE = X_RT
LEFT_STICK_MODE = LEFT_STICK
RIGHT_STICK_MODE = RIGHT_STICK";

/// The keyboard-and-mouse half of JSM's `GyroConfigs/Desktop.txt` (MIT).
const DESKTOP_CONFIG: &str = "\
# Windows interaction using gyro
# Clear previous settings
RESET_MAPPINGS

# Calibration
REAL_WORLD_CALIBRATION = 5.3333
IN_GAME_SENS = 1
COUNTER_OS_MOUSE_SPEED

# DPAD is arrows
LEFT = LEFT
RIGHT = RIGHT
UP = UP
DOWN = DOWN

# Mouse Buttons and wheel
R = FMOUSE
L = BMOUSE
ZR = LMOUSE LMOUSE
ZL = RMOUSE RMOUSE
L3 = MMOUSE
LEFT_STICK_MODE = SCROLL_WHEEL
LLEFT = SCROLLUP
LRIGHT = SCROLLDOWN
SCROLL_SENS = 60

# Button pad is common buttons
S = ENTER
W = SPACE
N = BACKSPACE";

fn errors(cfg: &Compiled) -> Vec<String> {
    cfg.lines.iter().enumerate()
        .filter_map(|(i, l)| match &l.status {
            LineStatus::Error(e) => Some(format!("line {}: {e}", i + 1)),
            _ => None,
        })
        .collect()
}

// A real config compiles with nothing unexplained, its button bindings are live,
// and the settings later phases own are pending rather than broken.
#[test]
fn jsms_own_xbox_config_loads_and_maps_its_buttons() {
    let cfg = compile(XBOX_CONFIG);
    assert!(errors(&cfg).is_empty(), "config should carry no errors: {:?}", errors(&cfg));
    // Fifteen button lines, all live.
    assert_eq!(cfg.bindings.len(), 15, "every button line binds");
    // The virtual controller is FlexInput's own wiring; the trigger and stick
    // modes belong to later phases.
    let ignored = cfg.lines.iter().filter(|l| matches!(l.status, LineStatus::Ignored(_))).count();
    let pending = cfg.lines.iter().filter(|l| matches!(l.status, LineStatus::Pending(_))).count();
    assert_eq!(ignored, 1, "VIRTUAL_CONTROLLER is ours to wire");
    assert_eq!(pending, 4, "two trigger modes and two stick modes wait their turn");

    // And it plays: pressing a face button drives the pad pin it was mapped to.
    let mut r = Rig::new(XBOX_CONFIG);
    r.set(Btn::S, true);
    assert!(r.tick().contains("btn_south"));
    r.set(Btn::Minus, true);
    assert!(r.tick().contains("btn_back"), "`-` is a button name, not a modifier");
}

// The desktop config's keyboard and mouse bindings play, including its tap/hold
// pairs and the arrow keys (whose names collide with button names).
#[test]
fn jsms_own_desktop_config_plays_its_keys_and_mouse() {
    let cfg = compile(DESKTOP_CONFIG);
    assert!(errors(&cfg).is_empty(), "config should carry no errors: {:?}", errors(&cfg));

    let mut r = Rig::new(DESKTOP_CONFIG);
    r.set(Btn::S, true);
    assert!(r.tick().contains("key_enter"), "S = ENTER");
    r.set(Btn::Left, true);
    assert!(r.tick().contains("key_arrowleft"), "LEFT = LEFT is the arrow key on the output side");
    r.set(Btn::R, true);
    assert!(r.tick().contains("mouse_forward"), "R = FMOUSE");

    // ZR = LMOUSE LMOUSE — tap and hold both click, so holding clicks and holds.
    r.set(Btn::Zr, true);
    assert!(r.ticks(20).contains("mouse_left"), "holding ZR holds the left button");

    // And its scroll wheel turns, all the way from a stick on the bus to a
    // wheel notch: SCROLL_SENS = 60, so a quarter turn is more than one notch.
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let turn = |x: f32, y: f32| vec![("left_stick", Signal::Vec2(glam::Vec2::new(x, y)))];
    let drove = run_frames(DESKTOP_CONFIG, false, &[turn(1.0, 0.0), turn(0.0, 1.0)]);
    assert!(drove.contains("scroll_up"), "turning the stick scrolls: {drove:?}");
    let drove = run_frames(DESKTOP_CONFIG, false, &[turn(0.0, 1.0), turn(1.0, 0.0)]);
    assert!(drove.contains("scroll_down"), "and the other way scrolls back: {drove:?}");
}
