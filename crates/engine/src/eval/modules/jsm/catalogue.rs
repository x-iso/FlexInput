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
        _ => 10,
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
