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

// A console command in quotes fires once, and can only be instant.
#[test]
fn a_quoted_command_is_instant() {
    // A name that isn't a config file stays a console command.
    let (s, _) = parse_mapping("\"RECONNECT_CONTROLLERS\"", &[]).expect("parses");
    assert_eq!(s[0].action, ActionMod::Instant);
    assert_eq!(s[0].out, Out::Command("RECONNECT_CONTROLLERS".into()));

    // A config file name is a layer switch, and resolves to the tab of that name.
    let tabs = vec![("driving".to_string(), String::new())];
    let (s, _) = parse_mapping("\"GyroConfigs/driving.txt\"", &tabs).expect("parses");
    assert_eq!(s[0].out, Out::Layer("driving".into()));

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
        "SCREEN_RESOLUTION_X = 1920",
        "STICKLIKE_FACTOR = 1",
    ] {
        assert!(
            matches!(one(line).status, LineStatus::Pending(_)),
            "{line} should be pending"
        );
    }
    // A modeshift waits with whatever setting it changes.
    assert!(
        matches!(one("ZL,STICKLIKE_FACTOR = 1").status, LineStatus::Pending(p) if p.contains("HYBRID_AIM"))
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

/// Naming a config with no tab of that name is an error that says which tabs there
/// are — the most useful thing it can say, since the fix is always "load it into a
/// tab" or "you meant one of these".
#[test]
fn naming_a_config_with_no_tab_says_which_tabs_there_are() {
    match one("GyroConfigs/xbox.txt").status {
        LineStatus::Error(why) => {
            assert!(why.contains("xbox"), "it names what was asked for: {why}");
            assert!(why.contains("load"), "and what to do about it: {why}");
        }
        other => panic!("a missing tab should be an error, got {other:?}"),
    }

    let tabs = vec![
        ("driving".to_string(), String::new()),
        ("onfoot".to_string(), String::new()),
    ];
    match compile_with("GyroConfigs/xbox.txt", &tabs).lines[0].status.clone() {
        LineStatus::Error(why) => {
            assert!(
                why.contains("driving") && why.contains("onfoot"),
                "with tabs present it lists them: {why}"
            );
        }
        other => panic!("a missing tab should be an error, got {other:?}"),
    }
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

/// As of phase 6 there is no JSM button left that this module cannot read: the
/// motion stick, the lean buttons and the touchpad were the last three. This is
/// the test that used to assert they waited for a phase — kept, turned around, so
/// the day a new input source appears it is a deliberate change rather than a
/// silent regression.
#[test]
fn every_button_jsm_has_can_be_read() {
    let c = compile("MUP = W\nTOUCH = LMOUSE\nLEAN_LEFT = Q\nT7 = E\nTRING = R\nMRING = F");
    assert!(
        c.lines.iter().all(|l| l.status == LineStatus::Ok),
        "every source is live: {:?}",
        c.lines
    );
    assert_eq!(c.bindings.len(), 6, "and every line binds");

    // Belt and braces: walk every name JSM accepts and check none of them comes
    // back as a source we cannot read.
    for name in [
        "UP", "DOWN", "LEFT", "RIGHT", "L", "ZL", "ZLF", "R", "ZR", "ZRF", "N", "E", "S", "W",
        "L3", "R3", "-", "+", "HOME", "CAPTURE", "MIC", "LSL", "LSR", "RSL", "RSR",
        "LUP", "LDOWN", "LLEFT", "LRIGHT", "LRING", "RUP", "RDOWN", "RLEFT", "RRIGHT", "RRING",
        "MUP", "MDOWN", "MLEFT", "MRIGHT", "MRING", "TUP", "TDOWN", "TLEFT", "TRIGHT", "TRING",
        "LEAN_LEFT", "LEAN_RIGHT", "TOUCH", "T1", "T25",
    ] {
        let line = format!("{name} = E");
        let info = one(&line);
        assert_eq!(
            info.status,
            LineStatus::Ok,
            "{name} should be readable, got {:?}",
            info.status
        );
    }
}

#[test]
fn the_summary_counts_what_the_editor_shows() {
    let c = compile("S = SPACE\nSTICKLIKE_FACTOR = 1\nAUTOLOAD = OFF\nFLOOMP = A");
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
    /// Gravity, and the accelerometer it is worked out from.
    motion: super::motion::Motion,
    accel: Option<glam::Vec3>,
    gravity: super::motion::Gravity,
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
            motion: super::motion::Motion::default(),
            accel: None,
            gravity: super::motion::Gravity::default(),
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
        // Gravity first, so a gravity-referenced space and the motion stick have
        // something to measure against, then all five sticks.
        let gravity = self.motion.tick(self.accel, DT);
        self.gravity = gravity;
        self.pad.sticks[super::analog::MOTION] =
            super::motion::motion_stick(gravity, self.cfg.settings.motion);
        self.analog.tick(&self.cfg.settings, DT, &self.pad);
        let down = self.down.clone();
        self.a.tick(
            &self.cfg.aim,
            &self.cfg.pad,
            &self.cfg.motion,
            &self.cfg.cc,
            gravity,
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
            super::motion::steer(
                self.gravity,
                self.cfg.settings.orientation,
                self.cfg.settings.motion.inner_dz * 180.0,
                (1.0 - self.cfg.settings.motion.outer_dz) * 180.0,
                self.cfg.aim.stick_power,
            ),
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

    // A gyro space measured against gravity is live, and says what it leans on.
    let space = one("GYRO_SPACE = PLAYER_TURN");
    assert_eq!(space.status, LineStatus::Ok);
    assert!(
        space.notes.iter().any(|n| n.contains("accelerometer")),
        "it says which sensor it needs: {:?}",
        space.notes
    );
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
    let _guard = alone();
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
/// keyboard), and while it is on the config's key and mouse output pauses. So any
/// test that runs a node has to hold this, or a focus test running beside it makes
/// every key read as released — an intermittent failure that looks like a bug in
/// whatever the test was actually about.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Take the lock *and* put editor focus back to off. Every helper that runs a node
/// goes through this, so a new test cannot forget it: the lock is held for the call
/// and dropped after, which is enough because nothing here nests.
fn alone() -> Alone {
    // Taking it twice on one thread would deadlock on a std mutex, and a deadlock is
    // the worst failure to debug: no message, no backtrace, just a test that never
    // ends. Say what went wrong instead. (This cost an hour once — the `*_unlocked`
    // helpers exist for callers that already hold it.)
    assert!(
        !HELD.with(|h| h.get()),
        "this thread already holds the test lock — use the `run_*_unlocked` form inside          a test that took `alone()` itself"
    );
    let guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    HELD.with(|h| h.set(true));
    crate::eval::set_jsm_editor_focus(false);
    Alone(guard)
}

thread_local! {
    static HELD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The lock, plus the flag that makes a second take on the same thread say so.
struct Alone(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

impl Drop for Alone {
    fn drop(&mut self) {
        HELD.with(|h| h.set(false));
    }
}

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
    let _guard = alone();
    run_node_unlocked_with(text, strict, sigs, ticks)
}

/// The same, for a caller that already holds [`alone`] — the one test that wants
/// editor focus left ON has to, since `alone` clears it.
fn run_node_unlocked(
    text: &str,
    strict: bool,
    held: &[&str],
    ticks: usize,
) -> HashMap<String, Signal> {
    let sigs: Vec<(&str, Signal)> = held.iter().map(|p| (*p, Signal::Bool(true))).collect();
    run_node_unlocked_with(text, strict, &sigs, ticks)
}

fn run_node_unlocked_with(
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
    let _guard = alone();
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
    let _guard = alone();
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
    let bus = run_node("S = X_LT", false, &["btn_south"], 2);
    assert_eq!(bus.get("left_trigger").map(|s| s.as_float()), Some(1.0));
}

// While an editor has keyboard focus, keys pause but pad output carries on —
// otherwise a binding under test types into the config you are writing.
#[test]
fn keys_pause_while_an_editor_has_focus() {
    // The only test that wants focus ON, so it drives the node itself: `run_node`
    // clears the flag on the way in, which is what stops every other test from
    // inheriting it.
    let _guard = alone();
    crate::eval::set_jsm_editor_focus(true);
    let bus = run_node_unlocked("S = SPACE\nE = X_A", false, &["btn_south", "btn_east"], 2);
    crate::eval::set_jsm_editor_focus(false);
    assert!(
        !on(&bus, "key_space"),
        "keys pause while typing in the editor"
    );
    assert!(
        on(&bus, "btn_south"),
        "pad output carries on, so the mapping is still felt"
    );

    // Still holding the lock, so the unlocked form again — `run_node` would try to
    // take it a second time, and a std mutex is not reentrant.
    let bus = run_node_unlocked("S = SPACE", false, &["btn_south"], 2);
    assert!(
        on(&bus, "key_space"),
        "and keys resume once the editor loses focus"
    );
}


// An analog trigger counts as a press without a digital trigger pin, which is
// how JSM reads ZL/ZR by default.
#[test]
fn an_analog_trigger_presses_its_button() {
    let _guard = alone();
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

// ── phase 6: gravity, the motion stick and the touchpad ──────────────────────

/// Our accelerometer, for a pad held in a named pose. The bus's accel basis is
/// `(F, -R, U)` and an accelerometer at rest reads the direction that is *up*.
fn accel_for(pose: &str) -> glam::Vec3 {
    match pose {
        // Up is out of the face.
        "flat" => glam::Vec3::new(0.0, 0.0, 1.0),
        // Nose points at the sky, so up is forward.
        "nose up" => glam::Vec3::new(1.0, 0.0, 0.0),
        "nose down" => glam::Vec3::new(-1.0, 0.0, 0.0),
        // The right grip is down, so up is towards the pad's left, which is +y.
        "right grip down" => glam::Vec3::new(0.0, 1.0, 0.0),
        "left grip down" => glam::Vec3::new(0.0, -1.0, 0.0),
        "upside down" => glam::Vec3::new(0.0, 0.0, -1.0),
        other => panic!("no such pose: {other}"),
    }
}

/// Gravity in JSM's frame, once the estimate has settled on a pose.
fn gravity_for(pose: &str) -> super::motion::Gravity {
    let mut m = super::motion::Motion::default();
    let a = accel_for(pose);
    let mut g = super::motion::Gravity::default();
    // The filter starts on its first reading, so one tick settles it; a few more
    // make sure nothing drifts.
    for _ in 0..40 {
        g = m.tick(Some(a), DT);
    }
    g
}

/// The frame conversion, pose by pose. Everything else in phase 6 is measured
/// against this, so if it is wrong nothing above it can be right — and a sign
/// error here is invisible without hardware unless it is pinned like this.
#[test]
fn gravity_lands_in_jsms_frame() {
    // JSM's frame is (right, up, forward) and gravity points down, so a pad held
    // flat has gravity straight down its Y.
    let flat = gravity_for("flat");
    assert!(flat.known, "a pad reporting an accelerometer knows which way is down");
    assert!((flat.v - glam::Vec3::new(0.0, -1.0, 0.0)).length() < 0.01, "flat: {:?}", flat.v);

    // Nose up: the forward axis points at the sky, so gravity is along -Z.
    let nose_up = gravity_for("nose up");
    assert!((nose_up.v - glam::Vec3::new(0.0, 0.0, -1.0)).length() < 0.01, "nose up: {:?}", nose_up.v);

    // Right grip down: gravity is along the pad's right, +X.
    let grip = gravity_for("right grip down");
    assert!((grip.v - glam::Vec3::new(1.0, 0.0, 0.0)).length() < 0.01, "right grip down: {:?}", grip.v);

    // And a pad with no accelerometer says so, rather than claiming to be flat —
    // the difference between a motion stick that rests and one that runs away.
    let mut m = super::motion::Motion::default();
    let g = m.tick(None, DT);
    assert!(!g.known, "no accelerometer means no idea which way is down");
}

/// The motion stick: tilt the pad and the stick pushes the way it was tilted.
#[test]
fn the_motion_stick_follows_how_the_pad_is_tilted() {
    let cfg = super::analog::StickCfg::default();
    let (x, y) = super::motion::motion_stick(gravity_for("flat"), cfg);
    assert!(x.abs() < 0.01 && y.abs() < 0.01, "held flat it rests: {x}, {y}");

    // Tipping the nose down pushes the stick down and tipping it up pushes it up,
    // which is JSM's `calY = -grav.z` — the pad tips the way the stick goes, like
    // steering a plane rather than pulling a lever.
    let (x, y) = super::motion::motion_stick(gravity_for("nose down"), cfg);
    assert!(y < -0.4, "nose down pushes the stick down: {x}, {y}");
    assert!(x.abs() < 0.01, "and not sideways: {x}");
    let (_, up) = super::motion::motion_stick(gravity_for("nose up"), cfg);
    assert!(up > 0.4, "and nose up pushes it up: {up}");

    // Right grip down pushes it right.
    let (x, y) = super::motion::motion_stick(gravity_for("right grip down"), cfg);
    assert!(x > 0.4, "right grip down pushes the stick right: {x}, {y}");
    assert!(y.abs() < 0.01, "and not up or down: {y}");

    // A quarter turn is half of the half-turn the stick's range covers.
    let (x, _) = super::motion::motion_stick(gravity_for("right grip down"), cfg);
    assert!((x - 0.5).abs() < 0.02, "a quarter turn is half deflection: {x}");

    // And with no accelerometer it stays centred rather than picking a direction.
    let (x, y) = super::motion::motion_stick(super::motion::Gravity::default(), cfg);
    assert_eq!((x, y), (0.0, 0.0));
}

/// `SET_MOTION_STICK_NEUTRAL` makes whatever pose the pad is in read as centred.
#[test]
fn the_motion_stick_can_be_recentred() {
    let mut m = super::motion::Motion::default();
    let a = accel_for("nose up");
    let mut g = super::motion::Gravity::default();
    for _ in 0..40 {
        g = m.tick(Some(a), DT);
    }
    let cfg = super::analog::StickCfg::default();
    let (_, before) = super::motion::motion_stick(g, cfg);
    assert!(before.abs() > 0.4, "nose up is a long way off centre: {before}");

    m.set_neutral(g);
    let g = m.tick(Some(a), DT);
    let (x, y) = super::motion::motion_stick(g, cfg);
    assert!(
        x.abs() < 0.02 && y.abs() < 0.02,
        "after re-centring the same pose reads as centred: {x}, {y}"
    );
}

/// The lean buttons fire past `LEAN_THRESHOLD` degrees of side tilt, and not
/// before — the threshold is what stops a pad resting at an angle from holding one
/// down for ever.
#[test]
fn leaning_the_pad_presses_the_lean_buttons() {
    let s = super::motion::Settings { lean_threshold: 15.0, ..Default::default() };
    let o = super::analog::Orientation::default();

    let (l, r) = super::motion::lean(gravity_for("flat"), &s, o);
    assert!(!l && !r, "held flat, neither");
    let (l, r) = super::motion::lean(gravity_for("right grip down"), &s, o);
    assert!(r && !l, "right grip down leans right");
    let (l, r) = super::motion::lean(gravity_for("left grip down"), &s, o);
    assert!(l && !r, "left grip down leans left");

    // Just under the threshold, nothing; just over it, something.
    let tilt = |deg: f32| {
        let rad = deg.to_radians();
        let mut m = super::motion::Motion::default();
        let a = glam::Vec3::new(0.0, rad.sin(), rad.cos());
        let mut g = super::motion::Gravity::default();
        for _ in 0..40 {
            g = m.tick(Some(a), DT);
        }
        super::motion::lean(g, &s, o)
    };
    assert!(!tilt(10.0).1, "10 degrees is inside the threshold");
    assert!(tilt(20.0).1, "20 degrees is past it");

    // No accelerometer, no lean — rather than both, or a stuck one.
    let (l, r) = super::motion::lean(super::motion::Gravity::default(), &s, o);
    assert!(!l && !r);
}

/// `PLAYER_TURN`: turning the pad about *gravity* turns the camera, whichever way
/// the pad happens to be tilted. That is the whole point of the space — and the
/// one thing a `LOCAL` config cannot do.
#[test]
fn player_turn_reads_a_turn_about_gravity_whatever_the_tilt() {
    use super::motion::{gravity_space, JsmGyro, Space};
    // Held flat, gravity is down JSM's -Y, so a yaw about the pad's own up axis is
    // a turn: JSM's Y component of the gyro.
    let flat = gravity_for("flat");
    let (x, _) = gravity_space(Space::PlayerTurn, flat, JsmGyro { x: 0.0, y: 10.0, z: 0.0 });
    assert!(x.abs() > 1.0, "flat, a yaw turns the camera: {x}");

    // Nose up, the pad's *roll* axis is the one pointing at the sky, so the same
    // turn now arrives on Z — and the space must still see it as a turn.
    let nose_up = gravity_for("nose up");
    let (x, _) = gravity_space(Space::PlayerTurn, nose_up, JsmGyro { x: 0.0, y: 0.0, z: 10.0 });
    assert!(x.abs() > 1.0, "nose up, a roll turns the camera: {x}");

    // And a pure pitch is not a turn in either pose.
    for (name, g) in [("flat", flat), ("nose up", nose_up)] {
        let (x, y) = gravity_space(Space::PlayerTurn, g, JsmGyro { x: 10.0, y: 0.0, z: 0.0 });
        assert!(x.abs() < 0.01, "{name}: a pitch is not a turn: {x}");
        assert!(y.abs() > 1.0, "{name}: but it is a pitch: {y}");
    }
}

/// The gravity spaces fade out when the pad is held on its side, where a pitch
/// axis worked out from gravity means almost nothing. Without that they flail.
#[test]
fn a_world_space_gives_up_when_the_pad_is_on_its_side() {
    use super::motion::{gravity_space, JsmGyro, Space};
    let upright = gravity_for("flat");
    let (_, y) = gravity_space(Space::WorldTurn, upright, JsmGyro { x: 10.0, y: 0.0, z: 0.0 });
    assert!(y.abs() > 1.0, "held flat a pitch reads as pitch: {y}");

    // Gravity along the pad's right: neither flat nor upright.
    let sideways = gravity_for("right grip down");
    let (_, y) = gravity_space(Space::WorldTurn, sideways, JsmGyro { x: 10.0, y: 0.0, z: 0.0 });
    assert!(y.abs() < 0.01, "on its side it stops guessing: {y}");
}

/// The touchpad grid: which cell a finger is in, by name.
#[test]
fn the_touchpad_grid_names_the_cell_a_finger_is_in() {
    let mut t = super::touch::Touch::default();
    let s = super::touch::Settings { grid: (2, 2), ..Default::default() };
    let cfg = super::analog::StickCfg::default();
    let at = |x: f32, y: f32| super::touch::Finger { active: true, x, y };
    let off = super::touch::Finger::default();

    // Top-left quarter is cell 1, then across, then down: 1 2 / 3 4.
    let out = t.tick(&s, [at(-0.5, -0.5), off], cfg);
    assert_eq!(out.cells[0], Some(1), "top left is T1");
    let out = t.tick(&s, [at(0.5, -0.5), off], cfg);
    assert_eq!(out.cells[0], Some(2), "top right is T2");
    let out = t.tick(&s, [at(-0.5, 0.5), off], cfg);
    assert_eq!(out.cells[0], Some(3), "bottom left is T3");
    let out = t.tick(&s, [at(0.5, 0.5), off], cfg);
    assert_eq!(out.cells[0], Some(4), "bottom right is T4");

    // A finger lifted is in no cell at all.
    let out = t.tick(&s, [off, off], cfg);
    assert_eq!(out.cells[0], None);

    // The far corners stay in range rather than falling off the end of T1..T25.
    let out = t.tick(&s, [at(1.0, 1.0), off], cfg);
    assert_eq!(out.cells[0], Some(4), "the very corner is still the last cell");
}

/// A touch stick is *relative*: it measures how far the finger has been dragged
/// from wherever it landed, not where on the pad it is.
#[test]
fn a_touch_stick_measures_the_drag_not_the_place() {
    let mut t = super::touch::Touch::default();
    // A radius of 192 points is a tenth of the nominal width, so a tenth of the
    // touchpad is full deflection — easy numbers to check.
    let s = super::touch::Settings { stick_radius: 192.0, ..Default::default() };
    let cfg = super::analog::StickCfg::default();
    let at = |x: f32, y: f32| super::touch::Finger { active: true, x, y };

    // Landing far to the right is not a push: the stick starts where the finger
    // lands.
    let out = t.tick(&s, [at(0.8, 0.0), super::touch::Finger::default()], cfg);
    assert_eq!(out.sticks[0], (0.0, 0.0), "the tick a finger lands is not a drag");

    // Dragging a fifth of the way right is a full push at this radius.
    let out = t.tick(&s, [at(1.0, 0.0), super::touch::Finger::default()], cfg);
    assert!(out.sticks[0].0 > 0.9, "dragging right pushes right: {:?}", out.sticks[0]);

    // Lifting re-centres it, so the next touch starts fresh.
    let out = t.tick(&s, [super::touch::Finger::default(); 2], cfg);
    let _ = out;
    let out = t.tick(&s, [at(-0.9, 0.0), super::touch::Finger::default()], cfg);
    assert_eq!(out.sticks[0], (0.0, 0.0), "a new touch starts from centre");
}

/// Dragging up the touchpad pushes the touch stick up, even though the touchpad's
/// y counts down and a stick's counts up.
#[test]
fn a_touch_stick_agrees_with_a_stick_about_which_way_is_up() {
    let mut t = super::touch::Touch::default();
    let s = super::touch::Settings { stick_radius: 108.0, ..Default::default() };
    let cfg = super::analog::StickCfg::default();
    let at = |y: f32| super::touch::Finger { active: true, x: 0.0, y };
    let off = super::touch::Finger::default();

    t.tick(&s, [at(0.5), off], cfg);
    // Towards the top of the touchpad is a smaller y.
    let out = t.tick(&s, [at(0.0), off], cfg);
    assert!(out.sticks[0].1 > 0.9, "dragging up pushes the stick up: {:?}", out.sticks[0]);
}

/// `TOUCHPAD_MODE = MOUSE` drags the pointer, and ignores a second finger rather
/// than doubling the speed.
#[test]
fn the_touchpad_can_drag_the_mouse() {
    let mut t = super::touch::Touch::default();
    let s = super::touch::Settings {
        mode: super::touch::Mode::Mouse,
        sens: (1.0, 1.0),
        ..Default::default()
    };
    let cfg = super::analog::StickCfg::default();
    let at = |x: f32, y: f32| super::touch::Finger { active: true, x, y };
    let off = super::touch::Finger::default();

    t.tick(&s, [at(0.0, 0.0), off], cfg);
    let out = t.tick(&s, [at(0.1, 0.0), off], cfg);
    assert!(out.mouse.x > 1.0, "dragging right moves the pointer right: {:?}", out.mouse);
    // Our bus counts mouse y up and the touchpad counts down.
    t.tick(&s, [at(0.1, 0.0), off], cfg);
    let out = t.tick(&s, [at(0.1, 0.2), off], cfg);
    assert!(out.mouse.y < -1.0, "dragging down moves the pointer down: {:?}", out.mouse);

    // In grid mode the pointer stays put.
    let mut t = super::touch::Touch::default();
    let grid = super::touch::Settings::default();
    t.tick(&grid, [at(0.0, 0.0), off], cfg);
    let out = t.tick(&grid, [at(0.5, 0.0), off], cfg);
    assert_eq!(out.mouse, glam::Vec2::ZERO, "grid mode is not a mouse");
}

/// The touchpad is a dual-stage trigger: a finger is the soft pull, a click the
/// full one. `NO_SKIP` (JSM's default here) fires the soft stage on touch and adds
/// the full one on click.
#[test]
fn a_finger_and_a_click_are_the_touchpads_two_stages() {
    let mut s = Stage::new("TOUCHPAD_DUAL_STAGE_MODE = NO_SKIP");
    let step = |s: &mut Stage, touching: bool, click: bool| {
        s.pad.touching = touching;
        s.pad.touch_click = click;
        s.a.tick(&s.s, DT, &s.pad);
        (s.a.down(Btn::Touch), s.a.down(Btn::Capture))
    };
    assert_eq!(step(&mut s, false, false), (false, false), "nothing on the pad");
    assert_eq!(step(&mut s, true, false), (true, false), "a finger is the soft pull");
    assert_eq!(step(&mut s, true, true), (true, true), "a click adds the full one");
    // Lifting the finger and releasing the click in one tick releases the full
    // stage first and the soft one a tick later. That is the dual-stage machine's
    // own shape — JSM's `DelayFullPress` keeps the soft press while the full one
    // goes — and it is shared with every real trigger, so it is left alone here
    // rather than special-cased for the touchpad.
    assert_eq!(step(&mut s, false, false), (true, false), "the click goes first");
    assert_eq!(step(&mut s, false, false), (false, false), "then the finger");
}

/// `MUST_SKIP` on the touchpad tells a tap from a press, which only works because
/// a finger reads as 0.99 rather than as a full pull.
#[test]
fn the_touchpad_can_tell_a_tap_from_a_click() {
    let mut s = Stage::new("TOUCHPAD_DUAL_STAGE_MODE = MUST_SKIP");
    s.pad.touching = true;
    s.a.tick(&s.s, DT, &s.pad);
    assert!(!s.a.down(Btn::Touch), "MUST_SKIP waits to see if a click follows");
    s.pad.touch_click = true;
    s.a.tick(&s.s, DT, &s.pad);
    assert!(s.a.down(Btn::Capture), "a quick click fires the full stage");
    assert!(!s.a.down(Btn::Touch), "and skips the soft one");
}

/// Every one of phase 6's settings is read, and a bad value is called out.
#[test]
fn every_motion_and_touch_setting_is_read() {
    let c = compile(
        "GYRO_SPACE = WORLD_LEAN\n\
         LEAN_THRESHOLD = 25\n\
         MOTION_STICK_MODE = NO_MOUSE\n\
         MOTION_RING_MODE = INNER\n\
         MOTION_DEADZONE_INNER = 18\n\
         MOTION_DEADZONE_OUTER = 90\n\
         MOTION_STICK_AXIS = INVERTED STANDARD\n\
         TOUCHPAD_MODE = MOUSE\n\
         GRID_SIZE = 5 5\n\
         TOUCHPAD_SENS = 2 3\n\
         TOUCHPAD_DUAL_STAGE_MODE = MAY_SKIP\n\
         TOUCH_STICK_MODE = AIM\n\
         TOUCH_RING_MODE = INNER\n\
         TOUCH_STICK_RADIUS = 200\n\
         TOUCH_DEADZONE_INNER = 0.4\n\
         TOUCH_STICK_AXIS = STANDARD INVERTED",
    );
    assert!(errors(&c).is_empty(), "all of these are live: {:?}", errors(&c));
    assert_eq!(c.motion.space, super::motion::Space::WorldLean);
    assert_eq!(c.motion.lean_threshold, 25.0);
    assert_eq!(c.settings.motion.ring, RingMode::Inner);
    assert!((c.settings.motion.inner_dz - 0.1).abs() < 1e-6, "18 of 180 degrees");
    assert!((c.settings.motion.outer_dz - 0.5).abs() < 1e-6, "90 of 180 degrees");
    assert!(c.settings.motion.invert_x && !c.settings.motion.invert_y);
    assert_eq!(c.touch.mode, super::touch::Mode::Mouse);
    assert_eq!(c.touch.grid, (5, 5));
    assert_eq!(c.touch.sens, (2.0, 3.0));
    assert_eq!(c.settings.touchpad_dual_stage, super::analog::TriggerMode::MaySkip);
    assert_eq!(c.settings.touch.mode, super::analog::StickMode::Aim);
    assert_eq!(c.touch.stick_radius, 200.0);
    assert_eq!(c.settings.touch.inner_dz, 0.4);
    assert!(!c.settings.touch.invert_x && c.settings.touch.invert_y);

    for bad in [
        "GYRO_SPACE = SIDEWAYS",
        "LEAN_THRESHOLD = 200",
        "MOTION_DEADZONE_INNER = 400",
        "TOUCHPAD_MODE = WOBBLE",
        "GRID_SIZE = 6 6",
        "TOUCH_STICK_RADIUS = 0",
        "TOUCH_DEADZONE_INNER = 3",
    ] {
        assert!(
            matches!(one(bad).status, LineStatus::Error(_)),
            "{bad} should be an error"
        );
    }
}

/// A grid too big to name is refused with the reason, not clamped in silence —
/// the buttons stop at `T25`, so a 6x6 grid has cells nothing can be bound to.
#[test]
fn a_grid_bigger_than_the_buttons_says_so() {
    match one("GRID_SIZE = 6 6").status {
        LineStatus::Error(why) => {
            assert!(why.contains("T25"), "it names the limit: {why}");
        }
        other => panic!("a 36-cell grid should be an error, got {other:?}"),
    }
}

/// `LEFT_STEER_X` is refused on a thumbstick and accepted on the motion stick —
/// the same name, two answers, which is exactly what JSM does.
#[test]
fn steering_belongs_to_the_motion_stick_alone() {
    assert!(matches!(
        one("LEFT_STICK_MODE = LEFT_STEER_X").status,
        LineStatus::Error(_)
    ));
    let ok = one("MOTION_STICK_MODE = LEFT_STEER_X");
    assert_eq!(ok.status, LineStatus::Ok, "the motion stick can steer");
    assert!(
        ok.notes.iter().any(|n| n.contains("virtual pad wired downstream")),
        "and says it needs a pad: {:?}",
        ok.notes
    );
}

/// Leaning the pad steers a virtual stick, with `MOTION_DEADZONE_OUTER` deciding
/// how far you have to lean for full lock.
#[test]
fn leaning_the_pad_steers_a_virtual_stick() {
    let reach = |pose: &str| {
        super::motion::steer(
            gravity_for(pose),
            super::analog::Orientation::default(),
            15.0,
            135.0,
            1.0,
        )
    };
    let (_, flat) = reach("flat").expect("gravity is known");
    assert!(flat < 0.01, "held flat it steers straight: {flat}");

    let (sign, tilted) = reach("right grip down").expect("gravity is known");
    assert!(sign > 0.0, "leaning right steers right");
    // 90 degrees of lean, 15 in and 135 out, is (90-15)/(180-135-15) = 2.5 → full.
    assert!(tilted > 0.99, "a quarter turn is already full lock: {tilted}");

    // No accelerometer, no steering.
    assert!(super::motion::steer(
        super::motion::Gravity::default(),
        super::analog::Orientation::default(),
        15.0,
        135.0,
        1.0
    )
    .is_none());
}

/// The motion stick's directions are ordinary stick directions, so `MUP` behaves
/// the way `LUP` does — running all five of JSM's sticks through one routine is
/// what buys that.
#[test]
fn the_motion_sticks_directions_are_buttons_like_any_other() {
    let out = run_node_with(
        "MOTION_STICK_MODE = NO_MOUSE\nMRIGHT = E",
        false,
        &[
            // Right grip down: gravity along the pad's right, so the motion stick
            // pushes right.
            ("accel_x", Signal::Float(0.0)),
            ("accel_y", Signal::Float(1.0)),
            ("accel_z", Signal::Float(0.0)),
        ],
        4,
    );
    assert!(
        out.get("key_e").map(|s| s.as_bool()).unwrap_or(false),
        "tilting the pad right presses the binding on MRIGHT: {out:?}"
    );
}

/// A touchpad grid cell is a button, and it only fires while a finger is in it.
#[test]
fn a_grid_cell_is_a_button() {
    let held = run_node_with(
        "GRID_SIZE = 2 1\nT1 = E\nT2 = F",
        false,
        &[
            ("touch1_active", Signal::Bool(true)),
            ("touch1_x", Signal::Float(-0.5)),
            ("touch1_y", Signal::Float(0.0)),
        ],
        3,
    );
    assert!(
        held.get("key_e").map(|s| s.as_bool()).unwrap_or(false),
        "a finger on the left half presses T1: {held:?}"
    );
    assert!(
        !held.get("key_f").map(|s| s.as_bool()).unwrap_or(false),
        "and not T2"
    );

    let lifted = run_node_with("GRID_SIZE = 2 1\nT1 = E\nT2 = F", false, &[], 3);
    assert!(
        !lifted.get("key_e").map(|s| s.as_bool()).unwrap_or(false),
        "no finger, no press: {lifted:?}"
    );
}

/// A config that reads the touchpad takes it over, so a Touch Zones module
/// downstream doesn't act on the same finger — except in `PS_TOUCHPAD` mode, which
/// exists to pass it on.
#[test]
fn reading_the_touchpad_claims_it_unless_the_mode_is_to_pass_it_on() {
    let claimed = run_node_with(
        "GRID_SIZE = 2 1\nT1 = E",
        false,
        &[("touch1_active", Signal::Bool(true)), ("touch1_x", Signal::Float(-0.5))],
        2,
    );
    assert_eq!(
        claimed.get("touch1_active").map(|s| s.as_bool()),
        Some(false),
        "the finger is consumed: {claimed:?}"
    );

    let passed = run_node_with(
        "TOUCHPAD_MODE = PS_TOUCHPAD",
        false,
        &[("touch1_active", Signal::Bool(true)), ("touch1_x", Signal::Float(-0.5))],
        2,
    );
    assert_eq!(
        passed.get("touch1_active").map(|s| s.as_bool()),
        Some(true),
        "PS_TOUCHPAD hands it on: {passed:?}"
    );
}

/// Gravity from an arbitrary direction of "up", for poses that have no name.
fn gravity_from(up: glam::Vec3) -> super::motion::Gravity {
    let mut m = super::motion::Motion::default();
    let a = up.normalize();
    let mut g = super::motion::Gravity::default();
    for _ in 0..40 {
        g = m.tick(Some(a), DT);
    }
    g
}

/// The world spaces fade out *gradually* as the pad is rolled onto its side —
/// partial trust, not just on or off. The cliff-edge version of this test passes
/// even with the fade removed, because a pad exactly on its side has no usable
/// pitch axis at all and reads zero either way.
#[test]
fn a_world_space_fades_out_as_the_pad_rolls_onto_its_side() {
    use super::motion::{gravity_space, JsmGyro, Space};
    let pitch = JsmGyro { x: 10.0, y: 0.0, z: 0.0 };

    // Held flat: fully trusted, the pitch comes through whole.
    let (_, square) = gravity_space(Space::WorldTurn, gravity_for("flat"), pitch);
    assert!(square.abs() > 9.0, "held flat a pitch comes through whole: {square}");

    // Now a pose just inside the give-up band. Gravity is almost exactly along the
    // pad's own pitch axis, which is the case the fade exists for: the world pitch
    // axis is still computable — so the projection alone would let a reduced
    // signal through — but it is meaningless, and the fade must silence it
    // outright rather than merely quieten it. This is the assertion that separates
    // "damped by the projection" from "faded out on purpose".
    let on_its_side = gravity_from(glam::Vec3::new(0.125, 0.992, 0.0));
    let (_, faded) = gravity_space(Space::WorldTurn, on_its_side, pitch);
    assert!(
        faded.abs() < 0.001,
        "at the edge of the band it goes quiet altogether, not just quieter: {faded}"
    );
}

/// The player spaces keep pitch as the pad's own, and keep it the right way up.
#[test]
fn the_player_spaces_keep_pitch_the_right_way_up() {
    use super::motion::{gravity_space, JsmGyro, Space};
    // The same pitch, through `LOCAL` and through `PLAYER_TURN`, must agree about
    // which way the camera goes — the player spaces re-reference the horizontal
    // half and leave pitch alone, so a config swapping between them shouldn't
    // suddenly aim upside down.
    let (_, player) = gravity_space(
        Space::PlayerTurn,
        gravity_for("flat"),
        JsmGyro { x: 10.0, y: 0.0, z: 0.0 },
    );
    // `LOCAL` with JSM's defaults does `gyroY += inGyroX` against a screen whose y
    // counts down, which phase 3 pins by the same reasoning: tilting up aims up.
    let mut a = Aiming::new(&format!("{AIM_CFG}\nGYRO_SMOOTH_TIME = 0"));
    a.gyro = Gyro { roll: 0.0, pitch: 10.0, yaw: 0.0 };
    let local = a.ticks(3);
    assert!(
        player.signum() == -local.y.signum(),
        "player pitch and local pitch agree about up (bus y counts up, JSM's down): \
         player {player}, local {}",
        local.y
    );
}

/// Leaning past vertical keeps steering the same way instead of unwinding, which
/// is what the fold-back past 90 degrees is for.
#[test]
fn steering_keeps_going_past_vertical() {
    let reach = |up: glam::Vec3| {
        super::motion::steer(
            gravity_from(up),
            super::analog::Orientation::default(),
            15.0,
            60.0,
            1.0,
        )
        .expect("gravity is known")
    };
    // Rolled 45 degrees right: partway.
    let (sign_45, at_45) = reach(glam::Vec3::new(0.0, 0.7071, 0.7071));
    // Rolled 135 degrees right — past vertical, still going the same way, and
    // further over than at 45.
    let (sign_135, at_135) = reach(glam::Vec3::new(0.0, 0.7071, -0.7071));
    assert!(sign_45 > 0.0 && sign_135 > 0.0, "both lean right");
    assert!(
        at_135 > at_45,
        "past vertical it keeps steering further, not back: {at_45} then {at_135}"
    );
}

/// Which cell owns the line between two of them. JSM rounds up and subtracts one,
/// so the boundary belongs to the cell before it — worth pinning, because the
/// obvious `floor` gives the other answer and only differs exactly here.
#[test]
fn a_finger_on_a_grid_line_belongs_to_the_cell_before_it() {
    let mut t = super::touch::Touch::default();
    let s = super::touch::Settings { grid: (2, 1), ..Default::default() };
    let cfg = super::analog::StickCfg::default();
    let at = |x: f32| super::touch::Finger { active: true, x, y: 0.0 };

    let out = t.tick(&s, [at(0.0), super::touch::Finger::default()], cfg);
    assert_eq!(
        out.cells[0],
        Some(1),
        "dead centre of a two-column grid is the left cell"
    );
}

/// A finger alone is never a full pull, however the dual-stage mode is set. That
/// is what the 0.99 is for: without it a resting finger would fire the click.
#[test]
fn a_finger_alone_never_counts_as_a_click() {
    for mode in ["NO_SKIP", "NO_SKIP_EXCLUSIVE", "MAY_SKIP", "MUST_SKIP"] {
        let mut s = Stage::new(&format!("TOUCHPAD_DUAL_STAGE_MODE = {mode}"));
        s.pad.touching = true;
        for tick in 0..40 {
            s.a.tick(&s.s, DT, &s.pad);
            assert!(
                !s.a.down(Btn::Capture),
                "{mode}: a finger with no click never presses CAPTURE (tick {tick})"
            );
        }
    }
}

// ── phase 7: feedback ────────────────────────────────────────────────────────

/// What the feedback side wants written, as a lookup.
fn fb_pins(text: &str, rumble: Option<(f32, f32)>) -> HashMap<String, f32> {
    let c = compile(text);
    assert!(errors(&c).is_empty(), "config should compile: {:?}", errors(&c));
    super::feedback::pins(&c.fb, rumble)
        .into_iter()
        .map(|(p, v)| (p.to_string(), v))
        .collect()
}

/// The light bar is set from a name or from hex, and reaches the pad as three
/// channels.
#[test]
fn the_light_bar_takes_a_name_or_a_hex_value() {
    let red = fb_pins("LIGHT_BAR = RED", None);
    assert_eq!(red.get("lightbar_r"), Some(&1.0));
    assert_eq!(red.get("lightbar_g"), Some(&0.0));
    assert_eq!(red.get("lightbar_b"), Some(&0.0));

    // JSM writes hex as `xRRGGBB`, and a bare one is taken too.
    for spelling in ["LIGHT_BAR = x0000FF", "LIGHT_BAR = 0000FF"] {
        let blue = fb_pins(spelling, None);
        assert_eq!(blue.get("lightbar_b"), Some(&1.0), "{spelling}");
        assert_eq!(blue.get("lightbar_r"), Some(&0.0), "{spelling}");
    }

    // And JSM's own default is white.
    let plain = fb_pins("", None);
    assert_eq!(plain.get("lightbar_r"), Some(&1.0));
    assert_eq!(plain.get("lightbar_g"), Some(&1.0));
    assert_eq!(plain.get("lightbar_b"), Some(&1.0));

    assert!(matches!(one("LIGHT_BAR = TARTAN").status, LineStatus::Error(_)));

    // `#RRGGBB` can never work: `#` starts a comment, so the value is gone before
    // the colour is read. The error has to point that out, or it reads as a parser
    // bug rather than as the config grammar.
    match one("LIGHT_BAR = #0000FF").status {
        LineStatus::Error(why) => assert!(
            why.contains("starts a comment"),
            "it explains why a hash cannot work: {why}"
        ),
        other => panic!("a commented-out value should be an error, got {other:?}"),
    }
}

/// `RUMBLE = ON` (the default) leaves the game's rumble alone; `OFF` takes the
/// group over and holds it at zero, which is the only way to stop it.
#[test]
fn rumble_off_takes_the_rumble_over_and_on_leaves_it_be() {
    let on = fb_pins("RUMBLE = ON", None);
    assert!(
        !on.contains_key("rumble_strong"),
        "with rumble on the group is left for the game: {on:?}"
    );

    let off = fb_pins("RUMBLE = OFF", None);
    assert_eq!(
        off.get("rumble_strong"),
        Some(&0.0),
        "with it off the group is claimed and held at zero: {off:?}"
    );
    assert_eq!(off.get("rumble_weak"), Some(&0.0));

    // And the line says what it costs.
    let line = one("RUMBLE = OFF");
    assert!(
        line.notes.iter().any(|n| n.contains("game's rumble stops")),
        "it says the game's rumble stops: {:?}",
        line.notes
    );
}

/// A binding that rumbles takes the group over for as long as it is rumbling, even
/// with `RUMBLE = ON` — and hands it straight back, or the pad would buzz for ever.
#[test]
fn a_rumble_binding_claims_the_group_only_while_it_rumbles() {
    let quiet = fb_pins("RUMBLE = ON", None);
    assert!(!quiet.contains_key("rumble_strong"), "nothing asking, nothing claimed");

    let buzzing = fb_pins("RUMBLE = ON", Some((1.0, 0.5)));
    assert_eq!(buzzing.get("rumble_strong"), Some(&1.0));
    assert_eq!(buzzing.get("rumble_weak"), Some(&0.5));
}

/// JSM's own rumble amplitudes, as its names and its hex form give them.
#[test]
fn a_rumble_binding_carries_jsms_own_amplitudes() {
    // `BIG_RUMBLE` is RFFFF, `SMALL_RUMBLE` is R0080 — high byte strong, low weak.
    let big = steps("S = BIG_RUMBLE");
    assert_eq!(big[0].out, super::names::Out::Rumble { strong: 1.0, weak: 1.0 });
    let small = steps("S = SMALL_RUMBLE");
    assert_eq!(
        small[0].out,
        super::names::Out::Rumble { strong: 0.0, weak: 128.0 / 255.0 }
    );
    let hex = steps("S = R8040");
    assert_eq!(
        hex[0].out,
        super::names::Out::Rumble { strong: 128.0 / 255.0, weak: 64.0 / 255.0 }
    );
}

/// The four adaptive-trigger effects our bus can carry, with JSM's own numbers
/// landing on the right pins at the right scale.
#[test]
fn the_trigger_effects_our_bus_carries_land_on_their_pins() {
    // `RESISTANCE start force` — zones are 0-9, force 0-7.
    let r = fb_pins("LEFT_TRIGGER_EFFECT = RESISTANCE 3 7", None);
    assert!((r["trigger_l_mode"] - 1.0 / 3.0).abs() < 1e-6, "feedback mode");
    assert!((r["trigger_l_start"] - 3.0 / 9.0).abs() < 1e-6, "zone 3 of 9");
    assert_eq!(r["trigger_l_strength"], 1.0, "force 7 of 7 is full");

    // `SEMI_AUTOMATIC start end force` — the one that uses both zones.
    let s = fb_pins("RIGHT_TRIGGER_EFFECT = SEMI_AUTOMATIC 2 6 4", None);
    assert!((s["trigger_r_mode"] - 2.0 / 3.0).abs() < 1e-6, "weapon mode");
    assert!((s["trigger_r_start"] - 2.0 / 9.0).abs() < 1e-6);
    assert!((s["trigger_r_end"] - 6.0 / 9.0).abs() < 1e-6);

    // `AUTOMATIC start force frequency` — frequency is 0-255.
    let a = fb_pins("LEFT_TRIGGER_EFFECT = AUTOMATIC 1 5 255", None);
    assert_eq!(a["trigger_l_mode"], 1.0, "vibration mode");
    assert_eq!(a["trigger_l_freq"], 1.0, "frequency 255 of 255");

    // `OFF` writes every pin of the group, so a previous effect can't shape it.
    let off = fb_pins("LEFT_TRIGGER_EFFECT = OFF", None);
    for pin in ["trigger_l_mode", "trigger_l_start", "trigger_l_end", "trigger_l_strength", "trigger_l_freq"] {
        assert_eq!(off.get(pin), Some(&0.0), "{pin} is zeroed");
    }

    // A missing parameter is an error naming what was wanted.
    match one("LEFT_TRIGGER_EFFECT = RESISTANCE 3").status {
        LineStatus::Error(why) => assert!(why.contains("force"), "{why}"),
        other => panic!("a short RESISTANCE should be an error, got {other:?}"),
    }
}

/// `ON` — JSM's default — means "no effect of my own", so the group is left
/// entirely alone and the game keeps whatever it set.
#[test]
fn an_adaptive_trigger_left_on_is_not_touched() {
    let auto = fb_pins("LEFT_TRIGGER_EFFECT = ON", None);
    assert!(
        !auto.keys().any(|k| k.starts_with("trigger_l_")),
        "nothing of the left trigger is claimed: {auto:?}"
    );
    // And the default is the same.
    let plain = fb_pins("", None);
    assert!(!plain.keys().any(|k| k.starts_with("trigger_")));
}

/// `ADAPTIVE_TRIGGER = OFF` overrules whatever effects are set — one switch to
/// stop the triggers fighting you, which is how JSM uses it.
#[test]
fn adaptive_trigger_off_overrules_the_effects() {
    let out = fb_pins(
        "LEFT_TRIGGER_EFFECT = RESISTANCE 3 7\nADAPTIVE_TRIGGER = OFF",
        None,
    );
    assert_eq!(out["trigger_l_mode"], 0.0, "the effect is switched off: {out:?}");
    assert_eq!(out["trigger_l_strength"], 0.0);
}

/// The three effects the bus has nowhere to put say so, name the nearest thing
/// that works, and leave the trigger as the game set it — rather than quietly
/// giving the player something that feels wrong.
#[test]
fn the_effects_our_bus_cannot_carry_say_so_and_suggest_one_that_works() {
    for (mode, expect) in [
        ("BOW 2 5 4 6", "SEMI_AUTOMATIC"),
        ("GALLOPING 1 8 3 5 100", "AUTOMATIC"),
        ("MACHINE 1 9 3 4 40 60", "AUTOMATIC"),
    ] {
        let line = one(&format!("LEFT_TRIGGER_EFFECT = {mode}"));
        assert_eq!(line.status, LineStatus::Ok, "{mode} is understood, not an error");
        let note = line.notes.join(" ");
        assert!(note.contains("four trigger effects"), "{mode}: says why: {note}");
        assert!(note.contains(expect), "{mode}: names the nearest: {note}");
    }
    // And the trigger is left alone rather than set to something arbitrary.
    let out = fb_pins("LEFT_TRIGGER_EFFECT = BOW 2 5 4 6", None);
    assert!(
        !out.keys().any(|k| k.starts_with("trigger_l_")),
        "the trigger is left as the game set it: {out:?}"
    );
}

/// Trigger travel calibration is the device card's, not the config's.
#[test]
fn trigger_travel_calibration_is_the_device_cards() {
    for line in [
        "LEFT_TRIGGER_OFFSET = 20",
        "LEFT_TRIGGER_RANGE = 150",
        "RIGHT_TRIGGER_OFFSET = 20",
        "RIGHT_TRIGGER_RANGE = 150",
    ] {
        match one(line).status {
            LineStatus::Ignored(why) => {
                assert!(why.contains("device card"), "{line}: {why}");
            }
            other => panic!("{line} should be ignored with a reason, got {other:?}"),
        }
    }
}

/// Feedback reaches the pad on the override layer, keyed to the pad upstream —
/// which is what lets it replace the game's rather than add to it.
#[test]
fn feedback_reaches_the_pad_it_came_from() {
    let _guard = alone();
    let uid = 4242;
    let mut snap = jsm_snap(uid, "LIGHT_BAR = RED\nRUMBLE = OFF", false);
    snap.params
        .insert("_jsm_dest_dev".into(), serde_json::Value::String(PAD.into()));
    let mut dev: HashMap<(String, String), Signal> = HashMap::new();
    let mut collector: HashMap<(String, String), Signal> = HashMap::new();
    let mut state: HashMap<usize, NodeState> = HashMap::new();
    for _ in 0..2 {
        super::eval::jsm_publish(&snap, uid, &dev, &mut collector, &mut state, DT);
    }
    let key = format!("feedback_override:{PAD}");
    assert_eq!(
        collector.get(&(key.clone(), "lightbar_r".to_string())).map(|s| s.as_float()),
        Some(1.0),
        "the light bar reaches the pad it came from"
    );
    assert_eq!(
        collector.get(&(key, "rumble_strong".to_string())).map(|s| s.as_float()),
        Some(0.0),
        "and rumble is held at zero rather than left to the game"
    );
    let _ = &mut dev;
}

/// With no pad resolved upstream there is nowhere for feedback to go, and nothing
/// is published — rather than a stray `feedback_override:` nobody drains.
#[test]
fn feedback_with_no_pad_upstream_goes_nowhere() {
    let _guard = alone();
    let uid = 4243;
    let snap = jsm_snap(uid, "LIGHT_BAR = RED", false);
    let dev: HashMap<(String, String), Signal> = HashMap::new();
    let mut collector: HashMap<(String, String), Signal> = HashMap::new();
    let mut state: HashMap<usize, NodeState> = HashMap::new();
    super::eval::jsm_publish(&snap, uid, &dev, &mut collector, &mut state, DT);
    assert!(
        !collector.keys().any(|(k, _)| k.starts_with("feedback_override:")),
        "nothing published with nowhere to send it"
    );
}

/// Every one of phase 7's settings is read.
#[test]
fn every_feedback_setting_is_read() {
    let c = compile(
        "RUMBLE = OFF\n\
         LIGHT_BAR = x123456\n\
         ADAPTIVE_TRIGGER = ON\n\
         LEFT_TRIGGER_EFFECT = RESISTANCE 4 5\n\
         RIGHT_TRIGGER_EFFECT = AUTOMATIC 2 3 120",
    );
    assert!(errors(&c).is_empty(), "all live: {:?}", errors(&c));
    assert!(!c.fb.rumble);
    assert_eq!(c.fb.light_bar, (0x12, 0x34, 0x56));
    assert!(c.fb.adaptive);
    assert_eq!(
        c.fb.trigger[0],
        super::feedback::Effect::Resistance { start: 4, force: 5 }
    );
    assert_eq!(
        c.fb.trigger[1],
        super::feedback::Effect::Automatic { start: 2, force: 3, frequency: 120 }
    );

    for bad in [
        "RUMBLE = MAYBE",
        "ADAPTIVE_TRIGGER = SOMETIMES",
        "LIGHT_BAR = xZZZZZZ",
        "LEFT_TRIGGER_EFFECT = SQUEEZE",
    ] {
        assert!(matches!(one(bad).status, LineStatus::Error(_)), "{bad}");
    }
}

/// A feedback setting can be chorded like any other, which is what phase 4 bought.
#[test]
fn feedback_settings_can_be_chorded() {
    let c = compile("ZL,LIGHT_BAR = RED");
    assert!(errors(&c).is_empty(), "{:?}", errors(&c));
    let held = super::parse::resolve(&c, &[Btn::Zl]);
    assert_eq!(held.fb.light_bar, (0xFF, 0x00, 0x00), "held, it is red");
    let idle = super::parse::resolve(&c, &[]);
    assert_eq!(idle.fb.light_bar, (0xFF, 0xFF, 0xFF), "let go, back to white");
}

/// Every colour name JSM accepts, and what it is. A config written against JSM's
/// names has to light the same colour here, and "it compiled" is not that.
#[test]
fn every_colour_name_is_the_colour_it_says() {
    for (name, want) in [
        ("BLACK", (0.0, 0.0, 0.0)),
        ("WHITE", (1.0, 1.0, 1.0)),
        ("RED", (1.0, 0.0, 0.0)),
        ("GREEN", (0.0, 1.0, 0.0)),
        ("BLUE", (0.0, 0.0, 1.0)),
        ("YELLOW", (1.0, 1.0, 0.0)),
        ("CYAN", (0.0, 1.0, 1.0)),
        ("MAGENTA", (1.0, 0.0, 1.0)),
        ("PINK", (1.0, 0.0, 1.0)),
        ("ORANGE", (1.0, 128.0 / 255.0, 0.0)),
        ("PURPLE", (128.0 / 255.0, 0.0, 1.0)),
        ("GREY", (128.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0)),
        ("GRAY", (128.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0)),
    ] {
        let out = fb_pins(&format!("LIGHT_BAR = {name}"), None);
        let got = (out["lightbar_r"], out["lightbar_g"], out["lightbar_b"]);
        assert!(
            (got.0 - want.0).abs() < 1e-6
                && (got.1 - want.1).abs() < 1e-6
                && (got.2 - want.2).abs() < 1e-6,
            "{name} should be {want:?}, got {got:?}"
        );
    }
}

/// A binding that rumbles reaches the pad, end to end — the binding fires, the
/// amplitude lands on the override layer keyed to the pad upstream.
#[test]
fn a_rumble_binding_reaches_the_pad() {
    let _guard = alone();
    let uid = 4244;
    let mut snap = jsm_snap(uid, "S = BIG_RUMBLE", false);
    snap.params
        .insert("_jsm_dest_dev".into(), serde_json::Value::String(PAD.into()));
    let mut dev: HashMap<(String, String), Signal> = HashMap::new();
    dev.insert((PAD.to_string(), "btn_south".to_string()), Signal::Bool(true));
    let mut collector: HashMap<(String, String), Signal> = HashMap::new();
    let mut state: HashMap<usize, NodeState> = HashMap::new();
    let key = format!("feedback_override:{PAD}");
    let mut strongest = 0.0f32;
    for _ in 0..4 {
        collector.clear();
        super::eval::jsm_publish(&snap, uid, &dev, &mut collector, &mut state, DT);
        if let Some(v) = collector.get(&(key.clone(), "rumble_strong".to_string())) {
            strongest = strongest.max(v.as_float());
        }
    }
    assert!(
        strongest > 0.9,
        "pressing the button rumbles the pad it came from, got {strongest}"
    );

    // And with nothing pressed, the rumble group is left to the game — `RUMBLE` is
    // on by default, so nothing is claimed.
    let dev: HashMap<(String, String), Signal> = HashMap::new();
    let mut collector: HashMap<(String, String), Signal> = HashMap::new();
    let mut state: HashMap<usize, NodeState> = HashMap::new();
    for _ in 0..4 {
        collector.clear();
        super::eval::jsm_publish(&snap, uid, &dev, &mut collector, &mut state, DT);
    }
    assert!(
        !collector.contains_key(&(key, "rumble_strong".to_string())),
        "with nothing rumbling the game keeps the group"
    );
}

// ── phase 8: action layers ───────────────────────────────────────────────────

/// A node holding several tabs, with the first one open.
fn tabbed_snap(uid: usize, tabs: &[(&str, &str)]) -> NodeSnap {
    let mut snap = jsm_snap(uid, tabs[0].1, false);
    let list: Vec<serde_json::Value> = tabs
        .iter()
        .map(|(n, t)| {
            serde_json::json!({ "name": n.to_string(), "text": t.to_string() })
        })
        .collect();
    snap.params.insert("jsm_tabs".into(), serde_json::Value::Array(list));
    snap.params.insert("jsm_active_tab".into(), serde_json::json!(0));
    snap
}

/// Run a tabbed node, pressing whatever is named, and return what it drove.
fn run_tabs(
    tabs: &[(&str, &str)],
    steps: &[(&[&str], usize)],
) -> Vec<std::collections::HashSet<String>> {
    let _guard = alone();
    let uid = 5150;
    let snap = tabbed_snap(uid, tabs);
    let mut collector: HashMap<(String, String), Signal> = HashMap::new();
    let mut state: HashMap<usize, NodeState> = HashMap::new();
    let mut seen = Vec::new();
    for (held, ticks) in steps {
        let mut dev: HashMap<(String, String), Signal> = HashMap::new();
        for pin in *held {
            dev.insert((PAD.to_string(), pin.to_string()), Signal::Bool(true));
        }
        for _ in 0..*ticks {
            collector.clear();
            super::eval::jsm_publish(&snap, uid, &dev, &mut collector, &mut state, DT);
        }
        seen.push(
            collector
                .iter()
                .filter(|((_, p), v)| p.starts_with("key_") && v.as_bool())
                .map(|((_, p), _)| p.clone())
                .collect(),
        );
    }
    seen
}

/// A tab is named by its file name alone, whatever path a JSM config used.
#[test]
fn a_tab_is_named_by_its_file_name_alone() {
    for raw in [
        "vehicle.txt",
        "vehicle",
        "GyroConfigs/vehicle.txt",
        "Autoload/GTA5/vehicle.txt",
        r"GyroConfigs\vehicle.txt",
    ] {
        assert_eq!(super::parse::tab_name(raw), "vehicle", "{raw}");
    }
}

/// A button bound to a config file switches which tab is running — JSM's action
/// layers, in a patch that keeps its configs as tabs.
#[test]
fn a_binding_can_switch_to_another_tab() {
    let seen = run_tabs(
        &[
            ("base", "S = A\nHOME = \"driving.txt\""),
            ("driving", "S = B\nHOME = \"base.txt\""),
        ],
        &[
            // Press S on the base layer.
            (&["btn_south"], 3),
            // Switch, then press S again — the same button, a different key.
            (&["btn_guide"], 3),
            (&["btn_south"], 3),
            // And back.
            (&["btn_guide"], 3),
            (&["btn_south"], 3),
        ],
    );
    assert!(seen[0].contains("key_a"), "base layer: S is A — {:?}", seen[0]);
    assert!(seen[2].contains("key_b"), "driving layer: S is B — {:?}", seen[2]);
    assert!(!seen[2].contains("key_a"), "and not A any more — {:?}", seen[2]);
    assert!(seen[4].contains("key_a"), "back on base: S is A again — {:?}", seen[4]);
}

/// Switching layers must release whatever the old one was driving. This is the
/// stuck-key bug in a different coat: a key held when the layer changes has no
/// binding left to release it.
#[test]
fn switching_layers_does_not_leave_a_key_held() {
    let _guard = alone();
    let uid = 5151;
    let snap = tabbed_snap(
        uid,
        &[("base", "S = A\nHOME = \"driving.txt\""), ("driving", "N = B")],
    );
    let mut collector: HashMap<(String, String), Signal> = HashMap::new();
    let mut state: HashMap<usize, NodeState> = HashMap::new();
    let mut dev: HashMap<(String, String), Signal> = HashMap::new();

    // Hold S so A is down.
    dev.insert((PAD.to_string(), "btn_south".to_string()), Signal::Bool(true));
    for _ in 0..3 {
        collector.clear();
        super::eval::jsm_publish(&snap, uid, &dev, &mut collector, &mut state, DT);
    }
    assert_eq!(
        collector.get(&(format!("collector:{uid}"), "key_a".to_string())).map(|s| s.as_bool()),
        Some(true),
        "A is held to start with"
    );

    // Now switch layers with S still held. The new tab has no binding for S at all.
    dev.insert((PAD.to_string(), "btn_guide".to_string()), Signal::Bool(true));
    for _ in 0..5 {
        collector.clear();
        super::eval::jsm_publish(&snap, uid, &dev, &mut collector, &mut state, DT);
    }
    let a = collector
        .get(&(format!("collector:{uid}"), "key_a".to_string()))
        .map(|s| s.as_bool());
    assert!(
        a != Some(true),
        "the key the old layer was holding is not left down: {a:?}"
    );
}

/// `RESET_MAPPINGS` bound to a button puts the module back on the tab the editor
/// has open, whatever layer it had wandered to.
#[test]
fn reset_mappings_goes_back_to_the_open_tab() {
    let seen = run_tabs(
        &[
            ("base", "S = A\nHOME = \"driving.txt\""),
            ("driving", "S = B\nN = \"RESET_MAPPINGS\""),
        ],
        &[
            (&["btn_guide"], 3),
            (&["btn_south"], 3),
            (&["btn_north"], 3),
            (&["btn_south"], 3),
        ],
    );
    assert!(seen[1].contains("key_b"), "on the driving layer — {:?}", seen[1]);
    assert!(seen[3].contains("key_a"), "reset put it back on base — {:?}", seen[3]);
}

/// A bare `RESET_MAPPINGS` line discards everything above it, which is what JSM
/// does — and at the top of a file, where it usually sits, it says plainly that
/// there was nothing to discard.
#[test]
fn a_bare_reset_mappings_line_discards_what_came_before() {
    // Mid-file: the binding and the setting above it are gone.
    let c = compile("S = A\nGYRO_SENS = 4\nRESET_MAPPINGS\nN = B");
    assert!(errors(&c).is_empty(), "{:?}", errors(&c));
    assert_eq!(c.bindings.len(), 1, "only the binding below it survives");
    assert_eq!(c.bindings[0].trigger, Trigger::Simple(Btn::N));
    assert_eq!(c.aim.min_sens, (0.0, 0.0), "and the setting is back to default");

    // At the top: nothing to discard, and the line says so rather than looking
    // like it did something.
    let c = compile("RESET_MAPPINGS\nS = A");
    assert_eq!(c.lines[0].status, LineStatus::Ok);
    assert!(
        c.lines[0].notes.iter().any(|n| n.contains("nothing above it")),
        "it says there was nothing to reset: {:?}",
        c.lines[0].notes
    );
    assert_eq!(c.bindings.len(), 1, "and the binding below it is untouched");
}

/// Naming a config on its own line applies everything in that tab right there —
/// JSM loads the file at that point, so its settings and bindings take effect.
#[test]
fn a_bare_config_name_applies_that_tab() {
    let tabs = vec![(
        "defaults".to_string(),
        "GYRO_SENS = 3\nS = A\nN = C".to_string(),
    )];
    // The included tab's binding for S is replaced by the later one here; its
    // binding for N, which nothing else touches, comes across.
    let c = compile_with("defaults.txt\nS = B", &tabs);
    assert!(errors(&c).is_empty(), "{:?}", errors(&c));
    assert_eq!(c.aim.min_sens, (3.0, 3.0), "its settings apply");
    let for_btn = |b: Btn| {
        c.bindings
            .iter()
            .find(|x| x.trigger == Trigger::Simple(b))
            .map(|x| x.steps[0].out.clone())
    };
    assert_eq!(for_btn(Btn::N), Some(pin("key_c")), "its bindings apply");
    assert_eq!(
        for_btn(Btn::S),
        Some(pin("key_b")),
        "and a later line here overrides one of them"
    );
    assert!(
        c.lines[0].notes.iter().any(|n| n.contains("chain stops here")),
        "the line says it does not follow a chain: {:?}",
        c.lines[0].notes
    );
}

/// Two configs naming each other must not compile for ever.
#[test]
fn configs_that_name_each_other_do_not_loop() {
    let tabs = vec![
        ("a".to_string(), "b.txt\nS = A".to_string()),
        ("b".to_string(), "a.txt\nN = B".to_string()),
    ];
    // Compiling either one terminates; that it returns at all is the assertion.
    let c = compile_with("b.txt", &tabs);
    assert!(errors(&c).is_empty(), "{:?}", errors(&c));
    assert!(
        c.bindings.iter().any(|x| x.trigger == Trigger::Simple(Btn::N)),
        "one level came across"
    );
}

/// A rebound button uses the LAST binding, not the first — a binding is an
/// assignment, as in JSM. The earlier line says which one took over.
#[test]
fn a_rebound_button_uses_the_last_binding() {
    let c = compile("S = A\nS = B");
    assert_eq!(c.bindings.len(), 1, "one binding, not two");
    assert_eq!(c.bindings[0].steps[0].out, pin("key_b"), "the later one wins");
    assert!(
        c.lines[0].notes.iter().any(|n| n.contains("replaced by the binding on line 2")),
        "the line that lost says so: {:?}",
        c.lines[0].notes
    );

    // End to end: pressing the button sends the second key, not the first.
    let out = run_node("S = A\nS = B", false, &["btn_south"], 3);
    assert!(out.get("key_b").map(|s| s.as_bool()).unwrap_or(false), "{out:?}");
    assert!(!out.get("key_a").map(|s| s.as_bool()).unwrap_or(false), "{out:?}");
}

/// Editing any tab recompiles, including one a layer switch would reach — and puts
/// the module back on the tab the editor has open, rather than running a
/// half-edited chain.
#[test]
fn an_edit_puts_the_module_back_on_the_open_tab() {
    let _guard = alone();
    let uid = 5152;
    let mut snap = tabbed_snap(
        uid,
        &[("base", "S = A\nHOME = \"driving.txt\""), ("driving", "S = B")],
    );
    let mut collector: HashMap<(String, String), Signal> = HashMap::new();
    let mut state: HashMap<usize, NodeState> = HashMap::new();
    let mut dev: HashMap<(String, String), Signal> = HashMap::new();
    dev.insert((PAD.to_string(), "btn_guide".to_string()), Signal::Bool(true));
    for _ in 0..3 {
        collector.clear();
        super::eval::jsm_publish(&snap, uid, &dev, &mut collector, &mut state, DT);
    }
    // On the driving layer now: S is B.
    let mut dev: HashMap<(String, String), Signal> = HashMap::new();
    dev.insert((PAD.to_string(), "btn_south".to_string()), Signal::Bool(true));
    for _ in 0..3 {
        collector.clear();
        super::eval::jsm_publish(&snap, uid, &dev, &mut collector, &mut state, DT);
    }
    assert_eq!(
        collector.get(&(format!("collector:{uid}"), "key_b".to_string())).map(|s| s.as_bool()),
        Some(true),
        "the layer switch took"
    );

    // Now edit the OTHER tab. It recompiles, and lands back on the open one.
    snap = tabbed_snap(
        uid,
        &[("base", "S = A\nHOME = \"driving.txt\"\n# edited"), ("driving", "S = B")],
    );
    for _ in 0..3 {
        collector.clear();
        super::eval::jsm_publish(&snap, uid, &dev, &mut collector, &mut state, DT);
    }
    assert_eq!(
        collector.get(&(format!("collector:{uid}"), "key_a".to_string())).map(|s| s.as_bool()),
        Some(true),
        "an edit puts it back on the tab being edited"
    );
}

/// A console command JSM has and we don't is not an error — it is deliberately
/// ignored, with the same reason the bare command line gives. A command means the
/// same thing whether it is written on its own or bound to a button.
#[test]
fn a_console_command_binding_says_it_does_nothing_and_why() {
    for (line, expect) in [
        ("HOME = \"RECONNECT_CONTROLLERS\"", "tracks connected controllers"),
        ("HOME = \"CALIBRATE_TRIGGERS\"", "device card"),
        ("HOME = \"WHITELIST_ADD\"", "HidHide"),
        ("HOME = \"QUIT\"", "nothing to do here"),
    ] {
        let info = one(line);
        assert!(
            matches!(info.status, LineStatus::Ignored(_)),
            "{line} is ignored, not an error or a claim: {:?}",
            info.status
        );
        let notes = info.notes.join(" ");
        assert!(notes.contains(expect), "{line} says why: {notes}");
    }

    // And a command the module DOES run is live: `SET_MOTION_STICK_NEUTRAL` bound to
    // a button is the useful form of it.
    let live = one("HOME = \"SET_MOTION_STICK_NEUTRAL\"");
    assert_eq!(live.status, LineStatus::Ok, "{live:?}");

    // A binding that mixes a real key with an inert command still runs the key.
    let mixed = one("HOME = A \"QUIT\"");
    assert_eq!(mixed.status, LineStatus::Ok, "the key still fires: {mixed:?}");
}

/// Switching layers must actively release what the old layer was driving, not just
/// stop mentioning it. A sink latches the last value it was told, so a key that
/// simply falls off the bus stays DOWN — the stuck-key bug in a different coat.
/// This is the assertion the weaker "it isn't true any more" version missed.
#[test]
fn switching_layers_publishes_the_old_layers_keys_as_released() {
    let _guard = alone();
    let uid = 5153;
    let snap = tabbed_snap(
        uid,
        &[("base", "S = A\nHOME = \"driving.txt\""), ("driving", "N = B")],
    );
    let mut collector: HashMap<(String, String), Signal> = HashMap::new();
    let mut state: HashMap<usize, NodeState> = HashMap::new();
    let mut dev: HashMap<(String, String), Signal> = HashMap::new();
    let key = (format!("collector:{uid}"), "key_a".to_string());

    dev.insert((PAD.to_string(), "btn_south".to_string()), Signal::Bool(true));
    for _ in 0..3 {
        collector.clear();
        super::eval::jsm_publish(&snap, uid, &dev, &mut collector, &mut state, DT);
    }
    assert_eq!(collector.get(&key).map(|s| s.as_bool()), Some(true), "A starts held");

    // Switch with S still held, then run on for a while.
    dev.insert((PAD.to_string(), "btn_guide".to_string()), Signal::Bool(true));
    for _ in 0..6 {
        collector.clear();
        super::eval::jsm_publish(&snap, uid, &dev, &mut collector, &mut state, DT);
    }
    // A fresh map every tick, so this can only pass if the module actively says
    // "off" — absence would leave the sink holding `true`.
    assert_eq!(
        collector.get(&key).map(|s| s.as_bool()),
        Some(false),
        "the new layer keeps telling the old layer's key it is released"
    );
}

/// An included tab's bindings replace the host's for the same button, because the
/// include happens at that point in the file and a binding is an assignment.
#[test]
fn an_included_tab_replaces_a_binding_made_above_it() {
    let tabs = vec![("defaults".to_string(), "S = C".to_string())];
    let c = compile_with("S = A\ndefaults.txt", &tabs);
    assert!(errors(&c).is_empty(), "{:?}", errors(&c));
    let s_binding: Vec<_> = c
        .bindings
        .iter()
        .filter(|x| x.trigger == Trigger::Simple(Btn::S))
        .collect();
    assert_eq!(s_binding.len(), 1, "one binding for S, not two: {:?}", c.bindings);
    assert_eq!(
        s_binding[0].steps[0].out,
        pin("key_c"),
        "the included tab's binding wins, being further down the file"
    );
}

/// Editing a tab the editor does NOT have open still recompiles — a layer a binding
/// can reach is as live as the one on screen.
#[test]
fn editing_a_tab_that_is_not_open_still_recompiles() {
    let _guard = alone();
    let uid = 5154;
    let run = |snap: &NodeSnap, state: &mut HashMap<usize, NodeState>| {
        let mut collector: HashMap<(String, String), Signal> = HashMap::new();
        let mut dev: HashMap<(String, String), Signal> = HashMap::new();
        dev.insert((PAD.to_string(), "btn_guide".to_string()), Signal::Bool(true));
        for _ in 0..3 {
            collector.clear();
            super::eval::jsm_publish(snap, uid, &dev, &mut collector, state, DT);
        }
        let mut dev: HashMap<(String, String), Signal> = HashMap::new();
        dev.insert((PAD.to_string(), "btn_south".to_string()), Signal::Bool(true));
        for _ in 0..3 {
            collector.clear();
            super::eval::jsm_publish(snap, uid, &dev, &mut collector, state, DT);
        }
        collector
            .iter()
            .filter(|((_, p), v)| p.starts_with("key_") && v.as_bool())
            .map(|((_, p), _)| p.clone())
            .collect::<std::collections::HashSet<String>>()
    };
    let mut state: HashMap<usize, NodeState> = HashMap::new();

    let first = tabbed_snap(
        uid,
        &[("base", "HOME = \"driving.txt\""), ("driving", "S = B")],
    );
    let got = run(&first, &mut state);
    assert!(got.contains("key_b"), "the second tab's binding runs: {got:?}");

    // Edit the SECOND tab — the one not open. It has to take effect.
    let edited = tabbed_snap(
        uid,
        &[("base", "HOME = \"driving.txt\""), ("driving", "S = D")],
    );
    let got = run(&edited, &mut state);
    assert!(
        got.contains("key_d") && !got.contains("key_b"),
        "editing a tab that isn't open still recompiles: {got:?}"
    );
}

/// After an edit the module is back on the open tab *and knows it*, so a binding can
/// switch away again. Forgetting the second half leaves the switch looking like a
/// no-op, which is invisible until someone presses the button twice.
#[test]
fn a_layer_can_be_switched_to_again_after_an_edit() {
    let _guard = alone();
    let uid = 5155;
    let mut state: HashMap<usize, NodeState> = HashMap::new();
    let press = |snap: &NodeSnap, state: &mut HashMap<usize, NodeState>, pins: &[&str]| {
        let mut collector: HashMap<(String, String), Signal> = HashMap::new();
        let mut dev: HashMap<(String, String), Signal> = HashMap::new();
        for p in pins {
            dev.insert((PAD.to_string(), p.to_string()), Signal::Bool(true));
        }
        for _ in 0..3 {
            collector.clear();
            super::eval::jsm_publish(snap, uid, &dev, &mut collector, state, DT);
        }
        collector
            .iter()
            .filter(|((_, p), v)| p.starts_with("key_") && v.as_bool())
            .map(|((_, p), _)| p.clone())
            .collect::<std::collections::HashSet<String>>()
    };
    let tabs: &[(&str, &str)] = &[("base", "S = A\nHOME = \"driving.txt\""), ("driving", "S = B")];
    let snap = tabbed_snap(uid, tabs);

    press(&snap, &mut state, &["btn_guide"]);
    let on_layer = press(&snap, &mut state, &["btn_south"]);
    assert!(on_layer.contains("key_b"), "switched once: {on_layer:?}");

    // An edit sends it back to the open tab.
    let edited = tabbed_snap(
        uid,
        &[("base", "S = A\nHOME = \"driving.txt\"\n# edited"), ("driving", "S = B")],
    );
    let back = press(&edited, &mut state, &["btn_south"]);
    assert!(back.contains("key_a"), "an edit puts it back on base: {back:?}");

    // And the switch works again — it is not remembered as already done.
    press(&edited, &mut state, &["btn_guide"]);
    let again = press(&edited, &mut state, &["btn_south"]);
    assert!(
        again.contains("key_b"),
        "the same binding switches away again: {again:?}"
    );
}

// ── phase 9: the JSM_custom_curve fork ───────────────────────────────────────

/// Each curve's shape, checked where its shape actually differs from a straight
/// line. The three settings a curve does or doesn't consult matter more than the
/// exact numbers, so each case picks a speed where the difference is unmistakable.
#[test]
fn every_acceleration_curve_has_the_shape_it_claims() {
    use super::cc::{sensitivity, Curve, Settings as Cc};
    let (lo, hi, cap) = (1.0f32, 5.0f32, 100.0f32);
    // The linear position between the thresholds, for `LINEAR` only.
    let t = |omega: f32| (omega / cap).min(1.0);
    let at = |c: Cc, omega: f32| sensitivity(&c, omega, t(omega), cap, lo, hi);

    // LINEAR is the straight line stock JSM draws.
    let lin = Cc { curve: Curve::Linear, ..Default::default() };
    assert!((at(lin, 0.0) - lo).abs() < 1e-5, "linear starts at the bottom");
    assert!((at(lin, 50.0) - 3.0).abs() < 1e-5, "and is halfway at halfway");
    assert!((at(lin, 100.0) - hi).abs() < 1e-5, "and reaches the top");

    // NATURAL is halfway up at `vhalf`, and never quite reaches the top.
    let nat = Cc { curve: Curve::Natural, natural_vhalf: 50.0, ..Default::default() };
    assert!((at(nat, 0.0) - lo).abs() < 1e-5, "natural starts at the bottom");
    assert!((at(nat, 50.0) - 3.0).abs() < 1e-4, "halfway at vhalf: {}", at(nat, 50.0));
    // Three half-lives up it is at 7/8 of the way, still short of the top. (Far
    // enough out the exponential underflows and it reaches `hi` exactly, which is a
    // float limit rather than anything to assert about.)
    let three_halves = at(nat, 150.0);
    assert!(
        three_halves < hi && three_halves > 4.0,
        "approaches the top without reaching it: {three_halves}"
    );

    // QUADRATIC is flat early and steep late — under the line at halfway.
    let quad = Cc { curve: Curve::Quadratic, ..Default::default() };
    assert!(at(quad, 50.0) < at(lin, 50.0), "quadratic is gentler at halfway");
    assert!((at(quad, 100.0) - hi).abs() < 1e-5, "and pegs at the cap");
    assert!((at(quad, 200.0) - hi).abs() < 1e-5, "and stays there past it");

    // SIGMOID starts exactly at the bottom, by construction.
    let sig = Cc { curve: Curve::Sigmoid, sigmoid_mid: 50.0, sigmoid_width: 8.0, ..Default::default() };
    assert!((at(sig, 0.0) - lo).abs() < 1e-5, "sigmoid is rescaled to start at lo");
    assert!((at(sig, 50.0) - 3.0).abs() < 0.05, "and is halfway at its midpoint");
    assert!(at(sig, 30.0) < at(lin, 30.0), "S-shaped: below the line early");
    assert!(at(sig, 70.0) > at(lin, 70.0), "and above it late");

    // JUMP hugs the bottom then steps up near the cap.
    let jump = Cc { curve: Curve::Jump, jump_tau: 10.0, ..Default::default() };
    assert!((at(jump, 0.0) - lo).abs() < 1e-5, "jump is rescaled to start at lo");
    assert!(at(jump, 50.0) < 1.1, "and stays down until the cap is near: {}", at(jump, 50.0));
    assert!((at(jump, 100.0) - hi).abs() < 1e-5, "then steps to the top");

    // POWER with a tiny reference speed climbs almost at once.
    let pow = Cc { curve: Curve::Power, power_vref: 0.01, power_exponent: 0.5, ..Default::default() };
    assert!(at(pow, 0.0) <= lo + 1e-5, "power is at the bottom when standing still");
    assert!(at(pow, 1.0) > at(lin, 1.0), "and climbs faster than the line: {}", at(pow, 1.0));
}

/// Only three of the six curves consult `MAX_GYRO_THRESHOLD` — the trap the plan
/// calls out, and the one the editor has to warn about.
#[test]
fn three_curves_ignore_the_top_threshold_and_say_so() {
    use super::cc::{sensitivity, Curve, Settings as Cc};
    for curve in [Curve::Linear, Curve::Quadratic, Curve::Jump] {
        assert!(curve.uses_max_threshold(), "{curve:?} uses the cap");
    }
    for curve in [Curve::Natural, Curve::Power, Curve::Sigmoid] {
        assert!(!curve.uses_max_threshold(), "{curve:?} does not");
        // And prove it: changing the cap changes nothing.
        let c = Cc { curve, ..Default::default() };
        let a = sensitivity(&c, 30.0, 0.3, 100.0, 1.0, 5.0);
        let b = sensitivity(&c, 30.0, 0.3, 900.0, 1.0, 5.0);
        assert!((a - b).abs() < 1e-6, "{curve:?} ignores the cap: {a} vs {b}");
    }

    // The editor says so on the line that picks the curve.
    let warned = one("ACCEL_CURVE = SIGMOID");
    assert_eq!(warned.status, LineStatus::Ok);
    let notes = warned.notes.join(" ");
    assert!(
        notes.contains("MAX_GYRO_THRESHOLD"),
        "it warns the top threshold stops mattering: {notes}"
    );
    // And it doesn't cry wolf for the three that do use it.
    let quiet = one("ACCEL_CURVE = QUADRATIC");
    assert!(
        !quiet.notes.join(" ").contains("never looks at"),
        "no warning for a curve that uses it: {:?}",
        quiet.notes
    );
}

/// Every fork setting says it is a fork setting, so nobody is surprised when a
/// config using one won't load in a stock JSM build.
#[test]
fn fork_settings_say_they_are_the_forks() {
    for line in [
        "ACCEL_CURVE = NATURAL",
        "GYRO_SMOOTHING_DECAY = ON",
        "GYRO_ANGLE_SNAP = 10",
        "DECEL_BRAKE_STRENGTH = 0.5",
        "ROLL_CONTRIBUTION = 50",
        "ONE_EURO_FILTER",
    ] {
        let info = one(line);
        assert_eq!(info.status, LineStatus::Ok, "{line}: {:?}", info.status);
        assert!(
            info.notes.iter().any(|n| n.contains("JSM_custom_curve")),
            "{line} names the fork: {:?}",
            info.notes
        );
    }
}

/// `ONE_EURO_FILTER` is a command, not a setting — global and sticky, so it cannot
/// be chorded. Reproducing that asymmetry is deliberate.
#[test]
fn the_one_euro_filter_is_a_command_and_cannot_be_chorded() {
    let c = compile("ONE_EURO_FILTER");
    assert!(errors(&c).is_empty(), "{:?}", errors(&c));
    assert!(c.cc.one_euro_enabled, "the command switches it on");
    let notes = c.lines[0].notes.join(" ");
    assert!(notes.contains("cannot be chorded"), "and says so: {notes}");

    // As a chord it is not a setting at all, so the modeshift is refused.
    assert!(
        matches!(one("ZL,ONE_EURO_FILTER = ON").status, LineStatus::Error(_)),
        "it is not a setting to chord"
    );

    // Its two tuning numbers, though, chord like anything else.
    let c = compile("ZL,ONE_EURO_MIN_CUTOFF = 20");
    assert!(errors(&c).is_empty(), "{:?}", errors(&c));
    let held = super::parse::resolve(&c, &[Btn::Zl]);
    assert_eq!(held.cc.one_euro_min_cutoff, 20.0, "held, it is 20");
    assert_eq!(
        super::parse::resolve(&c, &[]).cc.one_euro_min_cutoff,
        6.0,
        "let go, back to the default"
    );
}

/// The one-euro filter smooths a jittery signal at rest and lets a fast move
/// through — the whole reason for it over a plain low-pass.
#[test]
fn the_one_euro_filter_smooths_jitter_but_not_a_fast_move() {
    let settle = |values: &[f32]| {
        let mut f = super::cc::OneEuro::default();
        let mut last = 0.0;
        for v in values {
            last = f.filter(*v, DT, 6.0, 0.3);
        }
        last
    };
    // Jitter about zero: the filter should land near zero, not near the last sample.
    let jitter: Vec<f32> = (0..40).map(|i| if i % 2 == 0 { 3.0 } else { -3.0 }).collect();
    let calm = settle(&jitter);
    assert!(calm.abs() < 1.5, "jitter at rest is smoothed away: {calm}");

    // A sustained fast move: the filter should follow it closely.
    let fast: Vec<f32> = (0..40).map(|_| 300.0).collect();
    let followed = settle(&fast);
    assert!(followed > 250.0, "a fast move comes through: {followed}");
}

/// Angle snap straightens a nearly-level turn without slowing it — the magnitude is
/// kept, which is the point.
#[test]
fn angle_snap_straightens_a_turn_without_slowing_it() {
    use super::cc::{angle_snap, Settings as Cc};
    let s = Cc { angle_snap: 10.0, ..Default::default() };
    // 5 degrees off horizontal: inside the zone, so it snaps flat.
    let (x, y) = (100.0f32, 100.0 * 5.0f32.to_radians().tan());
    let mag = (x * x + y * y).sqrt();
    let (sx, sy) = angle_snap(&s, x, y);
    assert!(sy.abs() < 1e-3, "the off-axis part goes: {sy}");
    assert!((sx - mag).abs() < 1e-3, "and the speed is kept, not lost: {sx} vs {mag}");

    // 30 degrees off: well outside, untouched.
    let (x, y) = (100.0f32, 100.0 * 30.0f32.to_radians().tan());
    assert_eq!(angle_snap(&s, x, y), (x, y), "outside the zone nothing happens");

    // Nearly vertical snaps to vertical, and keeps its sign.
    let (x, y) = (100.0 * 5.0f32.to_radians().tan(), -100.0f32);
    let (sx, sy) = angle_snap(&s, x, y);
    assert!(sx.abs() < 1e-3 && sy < 0.0, "snaps down, still downward: {sx}, {sy}");

    // Switched off, it never touches anything.
    let off = Cc::default();
    assert_eq!(angle_snap(&off, 100.0, 3.0), (100.0, 3.0));
}

/// The eased snap ramps in rather than jumping — including the magnitude, which is
/// where this deliberately departs from the fork.
#[test]
fn the_eased_angle_snap_ramps_in_from_nothing() {
    use super::cc::{angle_snap, Settings as Cc};
    let eased = Cc { angle_snap: 20.0, angle_snap_ease: true, ..Default::default() };
    let at = |deg: f32| {
        let (x, y) = (100.0f32, 100.0 * deg.to_radians().tan());
        (angle_snap(&eased, x, y), (x * x + y * y).sqrt())
    };
    // Right at the edge of the zone the blend is zero, so NOTHING changes — neither
    // axis. The fork gains magnitude here, which is what this departs from.
    let ((sx, sy), _) = at(19.99);
    let (x, y) = (100.0f32, 100.0 * 19.99f32.to_radians().tan());
    assert!(
        (sx - x).abs() < 0.5 && (sy - y).abs() < 0.5,
        "at the edge of the zone the snap has not started: {sx}, {sy} against {x}, {y}"
    );
    // Halfway in, part way snapped.
    let ((_, mid_y), _) = at(10.0);
    let straight = 100.0 * 10.0f32.to_radians().tan();
    assert!(mid_y.abs() < straight && mid_y.abs() > 0.0, "partly snapped: {mid_y}");
    // All the way in, flat.
    let ((_, in_y), _) = at(0.5);
    assert!(in_y.abs() < straight * 0.2, "nearly flat: {in_y}");
}

/// The deceleration brake damps the tail of a fast stop, and does nothing when it is
/// switched off or when the pad is turning steadily.
#[test]
fn the_deceleration_brake_only_bites_when_the_pad_is_stopping() {
    use super::cc::{Brake, Settings as Cc};
    let on = Cc { brake_strength: 0.8, brake_threshold: 25.0, ..Default::default() };

    // Steady speed inside the band: nothing to brake.
    let mut b = Brake::default();
    let mut mult = 1.0;
    for _ in 0..40 {
        mult = b.tick(&on, DT, 30.0, 30.0);
    }
    assert!((mult - 1.0).abs() < 1e-6, "a steady turn is not braked: {mult}");

    // Now a real stop: turning at 60 deg/s and arrested in a single tick, ending
    // slow but still moving — that last part matters, because the speed gate is
    // 2..60 deg/s, so the brake damps a camera that is still coasting rather than
    // one already stopped. It is a brief pulse, so take the strongest damping seen.
    let mut b = Brake::default();
    for _ in 0..10 {
        b.tick(&on, DT, 60.0, 60.0);
    }
    let mut hardest = 1.0f32;
    for _ in 0..5 {
        hardest = hardest.min(b.tick(&on, DT, 8.0, 8.0));
    }
    assert!(hardest < 0.9, "a hard stop is damped: {hardest}");

    // And it lets go again once the deceleration is over.
    let mut released = 0.0;
    for _ in 0..20 {
        released = b.tick(&on, DT, 8.0, 8.0);
    }
    assert!((released - 1.0).abs() < 1e-6, "then releases: {released}");

    // Switched off, never anything.
    let off = Cc::default();
    let mut b = Brake::default();
    for _ in 0..10 {
        b.tick(&off, DT, 60.0, 60.0);
    }
    let mut mult = 1.0f32;
    for _ in 0..5 {
        mult = mult.min(b.tick(&off, DT, 8.0, 8.0));
    }
    assert_eq!(mult, 1.0, "with strength 0 it is inert");
}

/// `YAW_PLUS_ROLL` mixes a share of roll into the turn, and is default `LOCAL` when
/// that share is zero.
#[test]
fn yaw_plus_roll_mixes_roll_into_the_turn() {
    let with = |roll_pct: f32, roll_rate: f32| {
        let cfg = format!(
            "{AIM_CFG}\nGYRO_SPACE = YAW_PLUS_ROLL\nROLL_CONTRIBUTION = {roll_pct}"
        );
        let mut a = Aiming::new(&cfg);
        a.gyro = Gyro { roll: roll_rate, pitch: 0.0, yaw: 30.0 };
        a.ticks(3).x
    };
    // With no roll contribution, rolling the pad does nothing to the turn.
    let plain = with(0.0, 0.0);
    let rolled_but_ignored = with(0.0, 40.0);
    assert!(
        (plain - rolled_but_ignored).abs() < 1e-3,
        "at 0% roll is ignored: {plain} vs {rolled_but_ignored}"
    );
    // With a share of it, the same roll changes the turn.
    let mixed = with(100.0, 40.0);
    assert!(
        (mixed - plain).abs() > 0.1,
        "at 100% roll joins the turn: {plain} then {mixed}"
    );

    // And `ROLL_CONTRIBUTION` says when it is doing nothing.
    let idle = one("ROLL_CONTRIBUTION = 50");
    assert!(
        idle.notes.iter().any(|n| n.contains("YAW_PLUS_ROLL")),
        "it says which space it needs: {:?}",
        idle.notes
    );
}

/// Decay smoothing is an alternative to the rolling average, and says that the two
/// smoothing settings now mean something different.
#[test]
fn decay_smoothing_replaces_the_rolling_average() {
    use super::cc::Decay;
    // A slow step, well under the threshold, is smoothed: settle at rest first, since
    // the smoother (like the fork's) starts on whatever it first sees, and a steady
    // input converges to itself however hard it is smoothed.
    let mut d = Decay::default();
    for _ in 0..20 {
        d.smooth(0.0, 0.0, DT, 0.125, 100.0);
    }
    let stepped = d.smooth(10.0, 0.0, DT, 0.125, 100.0).0;
    assert!(stepped < 5.0, "a slow step is smoothed: {stepped}");

    // Fast, past the threshold: straight through even from rest.
    let mut d = Decay::default();
    for _ in 0..20 {
        d.smooth(0.0, 0.0, DT, 0.125, 100.0);
    }
    let fast = d.smooth(500.0, 0.0, DT, 0.125, 100.0).0;
    assert!(fast > 450.0, "a fast one is not: {fast}");

    // With either setting at zero it is a pass-through, as in the fork.
    let mut d = Decay::default();
    assert_eq!(d.smooth(10.0, 2.0, DT, 0.0, 100.0), (10.0, 2.0));
    assert_eq!(d.smooth(10.0, 2.0, DT, 0.125, 0.0), (10.0, 2.0));

    let line = one("GYRO_SMOOTHING_DECAY = ON");
    assert!(
        line.notes.iter().any(|n| n.contains("feel different")),
        "it says the same numbers now feel different: {:?}",
        line.notes
    );
}

/// The two fork settings that cannot mean anything here say why.
#[test]
fn the_fork_settings_that_do_not_apply_say_why() {
    match one("IGNORE_GYRO_DEVICES = 0x054c:0x0ce6").status {
        LineStatus::Ignored(why) => {
            assert!(why.contains("handed one device"), "{why}");
        }
        other => panic!("IGNORE_GYRO_DEVICES should be ignored with a reason, got {other:?}"),
    }
    for line in ["TELEMETRY_ENABLED = ON", "TELEMETRY_PORT = 5000"] {
        match one(line).status {
            LineStatus::Ignored(why) => {
                assert!(why.contains("the GUI"), "{line}: {why}");
            }
            other => panic!("{line} should be ignored with a reason, got {other:?}"),
        }
    }
}

/// The fork's `MISC1`…`MISC6` are our `btn_misc1`…`6`, one for one.
#[test]
fn the_forks_misc_buttons_are_our_misc_pins() {
    for n in 1..=6 {
        let line = format!("MISC{n} = E");
        let info = one(&line);
        assert_eq!(info.status, LineStatus::Ok, "{line}: {:?}", info.status);
    }
    let out = run_node("MISC3 = E", false, &["btn_misc3"], 3);
    assert!(
        out.get("key_e").map(|s| s.as_bool()).unwrap_or(false),
        "pressing the pad's third misc button fires the binding: {out:?}"
    );
    // Past six is not a button.
    assert!(matches!(one("MISC7 = E").status, LineStatus::Error(_)));
}

/// The four fork names our bus has no pin for compile, never fire, and say so —
/// rather than being called errors or being guessed onto a pin that might be
/// something else entirely.
#[test]
fn the_fork_names_we_have_no_pin_for_say_they_never_fire() {
    for (name, expect) in [
        ("LTOUCH", "capacitive touch"),
        ("RTOUCH", "capacitive touch"),
        ("LMINI", "mini shoulder"),
        ("RMINI", "mini shoulder"),
    ] {
        let info = one(&format!("{name} = E"));
        assert_eq!(
            info.status,
            LineStatus::Ok,
            "{name} is a real name, not an error: {:?}",
            info.status
        );
        let notes = info.notes.join(" ");
        assert!(notes.contains("never fires here"), "{name}: {notes}");
        assert!(notes.contains(expect), "{name} says what it would need: {notes}");
    }
}

/// Every one of phase 9's settings is read, with a bad value called out.
#[test]
fn every_fork_setting_is_read() {
    let c = compile(
        "ACCEL_CURVE = JUMP\n\
         ACCEL_NATURAL_VHALF = 150\n\
         ACCEL_POWER_VREF = 0.5\n\
         ACCEL_POWER_EXPONENT = 2\n\
         ACCEL_SIGMOID_MID = 40\n\
         ACCEL_SIGMOID_WIDTH = 12\n\
         ACCEL_JUMP_TAU = 3\n\
         GYRO_SMOOTHING_DECAY = ON\n\
         ONE_EURO_MIN_CUTOFF = 8\n\
         ONE_EURO_SPEED_COEFF = 0.6\n\
         GYRO_ANGLE_SNAP = 12\n\
         GYRO_ANGLE_SNAP_EASE = ON\n\
         DECEL_BRAKE_STRENGTH = 0.7\n\
         DECEL_BRAKE_THRESHOLD = 40\n\
         ROLL_CONTRIBUTION = -25",
    );
    assert!(errors(&c).is_empty(), "all live: {:?}", errors(&c));
    assert_eq!(c.cc.curve, super::cc::Curve::Jump);
    assert_eq!(c.cc.natural_vhalf, 150.0);
    assert_eq!(c.cc.power_vref, 0.5);
    assert_eq!(c.cc.power_exponent, 2.0);
    assert_eq!(c.cc.sigmoid_mid, 40.0);
    assert_eq!(c.cc.sigmoid_width, 12.0);
    assert_eq!(c.cc.jump_tau, 3.0);
    assert!(c.cc.decay_smoothing);
    assert_eq!(c.cc.one_euro_min_cutoff, 8.0);
    assert_eq!(c.cc.one_euro_speed_coeff, 0.6);
    assert_eq!(c.cc.angle_snap, 12.0);
    assert!(c.cc.angle_snap_ease);
    assert_eq!(c.cc.brake_strength, 0.7);
    assert_eq!(c.cc.brake_threshold, 40.0);
    assert_eq!(c.cc.roll_contribution, -25.0);

    for bad in [
        "ACCEL_CURVE = SQUIGGLE",
        "ACCEL_JUMP_TAU = -1",
        "GYRO_ANGLE_SNAP = 90",
        "GYRO_ANGLE_SNAP_EASE = MAYBE",
        "DECEL_BRAKE_STRENGTH = 2",
        "ROLL_CONTRIBUTION = 200",
        "GYRO_SMOOTHING_DECAY = SOMETIMES",
    ] {
        assert!(matches!(one(bad).status, LineStatus::Error(_)), "{bad}");
    }
}

/// The curve reaches the actual gyro output — the pipeline is wired, not just the
/// settings parsed.
#[test]
fn the_chosen_curve_changes_what_the_gyro_does() {
    let aim_with = |extra: &str, yaw: f32| {
        let cfg = format!(
            "MIN_GYRO_SENS = 1\nMAX_GYRO_SENS = 8\nMIN_GYRO_THRESHOLD = 0\n\
             MAX_GYRO_THRESHOLD = 100\nREAL_WORLD_CALIBRATION = 1\nGYRO_SMOOTH_TIME = 0\n{extra}"
        );
        let mut a = Aiming::new(&cfg);
        a.gyro = Gyro { roll: 0.0, pitch: 0.0, yaw };
        a.ticks(3).x
    };
    // At a quarter of the way up the threshold band, QUADRATIC is far gentler than
    // the straight line — the curve is doing the work, not the sens settings.
    let linear = aim_with("ACCEL_CURVE = LINEAR", 25.0);
    let quad = aim_with("ACCEL_CURVE = QUADRATIC", 25.0);
    assert!(linear > 0.0 && quad > 0.0, "both aim: {linear}, {quad}");
    assert!(
        quad < linear * 0.7,
        "quadratic is much gentler at a quarter speed: {quad} against {linear}"
    );
}

/// The deceleration brake reaches the gyro output too, and leaves a steady turn
/// alone.
#[test]
fn the_brake_reaches_the_gyro_output() {
    let cfg = format!(
        "{AIM_CFG}\nMIN_GYRO_SENS = 1\nMAX_GYRO_SENS = 1\nDECEL_BRAKE_STRENGTH = 1\n\
         DECEL_BRAKE_THRESHOLD = 10"
    );
    // The same turn, once with the brake and once without, arrested in a tick.
    let run = |cfg: &str| {
        let mut a = Aiming::new(cfg);
        a.gyro = Gyro { roll: 0.0, pitch: 0.0, yaw: 50.0 };
        let steady = a.ticks(20).x;
        a.gyro = Gyro { roll: 0.0, pitch: 0.0, yaw: 8.0 };
        let mut lowest = f32::MAX;
        for _ in 0..5 {
            lowest = lowest.min(a.tick().x.abs());
        }
        (steady.abs(), lowest)
    };
    let (steady, braked) = run(&cfg);
    // 50 deg/s at calibration 1 over a 10 ms tick is half a pixel — small numbers,
    // but the comparison is what matters.
    assert!(steady > 0.4, "a steady turn aims: {steady}");
    let plain = format!("{AIM_CFG}\nMIN_GYRO_SENS = 1\nMAX_GYRO_SENS = 1");
    let (_, unbraked) = run(&plain);
    assert!(
        braked < unbraked * 0.95,
        "the brake damps the tail: {braked} against {unbraked}"
    );
}


/// What `REAL_WORLD_CALIBRATION` means, pinned to a number anyone can check by hand:
/// it is **mouse counts per degree turned**, so turning the pad through 90 degrees at
/// `RWC = 1` and `GYRO_SENS = 1` moves the pointer 90 counts, and `IN_GAME_SENS`
/// divides that.
///
/// This exists because a report came in that a calibration measured elsewhere seemed
/// to need a multiplier of about four in this module. The formula here is JSM's own —
/// `moveMouse(velocity * RWC / IN_GAME_SENS * dt)`, verified against
/// `shapedSensitivityMoveMouse` in JSM's `InputHelpers.h` — so a constant factor can
/// only come from what a number MEANS, not from the arithmetic. Pinning the meaning
/// is how that stays true.
#[test]
fn real_world_calibration_is_mouse_counts_per_degree() {
    // Turn at 90 deg/s for exactly one second, in 100 ticks of 10 ms.
    let sweep = |extra: &str| {
        let cfg = format!(
            "GYRO_SENS = 1\nGYRO_SMOOTH_TIME = 0\nREAL_WORLD_CALIBRATION = 1\n{extra}"
        );
        let mut a = Aiming::new(&cfg);
        a.gyro = Gyro { roll: 0.0, pitch: 0.0, yaw: 90.0 };
        let mut total = 0.0;
        for _ in 0..100 {
            total += a.tick().x;
        }
        total
    };
    let ninety = sweep("");
    assert!(
        (ninety - 90.0).abs() < 0.5,
        "90 degrees at RWC 1 is 90 counts, got {ninety}"
    );

    // Ten times the calibration is ten times the counts — it is a plain scale, so no
    // multiplier can be hiding in it.
    let times_ten = sweep("REAL_WORLD_CALIBRATION = 10");
    assert!(
        (times_ten - 900.0).abs() < 5.0,
        "RWC scales linearly: {times_ten}"
    );

    // And `IN_GAME_SENS` divides it, which is the one place a factor of about four
    // can come from: a calibration measured in-game already has the in-game
    // sensitivity baked in, so setting both double-counts it.
    let with_sens = sweep("IN_GAME_SENS = 4");
    assert!(
        (with_sens - 22.5).abs() < 0.5,
        "IN_GAME_SENS divides the calibration: {with_sens}"
    );

    // `GYRO_SENS` multiplies on top, as JSM's own feel multiplier.
    let double = sweep("GYRO_SENS = 2");
    assert!((double - 180.0).abs() < 1.0, "GYRO_SENS multiplies: {double}");
}

/// `POWER` climbs steeply past its reference speed — the reference is what sets
/// where, so it has to be the divisor and not a plain multiplier.
#[test]
fn the_power_curve_climbs_from_its_reference_speed() {
    use super::cc::{sensitivity, Curve, Settings as Cc};
    let (lo, hi, cap) = (1.0f32, 5.0f32, 100.0f32);
    let at = |c: Cc, omega: f32| sensitivity(&c, omega, (omega / cap).min(1.0), cap, lo, hi);

    // A hundredth of a degree per second reference: by 1 deg/s it is already at the
    // top, because that is a hundred times the reference.
    let quick = Cc { curve: Curve::Power, power_vref: 0.01, power_exponent: 0.5, ..Default::default() };
    assert!(at(quick, 1.0) > 4.9, "far past the reference it is at the top: {}", at(quick, 1.0));

    // Move the reference out and the same speed is barely off the bottom — which is
    // the whole point of having one.
    let slow = Cc { curve: Curve::Power, power_vref: 100.0, power_exponent: 0.5, ..Default::default() };
    assert!(at(slow, 1.0) < 2.0, "well short of it, still low: {}", at(slow, 1.0));
    assert!(
        at(quick, 1.0) > at(slow, 1.0) * 2.0,
        "the reference speed decides, not the exponent alone"
    );
}

/// The one-euro filter's whole trick is that its cutoff opens up with speed, so it
/// tracks a fast ramp closely while still smoothing a slow one. A plain low-pass
/// cannot do both.
#[test]
fn the_one_euro_cutoff_opens_up_with_speed() {
    let ramp = |beta: f32| {
        let mut f = super::cc::OneEuro::default();
        let mut last = 0.0;
        // A steady climb of 20 deg/s per tick.
        for i in 0..30 {
            last = f.filter(i as f32 * 20.0, DT, 6.0, beta);
        }
        (29.0 * 20.0) - last
    };
    // With no speed coefficient it lags badly; with one it keeps up.
    let lag_without = ramp(0.0);
    let lag_with = ramp(3.0);
    assert!(lag_without > 0.0, "a plain low-pass lags a ramp: {lag_without}");
    assert!(
        lag_with < lag_without * 0.5,
        "opening the cutoff with speed cuts the lag: {lag_with} against {lag_without}"
    );
}

/// The brake only watches for *slowing*, only inside its speed band, and only past
/// its threshold. Each of the three is what stops it firing when it shouldn't.
#[test]
fn the_brake_ignores_speeding_up_a_dead_stop_and_a_gentle_slowdown() {
    use super::cc::{Brake, Settings as Cc};
    let on = Cc { brake_strength: 1.0, brake_threshold: 25.0, ..Default::default() };
    let lowest = |from: f32, to: f32, thr: f32| {
        let s = Cc { brake_threshold: thr, ..on };
        let mut b = Brake::default();
        for _ in 0..10 {
            b.tick(&s, DT, from, from);
        }
        let mut worst = 1.0f32;
        for _ in 0..5 {
            worst = worst.min(b.tick(&s, DT, to, to));
        }
        worst
    };

    // Speeding up just as hard: not braking. Braking an acceleration would fight the
    // flick it is supposed to be cleaning up after.
    assert_eq!(lowest(8.0, 60.0, 25.0), 1.0, "a hard acceleration is not braked");

    // Stopping dead: outside the 2..60 deg/s band, so nothing to damp — the camera
    // has already stopped, and damping zero achieves nothing but a surprise on the
    // next movement.
    assert_eq!(lowest(60.0, 0.0, 25.0), 1.0, "a dead stop is not braked");

    // Slowing gently, under the threshold: left alone. Without the threshold every
    // ordinary slowdown would be damped.
    assert_eq!(lowest(30.0, 28.0, 25.0), 1.0, "a gentle slowdown is under the threshold");

    // And the same slowdown with a low enough threshold IS braked, which proves the
    // threshold is what made the difference rather than the shape of the test.
    assert!(lowest(30.0, 28.0, 0.5) < 1.0, "with a low threshold it bites");
}

/// Decay smoothing reaches the gyro output, and is off unless asked for.
#[test]
fn decay_smoothing_reaches_the_gyro_output() {
    let step = |extra: &str| {
        let cfg = format!(
            "GYRO_SENS = 1\nREAL_WORLD_CALIBRATION = 1\nGYRO_SMOOTH_TIME = 0.125\n\
             GYRO_SMOOTH_THRESHOLD = 500\n{extra}"
        );
        let mut a = Aiming::new(&cfg);
        // Settle at rest, then step to a slow turn.
        a.gyro = Gyro::default();
        a.ticks(20);
        a.gyro = Gyro { roll: 0.0, pitch: 0.0, yaw: 30.0 };
        a.tick().x
    };
    let with_decay = step("GYRO_SMOOTHING_DECAY = ON");
    let without = step("GYRO_SMOOTHING_DECAY = OFF");
    // The full, unsmoothed value: 30 deg/s at calibration 1 over a 10 ms tick.
    let full = 0.3;
    assert!(with_decay > 0.0 && with_decay < full * 0.5, "decay smooths the step: {with_decay}");
    assert!(without > 0.0 && without < full * 0.5, "so does the rolling average: {without}");
    // Both smooth, so "is it smoothed" proves nothing. What proves the decay smoother
    // is the one running is that it gives a DIFFERENT answer to the same two
    // settings — which is exactly what the editor warns about on that line.
    assert!(
        (with_decay - without).abs() > full * 0.05,
        "the two smoothers differ on the same settings: {with_decay} against {without}"
    );
}

/// The one-euro filter reaches the gyro output, and only when its command is given.
#[test]
fn the_one_euro_filter_reaches_the_gyro_output() {
    let jitter = |extra: &str| {
        let cfg = format!(
            "GYRO_SENS = 1\nREAL_WORLD_CALIBRATION = 1\nGYRO_SMOOTH_TIME = 0\n\
             ONE_EURO_MIN_CUTOFF = 2\nONE_EURO_SPEED_COEFF = 0\n{extra}"
        );
        let mut a = Aiming::new(&cfg);
        let mut worst = 0.0f32;
        for i in 0..40 {
            a.gyro = Gyro {
                roll: 0.0,
                pitch: 0.0,
                yaw: if i % 2 == 0 { 60.0 } else { -60.0 },
            };
            if i > 10 {
                worst = worst.max(a.tick().x.abs());
            } else {
                a.tick();
            }
        }
        worst
    };
    let filtered = jitter("ONE_EURO_FILTER");
    let raw = jitter("");
    assert!(raw > 0.0, "the jitter comes through unfiltered: {raw}");
    assert!(
        filtered < raw * 0.5,
        "the filter damps it: {filtered} against {raw}"
    );
}

/// Angle snap reaches the gyro output: a turn a few degrees off level comes out
/// level, and keeps its speed.
#[test]
fn angle_snap_reaches_the_gyro_output() {
    let aim = |extra: &str| {
        let cfg = format!(
            "GYRO_SENS = 1\nREAL_WORLD_CALIBRATION = 1\nGYRO_SMOOTH_TIME = 0\n{extra}"
        );
        let mut a = Aiming::new(&cfg);
        // Mostly yaw with a little pitch: about 6 degrees off level.
        a.gyro = Gyro { roll: 0.0, pitch: 6.0, yaw: 60.0 };
        a.ticks(3)
    };
    let plain = aim("");
    assert!(plain.y.abs() > 0.01, "without snapping the tilt shows: {plain:?}");

    let snapped = aim("GYRO_ANGLE_SNAP = 20");
    assert!(
        snapped.y.abs() < plain.y.abs() * 0.2,
        "snapping levels it out: {snapped:?} against {plain:?}"
    );
    assert!(
        snapped.x.abs() >= plain.x.abs() * 0.99,
        "and the speed is kept, not lost: {snapped:?} against {plain:?}"
    );
}

/// Each `MISC` button is its own pin, not all of them the first one.
#[test]
fn each_misc_button_is_its_own_pin() {
    for n in 1..=6u8 {
        let pin = format!("btn_misc{n}");
        let out = run_node(&format!("MISC{n} = E"), false, &[&pin], 3);
        assert!(
            out.get("key_e").map(|s| s.as_bool()).unwrap_or(false),
            "MISC{n} reads {pin}: {out:?}"
        );
        // And it is not some other pin: pressing the NEXT one must not fire it.
        let other = format!("btn_misc{}", if n == 6 { 1 } else { n + 1 });
        let wrong = run_node(&format!("MISC{n} = E"), false, &[&other], 3);
        assert!(
            !wrong.get("key_e").map(|s| s.as_bool()).unwrap_or(false),
            "MISC{n} does not read {other}: {wrong:?}"
        );
    }
}

// ── sliders and the curve preview ────────────────────────────────────────────

/// A numeric setting the module runs gets a slider; one it doesn't, doesn't.
#[test]
fn a_numeric_setting_gets_a_slider() {
    let text = "GYRO_SENS = 4\nS = A\n# a comment\nRIGHT_STICK_MODE = AIM\nFLICK_TIME = 0.1";
    let ks = super::knobs::knobs(text);
    let names: Vec<&str> = ks.iter().map(|k| k.name.as_str()).collect();
    assert_eq!(names, vec!["GYRO_SENS", "FLICK_TIME"], "only the numeric settings");
    assert_eq!(ks[0].line, 0, "and it knows which line it is on");
    assert_eq!(ks[0].value, 4.0);
    assert_eq!(ks[1].line, 4);

    // A setting this module doesn't run gets none — a slider on a dead line lies.
    assert!(super::knobs::knobs("AUTOLOAD = ON").is_empty(), "an ignored setting");
    assert!(
        super::knobs::knobs("SCREEN_RESOLUTION_X = 1920").is_empty(),
        "a pending setting"
    );
    // Nor does a binding, or a word that isn't a setting at all.
    assert!(super::knobs::knobs("S = A\nFLOOMP = 3").is_empty());
}

/// A setting that takes two numbers gets no slider: one control cannot honestly
/// stand for a pair, and silently dropping half the line would be worse than
/// leaving it to the keyboard.
#[test]
fn a_paired_setting_gets_no_slider() {
    assert!(super::knobs::knobs("MIN_GYRO_SENS = 2 3").is_empty(), "a pair");
    assert_eq!(
        super::knobs::knobs("MIN_GYRO_SENS = 2").len(),
        1,
        "but one number is fine"
    );
    // And a chorded setting is a modeshift — which chord would the slider be for?
    assert!(super::knobs::knobs("ZL,GYRO_SENS = 4").is_empty());
}

/// A slider's range is the span it moves through, and its position follows the
/// value.
#[test]
fn a_slider_maps_its_value_onto_its_range() {
    let k = &super::knobs::knobs("DECEL_BRAKE_STRENGTH = 0.5")[0];
    assert_eq!((k.lo, k.hi), (0.0, 1.0), "a fraction has its natural bounds");
    assert!((k.t() - 0.5).abs() < 1e-6, "halfway along");
    assert!((k.at(0.25) - 0.25).abs() < 1e-6, "and back again");

    // Milliseconds are whole numbers, so the slider lands on whole numbers.
    let k = &super::knobs::knobs("HOLD_PRESS_TIME = 150")[0];
    assert!(k.integral);
    assert_eq!(k.at(0.1234), 123.0, "rounded, not 123.4 milliseconds");

    // A value outside the slider's range still reads; the handle just pegs.
    let k = &super::knobs::knobs("GYRO_SENS = 99")[0];
    assert_eq!(k.value, 99.0, "the value is what the config says");
    assert_eq!(k.t(), 1.0, "the handle pegs rather than running off the end");
}

/// Dragging a slider rewrites only the number, leaving the line otherwise as the
/// author wrote it — name, spacing and trailing comment.
#[test]
fn setting_a_slider_rewrites_only_the_number() {
    let text = "GYRO_SENS = 4   # feel free to tune\nS = A";
    let out = super::knobs::set_knob(text, 0, 7.5, false);
    assert_eq!(
        out,
        "GYRO_SENS = 7.5   # feel free to tune\nS = A",
        "the comment and the rest of the file survive"
    );

    // A whole number is written without a pointless decimal tail.
    assert_eq!(super::knobs::set_knob("GYRO_SENS = 4", 0, 6.0, false), "GYRO_SENS = 6");
    assert_eq!(super::knobs::set_knob("HOLD_PRESS_TIME = 150", 0, 200.4, true), "HOLD_PRESS_TIME = 200");
    // And a trailing newline is not eaten, or editing the last line would reflow
    // the file every time.
    assert_eq!(super::knobs::set_knob("GYRO_SENS = 4\n", 0, 5.0, false), "GYRO_SENS = 5\n");

    // The rewritten text parses back to the value that was set — the round trip is
    // the point, since the text is the only source of truth.
    let out = super::knobs::set_knob("MIN_GYRO_THRESHOLD = 0\n", 0, 12.5, false);
    assert_eq!(compile(&out).aim.min_threshold, 12.5);
}

/// The curve preview follows the curve the config chose, and its shape is the same
/// one the gyro actually runs.
#[test]
fn the_curve_preview_matches_the_curve_the_config_chose() {
    let curve_of = |extra: &str| {
        let cfg = compile(&format!(
            "MIN_GYRO_SENS = 1\nMAX_GYRO_SENS = 5\nMIN_GYRO_THRESHOLD = 0\n\
             MAX_GYRO_THRESHOLD = 100\n{extra}"
        ));
        super::knobs::sens_curve(&cfg, 41)
    };
    let linear = curve_of("ACCEL_CURVE = LINEAR");
    assert_eq!(linear.len(), 41, "as many samples as asked for");
    assert!(linear[0].dps == 0.0, "starts at a standstill");
    assert!((linear[0].sens - 1.0).abs() < 1e-5, "at the low sensitivity");
    assert!(linear.last().unwrap().dps > 100.0, "and runs past the top threshold");

    // Quadratic is below the straight line partway up, as it is in the engine.
    let quad = curve_of("ACCEL_CURVE = QUADRATIC");
    let mid = linear.len() / 4;
    assert!(
        quad[mid].sens < linear[mid].sens,
        "the preview shows the curve's shape: {} against {}",
        quad[mid].sens,
        linear[mid].sens
    );

    // A curve whose interesting range is well past the threshold is still drawn
    // over a span that shows it — otherwise the three that ignore the threshold
    // would be drawn over an arbitrary window.
    let wide = curve_of("ACCEL_CURVE = NATURAL\nACCEL_NATURAL_VHALF = 600");
    assert!(
        wide.last().unwrap().dps > 600.0,
        "the span follows the curve's own settings: {}",
        wide.last().unwrap().dps
    );
}
