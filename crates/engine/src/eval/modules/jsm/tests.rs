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
    assert_eq!(
        c.lines[0].status,
        LineStatus::Ok,
        "{line} should be live: {:?}",
        c.lines[0]
    );
    c.bindings.into_iter().next().expect("a binding").steps
}

fn pin(name: &str) -> Out {
    Out::Pin(name.to_string())
}

// ── the value side ───────────────────────────────────────────────────────────

// The only key of a binding follows the press; a second key is the hold and the
// first becomes the tap (JSM's defaults).
#[test]
fn single_key_follows_the_press_and_a_pair_is_tap_then_hold() {
    let s = steps("S = SPACE");
    assert_eq!(
        s,
        vec![Step {
            out: pin("key_space"),
            action: ActionMod::None,
            event: EventMod::Start
        }]
    );

    let s = steps("W = R E");
    assert_eq!(
        s,
        vec![
            Step {
                out: pin("key_r"),
                action: ActionMod::None,
                event: EventMod::Tap
            },
            Step {
                out: pin("key_e"),
                action: ActionMod::None,
                event: EventMod::Hold
            },
        ]
    );

    // NONE takes the slot without doing anything: tap reloads, hold does nothing.
    let s = steps("W = R NONE");
    assert_eq!(
        s[1],
        Step {
            out: Out::None,
            action: ActionMod::None,
            event: EventMod::Hold
        }
    );
}

// Action modifiers before the key, event modifiers after it.
#[test]
fn modifiers_parse_on_both_sides_of_the_key() {
    // JSM's own example: toggle ADS on tap, release the toggle on hold.
    let s = steps("ZL = ^RMOUSE\\ RMOUSE_");
    assert_eq!(
        s,
        vec![
            Step {
                out: pin("mouse_right"),
                action: ActionMod::Toggle,
                event: EventMod::Start
            },
            Step {
                out: pin("mouse_right"),
                action: ActionMod::None,
                event: EventMod::Hold
            },
        ]
    );

    // Instant press and release, turning an in-game toggle into a plain press.
    let s = steps("E = !C\\ !C/");
    assert_eq!(
        s,
        vec![
            Step {
                out: pin("key_c"),
                action: ActionMod::Instant,
                event: EventMod::Start
            },
            Step {
                out: pin("key_c"),
                action: ActionMod::Instant,
                event: EventMod::Release
            },
        ]
    );

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
    assert!(
        matches!(info.status, LineStatus::Error(ref e) if e.contains("third key")),
        "{info:?}"
    );
}

#[test]
fn a_key_on_release_needs_an_action_modifier() {
    let info = one("S = SPACE/");
    assert!(
        matches!(info.status, LineStatus::Error(ref e) if e.contains("action modifier")),
        "{info:?}"
    );
}

// The editor compiles the text on every keystroke, so a value that ends in a
// bare `^` or `!` (the instant you type one, before its key) must read as an
// error on the line — not crash the whole app.
#[test]
fn a_bare_action_modifier_is_an_error_not_a_crash() {
    for line in ["S = ^", "L = !", "E = ^ ", "R = !"] {
        let c = compile(line);
        assert!(
            matches!(c.lines[0].status, LineStatus::Error(_)),
            "{line} should be an error: {:?}",
            c.lines[0],
        );
    }

    // With a key after the modifier everything is fine again.
    let info = one("S = ^SPACE");
    assert_eq!(info.status, LineStatus::Ok, "{info:?}");
    let info = one("L = !C");
    assert_eq!(info.status, LineStatus::Ok, "{info:?}");
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
    assert!(
        matches!(info.status, LineStatus::Error(ref e) if e.contains("instant")),
        "{info:?}"
    );
}

// Calibration on a tap or release only makes sense as a toggle, so JSM makes it
// one. The line runs, but says that recalibrating is not ours to do.
#[test]
fn calibrate_on_a_tap_becomes_a_toggle() {
    let c = compile("HOME = CALIBRATE'");
    assert_eq!(c.lines[0].status, LineStatus::Ok, "{:?}", c.lines[0]);
    assert_eq!(c.bindings[0].steps[0].action, ActionMod::Toggle);
    assert!(
        c.lines[0].notes.iter().any(|n| n.contains("device card")),
        "the line should say who owns calibration: {:?}",
        c.lines[0].notes
    );
}

// The editor compiles the text on every keystroke, so a half-typed line has to
// come back as an error, never as a panic: `S = !` took the whole app down.
#[test]
fn every_half_typed_line_compiles_instead_of_crashing() {
    const LINES: &[&str] = &[
        "S = !C\\", "ZL = ^RMOUSE\\ RMOUSE_", "L,W = 3", "UP*RIGHT = 2", "N,N = X",
        "HOME = \"GyroConfigs/driving.txt\"", "GYRO_SENS = 1 2", "S = -LSHIFT/",
        "LEFT_STICK_MODE = SCROLL_WHEEL", "ZR = LMOUSE LMOUSE", "S = SPACE+",
        "GYRO_OFF = R3", "TRIGGER_THRESHOLD = -1", "T5 = A", "MIN_GYRO_SENS = 2 3",
        "RIGHT_STICK_MODE = FLICK", "L+R = Q", "-,S = SPACE+", "# a comment",
    ];
    for line in LINES {
        for n in 0..=line.chars().count() {
            let prefix: String = line.chars().take(n).collect();
            // A panic here fails the test; the status itself can be anything.
            let _ = compile(&prefix);
            let _ = compile(&format!("{prefix}\nS = SPACE"));
        }
    }
    // The fragments that did the damage, and their neighbours.
    for odd in ["!", "^", "-", "+", "\"", "'", ",", "*", "=", "S =", "S = ", "S = !",
                "S = ^", "S = -", "S = \"", "S = '", "S = _", "S = \\", "S = /",
                "L,", "L+", "L*", "S = A ", "S = A B ", "1 = 2"] {
        let _ = compile(odd);
    }

    // And the one that crashed says what it wants.
    let info = one("S = !");
    assert!(matches!(info.status, LineStatus::Error(ref e) if e.contains("needs a key")),
        "{info:?}");
}

// Every output name JoyShockMapper accepts, taken from its own `nameToKey`
// (`src/win32/PlatformDefinitions.cpp`) plus its pattern-matched and pad names.
// A config carrying any of them must get a truthful answer on the line: either it
// runs, or it says why not. Never an error — an error means "we don't know this
// name", which for a name JSM knows is a gap we've failed to describe — and never
// a silent Ok that does nothing.
const JSM_OUTPUT_NAMES: &[&str] = &[
    // Named keys.
    "ADD", "ALT", "BACKSPACE", "BMOUSE", "CAPS_LOCK", "CONTEXT", "CONTROL", "DECIMAL",
    "DELETE", "DIVIDE", "DOWN", "END", "ENTER", "ESC", "FMOUSE", "HOME", "INSERT",
    "LALT", "LCONTROL", "LEFT", "LMOUSE", "LSHIFT", "LWINDOWS", "MMOUSE", "MULTIPLY",
    "MUTE", "NEXT_TRACK", "NUM_LOCK", "PAGEDOWN", "PAGEUP", "PLAY_PAUSE", "PREV_TRACK",
    "RALT", "RCONTROL", "RIGHT", "RMOUSE", "RSHIFT", "RWINDOWS", "SCREENSHOT",
    "SCROLLDOWN", "SCROLLUP", "SCROLL_LOCK", "SHIFT", "SPACE", "STOP_TRACK",
    "SUBSTRACT", "SUBTRACT", "TAB", "UP", "VOLUME_DOWN", "VOLUME_UP",
    // The pattern-matched ones: characters, function keys, numpad digits, rumble.
    "A", "Z", "0", "9", "+", "-", ",", ".", ";", "/", "`", "[", "]", "'", "\\",
    "F1", "F9", "F10", "F20", "N0", "N9", "R8080", "RFFFF",
    // Actions.
    "NONE", "DEFAULT", "CALIBRATE", "GYRO_ON", "GYRO_OFF", "GYRO_INVERT", "GYRO_INV_X",
    "GYRO_INV_Y", "GYRO_TRACKBALL", "GYRO_TRACK_X", "GYRO_TRACK_Y",
    "SMALL_RUMBLE", "BIG_RUMBLE",
    // Virtual pad.
    "X_A", "X_B", "X_X", "X_Y", "X_LB", "X_RB", "X_LS", "X_RS", "X_BACK", "X_START",
    "X_GUIDE", "X_UP", "X_DOWN", "X_LEFT", "X_RIGHT", "X_LT", "X_RT",
    "PS_CROSS", "PS_CIRCLE", "PS_SQUARE", "PS_TRIANGLE", "PS_L1", "PS_R1", "PS_L3",
    "PS_R3", "PS_SHARE", "PS_OPTIONS", "PS_HOME", "PS_PAD_CLICK", "PS_UP", "PS_DOWN",
    "PS_LEFT", "PS_RIGHT", "PS_L2", "PS_R2",
];

#[test]
fn every_name_jsm_accepts_gets_a_truthful_status() {
    for name in JSM_OUTPUT_NAMES {
        let info = one(&format!("S = {name}"));
        match &info.status {
            LineStatus::Ok => {}
            LineStatus::Pending(why) | LineStatus::Ignored(why) => assert!(
                why.len() > 10,
                "`{name}` isn't live, so the line has to say why: {why:?}"
            ),
            other => panic!("`{name}` is a name JSM accepts, so it can't read as {other:?}"),
        }
        // Anything not live has to name itself in the notes or the status, so the
        // editor can point at what won't work.
        if !matches!(info.status, LineStatus::Ok) {
            let said = format!("{:?} {:?}", info.status, info.notes);
            assert!(
                said.contains(name) || said.len() > 20,
                "`{name}` needs a reason a user can act on: {said}"
            );
        }
    }
}

// A config written for another pad asks for buttons this one hasn't got. That is
// the device's business, not the config's, so the line keeps its status and gains
// a note naming the pin the pad is missing.
#[test]
fn a_button_this_pad_lacks_says_so_on_the_line() {
    let pins: std::collections::HashSet<String> = ["btn_south", "btn_lb", "left_stick"]
        .iter().map(|s| s.to_string()).collect();
    let mut c = compile("S = SPACE\nMIC = M\nL = E\nLUP = W");
    note_inputs_this_pad_lacks(&mut c, &pins);

    assert!(c.lines[0].notes.is_empty(), "a button the pad has needs no note");
    assert!(c.lines[1].notes.iter().any(|n| n.contains("btn_mute") && n.contains("MIC")),
        "the missing one names both the pin and the button: {:?}", c.lines[1].notes);
    assert!(c.lines[2].notes.is_empty(), "L is on this pad");
    assert!(c.lines[3].notes.is_empty(), "and so is the left stick");
    // The status is untouched: the config isn't wrong, this pad just can't do it.
    assert_eq!(c.lines[1].status, LineStatus::Ok);

    // Nothing known about the pad yet: claim nothing.
    let mut c = compile("MIC = M");
    note_inputs_this_pad_lacks(&mut c, &Default::default());
    assert!(c.lines[0].notes.is_empty(), "with no device, nothing is claimed");

    // Either trigger form will do — a pad with only the digital one still works.
    let digital_only: std::collections::HashSet<String> =
        ["btn_rt_dig"].iter().map(|s| s.to_string()).collect();
    let mut c = compile("ZR = LMOUSE");
    note_inputs_this_pad_lacks(&mut c, &digital_only);
    assert!(c.lines[0].notes.is_empty(), "a digital-only trigger is still a trigger");
}

// And a pad binding says out loud that it needs a pad wired up, because nothing
// in this module can tell whether one is.
#[test]
fn a_pad_binding_says_it_needs_a_pad() {
    for line in ["S = X_A", "UP = X_UP", "ZL = X_LT"] {
        let info = one(line);
        assert_eq!(info.status, LineStatus::Ok, "{line}: {info:?}");
        assert!(
            info.notes.iter().any(|n| n.contains("virtual pad")),
            "{line} should say it needs a pad: {:?}", info.notes
        );
    }
    // A keyboard binding has nothing to warn about.
    assert!(one("S = SPACE").notes.is_empty());
}

#[test]
fn unknown_names_are_errors() {
    let info = one("S = FLOOMP");
    assert!(
        matches!(info.status, LineStatus::Error(ref e) if e.contains("FLOOMP")),
        "{info:?}"
    );
    let info = one("FLOOMP = SPACE");
    assert!(
        matches!(info.status, LineStatus::Error(ref e) if e.contains("FLOOMP")),
        "{info:?}"
    );
}

// ── the trigger side ─────────────────────────────────────────────────────────

#[test]
fn combos_parse_into_their_own_triggers() {
    let t = |line: &str| {
        compile(line)
            .bindings
            .into_iter()
            .next()
            .expect("a binding")
            .trigger
    };
    assert_eq!(
        t("L,W = 3"),
        Trigger::Chord {
            chord: Btn::L,
            btn: Btn::W
        }
    );
    assert_eq!(t("N,N = X"), Trigger::Double(Btn::N));
    assert_eq!(t("L+R = Q"), Trigger::Sim(Btn::L, Btn::R));
    assert_eq!(t("UP*RIGHT = 2"), Trigger::Diag(Btn::Up, Btn::Right));
    // `-` and `+` are button names, so a chord can be built from them.
    assert_eq!(
        t("-,S = SPACE+"),
        Trigger::Chord {
            chord: Btn::Minus,
            btn: Btn::S
        }
    );
    assert_eq!(
        t("+,S = SPACE"),
        Trigger::Chord {
            chord: Btn::Plus,
            btn: Btn::S
        }
    );
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
    let c = compile(
        "HOLD_PRESS_TIME = 200\nTURBO_PERIOD = 40\nSIM_PRESS_WINDOW = 60\nDBL_PRESS_WINDOW = 300",
    );
    assert!(
        c.lines.iter().all(|l| l.status == LineStatus::Ok),
        "{:?}",
        c.lines
    );
    assert_eq!(c.timings.hold, 0.2);
    assert_eq!(c.timings.turbo, 0.04);
    assert_eq!(c.timings.sim, 0.06);
    assert_eq!(c.timings.double, 0.3);

    let info = one("HOLD_PRESS_TIME = soon");
    assert!(
        matches!(info.status, LineStatus::Error(ref e) if e.contains("milliseconds")),
        "{info:?}"
    );
}

// A setting we recognise but don't run yet says which phase will run it, rather
// than being dropped or called an error.
#[test]
fn recognised_but_not_live_settings_name_their_phase() {
    for line in [
        "RIGHT_STICK_MODE = HYBRID_AIM",
        "RIGHT_STICK_MODE = MOUSE_RING",
        "GYRO_SPACE = WORLD_TURN",
        "TOUCHPAD_MODE = MOUSE",
        "RUMBLE = OFF",
    ] {
        assert!(
            matches!(one(line).status, LineStatus::Pending(_)),
            "{line} should be pending"
        );
    }
    // A modeshift waits with whatever setting it changes.
    assert!(
        matches!(one("ZL,TOUCHPAD_MODE = MOUSE").status, LineStatus::Pending(p) if p.contains("touchpad"))
    );
}

// Settings FlexInput owns are ignored on purpose, with the reason.
#[test]
fn settings_flexinput_owns_are_ignored_with_a_reason() {
    for line in [
        "AUTOLOAD = OFF",
        "JSM_DIRECTORY = D:\\JSM",
        "TICK_TIME = 3",
        "VIRTUAL_CONTROLLER = DS4",
        "AUTO_CALIBRATE_GYRO = ON",
        "WHITELIST_ADD",
    ] {
        assert!(
            matches!(one(line).status, LineStatus::Ignored(_)),
            "{line} should be ignored"
        );
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
    assert!(
        matches!(one("GyroConfigs/xbox.txt").status, LineStatus::Pending(p) if p.contains("another config"))
    );
}

// ── name mapping caveats ─────────────────────────────────────────────────────

// JSM's left/right modifier variants land on our generic pins, and the line says so.
#[test]
fn left_right_modifiers_note_that_they_become_generic() {
    let c = compile("E = LSHIFT");
    assert_eq!(c.lines[0].status, LineStatus::Ok);
    assert_eq!(c.bindings[0].steps[0].out, pin("key_shift"));
    assert!(
        c.lines[0].notes[0].contains("generic Shift"),
        "{:?}",
        c.lines[0].notes
    );
}

// Keys our sink can't send yet are reported, not silently dropped.
#[test]
fn keys_we_cannot_send_are_reported() {
    let c = compile("E = N5");
    assert!(
        matches!(c.lines[0].status, LineStatus::Ignored(_)),
        "{:?}",
        c.lines[0]
    );
    assert!(
        c.lines[0].notes[0].contains("numpad"),
        "{:?}",
        c.lines[0].notes
    );
    assert!(c.bindings.is_empty());

    let c = compile("E = PLAY_PAUSE");
    assert!(
        c.lines[0].notes[0].contains("media"),
        "{:?}",
        c.lines[0].notes
    );
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
    assert!(
        c.lines
            .iter()
            .all(|l| matches!(l.status, LineStatus::Pending(_))),
        "{:?}",
        c.lines
    );
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
        let bad: Vec<&LineInfo> = cfg
            .lines
            .iter()
            .filter(|l| matches!(l.status, LineStatus::Error(_)))
            .collect();
        assert!(bad.is_empty(), "config should compile: {bad:?}");
        Rig {
            cfg,
            rt: Runtime::default(),
            down: std::collections::HashSet::new(),
        }
    }
    fn set(&mut self, b: Btn, on: bool) {
        if on {
            self.down.insert(b);
        } else {
            self.down.remove(&b);
        }
    }
    /// One tick; returns the pins driven this tick.
    fn tick(&mut self) -> std::collections::HashSet<String> {
        let down = self.down.clone();
        // Resolved the same way the evaluator does — from the chord stack as the
        // last tick left it — so a modeshift on a timing is felt here too.
        let timings = resolve(&self.cfg, self.rt.chords()).timings;
        self.rt
            .tick(&self.cfg, &timings, DT, &|b| down.contains(&b), &|_| false)
            .pins
            .clone()
    }
    /// `n` ticks; returns the pins driven on the last one.
    fn ticks(&mut self, n: usize) -> std::collections::HashSet<String> {
        let mut last = std::collections::HashSet::new();
        for _ in 0..n {
            last = self.tick();
        }
        last
    }
    /// Whether a pin is driven at any point over `n` ticks.
    fn seen_within(&mut self, n: usize, pin: &str) -> bool {
        for _ in 0..n {
            if self.tick().contains(pin) {
                return true;
            }
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
        if on && !was {
            pulses += 1;
        }
        was = on;
    }
    // 600 ms: 150 ms of hold, then a pulse every 80 ms.
    assert!(
        (4..=6).contains(&pulses),
        "expected a handful of turbo pulses, got {pulses}"
    );
}

// A toggle stays on after the button is released, and the next press clears it.
#[test]
fn a_toggle_latches_until_pressed_again() {
    let mut r = Rig::new("ZL = ^RMOUSE\\");
    r.set(Btn::Zl, true);
    assert!(r.tick().contains("mouse_right"));
    r.set(Btn::Zl, false);
    assert!(
        r.ticks(10).contains("mouse_right"),
        "the toggle holds after release"
    );
    r.set(Btn::Zl, true);
    assert!(
        !r.tick().contains("mouse_right"),
        "pressing again clears it"
    );
}

// An instant press lets go by itself while the button is still down.
#[test]
fn an_instant_press_releases_itself() {
    let mut r = Rig::new("E = !C\\");
    r.set(Btn::E, true);
    assert!(r.tick().contains("key_c"));
    assert!(
        !r.ticks(8).contains("key_c"),
        "instant presses release themselves"
    );
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
    assert!(
        !r.tick().contains("key_space"),
        "the release modifier clears the latch"
    );
}

// While a chord button is held, the chorded binding replaces the button own one —
// and the chord button keeps doing its own job.
#[test]
fn a_chord_replaces_the_buttons_own_binding() {
    let mut r = Rig::new("W = R\nL = Q\nL,W = 3");
    r.set(Btn::L, true);
    assert!(
        r.tick().contains("key_q"),
        "the chord button still does its own job"
    );
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
    assert!(
        pins.contains("key_4"),
        "the last chord pressed wins: {pins:?}"
    );
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
    assert!(
        !r.ticks(3).contains("scroll_down"),
        "the tap waits for the double window"
    );
    assert!(
        r.seen_within(25, "scroll_down"),
        "a lone tap still fires, a little late"
    );

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
    assert!(
        r.seen_within(10, "key_shift"),
        "L own binding starts after the window"
    );
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
    assert!(
        pins.contains("key_2"),
        "the diagonal binding applies: {pins:?}"
    );
    assert!(!pins.contains("key_1") && !pins.contains("key_3"));
    r.set(Btn::Right, false);
    let pins = r.ticks(2);
    assert!(
        pins.contains("key_1"),
        "the still-held button gets its binding back: {pins:?}"
    );
    assert!(!pins.contains("key_2"));
}

// The timings a config sets are the ones used.
#[test]
fn the_configs_own_hold_time_applies() {
    let mut r = Rig::new("HOLD_PRESS_TIME = 50\nW = R E");
    r.set(Btn::W, true);
    assert!(
        r.ticks(7).contains("key_e"),
        "50 ms hold should have fired by 70 ms"
    );
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
        let bad: Vec<&LineInfo> = c
            .lines
            .iter()
            .filter(|l| matches!(l.status, LineStatus::Error(_)))
            .collect();
        assert!(bad.is_empty(), "config should compile: {bad:?}");
        Stage {
            s: c.settings,
            a: Analog::default(),
            pad: Pad::default(),
        }
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
        for _ in 0..ticks {
            self.pull(v);
        }
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
    assert!(
        !t.pull(0.4).down(Btn::Zl),
        "under the threshold is not a press"
    );
    assert!(t.pull(0.6).down(Btn::Zl), "over it is");
    assert!(
        !t.pull(0.5).down(Btn::Zl),
        "exactly at it is not — JSM compares strictly"
    );

    let mut t = Stage::new("S = SPACE");
    assert!(
        t.pull(0.10).down(Btn::Zl),
        "the default threshold is the slightest press"
    );
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
    assert!(
        pressed,
        "pulling it down presses, well short of any threshold"
    );

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
    assert!(
        c.lines[0]
            .notes
            .iter()
            .any(|n| n.contains("ZL_MODE is NO_FULL")),
        "the line should explain itself: {:?}",
        c.lines[0].notes
    );
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
    assert!(
        !a.down(Btn::Zl) && a.down(Btn::Zlf),
        "EXCLUSIVE trades one for the other"
    );
    let a = t.pull(0.5);
    assert!(
        a.down(Btn::Zl) && !a.down(Btn::Zlf),
        "and gives the soft pull back"
    );
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
    assert!(
        a.down(Btn::Zl),
        "waiting out the skip delay presses the soft binding"
    );
    let a = t.pull(1.0);
    assert!(
        a.down(Btn::Zl) && !a.down(Btn::Zlf),
        "and once firing, reaching the full pull doesn't stop you"
    );
}

// MAY_SKIP is the same quick pull, but a full pull after the soft one lands on
// top instead of being ignored.
#[test]
fn may_skip_allows_the_full_pull_afterwards() {
    let mut t = Stage::new("ZL_MODE = MAY_SKIP");
    t.pull(0.5);
    let a = t.pull(1.0);
    assert!(
        a.down(Btn::Zlf) && !a.down(Btn::Zl),
        "quick full pull still skips the soft one"
    );

    let mut t = Stage::new("ZL_MODE = MAY_SKIP");
    t.pull_for(0.5, SKIP);
    let a = t.pull(1.0);
    assert!(
        a.down(Btn::Zl) && a.down(Btn::Zlf),
        "a later full pull joins the soft one"
    );
}

// The responsive modes press the soft binding at once and take it back if the
// full pull turns up quickly.
#[test]
fn a_responsive_mode_presses_first_and_takes_it_back() {
    let mut t = Stage::new("ZL_MODE = MUST_SKIP_R");
    assert!(
        t.pull(0.5).down(Btn::Zl),
        "the soft binding is on straight away"
    );
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
    assert!(
        t.pull(0.0).down(Btn::Zl),
        "the held-back soft press fires on release"
    );
    assert!(!t.pull(0.0).down(Btn::Zl), "and is over the next tick");
}

// A pad that reports both an analog trigger and a digital trigger button trips
// the button a fraction of the way down. That button must not count as a full
// pull, or every dual-stage mode fires its full binding almost at once (reported
// as "15% counts as a full pull").
#[test]
fn a_digital_trigger_pin_is_not_a_full_pull() {
    let mut t = Stage::new("ZR_MODE = NO_SKIP");
    t.pad.trigger_digital[1] = true; // asserted early, as a real pad does
    let mut ever_full = false;
    for v in [0.15, 0.15, 0.3, 0.5, 0.8, 0.9] {
        t.pad.triggers[1] = Some(v);
        ever_full |= t.tick().down(Btn::Zrf);
    }
    assert!(t.a.down(Btn::Zr), "the soft pull is pressed all along");
    assert!(!ever_full, "nothing short of the end of the travel is a full pull");

    // Pulled all the way, it fires.
    t.pad.triggers[1] = Some(1.0);
    t.tick();
    assert!(t.tick().down(Btn::Zrf), "at the end of the travel it does");
}

// A pad with no analog trigger can't tell a soft pull from a full one, so JSM
// forces NO_FULL on it rather than firing both.
#[test]
fn a_digital_trigger_never_fires_the_full_pull() {
    let mut t = Stage::new("ZL_MODE = NO_SKIP");
    t.pad.trigger_digital[0] = true;
    // Over several ticks, because a full pull would only land on the second one.
    let mut ever_full = false;
    for _ in 0..4 {
        ever_full |= t.tick().down(Btn::Zlf);
    }
    assert!(
        t.a.down(Btn::Zl),
        "the button still presses the soft binding"
    );
    assert!(
        !ever_full,
        "but nothing on a digital trigger reads as a full pull"
    );
}

// A stick in a digital mode is eight sectors, and a diagonal lights both of its
// directions the way JSM's do.
#[test]
fn stick_directions_are_eight_sectors() {
    let mut t = Stage::new("LUP = W");
    let a = t.stick(0.0, 1.0);
    assert!(a.down(Btn::Lup) && !a.down(Btn::Lleft) && !a.down(Btn::Lright));
    let a = t.stick(0.7, 0.7);
    assert!(
        a.down(Btn::Lup) && a.down(Btn::Lright),
        "a diagonal is both"
    );
    let a = t.stick(0.3, 1.0);
    assert!(
        a.down(Btn::Lup) && !a.down(Btn::Lright),
        "but leaning isn't: a sector is 90° wide"
    );
    let a = t.stick(-1.0, 0.0);
    assert!(a.down(Btn::Lleft) && !a.down(Btn::Lup) && !a.down(Btn::Ldown));
    let a = t.stick(0.05, 0.0);
    for b in [Btn::Lleft, Btn::Lright, Btn::Lup, Btn::Ldown, Btn::Lring] {
        assert!(
            !a.down(b),
            "inside the deadzone is nothing at all, and {} fired",
            b.name()
        );
    }
}

// The ring fires whatever else the stick is doing, and which ring it is decides
// whether it means "moved a little" or "pushed to the edge".
#[test]
fn the_ring_reads_how_far_the_stick_is_pushed() {
    let mut t = Stage::new("LRING = LMOUSE");
    assert!(
        t.stick(0.0, 1.0).down(Btn::Lring),
        "OUTER is the default ring"
    );
    assert!(!t.stick(0.0, 0.5).down(Btn::Lring));

    let mut t = Stage::new("LEFT_RING_MODE = INNER\nLRING = LMOUSE");
    assert!(
        t.stick(0.0, 0.5).down(Btn::Lring),
        "INNER is the other half of the travel"
    );
    assert!(!t.stick(0.0, 1.0).down(Btn::Lring));
    assert!(
        !t.stick(0.0, 0.0).down(Btn::Lring),
        "centred is not a ring press"
    );
}

// Axis inversion and controller orientation both turn the stick before its
// directions are read.
#[test]
fn a_stick_can_be_inverted_or_turned_with_the_controller() {
    let mut t = Stage::new("LEFT_STICK_AXIS = INVERTED\nLDOWN = S");
    assert!(
        t.stick(0.0, 1.0).down(Btn::Ldown),
        "INVERTED flips both axes"
    );

    let mut t = Stage::new("LEFT_STICK_AXIS = STANDARD INVERTED\nLDOWN = S");
    let a = t.stick(1.0, 1.0);
    assert!(
        a.down(Btn::Ldown) && a.down(Btn::Lright),
        "one sign each, x then y"
    );

    let mut t = Stage::new("CONTROLLER_ORIENTATION = LEFT\nLUP = W");
    assert!(
        t.stick(1.0, 0.0).down(Btn::Lup),
        "held sideways, right is up"
    );
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
    assert!(
        t.stick(1.0, 0.0).down(Btn::Lright),
        "and the other way taps the right one"
    );

    // Back to the middle forgets the turn rather than banking it.
    let mut t = Stage::new("LEFT_STICK_MODE = SCROLL_WHEEL\nLLEFT = SCROLLUP");
    t.stick(1.0, 0.0);
    t.stick(0.0, 0.0);
    assert!(
        !t.stick(0.0, 1.0).down(Btn::Lleft),
        "a lap through the centre is not a notch"
    );
}

// A stick that aims the mouse drives no directions, and the line that bound one
// says why rather than looking broken.
#[test]
fn a_stick_that_aims_the_mouse_has_no_directions() {
    let mut t = Stage::new("LEFT_STICK_MODE = AIM\nLUP = W");
    assert!(!t.stick(0.0, 1.0).down(Btn::Lup));

    let c = compile("LEFT_STICK_MODE = AIM\nLUP = W");
    assert_eq!(c.lines[0].status, LineStatus::Ok, "{:?}", c.lines[0]);
    assert!(
        c.lines[1]
            .notes
            .iter()
            .any(|n| n.contains("LEFT_STICK_MODE")),
        "the binding should explain itself: {:?}",
        c.lines[1].notes
    );

    // A mode a later phase owns is pending, and drives nothing at all.
    let c = compile("LEFT_STICK_MODE = MOUSE_RING\nLUP = W");
    assert!(
        matches!(c.lines[0].status, LineStatus::Pending(p) if p.contains("absolute mouse")),
        "{:?}",
        c.lines[0]
    );
}

// The settings this phase runs are read off the config, not guessed at.
#[test]
fn trigger_and_stick_settings_are_read() {
    let c = compile(
        "ZR_MODE = MAY_SKIP_R\nRIGHT_STICK_MODE = SCROLL_WHEEL\nSCROLL_SENS = 45\n\
        LEFT_STICK_DEADZONE_INNER = 0.2\nSTICK_DEADZONE_OUTER = 0.05\nRIGHT_RING_MODE = INNER",
    );
    assert!(
        c.lines.iter().all(|l| l.status == LineStatus::Ok),
        "{:?}",
        c.lines
    );
    assert_eq!(c.settings.zr, TriggerMode::MaySkipR);
    assert_eq!(c.settings.right.mode, StickMode::ScrollWheel);
    assert_eq!(c.settings.right.scroll_sens, 45.0);
    assert_eq!(c.settings.left.inner_dz, 0.2);
    assert_eq!(
        c.settings.right.inner_dz, 0.15,
        "one stick's deadzone is its own"
    );
    assert_eq!(
        c.settings.left.outer_dz, 0.05,
        "the unprefixed one sets both"
    );
    assert_eq!(c.settings.right.outer_dz, 0.05);
    assert_eq!(c.settings.right.ring, RingMode::Inner);
    assert_eq!(c.settings.left.ring, RingMode::Outer);

    // A value JSM knows but we don't run yet names its phase; a typo is an error.
    assert!(
        matches!(one("LEFT_STICK_MODE = MOUSE_RING").status, LineStatus::Pending(p)
        if p.contains("absolute mouse pin"))
    );
    assert!(matches!(
        one("ZL_MODE = SORT_OF").status,
        LineStatus::Error(_)
    ));
    assert!(matches!(
        one("TRIGGER_THRESHOLD = half").status,
        LineStatus::Error(_)
    ));
}

// ── aiming: gyro, stick aim, flick ───────────────────────────────────────────

use super::aim::{Aim, AxisMask, Gyro, SnapMode};

/// A config's aiming, driven a tick at a time with a gyro and sticks in hand.
struct Aiming {
    cfg: Compiled,
    a: Aim,
    analog: Analog,
    /// What the physical pad is reporting.
    pad: Pad,
    /// The virtual pad the config drives, when it drives one.
    pad_out: super::pad::Pad,
    gyro: Gyro,
    actions: std::collections::HashSet<GyroAction>,
    down: std::collections::HashSet<Btn>,
}

impl Aiming {
    fn new(text: &str) -> Aiming {
        let cfg = compile(text);
        let bad: Vec<&LineInfo> = cfg
            .lines
            .iter()
            .filter(|l| matches!(l.status, LineStatus::Error(_)))
            .collect();
        assert!(bad.is_empty(), "config should compile: {bad:?}");
        Aiming {
            cfg,
            a: Aim::default(),
            analog: Analog::default(),
            pad: Pad::default(),
            pad_out: super::pad::Pad::default(),
            gyro: Gyro::default(),
            actions: std::collections::HashSet::new(),
            down: std::collections::HashSet::new(),
        }
    }
    /// Turn the pad at this many degrees a second and advance one tick.
    fn turn(&mut self, pitch: f32, yaw: f32) -> glam::Vec2 {
        self.gyro = Gyro {
            roll: 0.0,
            pitch,
            yaw,
        };
        self.tick()
    }
    fn stick(&mut self, side: usize, x: f32, y: f32) -> glam::Vec2 {
        self.pad.sticks[side] = (x, y);
        self.tick()
    }
    fn tick(&mut self) -> glam::Vec2 {
        self.aimed().mouse
    }
    /// Everything one tick of aiming produced, not just the mouse half.
    fn aimed(&mut self) -> super::aim::Aimed {
        self.analog.tick(&self.cfg.settings, DT, &self.pad);
        let down = self.down.clone();
        self.a.tick(
            &self.cfg.aim,
            &self.cfg.pad,
            DT,
            self.gyro,
            &self.analog,
            &self.actions,
            &|b| down.contains(&b),
        )
    }
    /// One tick of aiming, then one of the pad side it feeds.
    fn to_pad(&mut self) -> super::pad::Out {
        let aimed = self.aimed();
        self.pad_out.tick(
            &self.cfg.pad,
            DT,
            &self.analog,
            self.cfg.aim.stick_power,
            aimed.gyro_dps,
            aimed.flick_dps,
        )
    }
    fn pad_ticks(&mut self, n: usize) -> super::pad::Out {
        let mut last = super::pad::Out::default();
        for _ in 0..n {
            last = self.to_pad();
        }
        last
    }
    fn ticks(&mut self, n: usize) -> glam::Vec2 {
        let mut last = glam::Vec2::ZERO;
        for _ in 0..n {
            last = self.tick();
        }
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
    assert!(
        (moved - 100.0 * DT).abs() < 0.01,
        "a degree is a count: {moved}"
    );
}

// Sensitivity multiplies the turn, and zero sensitivity — JSM's default — aims
// with nothing at all.
#[test]
fn gyro_sensitivity_scales_the_turn_and_zero_means_off() {
    let mut a = Aiming::new(AIM_CFG);
    let one = a.turn(0.0, 90.0).x;
    let mut b = Aiming::new("GYRO_SENS = 4\nREAL_WORLD_CALIBRATION = 1\nGYRO_SMOOTH_TIME = 0");
    let four = b.turn(0.0, 90.0).x;
    assert!(
        (four - one * 4.0).abs() < 0.01,
        "four times the sensitivity: {four} vs {one}"
    );

    let mut off = Aiming::new("REAL_WORLD_CALIBRATION = 1");
    assert_eq!(
        off.turn(0.0, 90.0),
        glam::Vec2::ZERO,
        "JSM starts at zero sensitivity"
    );
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
    assert!(
        (slow - 1.0).abs() < 0.05,
        "slow turns stay at MIN_GYRO_SENS: {slow}"
    );
    // Halfway up, halfway between the two.
    let mid = a.turn(0.0, 60.0).x / (60.0 * DT);
    assert!((mid - 3.0).abs() < 0.05, "halfway is halfway: {mid}");
    // At the top, and beyond it, the high sensitivity.
    let fast = a.turn(0.0, 200.0).x / (200.0 * DT);
    assert!(
        (fast - 5.0).abs() < 0.05,
        "fast turns reach MAX_GYRO_SENS: {fast}"
    );
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
    assert!(
        half > 0.0 && half < 15.0 * DT,
        "halfway through recovery it is faded: {half}"
    );
    assert!(
        (full - 40.0 * DT).abs() < 0.01,
        "past recovery it is all there: {full}"
    );
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
    assert!(
        a.turn(0.0, 90.0).x < 0.0,
        "inverted, turning right aims left"
    );

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
    assert!(
        a.turn(0.0, 90.0).x > 0.0,
        "with no button, the gyro is simply on"
    );

    // A binding's action wins over the setting while it is asserted.
    let mut a = Aiming::new(AIM_CFG);
    a.actions.insert(GyroAction::Off);
    assert_eq!(
        a.turn(0.0, 90.0).x,
        0.0,
        "GYRO_OFF as a binding blocks it too"
    );
}

// Aiming with a stick: pushing it moves the mouse, and the deadzone holds it
// still.
#[test]
fn a_stick_can_aim_the_mouse() {
    let cfg = "RIGHT_STICK_MODE = AIM\nSTICK_SENS = 100\nREAL_WORLD_CALIBRATION = 1";
    let mut a = Aiming::new(cfg);
    let out = a.stick(1, 1.0, 0.0);
    assert!(
        out.x > 0.0 && out.y.abs() < 1e-6,
        "pushing right aims right: {out:?}"
    );
    let up = a.stick(1, 0.0, 1.0);
    assert!(up.y > 0.0, "and pushing up aims up: {up:?}");
    assert_eq!(
        a.stick(1, 0.05, 0.0),
        glam::Vec2::ZERO,
        "inside the deadzone, nothing"
    );

    // How far it is pushed is shaped by STICK_POWER and only then scaled, so a
    // half push with a squared curve aims at a quarter speed.
    let cfg = "RIGHT_STICK_MODE = AIM\nSTICK_SENS = 100\nREAL_WORLD_CALIBRATION = 1\n\
        STICK_POWER = 2\nSTICK_DEADZONE_INNER = 0\nSTICK_DEADZONE_OUTER = 0";
    let mut a = Aiming::new(cfg);
    let half = a.stick(1, 0.5, 0.0).x;
    let full = a.stick(1, 1.0, 0.0).x;
    assert!(
        (half - 0.25 * 100.0 * DT).abs() < 1e-4,
        "half push, quarter speed: {half}"
    );
    assert!(
        (full - 100.0 * DT).abs() < 1e-4,
        "full push, full speed: {full}"
    );
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
    assert!(
        (total.abs() - 180.0).abs() < 2.0,
        "a flick backwards turns 180 degrees' worth: {total}"
    );
    assert!(
        biggest < total.abs() * 0.5,
        "and it is eased over the flick time, not dumped in a tick: {biggest}"
    );
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
    assert!(
        (traced.abs() - 90.0).abs() < 2.0,
        "the camera follows the stick round: {traced}"
    );

    // FLICK_ONLY leaves the tracing out.
    let mut b = Aiming::new(
        "RIGHT_STICK_MODE = FLICK_ONLY\nREAL_WORLD_CALIBRATION = 1\nROTATE_SMOOTH_OVERRIDE = 0",
    );
    b.stick(1, 0.0, 1.0);
    b.ticks(30);
    let traced = b.stick(1, 1.0, 0.0).x;
    assert!(traced.abs() < 1.0, "FLICK_ONLY doesn't trace: {traced}");

    // ROTATE_ONLY is the other half: it traces without ever flicking.
    let mut c = Aiming::new(
        "RIGHT_STICK_MODE = ROTATE_ONLY\nREAL_WORLD_CALIBRATION = 1\nROTATE_SMOOTH_OVERRIDE = 0",
    );
    c.stick(1, 0.0, 0.0);
    let flick = c.stick(1, 0.0, -1.0).x + c.ticks(30).x;
    assert!(flick.abs() < 1.0, "ROTATE_ONLY doesn't flick: {flick}");
    assert!(
        (c.stick(1, 1.0, 0.0).x.abs() - 90.0).abs() < 2.0,
        "but it does trace"
    );
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
    for _ in 0..40 {
        total += a.tick().x;
    }
    assert!(
        (total.abs() - 90.0).abs() < 2.0,
        "snapped to a quarter turn: {total}"
    );

    // Without snapping it turns through the angle it was actually given.
    let mut b = Aiming::new("RIGHT_STICK_MODE = FLICK\nREAL_WORLD_CALIBRATION = 1");
    b.stick(1, 0.0, 0.0);
    b.stick(1, 0.95, 0.31);
    let mut total = 0.0;
    for _ in 0..40 {
        total += b.tick().x;
    }
    assert!(
        (total.abs() - 72.0).abs() < 2.0,
        "unsnapped, the angle is its own: {total}"
    );
}

// The settings this phase runs are read off the config.
#[test]
fn aiming_settings_are_read() {
    let c = compile(
        "MIN_GYRO_SENS = 2 3\nGYRO_SMOOTH_TIME = 0.05\nFLICK_TIME = 0.25\n\
        IN_GAME_SENS = 2\nSTICK_POWER = 1.5\nTRACKBALL_DECAY = 3\nGYRO_SPACE = LOCAL",
    );
    assert!(
        c.lines.iter().all(|l| l.status == LineStatus::Ok),
        "{:?}",
        c.lines
    );
    assert_eq!(
        c.aim.min_sens,
        (2.0, 3.0),
        "a pair setting takes one number per axis"
    );
    assert_eq!(c.aim.smooth_time, 0.05);
    assert_eq!(c.aim.flick_time, 0.25);
    assert_eq!(c.aim.in_game_sens, 2.0);
    assert_eq!(c.aim.stick_power, 1.5);
    assert_eq!(c.aim.trackball_decay, 3.0);

    // A gyro space measured against gravity waits for the phase that has one.
    assert!(matches!(one("GYRO_SPACE = PLAYER_TURN").status,
        LineStatus::Pending(p) if p.contains("gravity")));
    assert!(
        matches!(
            one("REAL_WORLD_CALIBRATION = 0").status,
            LineStatus::Error(_)
        ),
        "a calibration of zero would divide the aim by nothing"
    );
}

// ── modeshifts: settings that change while a button is held ──────────────────

// `button,SETTING = value` reads that value while the button is held, and the
// config's own value again when it isn't.
#[test]
fn a_chord_changes_a_setting_while_it_is_held() {
    let cfg = compile("GYRO_SENS = 1\nZL,GYRO_SENS = 4");
    assert!(cfg.lines.iter().all(|l| l.status == LineStatus::Ok), "{:?}", cfg.lines);
    assert_eq!(cfg.modeshifts.len(), 1);

    assert_eq!(resolve(&cfg, &[]).aim.min_sens, (1.0, 1.0), "nothing held: the config's own");
    assert_eq!(resolve(&cfg, &[Btn::Zl]).aim.min_sens, (4.0, 4.0), "held: the chord's");
    assert_eq!(resolve(&cfg, &[Btn::R]).aim.min_sens, (1.0, 1.0), "another button changes nothing");
}

// Two chords over the same setting: the one pressed last wins, and each chord
// still gets its say over the settings the other doesn't touch.
#[test]
fn the_latest_chord_wins_setting_by_setting() {
    let cfg = compile("GYRO_SENS = 1\nZL,GYRO_SENS = 4\nZR,GYRO_SENS = 9\nZR,IN_GAME_SENS = 2");
    // The stack is oldest first, as the press machinery keeps it.
    assert_eq!(resolve(&cfg, &[Btn::Zl, Btn::Zr]).aim.min_sens, (9.0, 9.0), "ZR pressed later");
    assert_eq!(resolve(&cfg, &[Btn::Zr, Btn::Zl]).aim.min_sens, (4.0, 4.0), "ZL pressed later");

    // IN_GAME_SENS is only ZR's, so it applies whichever went down last.
    let both = resolve(&cfg, &[Btn::Zr, Btn::Zl]);
    assert_eq!(both.aim.in_game_sens, 2.0, "a setting only one chord touches still applies");
}

// A modeshift is checked when the config is compiled, so a bad value is an error
// on its own line rather than a surprise when the button is pressed.
#[test]
fn a_modeshift_with_a_bad_value_is_an_error() {
    let info = one("ZL,GYRO_SENS = lots");
    assert!(matches!(info.status, LineStatus::Error(ref e) if e.contains("number")), "{info:?}");
    assert!(compile("ZL,GYRO_SENS = lots").modeshifts.is_empty(), "and it doesn't run");

    // Only a chord can carry a setting; the other combo operators can't.
    let info = one("ZL+GYRO_SENS = 2");
    assert!(matches!(info.status, LineStatus::Error(ref e) if e.contains("chord")), "{info:?}");
}

// Timings shift too, and the press machinery feels it: a shorter hold time while
// the chord is held means the hold binding fires sooner.
#[test]
fn a_chord_can_shorten_the_hold_time() {
    let mut r = Rig::new("W = R E\nZL,HOLD_PRESS_TIME = 30");
    r.set(Btn::W, true);
    // The default 150 ms hold hasn't arrived yet at 50 ms.
    assert!(!r.ticks(5).contains("key_e"), "the base hold time still applies");
    r.set(Btn::W, false);
    r.ticks(2);

    r.set(Btn::Zl, true);
    r.ticks(2); // the chord stack is read as the last tick left it
    r.set(Btn::W, true);
    assert!(r.ticks(5).contains("key_e"), "held, the chord's 30 ms hold fires sooner");
}

// The stick-recentre rule: when a chord that changed a stick's mode is released
// while the stick is still pushed, the stick does nothing at all until it comes
// back to centre — otherwise the mode underneath would be handed a stick already
// out at full.
#[test]
fn a_stick_waits_for_centre_after_its_modeshift_ends() {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let uid = 7101;
    let cfg = "LUP = W\nZL,LEFT_STICK_MODE = AIM\nSTICK_SENS = 100";
    let snap = jsm_snap(uid, cfg, false);
    let key = format!("collector:{uid}");
    let mut state: HashMap<usize, NodeState> = HashMap::new();
    let up = Signal::Vec2(glam::Vec2::new(0.0, 1.0));

    let mut run = |chord: bool, stick: bool| -> HashMap<String, Signal> {
        let mut dev: HashMap<(String, String), Signal> = HashMap::new();
        if chord {
            dev.insert((PAD.to_string(), "left_trigger".to_string()), Signal::Float(1.0));
        }
        dev.insert(
            (PAD.to_string(), "left_stick".to_string()),
            if stick { up } else { Signal::Vec2(glam::Vec2::ZERO) },
        );
        let mut collector: HashMap<(String, String), Signal> = HashMap::new();
        super::eval::jsm_publish(&snap, uid, &dev, &mut collector, &mut state, 0.010);
        collector.into_iter()
            .filter(|((d, _), _)| *d == key)
            .map(|((_, p), s)| (p, s))
            .collect()
    };

    // Base mode: the stick's directions drive their key.
    let bus = run(false, true);
    assert!(on(&bus, "key_w"), "the base mode reads directions");

    // Hold ZL: the stick aims instead, so the direction stops.
    run(true, true);
    let bus = run(true, true);
    assert!(!on(&bus, "key_w"), "while the chord is held the stick aims: {bus:?}");

    // Let go of ZL with the stick still pushed: neither mode acts.
    let bus = run(false, true);
    assert!(!on(&bus, "key_w"), "still pushed, the stick waits for centre");
    let bus = run(false, true);
    assert!(!on(&bus, "key_w"), "and keeps waiting");

    // Back to centre, then pushed again: the base mode is back.
    run(false, false);
    let bus = run(false, true);
    assert!(on(&bus, "key_w"), "once recentred the base mode reads it again: {bus:?}");
}

// A flick that hasn't finished paying out isn't cut off by a modeshift: the stick
// stays in flick mode (JSM drops it to FLICK_ONLY) until the turn is done.
#[test]
fn a_modeshift_cannot_cut_a_flick_off_mid_turn() {
    let mut a = Aiming::new("RIGHT_STICK_MODE = FLICK\nREAL_WORLD_CALIBRATION = 1\nFLICK_TIME = 0.2");
    a.stick(1, 0.0, 0.0);
    a.stick(1, 0.0, -1.0);
    assert!(a.a.flick_unfinished(1), "the flick is still paying out");
    // Once it has run its course it lets go again.
    a.ticks(40);
    assert!(!a.a.flick_unfinished(1), "and stops holding the mode when it is done");

    // A stick that never flicked never claims the mode — JSM's own check reads
    // "percent done < 1", which is true before any flick at all.
    let mut b = Aiming::new("RIGHT_STICK_MODE = AIM\nSTICK_SENS = 100");
    b.stick(1, 0.0, 0.0);
    assert!(!b.a.flick_unfinished(1), "an untouched stick isn't mid-flick");
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
    params.insert(
        "jsm_tabs".to_string(),
        serde_json::json!([{ "name": "main", "text": text }]),
    );
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
    collector
        .into_iter()
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
            if d == key && sig.as_bool() {
                drove.insert(pin);
            }
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
    assert!(
        on(&bus, "btn_north"),
        "a button it never mentions passes through"
    );
    // Downstream Combiners learn which pins this module consumed.
    assert!(
        bus.keys()
            .any(|p| p.starts_with("__consumed__") && p.contains("btn_south")),
        "the claimed pin is marked consumed: {:?}",
        bus.keys().collect::<Vec<_>>()
    );
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
    assert_eq!(
        stick(&bus),
        glam::Vec2::ZERO,
        "and the stick itself is taken over"
    );
    assert_eq!(bus.get("left_stick_y").map(|s| s.as_float()), Some(0.0));

    // A stick this config never reads as directions is left alone instead.
    let bus = run_node_with("S = SPACE", false, &[("left_stick", up)], 2);
    assert_eq!(
        stick(&bus),
        glam::Vec2::new(0.0, 1.0),
        "an untouched stick passes straight through"
    );

    // And so is one whose mode a later phase owns: taking it over would silence
    // the stick for a binding that can't run yet.
    let bus = run_node_with(
        "LEFT_STICK_MODE = MOUSE_RING\nLUP = W",
        false,
        &[("left_stick", up)],
        2,
    );
    assert_eq!(
        stick(&bus),
        glam::Vec2::new(0.0, 1.0),
        "a stick in a mode we don't run keeps passing through"
    );
    assert!(!on(&bus, "key_w"));
}

// The axes on their own are enough: a pad that publishes x and y but no vector
// still drives the directions.
#[test]
fn stick_axes_work_without_the_vector() {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let bus = run_node_with(
        "LRIGHT = D",
        false,
        &[("left_stick_x", Signal::Float(1.0))],
        2,
    );
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
    assert!(
        on(&bus, "mouse_left") && on(&bus, "mouse_right"),
        "NO_SKIP keeps both: {bus:?}"
    );
    assert_eq!(
        bus.get("right_trigger").map(|s| s.as_float()),
        Some(0.0),
        "the trigger is the config's now"
    );

    // Without a mode, the full pull can't fire — so the trigger isn't taken over
    // on its account either.
    let bus = run_node_with(
        "ZRF = RMOUSE",
        false,
        &[("right_trigger", Signal::Float(1.0))],
        2,
    );
    assert!(!on(&bus, "mouse_right"));
    assert_eq!(
        bus.get("right_trigger").map(|s| s.as_float()),
        Some(1.0),
        "a trigger nothing live reads keeps passing through"
    );
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
    assert_eq!(
        bus.get("gyro_z").map(|s| s.as_float()),
        Some(0.0),
        "and the gyro belongs to the config now"
    );

    // At JSM's default sensitivity of zero, nothing aims and the gyro passes on.
    let bus = run_node_with("S = SPACE", false, &turn, 2);
    assert!(
        bus.get("mouse_move").is_none(),
        "no sensitivity, no movement"
    );
    assert_eq!(
        bus.get("gyro_z").map(|s| s.as_float()),
        Some(0.5),
        "a gyro nothing reads keeps passing through"
    );

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


// The bug that made a bound key stick down for ever: a sink latches what it was
// last told, so a pin the config can drive has to be answered for on EVERY tick,
// not only on the one where it happens to be released.
#[test]
fn a_released_key_stays_released() {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let uid = 7001;
    let snap = jsm_snap(uid, "L = E", false);
    let key = format!("collector:{uid}");
    let mut state: HashMap<usize, NodeState> = HashMap::new();
    let mut seen = Vec::new();
    for pressed in [true, true, true, false, false, false] {
        let mut dev: HashMap<(String, String), Signal> = HashMap::new();
        if pressed {
            dev.insert((PAD.to_string(), "btn_lb".to_string()), Signal::Bool(true));
        }
        let mut collector: HashMap<(String, String), Signal> = HashMap::new();
        super::eval::jsm_publish(&snap, uid, &dev, &mut collector, &mut state, 0.010);
        seen.push(collector.get(&(key.clone(), "key_e".to_string())).map(|s| s.as_bool()));
    }
    assert_eq!(seen, vec![Some(true), Some(true), Some(true),
                          Some(false), Some(false), Some(false)],
        "the key must be held while the button is, and released for good after");
}

// The same trap for aiming: `mouse_move` is a one-shot displacement, so a sink
// left holding the last one would nudge the cursor for ever.
#[test]
fn a_still_pad_publishes_no_more_movement() {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = "GYRO_SENS = 1
REAL_WORLD_CALIBRATION = 1";
    let turning = [("gyro_z", Signal::Float(0.5))];
    let bus = run_node_with(cfg, false, &turning, 2);
    match bus.get("mouse_move") {
        Some(Signal::Vec2(v)) => assert!(v.x > 0.0, "turning moves the mouse: {v:?}"),
        other => panic!("expected movement: {other:?}"),
    }
    let bus = run_node_with(cfg, false, &[("gyro_z", Signal::Float(0.0))], 2);
    match bus.get("mouse_move") {
        Some(Signal::Vec2(v)) => assert_eq!(*v, glam::Vec2::ZERO, "a still pad says so"),
        other => panic!("a config that aims must answer every tick: {other:?}"),
    }
}

// A binding onto the virtual pad's D-pad has to drive all three forms the D-pad
// reaches a sink in. Driving only the direction Bool left the zeroed `dpad` Vec2
// to land after it and cancel it, so the binding did nothing at the pad.
#[test]
fn a_dpad_binding_drives_the_axis_and_vector_too() {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let bus = run_node("S = X_UP", false, &["btn_south"], 2);
    assert!(on(&bus, "dpad_up"), "the direction itself");
    assert_eq!(bus.get("dpad_y").map(|s| s.as_float()), Some(1.0), "the axis form");
    match bus.get("dpad") {
        Some(Signal::Vec2(v)) => assert_eq!(*v, glam::Vec2::new(0.0, 1.0), "and the vector form"),
        other => panic!("the D-pad vector should be published: {other:?}"),
    }

    // Let go and all three forms say so.
    let bus = run_node("S = X_UP", false, &[], 2);
    assert!(!on(&bus, "dpad_up"));
    assert_eq!(bus.get("dpad_y").map(|s| s.as_float()), Some(0.0));

    // A direction the config never touches keeps whatever the pad is doing.
    let bus = run_node_with(
        "S = X_UP",
        false,
        &[("btn_south", Signal::Bool(true)), ("dpad_left", Signal::Bool(true))],
        2,
    );
    match bus.get("dpad") {
        Some(Signal::Vec2(v)) => assert_eq!(*v, glam::Vec2::new(-1.0, 1.0),
            "the pad's own direction survives alongside the config's"),
        other => panic!("{other:?}"),
    }
}

// Strict mode: nothing but the config's own output leaves the module.
#[test]
fn strict_mode_publishes_only_what_the_config_says() {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let bus = run_node("S = SPACE", true, &["btn_south", "btn_north"], 2);
    assert!(on(&bus, "key_space"));
    assert!(
        !on(&bus, "btn_north"),
        "strict mode holds back the rest of the pad"
    );
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
    assert!(
        !on(&bus, "key_space"),
        "keys pause while typing in the editor"
    );
    assert!(
        on(&bus, "btn_south"),
        "pad output carries on, so the mapping is still felt"
    );

    let bus = run_node("S = SPACE", false, &["btn_south"], 2);
    assert!(
        on(&bus, "key_space"),
        "and keys resume once the editor loses focus"
    );
}


// An analog trigger counts as a press without a digital trigger pin, which is
// how JSM reads ZL/ZR by default.
#[test]
fn an_analog_trigger_presses_its_button() {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let uid = 4243;
    let snap = jsm_snap(uid, "ZR = LMOUSE", false);
    let mut dev: HashMap<(String, String), Signal> = HashMap::new();
    dev.insert(
        (PAD.to_string(), "right_trigger".to_string()),
        Signal::Float(0.6),
    );
    let mut collector: HashMap<(String, String), Signal> = HashMap::new();
    let mut state: HashMap<usize, NodeState> = HashMap::new();
    for _ in 0..2 {
        super::eval::jsm_publish(&snap, uid, &dev, &mut collector, &mut state, 0.010);
    }
    let key = format!("collector:{uid}");
    assert_eq!(
        collector
            .get(&(key.clone(), "mouse_left".to_string()))
            .map(|s| s.as_bool()),
        Some(true)
    );
    assert_eq!(
        collector
            .get(&(key, "right_trigger".to_string()))
            .map(|s| s.as_float()),
        Some(0.0),
        "and the trigger itself is taken over"
    );
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
    cfg.lines
        .iter()
        .enumerate()
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
    assert!(
        errors(&cfg).is_empty(),
        "config should carry no errors: {:?}",
        errors(&cfg)
    );
    // Fifteen button lines, all live.
    assert_eq!(cfg.bindings.len(), 15, "every button line binds");
    // The virtual controller is FlexInput's own wiring. Everything else in it —
    // both triggers passed through to the pad, both sticks driving the pad's own
    // — runs as of phase 5, so JSM's shipped Xbox config is now live end to end.
    let ignored = cfg
        .lines
        .iter()
        .filter(|l| matches!(l.status, LineStatus::Ignored(_)))
        .count();
    let pending: Vec<_> = cfg
        .lines
        .iter()
        .filter(|l| matches!(l.status, LineStatus::Pending(_)))
        .collect();
    assert_eq!(ignored, 1, "VIRTUAL_CONTROLLER is ours to wire");
    assert!(
        pending.is_empty(),
        "nothing in JSM's own Xbox config is still waiting: {pending:?}"
    );

    // And it plays: pressing a face button drives the pad pin it was mapped to.
    let mut r = Rig::new(XBOX_CONFIG);
    r.set(Btn::S, true);
    assert!(r.tick().contains("btn_south"));
    r.set(Btn::Minus, true);
    assert!(
        r.tick().contains("btn_back"),
        "`-` is a button name, not a modifier"
    );
}

// The desktop config's keyboard and mouse bindings play, including its tap/hold
// pairs and the arrow keys (whose names collide with button names).
#[test]
fn jsms_own_desktop_config_plays_its_keys_and_mouse() {
    let cfg = compile(DESKTOP_CONFIG);
    assert!(
        errors(&cfg).is_empty(),
        "config should carry no errors: {:?}",
        errors(&cfg)
    );

    let mut r = Rig::new(DESKTOP_CONFIG);
    r.set(Btn::S, true);
    assert!(r.tick().contains("key_enter"), "S = ENTER");
    r.set(Btn::Left, true);
    assert!(
        r.tick().contains("key_arrowleft"),
        "LEFT = LEFT is the arrow key on the output side"
    );
    r.set(Btn::R, true);
    assert!(r.tick().contains("mouse_forward"), "R = FMOUSE");

    // ZR = LMOUSE LMOUSE — tap and hold both click, so holding clicks and holds.
    r.set(Btn::Zr, true);
    assert!(
        r.ticks(20).contains("mouse_left"),
        "holding ZR holds the left button"
    );

    // And its scroll wheel turns, all the way from a stick on the bus to a
    // wheel notch: SCROLL_SENS = 60, so a quarter turn is more than one notch.
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let turn = |x: f32, y: f32| vec![("left_stick", Signal::Vec2(glam::Vec2::new(x, y)))];
    let drove = run_frames(DESKTOP_CONFIG, false, &[turn(1.0, 0.0), turn(0.0, 1.0)]);
    assert!(
        drove.contains("scroll_up"),
        "turning the stick scrolls: {drove:?}"
    );
    let drove = run_frames(DESKTOP_CONFIG, false, &[turn(0.0, 1.0), turn(1.0, 0.0)]);
    assert!(
        drove.contains("scroll_down"),
        "and the other way scrolls back: {drove:?}"
    );
}

// ── phase 5: virtual pad output ──────────────────────────────────────────────

/// A stick told to be a virtual stick drives that stick, and a push out reads as
/// a push out — the whole round trip through camera degrees per second and back.
#[test]
fn a_stick_can_be_the_virtual_pads_stick() {
    let mut a = Aiming::new("LEFT_STICK_MODE = LEFT_STICK");
    // Two ticks: JSM reads the previous tick's direction, so the first push has
    // nothing to point at yet.
    a.pad.sticks[0] = (1.0, 0.0);
    let out = a.pad_ticks(2);
    let v = out.sticks[0].expect("the left virtual stick is driven");
    assert!(v.x > 0.9, "a stick pushed right pushes the virtual stick right: {v:?}");
    assert!(v.y.abs() < 0.01, "and not up or down: {v:?}");
    assert_eq!(out.sticks[1], None, "the right stick is left alone");

    // And up is up. This one is worth its own assertion: the conversion runs
    // through camera space, where y counts *down*, and it has to come back — get
    // the round trip wrong and every virtual stick is inverted vertically, which
    // is both very obvious to a player and invisible in a test that only pushes
    // sideways.
    let mut a = Aiming::new("LEFT_STICK_MODE = LEFT_STICK");
    a.pad.sticks[0] = (0.0, 1.0);
    let v = a.pad_ticks(2).sticks[0].expect("driven");
    assert!(v.y > 0.9, "a stick pushed up pushes the virtual stick up: {v:?}");
    assert!(v.x.abs() < 0.01, "and not left or right: {v:?}");
}

/// The gyro can drive a virtual stick instead of the mouse — and when it does,
/// JSM stops moving the mouse entirely. Both halves are the point.
#[test]
fn the_gyro_can_drive_a_virtual_stick_instead_of_the_mouse() {
    let cfg = format!("{AIM_CFG}\nGYRO_OUTPUT = RIGHT_STICK\nVIRTUAL_STICK_CALIBRATION = 100");
    let mut a = Aiming::new(&cfg);
    a.gyro = Gyro { roll: 0.0, pitch: 0.0, yaw: 50.0 };
    let out = a.pad_ticks(3);
    let v = out.sticks[1].expect("the right virtual stick is driven");
    assert!(v.x > 0.1, "turning the pad pushes the virtual stick: {v:?}");
    // Half the calibrated speed is half the stick, give or take the smoother.
    assert!(v.x < 0.9, "and not all the way over at half speed: {v:?}");

    let mut a = Aiming::new(&cfg);
    a.gyro = Gyro { roll: 0.0, pitch: 0.0, yaw: 50.0 };
    let aimed = a.aimed();
    assert_eq!(
        aimed.mouse,
        glam::Vec2::ZERO,
        "with the gyro pointed at a stick the mouse gets nothing"
    );
}

/// A stick and the gyro pointed at the same virtual stick combine, rather than
/// one overwriting the other — that is the whole reason JSM works in deg/s.
#[test]
fn a_stick_and_the_gyro_share_one_virtual_stick() {
    let cfg = format!(
        "{AIM_CFG}\nLEFT_STICK_MODE = LEFT_STICK\nGYRO_OUTPUT = LEFT_STICK\n\
         VIRTUAL_STICK_CALIBRATION = 100"
    );
    let stick_only = {
        let mut a = Aiming::new(&cfg);
        a.pad.sticks[0] = (0.3, 0.0);
        a.pad_ticks(3).sticks[0].expect("driven").x
    };
    let both = {
        let mut a = Aiming::new(&cfg);
        a.pad.sticks[0] = (0.3, 0.0);
        a.gyro = Gyro { roll: 0.0, pitch: 0.0, yaw: 30.0 };
        a.pad_ticks(3).sticks[0].expect("driven").x
    };
    assert!(
        both > stick_only + 0.05,
        "the gyro adds to the stick rather than replacing it: {stick_only} then {both}"
    );
}

/// `ANGLE_TO_X`: how far the stick is turned off the Y axis becomes an X push,
/// with no sideways travel needed — a stick that steers like a wheel.
#[test]
fn angle_to_axis_turns_a_lean_into_a_push() {
    let cfg = "LEFT_STICK_MODE = LEFT_ANGLE_TO_X\nANGLE_TO_AXIS_DEADZONE_OUTER = 0";
    // Straight up: no lean, no push.
    let mut a = Aiming::new(cfg);
    a.pad.sticks[0] = (0.0, 1.0);
    let straight = a.pad_ticks(2).sticks[0].expect("driven");
    assert!(straight.x.abs() < 0.01, "straight ahead is no steer: {straight:?}");
    // Leaned 45 degrees to the right: half of the 90 degree span.
    let mut a = Aiming::new(cfg);
    a.pad.sticks[0] = (0.7071, 0.7071);
    let leaned = a.pad_ticks(2).sticks[0].expect("driven");
    assert!(
        (leaned.x - 0.5).abs() < 0.05,
        "45 degrees off the axis is half the range: {leaned:?}"
    );
    assert!(leaned.y.abs() < 0.001, "and the other axis stays put: {leaned:?}");
}

/// Winding: turning the stick round winds a value up, and it only comes back
/// when the stick is let go.
#[test]
fn winding_builds_up_by_turning_and_unwinds_when_let_go() {
    let cfg = "LEFT_STICK_MODE = LEFT_WIND_X\nWIND_STICK_RANGE = 360\nUNWIND_RATE = 100";
    let mut a = Aiming::new(cfg);
    // Turn the stick a quarter of the way round, held right out.
    let mut wound = 0.0;
    for step in 0..=8 {
        let angle = std::f32::consts::FRAC_PI_2 * step as f32 / 8.0;
        a.pad.sticks[0] = (angle.sin(), angle.cos());
        wound = a.to_pad().sticks[0].expect("driven").x;
    }
    assert!(wound > 0.1, "turning one way winds one way: {wound}");

    // Let the stick go: it unwinds back towards nothing.
    a.pad.sticks[0] = (0.0, 0.0);
    let after = a.pad_ticks(30).sticks[0].expect("driven").x;
    assert!(
        after.abs() < wound.abs() - 0.05,
        "a released stick unwinds: {wound} then {after}"
    );

    // How hard the stick is pushed scales the winding: the same turn made with a
    // half-pushed stick winds about half as far. Without that, a lazy nudge round
    // the edge of the deadzone would wind as fast as a deliberate sweep.
    let quarter_turn = |reach: f32| {
        let mut a = Aiming::new(cfg);
        let mut last = 0.0;
        for step in 0..=8 {
            let angle = std::f32::consts::FRAC_PI_2 * step as f32 / 8.0;
            a.pad.sticks[0] = (reach * angle.sin(), reach * angle.cos());
            last = a.to_pad().sticks[0].expect("driven").x;
        }
        last
    };
    let full = quarter_turn(1.0);
    let half = quarter_turn(0.6);
    assert!(
        half < full * 0.8,
        "a gentler push winds less for the same turn: {full} at full, {half} at 0.6"
    );
}

/// `ZL_MODE = X_LT` hands the trigger straight to the virtual pad, and takes its
/// own bindings out of the picture.
#[test]
fn a_trigger_can_pass_straight_through_to_the_pad() {
    let out = run_node_with(
        "ZL_MODE = X_LT\nZL = E",
        false,
        &[("left_trigger", Signal::Float(0.6))],
        3,
    );
    assert_eq!(
        out.get("left_trigger").map(|s| s.as_float()),
        Some(0.6),
        "the pull reaches the virtual trigger as it stands: {out:?}"
    );
    assert!(
        !out.get("key_e").map(|s| s.as_bool()).unwrap_or(false),
        "and the trigger's own binding no longer fires"
    );
}

/// The same trigger still counts as pressed for chords, which is JSM's rule and
/// the reason the pass-through doesn't simply skip the button machinery. Note the
/// looser rule it uses: any pull at all is the soft press, and only the very end
/// of travel is the full one — there is no state machine left to ask.
#[test]
fn a_passed_through_trigger_still_reads_as_pressed_for_chords() {
    let mut s = Stage::new("ZL_MODE = X_LT");
    assert!(!s.pull(0.0).down(Btn::Zl), "a resting trigger is not held");
    assert!(
        s.pull(0.5).down(Btn::Zl),
        "half a pull is enough to chord with"
    );
    assert!(
        !s.pull(0.5).down(Btn::Zlf),
        "but half a pull is not the full pull"
    );
    assert!(s.pull(1.0).down(Btn::Zlf), "the end of travel is");
}

/// And the chord it supplies reaches all the way through to a chorded binding —
/// the passed-through trigger has to stack a chord without ever entering the press
/// machinery, so this is the end-to-end proof that the split works.
#[test]
fn a_passed_through_trigger_still_supplies_a_chord() {
    let out = run_node_with(
        "ZL_MODE = X_LT
ZL,S = E",
        false,
        &[
            ("left_trigger", Signal::Float(0.6)),
            ("btn_south", Signal::Bool(true)),
        ],
        3,
    );
    assert!(
        out.get("key_e").map(|s| s.as_bool()).unwrap_or(false),
        "the chorded binding fires with the trigger pulled: {out:?}"
    );

    // Not pulled, no chord, no binding.
    let out = run_node_with(
        "ZL_MODE = X_LT
ZL,S = E",
        false,
        &[("btn_south", Signal::Bool(true))],
        3,
    );
    assert!(
        !out.get("key_e").map(|s| s.as_bool()).unwrap_or(false),
        "and not without it: {out:?}"
    );
}

/// `STEER_X` is the motion stick's alone in JSM, which refuses it on a thumbstick.
/// Saying "a later phase will do this" would be a lie, so it is an error with the
/// alternative named.
#[test]
fn steer_on_a_thumbstick_says_it_will_never_work() {
    let info = one("LEFT_STICK_MODE = LEFT_STEER_X");
    match &info.status {
        LineStatus::Error(why) => {
            assert!(why.contains("MOTION_STICK_MODE"), "says where it does work: {why}");
            assert!(why.contains("WIND_X"), "and what to use instead: {why}");
        }
        other => panic!("STEER_X on a thumbstick should be an error, got {other:?}"),
    }
}

/// Every pad-output line says it needs a pad wired downstream, and the gyro one
/// also warns that it silences the mouse — the surprise JSM has and never says.
#[test]
fn pad_output_lines_say_what_they_need_and_what_they_cost() {
    let stick = one("LEFT_STICK_MODE = LEFT_STICK");
    assert_eq!(stick.status, LineStatus::Ok);
    assert!(
        stick.notes.iter().any(|n| n.contains("virtual pad wired downstream")),
        "a stick driving a pad says so: {:?}",
        stick.notes
    );

    let gyro = one("GYRO_OUTPUT = LEFT_STICK");
    assert!(
        gyro.notes.iter().any(|n| n.contains("MOUSE_AREA")),
        "and the gyro warns it silences the aiming sticks too: {:?}",
        gyro.notes
    );

    // PS_MOTION claims nothing and explains what actually happens.
    let motion = one("GYRO_OUTPUT = PS_MOTION");
    assert_eq!(motion.status, LineStatus::Ok);
    assert!(
        motion.notes.iter().any(|n| n.contains("passes straight through")),
        "PS_MOTION says the pad's own motion goes downstream: {:?}",
        motion.notes
    );
}

/// `GYRO_OUTPUT = PS_MOTION` must leave the gyro pins alone, or the DS4 sink it
/// exists to feed would never see them.
#[test]
fn ps_motion_lets_the_pads_own_gyro_through() {
    let cfg = format!("{AIM_CFG}\nGYRO_OUTPUT = PS_MOTION");
    let out = run_node_with(&cfg, false, &[("gyro_z", Signal::Float(0.25))], 2);
    assert_eq!(
        out.get("gyro_z").map(|s| s.as_float()),
        Some(0.25),
        "the gyro passes through untouched: {out:?}"
    );
}

/// A virtual stick lands on the bus in all three forms a sink might read, the
/// same trap the D-pad hit: a sink prefers the Vec2, so a disagreeing pair of
/// floats would be silently ignored — or worse, win.
#[test]
fn a_virtual_stick_drives_the_vector_and_both_axes() {
    let out = run_node_with(
        "RIGHT_STICK_MODE = RIGHT_STICK",
        false,
        &[("right_stick", Signal::Vec2(glam::Vec2::new(1.0, 0.0)))],
        3,
    );
    let v = match out.get("right_stick") {
        Some(Signal::Vec2(v)) => *v,
        other => panic!("the vector form is published: {other:?}"),
    };
    assert!(v.x > 0.9, "and carries the push: {v:?}");
    assert_eq!(out.get("right_stick_x").map(|s| s.as_float()), Some(v.x));
    assert_eq!(out.get("right_stick_y").map(|s| s.as_float()), Some(v.y));
}

/// The undeadzone runs a game's own deadzone backwards: told the game ignores the
/// first 30%, a gentle push should come out above 30% rather than under it.
#[test]
fn the_undeadzone_lifts_a_small_push_over_the_games_deadzone() {
    // It bites on the gyro's contribution, not a stick's: a stick push has its
    // own length divided out and multiplied straight back in, so for a stick
    // alone the setting is a no-op. That is JSM's arithmetic, and it is the right
    // answer — the stick already reaches the whole range, and it is the gyro's
    // small nudges that the game's deadzone would swallow.
    let nudge = |extra: &str| {
        let cfg = format!(
            "{AIM_CFG}\nGYRO_OUTPUT = LEFT_STICK\nVIRTUAL_STICK_CALIBRATION = 400\n{extra}"
        );
        let mut a = Aiming::new(&cfg);
        a.gyro = Gyro { roll: 0.0, pitch: 0.0, yaw: 40.0 };
        a.pad_ticks(3).sticks[0].expect("driven").x
    };
    let bare = nudge("");
    let lifted = nudge("LEFT_STICK_UNDEADZONE_INNER = 0.3");
    assert!(
        bare < 0.3,
        "without it a gentle turn stays inside the game's deadzone: {bare}"
    );
    assert!(lifted > 0.3, "with it the same turn clears it: {lifted}");
}

/// Every one of phase 5's settings is read, with a bad value called out rather
/// than quietly ignored.
#[test]
fn every_pad_setting_is_read() {
    let c = compile(
        "VIRTUAL_STICK_CALIBRATION = 180\n\
         LEFT_STICK_UNDEADZONE_INNER = 0.1\n\
         LEFT_STICK_UNDEADZONE_OUTER = 0.2\n\
         LEFT_STICK_UNPOWER = 2\n\
         LEFT_STICK_VIRTUAL_SCALE = 1.5\n\
         RIGHT_STICK_UNDEADZONE_INNER = 0.3\n\
         RIGHT_STICK_UNPOWER = 3\n\
         RIGHT_STICK_VIRTUAL_SCALE = 0.5\n\
         ANGLE_TO_AXIS_DEADZONE_INNER = 5\n\
         ANGLE_TO_AXIS_DEADZONE_OUTER = 15\n\
         WIND_STICK_RANGE = 720\n\
         WIND_STICK_POWER = 2\n\
         UNWIND_RATE = 900\n\
         FLICK_STICK_OUTPUT = LEFT_STICK",
    );
    assert!(errors(&c).is_empty(), "all of these are live: {:?}", errors(&c));
    assert_eq!(c.pad.calibration, 180.0);
    assert_eq!(c.pad.out[0].undeadzone_inner, 0.1);
    assert_eq!(c.pad.out[0].undeadzone_outer, 0.2);
    assert_eq!(c.pad.out[0].unpower, 2.0);
    assert_eq!(c.pad.out[0].virtual_scale, 1.5);
    assert_eq!(c.pad.out[1].undeadzone_inner, 0.3);
    assert_eq!(c.pad.out[1].unpower, 3.0);
    assert_eq!(c.pad.out[1].virtual_scale, 0.5);
    assert_eq!(c.pad.angle_dz_inner, 5.0);
    assert_eq!(c.pad.angle_dz_outer, 15.0);
    assert_eq!(c.pad.wind_range, 720.0);
    assert_eq!(c.pad.wind_power, 2.0);
    assert_eq!(c.pad.unwind_rate, 900.0);
    assert_eq!(c.pad.flick_dest, super::pad::Dest::LeftStick);

    // And a value out of range is an error naming what it wanted.
    for bad in [
        "VIRTUAL_STICK_CALIBRATION = 0",
        "LEFT_STICK_UNDEADZONE_INNER = 2",
        "ANGLE_TO_AXIS_DEADZONE_INNER = 100",
        "WIND_STICK_RANGE = 0",
        "GYRO_OUTPUT = SIDEWAYS",
    ] {
        assert!(
            matches!(one(bad).status, LineStatus::Error(_)),
            "{bad} should be an error"
        );
    }
}

/// A flick bound for a stick turns at one speed for as long as the angle needs,
/// rather than being eased out the way a mouse flick is.
#[test]
fn a_flick_to_a_stick_holds_a_steady_turn() {
    let cfg = format!(
        "{AIM_CFG}\nRIGHT_STICK_MODE = FLICK\nFLICK_STICK_OUTPUT = RIGHT_STICK\n\
         VIRTUAL_STICK_CALIBRATION = 180"
    );
    let mut a = Aiming::new(&cfg);
    // Push the stick straight left: a quarter turn to make.
    a.pad.sticks[1] = (-1.0, 0.0);
    let first = a.to_pad();
    let v = first.sticks[1].expect("the flick drives the stick");
    assert!(v.x.abs() > 0.9, "a flick pushes the stick right over: {v:?}");

    // A quarter turn at 180 deg/s takes half a second — still going a tick later,
    // and done well before a second is out.
    let mid = a.pad_ticks(10).sticks[1].expect("driven");
    assert!(mid.x.abs() > 0.9, "and holds it while the turn runs: {mid:?}");
    let end = a.pad_ticks(500).sticks[1].expect("driven");
    assert!(end.x.abs() < 0.1, "then stops when the angle is covered: {end:?}");
}
