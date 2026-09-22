//! The vocabulary the editor can offer: every setting and command this module
//! knows by name.
//!
//! The names live here, but **nothing about what they DO does** — whether a
//! setting is live, not yet implemented, or deliberately ignored comes from
//! `setting_support`, and a button's pin from `Btn::source`. A list that carried
//! its own copy of that would eventually promise something the module doesn't
//! run, which is the one thing this module must never do.
//!
//! Kept honest by `catalogue_tests`: every name here has to be one the parser
//! recognises, and every name the parser recognises has to be here. The second
//! direction is the one that rots on its own, so it is checked against the
//! parser's own source rather than a second hand-written list.

use super::cursor::{self, Cursor, TokenKind};
use super::names::{out_from_name, Out};
use super::help::help_for;
use super::names::Btn;
use super::parse::{button_reaches, command_does_nothing, setting_support, Support};

/// Where a name may legally appear in a config.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// `NAME = VALUE`.
    Setting,
    /// A line on its own.
    Command,
    /// A button, left of the `=`.
    Trigger,
    /// A value, right of the `=`: a key, a mouse button, a pad output, or one
    /// of JSM's own actions. A separate vocabulary from the names, because the
    /// two are never legal in the same place.
    Binding,
}

/// What the module does with a name today, for the list to show honestly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Live,
    /// Parsed, but not yet driving anything — with the reason.
    Pending(&'static str),
    /// Deliberately not run here — with the reason.
    Ignored(&'static str),
}

/// One offerable name.
#[derive(Clone, PartialEq, Debug)]
pub struct Item {
    pub name: String,
    pub kind: Kind,
    /// The group it belongs to, for a list worth reading: "Gyro", "Sticks"…
    pub group: &'static str,
    pub state: State,
    /// JoyShockMapper's own description, where it has one. Absent rather than
    /// invented — a made-up explanation of a setting is worse than silence.
    pub help: Option<&'static str>,
}

/// Every setting name, in the order the parser lists them.
pub(crate) const SETTINGS: &[&str] = &[
    "HOLD_PRESS_TIME", "TURBO_PERIOD", "SIM_PRESS_WINDOW",
    "DBL_PRESS_WINDOW", "TRIGGER_THRESHOLD", "TRIGGER_SKIP_DELAY",
    "ZL_MODE", "ZR_MODE", "LEFT_STICK_MODE",
    "RIGHT_STICK_MODE", "LEFT_RING_MODE", "RIGHT_RING_MODE",
    "STICK_DEADZONE_INNER", "STICK_DEADZONE_OUTER", "LEFT_STICK_DEADZONE_INNER",
    "LEFT_STICK_DEADZONE_OUTER", "RIGHT_STICK_DEADZONE_INNER", "RIGHT_STICK_DEADZONE_OUTER",
    "LEFT_STICK_AXIS", "RIGHT_STICK_AXIS", "SCROLL_SENS",
    "CONTROLLER_ORIENTATION", "STICK_AXIS_X", "STICK_AXIS_Y",
    "GYRO_SENS", "MIN_GYRO_SENS", "MAX_GYRO_SENS",
    "MIN_GYRO_THRESHOLD", "MAX_GYRO_THRESHOLD", "GYRO_SPACE",
    "GYRO_AXIS_X", "GYRO_AXIS_Y", "MOUSE_X_FROM_GYRO_AXIS",
    "MOUSE_Y_FROM_GYRO_AXIS", "GYRO_CUTOFF_SPEED", "GYRO_CUTOFF_RECOVERY",
    "GYRO_SMOOTH_THRESHOLD", "GYRO_SMOOTH_TIME", "TRACKBALL_DECAY",
    "GYRO_OFF", "GYRO_ON", "REAL_WORLD_CALIBRATION",
    "IN_GAME_SENS", "STICK_SENS", "STICK_POWER",
    "STICK_ACCELERATION_RATE", "STICK_ACCELERATION_CAP", "FLICK_TIME",
    "FLICK_TIME_EXPONENT", "FLICK_SNAP_MODE", "FLICK_SNAP_STRENGTH",
    "FLICK_DEADZONE_ANGLE", "ROTATE_SMOOTH_OVERRIDE", "MOUSE_RING_RADIUS",
    "SCREEN_RESOLUTION_X", "SCREEN_RESOLUTION_Y", "STICKLIKE_FACTOR",
    "MOUSELIKE_FACTOR", "RETURN_DEADZONE_IS_ACTIVE", "RETURN_DEADZONE_ANGLE",
    "RETURN_DEADZONE_ANGLE_CUTOFF", "EDGE_PUSH_IS_ACTIVE", "FLICK_STICK_OUTPUT",
    "GYRO_OUTPUT", "VIRTUAL_STICK_CALIBRATION", "LEFT_STICK_UNDEADZONE_INNER",
    "LEFT_STICK_UNDEADZONE_OUTER", "RIGHT_STICK_UNDEADZONE_INNER", "RIGHT_STICK_UNDEADZONE_OUTER",
    "LEFT_STICK_UNPOWER", "RIGHT_STICK_UNPOWER", "LEFT_STICK_VIRTUAL_SCALE",
    "RIGHT_STICK_VIRTUAL_SCALE", "WIND_STICK_RANGE", "WIND_STICK_POWER",
    "UNWIND_RATE", "ANGLE_TO_AXIS_DEADZONE_INNER", "ANGLE_TO_AXIS_DEADZONE_OUTER",
    "TOUCHPAD_MODE", "GRID_SIZE", "TOUCHPAD_SENS",
    "TOUCHPAD_DUAL_STAGE_MODE", "TOUCH_STICK_MODE", "TOUCH_STICK_RADIUS",
    "TOUCH_DEADZONE_INNER", "TOUCH_RING_MODE", "TOUCH_STICK_AXIS",
    "MOTION_STICK_MODE", "MOTION_RING_MODE", "MOTION_DEADZONE_INNER",
    "MOTION_DEADZONE_OUTER", "MOTION_STICK_AXIS", "LEAN_THRESHOLD",
    "ACCEL_CURVE", "ACCEL_NATURAL_VHALF", "ACCEL_POWER_VREF",
    "ACCEL_POWER_EXPONENT", "ACCEL_SIGMOID_MID", "ACCEL_SIGMOID_WIDTH",
    "ACCEL_JUMP_TAU", "GYRO_SMOOTHING_DECAY", "ONE_EURO_MIN_CUTOFF",
    "ONE_EURO_SPEED_COEFF", "GYRO_ANGLE_SNAP", "GYRO_ANGLE_SNAP_EASE",
    "DECEL_BRAKE_STRENGTH", "DECEL_BRAKE_THRESHOLD", "ROLL_CONTRIBUTION",
    "IGNORE_GYRO_DEVICES", "TELEMETRY_ENABLED", "TELEMETRY_PORT",
    "RUMBLE", "LIGHT_BAR", "ADAPTIVE_TRIGGER",
    "LEFT_TRIGGER_EFFECT", "RIGHT_TRIGGER_EFFECT", "LEFT_TRIGGER_OFFSET",
    "LEFT_TRIGGER_RANGE", "RIGHT_TRIGGER_OFFSET", "RIGHT_TRIGGER_RANGE",
    "AUTOLOAD", "AUTOCONNECT", "AUTO_CALIBRATE_GYRO",
    "JSM_DIRECTORY", "TICK_TIME", "HIDE_MINIMIZED",
    "VIRTUAL_CONTROLLER", "COUNTER_OS_MOUSE_SPEED", "IGNORE_OS_MOUSE_SPEED",
    "JOYCON_GYRO_MASK", "JOYCON_MOTION_MASK",
];

/// Every command that stands on its own line.
pub(crate) const COMMANDS: &[&str] = &[
    "RESET_MAPPINGS", "ONE_EURO_FILTER", "SET_MOTION_STICK_NEUTRAL",
    "CALIBRATE_TRIGGERS", "RESTART_GYRO_CALIBRATION", "FINISH_GYRO_CALIBRATION",
    "CALCULATE_REAL_WORLD_CALIBRATION", "RECONNECT_CONTROLLERS", "WHITELIST_ADD",
    "WHITELIST_REMOVE", "WHITELIST_SHOW", "CLEAR",
    "MERGE", "SPLIT", "SLEEP",
    "HELP", "README", "QUIT",
];

/// Every binding name JSM takes on the right of an `=`, apart from the families
/// a loop can generate (letters, digits, function keys, punctuation).
///
/// Kept honest the same way the settings are: a test reads `names.rs` and
/// requires every name the binding parser matches to be offered here.
pub(crate) const BINDINGS: &[&str] = &[
    // Named keys.
    "ENTER", "ESC", "SPACE", "BACKSPACE", "TAB", "CAPS_LOCK",
    "PAGEUP", "PAGEDOWN", "HOME", "END", "INSERT", "DELETE", "SCREENSHOT",
    "UP", "DOWN", "LEFT", "RIGHT",
    "SHIFT", "CONTROL", "ALT", "LSHIFT", "RSHIFT", "LCONTROL", "RCONTROL",
    "LALT", "RALT", "LWINDOWS", "RWINDOWS",
    "SCROLL_LOCK", "NUM_LOCK", "PAUSE", "CONTEXT",
    "ADD", "SUBTRACT", "DIVIDE", "MULTIPLY", "DECIMAL",
    "VOLUME_UP", "VOLUME_DOWN", "MUTE",
    "NEXT_TRACK", "PREV_TRACK", "STOP_TRACK", "PLAY_PAUSE",
    // Mouse.
    "LMOUSE", "RMOUSE", "MMOUSE", "BMOUSE", "FMOUSE", "SCROLLUP", "SCROLLDOWN",
    // Gyro control and calibration, which overlap whatever else the button does.
    "GYRO_ON", "GYRO_OFF", "GYRO_INVERT", "GYRO_INV_X", "GYRO_INV_Y",
    "GYRO_TRACKBALL", "GYRO_TRACK_X", "GYRO_TRACK_Y", "CALIBRATE",
    // Rumble.
    "SMALL_RUMBLE", "BIG_RUMBLE",
    // Binding a button to nothing at all, which is how you take one away.
    "NONE",
    // The virtual pad, under both of JSM's spellings.
    "X_A", "X_B", "X_X", "X_Y", "X_LB", "X_RB", "X_LS", "X_RS",
    "X_BACK", "X_START", "X_GUIDE",
    "X_UP", "X_DOWN", "X_LEFT", "X_RIGHT", "X_LT", "X_RT",
    "PS_CROSS", "PS_CIRCLE", "PS_SQUARE", "PS_TRIANGLE", "PS_L1", "PS_R1",
    "PS_L3", "PS_R3", "PS_SHARE", "PS_OPTIONS", "PS_HOME", "PS_PAD_CLICK",
    "PS_UP", "PS_DOWN", "PS_LEFT", "PS_RIGHT", "PS_L2", "PS_R2",
];

/// The punctuation keys, whose JSM names are the characters themselves.
pub(crate) const PUNCTUATION: &[&str] = &[
    ";", "'", ",", ".", "/", "\\", "[", "]", "+", "-", "`",
];

/// How a FlexInput target's name is written in a config.
///
/// `@Name`, or `@"Name with spaces"` — quoted whenever the bare form couldn't
/// be read back, which is anything outside letters, digits, `_`, `-` and `.`
/// (the unquoted charset stops short of JSM's event modifiers so a name can't
/// swallow one). The one place that decides this, so what a picker inserts is
/// what the parser reads.
pub fn fi_tag(name: &str) -> String {
    let bare = !name.is_empty()
        && name.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
        && !name.ends_with('_');
    if bare {
        format!("@{name}")
    } else {
        format!("@\"{name}\"")
    }
}

/// The JSM name for each of our bus pins a binding can write.
///
/// Derived from the binding parser rather than kept as a second table: every
/// name the list offers is asked what it writes, and the answer is indexed. So a
/// key on a virtual keyboard gets the name JSM would accept for it, and a cell
/// no binding can reach gets none — which is what greys it.
///
/// Where several names write one pin (SHIFT, LSHIFT and RSHIFT all send our one
/// generic shift), the shortest wins. That is JSM's own generic spelling, and
/// the one that doesn't carry a note explaining what was lost on the way.
///
/// Ties keep whichever the vocabulary lists first, which makes [`BINDINGS`]'s
/// order load-bearing for exactly one pin: `btn_guide`, where X_GUIDE and
/// PS_HOME are the same length. That one matters, because a picker showing a
/// gamepad has to name the whole pad in one dialect — `PS_HOME` sitting between
/// `X_START` and `X_BACK` reads as a mistake. A test pins both halves.
pub fn names_by_pin() -> std::collections::HashMap<String, String> {
    let mut out: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for item in bindings() {
        let Some(o) = out_from_name(&item.name) else { continue };
        let pin = match o.out {
            Out::Pin(p) | Out::Pulse(p) => p,
            // A gyro action, a rumble or NONE writes no pin, so no key on a
            // keyboard stands for it. Those stay in the list, where they can be
            // read and chosen by name.
            _ => continue,
        };
        let better = match out.get(&pin) {
            Some(had) => item.name.len() < had.len(),
            None => true,
        };
        if better {
            out.insert(pin, item.name);
        }
    }
    out
}

/// Which heading a binding belongs under. Grouped finely, because "Keyboard" as
/// one heading is a hundred rows, and the triggers jump by heading.
fn binding_group(name: &str) -> &'static str {
    let one = name.chars().count() == 1;
    if name.starts_with("X_") || name.starts_with("PS_") {
        "Virtual pad"
    } else if matches!(
        name,
        "LMOUSE" | "RMOUSE" | "MMOUSE" | "BMOUSE" | "FMOUSE" | "SCROLLUP" | "SCROLLDOWN"
    ) {
        "Mouse"
    } else if name.starts_with("GYRO_") || name == "CALIBRATE" {
        "Gyro actions"
    } else if name.ends_with("RUMBLE") {
        "Rumble"
    } else if name == "NONE" {
        "Nothing"
    } else if one && name.chars().all(|c| c.is_ascii_alphanumeric()) {
        "Letters & digits"
    } else if one {
        "Punctuation"
    } else if name.starts_with('F') && name[1..].chars().all(|c| c.is_ascii_digit()) {
        "Function keys"
    } else {
        "Keys"
    }
}

/// Every value a binding can take, for the right of an `=`.
///
/// The state comes from the binding parser itself, so a key our keyboard sink
/// cannot send is offered greyed with JSM's own reason rather than looking like
/// it would work.
pub fn bindings() -> Vec<Item> {
    let mut out: Vec<Item> = Vec::with_capacity(BINDINGS.len() + 70);
    let mut push = |name: String| {
        let state = match out_from_name(&name) {
            Some(o) => match o.out {
                Out::Unsupported { why, .. } => State::Ignored(why),
                _ => State::Live,
            },
            // Unreachable while the drift test passes. Offering a name the
            // parser doesn't know is the one thing this list must never do.
            None => State::Ignored("this module doesn't know this name"),
        };
        let group = binding_group(&name);
        out.push(Item { help: help_for(&name), name, kind: Kind::Binding, group, state });
    };
    for c in ('A'..='Z').chain('0'..='9') {
        push(c.to_string());
    }
    for p in PUNCTUATION {
        push((*p).to_string());
    }
    for n in 1..=20 {
        push(format!("F{n}"));
    }
    for name in BINDINGS {
        push((*name).to_string());
    }
    out.sort_by_key(|i| group_order(i.group));
    out
}

/// Which group a setting's support puts it in.
fn group_of(s: &Support) -> &'static str {
    match s {
        Support::Timing(_) => "Timings",
        Support::Analog(_) => "Triggers & sticks",
        Support::Aim(_) => "Gyro & aim",
        Support::Pad(_) => "Virtual pad output",
        Support::Motion(_) => "Motion & gravity",
        Support::Fb(_) => "Rumble & lights",
        Support::Cc(_) => "Custom-curve fork",
        Support::Pending(_) => "Not live yet",
        Support::Ignored(_) => "Ignored here",
    }
}

fn state_of(s: &Support) -> State {
    match s {
        Support::Pending(why) => State::Pending(why),
        Support::Ignored(why) => State::Ignored(why),
        _ => State::Live,
    }
}

/// The whole vocabulary, grouped and with each name's real state.
///
/// `pad_pins` is what the connected controller actually reports; a button it
/// hasn't got is still listed (the config may be shared with another pad) but
/// comes back `Ignored` with that reason, rather than being offered as if it
/// would work.
pub fn catalogue(pad_pins: &std::collections::HashSet<String>) -> Vec<Item> {
    let mut out = Vec::with_capacity(SETTINGS.len() + COMMANDS.len() + 64);
    for name in SETTINGS {
        let Some(sup) = setting_support(name) else { continue };
        out.push(Item {
            name: (*name).to_string(),
            kind: Kind::Setting,
            help: help_for(name),
            group: group_of(&sup),
            state: state_of(&sup),
        });
    }
    for name in COMMANDS {
        out.push(Item {
            name: (*name).to_string(),
            kind: Kind::Command,
            help: help_for(name),
            group: "Commands",
            state: match command_does_nothing(name) {
                Some(why) => State::Ignored(why),
                None => State::Live,
            },
        });
    }
    for b in Btn::ALL {
        let name = b.name();
        let state = match b.source() {
            super::names::BtnSource::Absent(why) => State::Ignored(why),
            _ if !button_reaches(*b, pad_pins) =>
                State::Ignored("this controller doesn't report it"),
            _ => State::Live,
        };
        let help = help_for(&name);
        out.push(Item { name, kind: Kind::Trigger, group: "Buttons", state, help });
    }
    // Grouped, so a reader sees each heading once. `sort_by_key` is stable, so
    // within a group the parser's own order survives — which is roughly the
    // order JSM's documentation introduces them in, and better than alphabetical
    // for finding the setting next to the one you just read about.
    out.sort_by_key(|i| group_order(i.group));
    out
}

/// What kind of name can legally stand where the cursor is.
///
/// JSM's grammar is positional: left of the `=` is a setting's name or a
/// button, a line on its own is a command, and right of the `=` is a VALUE — a
/// number, a key, or one of a setting's own words. None of those are names, so
/// there the list has nothing honest to offer and says where values do come
/// from, rather than offering `GYRO_SENS` as something to set `GYRO_SENS` to.
pub fn kinds_at(text: &str, cur: Cursor) -> &'static [Kind] {
    const NAMES: &[Kind] = &[Kind::Setting, Kind::Command, Kind::Trigger];
    const VALUES: &[Kind] = &[Kind::Binding];
    match cursor::selection(text, cur) {
        // A blank line is where a line begins, so anything can start there.
        None => NAMES,
        Some((t, _, _)) if t.kind == TokenKind::Name => NAMES,
        // Right of the `=`, what is legal depends on what is left of it. A
        // button takes a key, a mouse button, a pad output or one of JSM's own
        // actions; a setting takes a number or one of its own words, and those
        // come from the stick rather than from a list of names.
        Some((t, _, _)) if t.kind == TokenKind::Value && line_binds_a_button(text, cur.line) => {
            VALUES
        }
        Some(_) => &[],
    }
}

/// Is the name on this line a BUTTON rather than a setting?
///
/// A modeshift puts a chord in front of it (`ZL,S = SPACE` binds S while ZL is
/// held), so it is the last piece of the name that says what is being bound.
fn line_binds_a_button(text: &str, line: usize) -> bool {
    let (ls, le) = cursor::line_span(text, line);
    let l = &text[ls..le];
    let Some(tok) = cursor::tokenize(l).into_iter().find(|t| t.kind == TokenKind::Name) else {
        return false;
    };
    let name = tok.text(l);
    let last = name.rsplit([',', '+']).next().unwrap_or(name).to_ascii_uppercase();
    Btn::ALL.iter().any(|b| b.name() == last)
}

/// What picking `name` from the list puts under the cursor, and where the
/// cursor lands.
///
/// Replacing is what the cursor does, so a pick lands on the token you are
/// standing on rather than at the end of the file — the pad HAS a cursor, and
/// ignoring it would make the list the one part of the editor that doesn't.
///
/// The `=` and an empty slot come with it when the line hasn't got them yet: a
/// setting's name on its own is an error, and the next thing you want after
/// choosing a setting is somewhere to put its number. The cursor ends up on
/// that slot, so choosing a setting and setting it is two gestures, not five.
pub fn insert_pick(text: &str, cur: Cursor, name: &str, kind: Kind) -> (String, Cursor) {
    let has_equals =
        cursor::tokens_at(text, cur.line).iter().any(|t| t.kind == TokenKind::Equals);
    // A command is a line on its own; giving it an `=` would make it an error.
    let wants_value = matches!(kind, Kind::Setting | Kind::Trigger);
    if !wants_value || has_equals {
        return (cursor::replace(text, cur, name), cur);
    }
    (
        cursor::replace(text, cur, &format!("{name} = {}", cursor::SLOT)),
        Cursor { line: cur.line, token: cur.token + 2 },
    )
}

/// Where a group sits in the list. Explicit rather than alphabetical: aiming is
/// what most people open this list for, and "Ignored here" belongs at the bottom.
fn group_order(group: &str) -> u8 {
    match group {
        "Gyro & aim" => 0,
        "Custom-curve fork" => 1,
        "Triggers & sticks" => 2,
        "Virtual pad output" => 3,
        "Motion & gravity" => 4,
        "Rumble & lights" => 5,
        "Timings" => 6,
        "Buttons" => 7,
        "Commands" => 8,
        "Not live yet" => 9,
        // The binding vocabulary, which is never mixed with the names above:
        // what you reach for most on the right of an `=` comes first.
        "Letters & digits" => 10,
        "Keys" => 11,
        "Mouse" => 12,
        "Punctuation" => 13,
        "Function keys" => 14,
        "Gyro actions" => 15,
        "Virtual pad" => 16,
        "Rumble" => 17,
        "Nothing" => 18,
        _ => 19,
    }
}

#[cfg(test)]
mod position_tests {
    use super::*;
    use crate::eval::jsm_compile;

    fn at(line: usize, token: usize) -> Cursor {
        Cursor { line, token }
    }

    /// The list offers names where a name can go, and nothing where a value
    /// goes — offering `GYRO_SENS` as something to set `GYRO_SENS` to would be
    /// the list forgetting what it is for.
    #[test]
    fn the_list_offers_names_only_where_a_name_can_stand() {
        let text = "GYRO_SENS = 2 # feel

RESET_MAPPINGS
";
        assert_eq!(kinds_at(text, at(0, 0)).len(), 3, "the setting's name");
        assert!(kinds_at(text, at(0, 1)).is_empty(), "the `=` itself");
        assert!(kinds_at(text, at(0, 2)).is_empty(), "the value");
        assert!(kinds_at(text, at(0, 3)).is_empty(), "a comment");
        // A blank line is where a line begins, so anything can start there.
        assert_eq!(kinds_at(text, at(1, 0)).len(), 3);
        // A bare command sits in the name position too.
        assert_eq!(kinds_at(text, at(2, 0)).len(), 3);
    }

    /// A pick lands on the cursor, and brings what the line still needs.
    #[test]
    fn picking_a_setting_leaves_a_line_that_says_what_is_missing() {
        // On a blank line: the `=` and a slot come too, and the cursor ends up
        // on the slot so the stick can fill it straight away.
        let (out, cur) = insert_pick("A = B

", at(1, 0), "GYRO_SENS", Kind::Setting);
        assert_eq!(out, "A = B
GYRO_SENS = ?
");
        assert_eq!(cur, at(1, 2));
        assert_eq!(
            cursor::selection(&out, cur).unwrap().0.kind,
            TokenKind::Value,
            "and the slot is a value, which is what the stick scrubs"
        );
        // Unfinished, and the config says so rather than looking complete.
        let status = format!("{:?}", jsm_compile(&out, &[]).lines[1].status);
        assert!(status.contains("Error"), "an empty slot is an error: {status}");

        // A line that already has its `=` just gets the new name; the value it
        // was set to is left alone.
        let (out, cur) = insert_pick("GYRO_SENS = 2
", at(0, 0), "MIN_GYRO_SENS", Kind::Setting);
        assert_eq!(out, "MIN_GYRO_SENS = 2
");
        assert_eq!(cur, at(0, 0), "the cursor stays on the name it just changed");

        // A button is the left side of a binding, so it wants the same.
        let (out, _) = insert_pick("
", at(0, 0), "ZL", Kind::Trigger);
        assert_eq!(out, "ZL = ?
");

        // A command stands alone — an `=` would turn it into an error.
        let (out, cur) = insert_pick("
", at(0, 0), "RESET_MAPPINGS", Kind::Command);
        assert_eq!(out, "RESET_MAPPINGS
");
        assert_eq!(cur, at(0, 0));
        assert!(
            !format!("{:?}", jsm_compile(&out, &[]).lines[0].status).contains("Error"),
            "and it parses as it stands"
        );
    }

    /// Everything the list can offer has to survive being picked: each kind
    /// lands as a line the parser doesn't reject for a reason the pick caused.
    #[test]
    fn every_kind_in_the_catalogue_can_be_picked_onto_a_blank_line() {
        for item in catalogue(&std::collections::HashSet::new()) {
            let (out, cur) = insert_pick("
", at(0, 0), &item.name, item.kind);
            assert!(
                cursor::selection(&out, cur).is_some(),
                "{} left the cursor on nothing: {out:?}",
                item.name
            );
            let wants_value = matches!(item.kind, Kind::Setting | Kind::Trigger);
            assert_eq!(
                out.contains(" = "),
                wants_value,
                "{} got the wrong shape: {out:?}",
                item.name
            );
        }
    }
}

#[cfg(test)]
mod catalogue_tests {
    use super::*;
    use std::collections::HashSet;

    /// Match-arm string literals inside one function of the parser's source.
    ///
    /// Reading the source is deliberate. A second hand-written list would rot the
    /// moment someone adds a setting to the parser and not to it — and the
    /// failure would be silent, which for a list people pick from is the worst
    /// kind. This way the parser stays the single source of truth and the list
    /// has to keep up with it.
    fn arm_literals(src: &str, func: &str) -> HashSet<String> {
        let start = src.find(&format!("fn {func}(")).expect("function is there");
        let open = start + src[start..].find('{').expect("a body");
        let mut depth = 0usize;
        let mut end = open;
        for (i, c) in src[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let body = &src[open..end];
        let mut out = HashSet::new();
        // `"NAME"` (possibly `|`-joined) immediately before a `=>`.
        for (i, _) in body.match_indices("=>") {
            let mut j = i;
            loop {
                let head = body[..j].trim_end();
                if !head.ends_with('"') {
                    break;
                }
                let open_q = head[..head.len() - 1].rfind('"').expect("a closing quote has an opener");
                let lit = &head[open_q + 1..head.len() - 1];
                if lit.is_empty()
                    || !lit.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()
                        || c == '_' || c == '+' || c == '-')
                {
                    break;
                }
                out.insert(lit.to_string());
                let before = head[..open_q].trim_end();
                match before.strip_suffix('|') {
                    Some(rest) => j = rest.len(),
                    None => break,
                }
            }
        }
        out
    }

    #[test]
    fn every_setting_the_parser_knows_is_offered_and_nothing_else_is() {
        let src = include_str!("parse.rs");
        let from_source = arm_literals(src, "setting_support");
        let listed: HashSet<String> = SETTINGS.iter().map(|s| s.to_string()).collect();

        let missing: Vec<&String> = from_source.difference(&listed).collect();
        assert!(
            missing.is_empty(),
            "the parser knows settings the list doesn't offer: {missing:?}"
        );
        // The other direction is what stops the list promising something that
        // isn't there at all.
        for name in SETTINGS {
            assert!(
                setting_support(name).is_some(),
                "`{name}` is offered but the parser doesn't recognise it"
            );
        }
    }

    #[test]
    fn every_bare_command_is_offered_and_recognised() {
        let src = include_str!("parse.rs");
        let from_source = arm_literals(src, "command_does_nothing");
        let listed: HashSet<String> = COMMANDS.iter().map(|s| s.to_string()).collect();
        let missing: Vec<&String> = from_source.difference(&listed).collect();
        assert!(missing.is_empty(), "commands the list doesn't offer: {missing:?}");
        // A setting's name on its own line prints its value, so it is not a
        // command — the two lists must not overlap or the editor would offer the
        // same name twice meaning different things.
        for c in COMMANDS {
            assert!(
                setting_support(c).is_none(),
                "`{c}` is listed as a command but is also a setting"
            );
        }
    }

    /// Every name the binding parser takes is offered, and everything offered
    /// is a name it takes. The list of what goes on the right of an `=` rots
    /// the same way the settings would, so it is checked the same way.
    #[test]
    fn every_binding_the_parser_takes_is_offered_and_nothing_else_is() {
        let src = include_str!("names.rs");
        let mut from_source = arm_literals(src, "out_from_name");
        from_source.extend(arm_literals(src, "pad_pin"));
        // Two the parser takes but the list must not spread: JSM's own
        // misspelling of SUBTRACT, and DEFAULT, a second spelling of NONE.
        // Both are accepted so existing configs load; offering them would put
        // them into new ones.
        let aliases: HashSet<String> =
            ["SUBSTRACT", "DEFAULT"].iter().map(|s| s.to_string()).collect();
        let offered: HashSet<String> = bindings().into_iter().map(|i| i.name).collect();
        let missing: Vec<&String> =
            from_source.difference(&offered).filter(|n| !aliases.contains(*n)).collect();
        assert!(
            missing.is_empty(),
            "the parser binds names the list doesn't offer: {missing:?}"
        );
        for item in bindings() {
            assert!(
                super::out_from_name(&item.name).is_some(),
                "`{}` is offered but the parser doesn't bind it",
                item.name
            );
        }
    }

    /// Every key a virtual keyboard can show has the name JSM would accept for
    /// it, so a picker built on our pins can speak JSM without a second table
    /// of its own to keep in step.
    #[test]
    fn our_pins_carry_the_names_jsm_binds_them_by() {
        let by_pin = names_by_pin();
        let name = |p: &str| by_pin.get(p).map(String::as_str);
        assert_eq!(name("key_space"), Some("SPACE"));
        assert_eq!(name("key_a"), Some("A"));
        // A digit's pin is `key_7`: JSM's own spelling, and one both keyboard
        // sinks take. (A virtual keyboard laid out from egui's captures spells
        // the same key `key_num7`, so it normalises before asking.)
        assert_eq!(name("key_7"), Some("7"));
        assert_eq!(name("key_f9"), Some("F9"));
        assert_eq!(name("mouse_left"), Some("LMOUSE"));
        assert_eq!(name("scroll_up"), Some("SCROLLUP"), "a pulse names its pin too");
        assert_eq!(name("key_arrowup"), Some("UP"));
        assert_eq!(name("key_quote"), Some("'"), "punctuation is named by itself");
        // Where several names write one pin, the plain one wins.
        assert_eq!(name("key_shift"), Some("SHIFT"));
        assert_eq!(name("key_ctrl"), Some("CONTROL"));
        assert_eq!(name("btn_south"), Some("X_A"));
        // Same length, so the tie goes to whichever the vocabulary lists
        // first — see the dialect test below for why that is the answer that
        // matters.
        assert_eq!(name("btn_guide"), Some("X_GUIDE"));
        // Every entry round-trips: the name filed under a pin really writes it.
        for (pin, n) in &by_pin {
            let o = super::out_from_name(n).expect("an offered name");
            let wrote = match o.out {
                Out::Pin(p) | Out::Pulse(p) => p,
                _ => panic!("`{n}` is filed under a pin but writes none"),
            };
            assert_eq!(&wrote, pin, "`{n}` is filed under the wrong pin");
        }
        // A pin no binding writes has no name, which is what greys its key.
        assert!(name("touch_swipe_x").is_none());
        assert!(name("btn_misc1").is_none(), "a fork button our pad has, JSM does not bind");
    }

    /// A picker showing a gamepad names the whole pad in ONE dialect. `PS_HOME`
    /// sitting between `X_BACK` and `X_START` reads as a mistake, whichever pad
    /// the reader actually holds.
    #[test]
    fn the_pad_is_named_in_one_dialect() {
        let by_pin = names_by_pin();
        for pin in [
            "btn_south", "btn_east", "btn_west", "btn_north", "btn_lb", "btn_rb",
            "btn_ls", "btn_rs", "btn_back", "btn_start", "btn_guide",
            "dpad_up", "dpad_down", "dpad_left", "dpad_right",
            "left_trigger", "right_trigger",
        ] {
            let n = by_pin.get(pin).unwrap_or_else(|| panic!("{pin} has no name"));
            assert!(n.starts_with("X_"), "{pin} is named {n}, which is the other family");
        }
    }

    /// Ties go to whichever name the vocabulary lists first, so its order is
    /// load-bearing. Said out loud here, rather than left for someone to
    /// discover by alphabetising the list and changing what a picker inserts.
    #[test]
    fn the_vocabulary_lists_the_name_to_prefer_first() {
        let at = |n: &str| {
            BINDINGS.iter().position(|b| *b == n).unwrap_or_else(|| panic!("{n} is listed"))
        };
        for (prefer, over) in [
            ("SHIFT", "LSHIFT"), ("CONTROL", "LCONTROL"), ("ALT", "LALT"),
            ("X_GUIDE", "PS_HOME"),
        ] {
            assert!(at(prefer) < at(over), "{prefer} must be listed before {over}");
        }
    }

    /// A key this module cannot send is offered greyed, with the reason, rather
    /// than as an equal of the ones that work.
    ///
    /// This is the property the whole list exists for: it is generated from the
    /// binding parser instead of typed out precisely so it cannot promise a key
    /// that goes nowhere.
    #[test]
    fn the_key_list_says_which_keys_go_nowhere() {
        let all = bindings();
        let state = |n: &str| {
            all.iter().find(|i| i.name == n).unwrap_or_else(|| panic!("{n} is offered")).state
        };
        // Media keys, the numpad and the lock keys have no scancode in our
        // keyboard sink. JSM takes them; we cannot send them.
        for name in ["PLAY_PAUSE", "MUTE", "VOLUME_UP", "NUM_LOCK", "PAUSE", "ADD", "DECIMAL"] {
            assert!(
                matches!(state(name), State::Ignored(_)),
                "`{name}` cannot be sent, so it must not be offered as if it could"
            );
        }
        // And everything that does work is offered plainly, or the greying
        // would mean nothing.
        for name in ["A", "7", "SPACE", "LMOUSE", "SCROLLUP", "X_A", "PS_CROSS",
                     "GYRO_ON", "CALIBRATE", "SMALL_RUMBLE", "NONE", "F12"] {
            assert_eq!(state(name), State::Live, "`{name}` works and should read that way");
        }
    }

    /// A binding is only ever offered where a binding can go, and a name only
    /// where a name can. The two vocabularies never appear together, because
    /// nowhere in JSM's grammar are both legal.
    #[test]
    fn the_two_vocabularies_never_meet() {
        use crate::eval::JsmCursor as Cur;
        let text = "S = SPACE\nGYRO_SENS = 2\n";
        let value_of_a_button = Cur { line: 0, token: 2 };
        let value_of_a_setting = Cur { line: 1, token: 2 };
        assert_eq!(kinds_at(text, value_of_a_button), &[Kind::Binding]);
        assert!(
            kinds_at(text, value_of_a_setting).is_empty(),
            "a setting takes a number, which comes from the stick"
        );
        // The name position is names, on both kinds of line.
        for line in 0..2 {
            let ks = kinds_at(text, Cur { line, token: 0 });
            assert!(!ks.contains(&Kind::Binding) && ks.len() == 3);
        }
        // A modeshift binds what is after the chord, so the chord doesn't
        // change which vocabulary the value comes from.
        let shift = "ZL,S = SPACE\nZL,GYRO_SENS = 4\n";
        assert_eq!(kinds_at(shift, value_of_a_button), &[Kind::Binding]);
        assert!(kinds_at(shift, value_of_a_setting).is_empty());
    }

    #[test]
    fn every_button_round_trips_through_its_name() {
        let mut seen = HashSet::new();
        for b in Btn::ALL {
            let name = b.name();
            assert_eq!(
                Btn::from_name(&name),
                Some(*b),
                "`{name}` doesn't parse back to the button it came from"
            );
            assert!(seen.insert(name.clone()), "`{name}` is listed twice");
        }
        // The families' bounds, which are the easy thing to get wrong.
        assert!(Btn::from_name("T25").is_some() && Btn::from_name("T26").is_none());
        assert!(Btn::from_name("MISC6").is_some() && Btn::from_name("MISC7").is_none());
    }

    #[test]
    fn the_catalogue_tells_the_truth_about_what_runs() {
        let none = HashSet::new();
        let all = catalogue(&none);
        let find = |n: &str| all.iter().find(|i| i.name == n).expect("listed");

        assert_eq!(find("GYRO_SENS").state, State::Live);
        assert_eq!(find("GYRO_SENS").group, "Gyro & aim");
        assert_eq!(find("GYRO_SENS").kind, Kind::Setting);
        assert_eq!(find("RESET_MAPPINGS").kind, Kind::Command);
        assert_eq!(find("S").kind, Kind::Trigger);
        // A name the module deliberately doesn't run says so, rather than being
        // offered as though it would work.
        assert!(matches!(find("TELEMETRY_PORT").state, State::Ignored(_)));
        assert!(matches!(find("LMINI").state, State::Ignored(_)));

        // With a pad connected, a button it hasn't got is marked — but still
        // listed, since a config is often shared with another controller.
        let pad: HashSet<String> = ["btn_south", "left_stick"].iter().map(|s| s.to_string()).collect();
        let on_pad = catalogue(&pad);
        let f = |n: &str| on_pad.iter().find(|i| i.name == n).expect("still listed");
        assert_eq!(f("S").state, State::Live, "the pad reports it");
        assert!(matches!(f("N").state, State::Ignored(_)), "this pad hasn't got it");
    }
}

#[cfg(test)]
mod grouping_and_help_tests {
    use super::*;
    use std::collections::HashSet;

    /// Each heading appears once. The list is drawn by printing a group header
    /// whenever the group changes, so an unsorted list showed "Gyro & aim" over
    /// and over as the parser's order wandered between categories.
    #[test]
    fn the_list_shows_each_group_once() {
        let all = catalogue(&HashSet::new());
        let mut seen: Vec<&str> = Vec::new();
        for item in &all {
            match seen.last() {
                Some(g) if *g == item.group => {}
                _ => {
                    assert!(
                        !seen.contains(&item.group),
                        "`{}` comes back after other groups — its heading would print twice",
                        item.group
                    );
                    seen.push(item.group);
                }
            }
        }
        assert!(seen.len() > 4, "there really are several groups: {seen:?}");
        assert_eq!(seen.first(), Some(&"Gyro & aim"), "what people open the list for");
    }

    /// Descriptions come from JoyShockMapper's own help, so they say what the
    /// setting does — not what we guessed it does.
    #[test]
    fn settings_carry_jsms_own_description() {
        let all = catalogue(&HashSet::new());
        let find = |n: &str| all.iter().find(|i| i.name == n).expect("listed");

        let sens = find("GYRO_SENS").help.expect("GYRO_SENS is described");
        assert!(
            sens.to_ascii_lowercase().contains("sensitivity"),
            "reads like JSM's own text: {sens}"
        );
        // Flattened to one line: JSM writes its help with embedded newlines, and
        // a tooltip with raw `\n` escapes in it would look broken.
        assert!(!sens.contains("\n"), "escapes are resolved: {sens}");
        assert!(!sens.trim().is_empty());

        // The fork's own settings are described too, from the fork's source.
        assert!(find("ACCEL_CURVE").help.is_some(), "the fork's settings are covered");

        // Most of the vocabulary is described; the rest carries nothing rather
        // than an invented sentence.
        let described = all.iter().filter(|i| i.help.is_some()).count();
        assert!(
            described > all.len() / 3,
            "only {described} of {} described",
            all.len()
        );
    }
}
