//! Compiling a JSM config into something the evaluator can run, and a status
//! for every line so the editor can show what became of it.
//!
//! The grammar follows JSM itself (`src/CmdRegistry.cpp` for the command line,
//! `src/Mapping.cpp` for a binding's value):
//!
//! ```text
//! [button][, + or * [button]] = [action modifier][key][event modifier] …
//! [setting] = value
//! [button],[setting] = value        # modeshift
//! [command]                         # RESET_MAPPINGS, a file name, …
//! # comment to end of line
//! ```
//!
//! A line JSM accepts but we don't run yet is NOT an error: it compiles to
//! [`LineStatus::Pending`] with the phase that will pick it up, or
//! [`LineStatus::Ignored`] when FlexInput owns that setting instead. Nothing is
//! dropped silently.

use std::collections::HashSet;

use super::aim::{AxisMask, GyroButton, GyroSource, SnapMode};
use super::analog::{Orientation, RingMode, Settings, StickMode, TriggerMode};
use super::names::{out_from_name, Btn, BtnSource, Out, StickId};

/// What became of one line of the config.
#[derive(Clone, PartialEq, Debug)]
pub enum LineStatus {
    /// Empty, or only a comment.
    Blank,
    /// Understood and live.
    Ok,
    /// Understood; the phase named here will make it live.
    Pending(&'static str),
    /// Understood; deliberately not this module's job.
    Ignored(&'static str),
    /// Not understood.
    Error(String),
}

/// One line's status plus any caveats worth showing on hover.
#[derive(Clone, PartialEq, Debug)]
pub struct LineInfo {
    pub status: LineStatus,
    pub notes: Vec<String>,
}

impl LineInfo {
    fn of(status: LineStatus) -> Self {
        LineInfo {
            status,
            notes: Vec::new(),
        }
    }
}

/// How a binding is triggered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trigger {
    /// `S = …`
    Simple(Btn),
    /// `L,S = …` — S's binding while L is held (JSM's chorded press).
    Chord { chord: Btn, btn: Btn },
    /// `S,S = …` — pressed twice inside the double-press window.
    Double(Btn),
    /// `L+R = …` — both pressed inside the simultaneous-press window.
    Sim(Btn, Btn),
    /// `UP*RIGHT = …` — both held, in either order, no window.
    Diag(Btn, Btn),
}

/// Modifiers on the key itself (JSM's action modifiers).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ActionMod {
    None,
    Toggle,
    Instant,
    Release,
}

/// Which button event the key follows (JSM's event modifiers).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EventMod {
    Start,
    Release,
    Tap,
    Hold,
    Turbo,
}

/// One key of a binding, with its modifiers resolved.
#[derive(Clone, PartialEq, Debug)]
pub struct Step {
    pub out: Out,
    pub action: ActionMod,
    pub event: EventMod,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Binding {
    pub trigger: Trigger,
    pub steps: Vec<Step>,
    /// The line it came from, 0-based — so a note can be put back on it.
    pub line: usize,
}

/// `button,SETTING = value` — while `chord` is held, the setting reads `value`.
#[derive(Clone, PartialEq, Debug)]
pub struct Modeshift {
    pub chord: Btn,
    pub(crate) support: Support,
    /// Kept as written so the value is parsed by exactly the same code that
    /// parses the plain line.
    pub name: String,
    pub value: String,
}

/// The settings in force this tick, once the held chords have had their say.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Resolved {
    pub timings: Timings,
    pub settings: Settings,
    pub aim: super::aim::Settings,
    pub pad: super::pad::Settings,
    pub motion: super::motion::Settings,
    pub touch: super::touch::Settings,
    pub fb: super::feedback::Settings,
    pub cc: super::cc::Settings,
    /// Per stick: a chord is supplying its mode right now. When that stops the
    /// stick has to be let alone until it comes back to centre, or releasing the
    /// chord mid-push would hand the base mode a stick already out at full.
    pub stick_mode_chorded: [bool; 2],
}

/// Work out the settings in force, given the chords held — oldest first, as the
/// press machinery stacks them. JSM resolves each setting against the stack from
/// the newest chord down, taking the first that has something to say, so applying
/// them oldest-first and letting later ones overwrite comes to the same thing.
pub fn resolve(cfg: &Compiled, chords: &[Btn]) -> Resolved {
    let mut r = Resolved {
        timings: cfg.timings,
        settings: cfg.settings,
        aim: cfg.aim,
        pad: cfg.pad,
        motion: cfg.motion,
        touch: cfg.touch,
        fb: cfg.fb,
        cc: cfg.cc,
        stick_mode_chorded: [false; 2],
    };
    if cfg.modeshifts.is_empty() {
        return r;
    }
    for &chord in chords {
        for ms in cfg.modeshifts.iter().filter(|m| m.chord == chord) {
            apply_setting(
                &ms.name,
                &ms.value,
                ms.support,
                Knobs {
                    timings: &mut r.timings,
                    settings: &mut r.settings,
                    aim: &mut r.aim,
                    pad: &mut r.pad,
                    motion: &mut r.motion,
                    touch: &mut r.touch,
                    fb: &mut r.fb,
                    cc: &mut r.cc,
                },
            );
            if let Support::Analog(AnalogId::StickMode(side)) = ms.support {
                if side != Side::Right { r.stick_mode_chorded[0] = true; }
                if side != Side::Left { r.stick_mode_chorded[1] = true; }
            }
        }
    }
    r
}

/// JSM's press timings, in seconds.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Timings {
    pub hold: f32,
    pub turbo: f32,
    pub sim: f32,
    pub double: f32,
}

impl Default for Timings {
    fn default() -> Self {
        // JSM's defaults: HOLD_PRESS_TIME 150, TURBO_PERIOD 80,
        // SIM_PRESS_WINDOW 50, DBL_PRESS_WINDOW 150 (milliseconds).
        Timings {
            hold: 0.150,
            turbo: 0.080,
            sim: 0.050,
            double: 0.150,
        }
    }
}

/// A compiled config: what to run, and what to tell the user about every line.
#[derive(Clone, PartialEq, Debug)]
pub struct Compiled {
    pub lines: Vec<LineInfo>,
    pub bindings: Vec<Binding>,
    pub timings: Timings,
    /// Trigger and stick settings, as the analog side reads them.
    pub settings: Settings,
    /// Gyro, stick-aim and flick settings.
    pub aim: super::aim::Settings,
    /// Virtual pad output settings.
    pub pad: super::pad::Settings,
    /// Gravity: the lean buttons, the motion stick, the gravity gyro spaces.
    pub motion: super::motion::Settings,
    /// The touchpad.
    pub touch: super::touch::Settings,
    /// Rumble, the light bar, the adaptive triggers.
    pub fb: super::feedback::Settings,
    /// The custom-curve fork's additions to the gyro pipeline.
    pub cc: super::cc::Settings,
    /// A bare `SET_MOTION_STICK_NEUTRAL` line: take the pad's resting orientation
    /// as the motion stick's centre once the config is running.
    pub neutral_at_load: bool,
    /// Settings that change while a button is held.
    pub modeshifts: Vec<Modeshift>,
    /// Every button the config mentions, however it mentions it. Pass-through
    /// mode hands the rest of the bus straight on.
    pub mentioned: HashSet<Btn>,
}

impl Compiled {
    /// Counts for the editor's summary: (errors, pending, ignored).
    pub fn summary(&self) -> (usize, usize, usize) {
        let mut errors = 0;
        let mut pending = 0;
        let mut ignored = 0;
        for l in &self.lines {
            match l.status {
                LineStatus::Error(_) => errors += 1,
                LineStatus::Pending(_) => pending += 1,
                LineStatus::Ignored(_) => ignored += 1,
                _ => {}
            }
        }
        (errors, pending, ignored)
    }
}

/// A config's sibling tabs, as (name, text) — what a layer switch can reach.
///
/// A tab is named by its file name alone, so a config that loads
/// `GyroConfigs/vehicle.txt`, `Autoload/GTA5/vehicle.txt` or `vehicle.txt` all mean
/// the tab `vehicle`. That is what lets a set of related JSM configs be imported
/// from wherever they sit and then travel inside the patch.
pub type Tabs<'a> = &'a [(String, String)];

/// The tab a JSM file name refers to: the base name, without directory or `.txt`.
pub fn tab_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    base.strip_suffix(".txt")
        .or_else(|| base.strip_suffix(".TXT"))
        .unwrap_or(base)
        .trim()
        .to_string()
}

/// Compile a whole config, with no sibling tabs known — every layer switch then
/// says it cannot find its tab, which is the right answer when there are none.
pub fn compile(text: &str) -> Compiled {
    compile_with(text, &[])
}

/// Compile a whole config against the tabs it can reach.
pub fn compile_with(text: &str, tabs: Tabs) -> Compiled {
    let mut out = Compiled {
        lines: Vec::new(),
        bindings: Vec::new(),
        timings: Timings::default(),
        settings: Settings::default(),
        aim: super::aim::Settings::default(),
        pad: super::pad::Settings::default(),
        motion: super::motion::Settings::default(),
        touch: super::touch::Settings::default(),
        fb: super::feedback::Settings::default(),
        cc: super::cc::Settings::default(),
        neutral_at_load: false,
        modeshifts: Vec::new(),
        mentioned: HashSet::new(),
    };
    for (n, line) in text.lines().enumerate() {
        let info = compile_line(line, n, &mut out, tabs);
        out.lines.push(info);
    }
    annotate_analog(&mut out);
    out
}

/// Settings are read wherever they sit in the file, so what a binding will do
/// can only be said once the whole config is compiled: a stick direction bound
/// while that stick aims the mouse never fires, and neither does a full pull the
/// trigger mode doesn't allow. JSM is quiet about both.
fn annotate_analog(out: &mut Compiled) {
    let mut notes: Vec<(usize, String)> = Vec::new();
    for b in &out.bindings {
        let buttons = match b.trigger {
            Trigger::Simple(x) | Trigger::Double(x) => vec![x],
            Trigger::Chord { chord, btn } => vec![chord, btn],
            Trigger::Sim(x, y) | Trigger::Diag(x, y) => vec![x, y],
        };
        for btn in buttons {
            let note =
                match btn.source() {
                    BtnSource::Stick { stick, .. } => {
                        let cfg = match stick {
                            StickId::Left => out.settings.left,
                            StickId::Right => out.settings.right,
                        };
                        let side = match stick {
                            StickId::Left => "LEFT",
                            StickId::Right => "RIGHT",
                        };
                        (!cfg.mode.is_digital()).then(|| {
                            format!(
                                "{side}_STICK_MODE isn't a digital mode, so `{}` never fires — \
                         that mode arrives in a later phase",
                                btn.name()
                            )
                        })
                    }
                    BtnSource::TriggerFull { .. } => {
                        let (mode, name) = if btn == Btn::Zlf {
                            (out.settings.zl, "ZL_MODE")
                        } else {
                            (out.settings.zr, "ZR_MODE")
                        };
                        (!mode.has_full()).then(|| {
                            format!(
                        "{name} is NO_FULL (JSM's default), so `{}` never fires — set it to \
                         NO_SKIP, NO_SKIP_EXCLUSIVE, MUST_SKIP or MAY_SKIP", btn.name())
                        })
                    }
                    _ => None,
                };
            if let Some(note) = note {
                notes.push((b.line, note));
            }
        }
    }
    for (line, note) in notes {
        if let Some(info) = out.lines.get_mut(line) {
            if !info.notes.contains(&note) {
                info.notes.push(note);
            }
        }
    }
}

fn compile_line(raw: &str, n: usize, out: &mut Compiled, tabs: Tabs) -> LineInfo {
    // '#' starts a comment to end of line (JSM cuts it before parsing, so a '#'
    // inside a quoted command ends the line there too).
    let line = raw.split('#').next().unwrap_or("").trim();
    if line.is_empty() {
        return LineInfo::of(LineStatus::Blank);
    }

    // A line with no value: RESET_MAPPINGS, a config file name, the console-only ones.
    let Some((lhs, rhs)) = line.split_once('=') else {
        return command_line(line, out, tabs);
    };
    let (lhs, rhs) = (lhs.trim(), rhs.trim());
    let Some((first, combo)) = split_combo(lhs) else {
        return LineInfo::of(LineStatus::Error(
            "this line doesn't look like a command".into(),
        ));
    };

    // `button,SETTING = value` — a modeshift.
    if let Some((op, second)) = &combo {
        if let Some(support) = setting_support(second) {
            if *op != ',' {
                return LineInfo::of(LineStatus::Error(
                    "only a chord (`button,SETTING`) can change a setting".into(),
                ));
            }
            let Some(chord) = Btn::from_name(&first) else {
                return LineInfo::of(LineStatus::Error(format!("`{first}` isn't a button")));
            };
            out.mentioned.insert(chord);
            return modeshift_line(chord, second, rhs, support, out);
        }
    }

    // `SETTING = value`.
    if combo.is_none() {
        if let Some(support) = setting_support(&first) {
            return setting_line(&first, rhs, support, out);
        }
    }

    // Everything else is a binding.
    binding_line(&first, combo, rhs, n, out, tabs)
}

// ── line kinds ───────────────────────────────────────────────────────────────

/// Why a JSM console command does nothing here, or `None` when it does something.
///
/// These are the same reasons a bare command line gives, so a command means the same
/// thing whether it is written on its own or bound to a button.
fn command_does_nothing(name: &str) -> Option<&'static str> {
    Some(match name.trim().to_ascii_uppercase().as_str() {
        // The ones that DO something: re-centring the motion stick, resetting, and
        // switching layers (the last two never arrive as `Out::Command` at all).
        "SET_MOTION_STICK_NEUTRAL" | "RESET_MAPPINGS" => return None,
        "CALCULATE_REAL_WORLD_CALIBRATION" => {
            "the RWS Aim module measures real-world sensitivity; set \
             REAL_WORLD_CALIBRATION here from what it tells you"
        }
        "RESTART_GYRO_CALIBRATION" | "FINISH_GYRO_CALIBRATION" | "CALIBRATE_TRIGGERS" => {
            WHY_DEVICE_CARD
        }
        "RECONNECT_CONTROLLERS" | "MERGE" | "SPLIT" => {
            "FlexInput tracks connected controllers itself"
        }
        "WHITELIST_SHOW" | "WHITELIST_ADD" | "WHITELIST_REMOVE" => {
            "FlexInput hides pads through HidHide, in Settings"
        }
        "README" | "HELP" | "CLEAR" | "QUIT" | "SLEEP" => {
            "it is a JSM console command with nothing to do here"
        }
        // Anything else is a name JSM would print or refuse; say so plainly.
        _ => "it is not a command this module runs",
    })
}


/// Does this name refer to another config? `Some(Ok(tab))` when it does and the tab
/// exists, `Some(Err(why))` when it plainly means a config we haven't got, and
/// `None` when it isn't a config name at all.
///
/// What counts as a config name is JSM's own giveaway: a `.txt` suffix or a path
/// separator. Without either, a bare word is far more likely a typo'd command than
/// a file, and calling it a missing tab would bury the real mistake.
fn layer_target(name: &str, tabs: Tabs) -> Option<Result<String, String>> {
    let looks_like_a_file =
        name.to_ascii_uppercase().ends_with(".TXT") || name.contains('/') || name.contains('\\');
    if !looks_like_a_file {
        return None;
    }
    let want = tab_name(name);
    if want.is_empty() {
        return Some(Err(format!("`{name}` doesn't name a config")));
    }
    match tabs.iter().find(|(n, _)| n.eq_ignore_ascii_case(&want)) {
        Some((n, _)) => Some(Ok(n.clone())),
        None if tabs.is_empty() => Some(Err(format!(
            "no tab called `{want}` — this module holds each config as a tab, so load \
             `{name}` into one and it will be found"
        ))),
        None => {
            let have: Vec<&str> = tabs.iter().map(|(n, _)| n.as_str()).collect();
            Some(Err(format!(
                "no tab called `{want}` — the tabs here are {}",
                have.join(", ")
            )))
        }
    }
}

/// Fold another tab's settings and bindings in at this point, the way JSM loading
/// that file mid-config would.
///
/// Its own line statuses are NOT merged: they belong to that tab's text and are
/// shown when that tab is open. What comes across is what it does.
fn include_tab(tab: &str, out: &mut Compiled, tabs: Tabs) -> LineInfo {
    let Some((_, text)) = tabs.iter().find(|(n, _)| n == tab) else {
        return LineInfo::of(LineStatus::Error(format!("no tab called `{tab}`")));
    };
    // Compiled WITHOUT the tab list, so a pair of configs naming each other cannot
    // loop for ever. One level of include is what a JSM config actually uses (a
    // base and a layer); deeper nesting would need a visited set, and saying plainly
    // that it stops here beats a stack overflow.
    let inner = compile(text);
    out.timings = inner.timings;
    out.settings = inner.settings;
    out.aim = inner.aim;
    out.pad = inner.pad;
    out.motion = inner.motion;
    out.touch = inner.touch;
    out.fb = inner.fb;
    out.cc = inner.cc;
    out.neutral_at_load |= inner.neutral_at_load;
    out.modeshifts.extend(inner.modeshifts);
    out.mentioned.extend(inner.mentioned.iter().copied());
    // A binding is an assignment, so the included tab's win — and they are folded in
    // one at a time so the replacement rule applies to each.
    for b in inner.bindings {
        out.bindings.retain(|x| x.trigger != b.trigger);
        out.bindings.push(b);
    }
    let mut info = LineInfo::of(LineStatus::Ok);
    let errors = inner
        .lines
        .iter()
        .filter(|l| matches!(l.status, LineStatus::Error(_)))
        .count();
    if errors > 0 {
        info.notes.push(format!(
            "tab `{tab}` has {errors} line{} with errors of its own — open that tab to see them",
            if errors == 1 { "" } else { "s" }
        ));
    }
    info.notes.push(format!(
        "applies everything in tab `{tab}` here, as JSM would by loading that file. \
         A config naming another one INSIDE `{tab}` is not followed, so the chain stops here."
    ));
    info
}


fn command_line(name: &str, out: &mut Compiled, tabs: Tabs) -> LineInfo {
    let name = name.trim();
    let upper = name.to_ascii_uppercase();
    // Take the gyro's on/off button away again, so the gyro is simply always on.
    if upper == "NO_GYRO_BUTTON" {
        out.aim.gyro_button = None;
        return LineInfo::of(LineStatus::Ok);
    }
    // A setting's name on its own prints its value in JSM's console.
    if setting_support(&upper).is_some() {
        return LineInfo::of(LineStatus::Ignored(
            "printing a setting's value is a console thing; the editor shows the config instead",
        ));
    }
    match upper.as_str() {
        // A command in the fork too, and that asymmetry is the point: it is global
        // and sticky until `RESET_MAPPINGS`, so unlike the two numbers that tune it,
        // it cannot be chorded. A config relying on that would behave differently if
        // this were quietly made a setting.
        "ONE_EURO_FILTER" => {
            out.cc.one_euro_enabled = true;
            let mut info = LineInfo::of(LineStatus::Ok);
            info.notes.push(FORK_NOTE.to_string());
            info.notes.push(
                "a command, not a setting — it stays on for the whole config and cannot be \
                 chorded. ONE_EURO_MIN_CUTOFF and ONE_EURO_SPEED_COEFF can."
                    .to_string(),
            );
            info
        }
        // Everything above this line is discarded, which is what JSM does: a config
        // is a list of console commands and this one resets them all. At the very
        // top of a file — where it usually sits — there is nothing to discard, and
        // saying so is worth more than silence, because a JSM user puts it there to
        // clear a PREVIOUS config and here there is no previous config to clear.
        "RESET_MAPPINGS" => {
            let had_anything = !out.bindings.is_empty()
                || !out.modeshifts.is_empty()
                || out.settings != Settings::default()
                || out.aim != super::aim::Settings::default()
                || out.cc != super::cc::Settings::default();
            out.bindings.clear();
            out.modeshifts.clear();
            out.mentioned.clear();
            out.timings = Timings::default();
            out.settings = Settings::default();
            out.aim = super::aim::Settings::default();
            out.pad = super::pad::Settings::default();
            out.motion = super::motion::Settings::default();
            out.touch = super::touch::Settings::default();
            out.fb = super::feedback::Settings::default();
            out.cc = super::cc::Settings::default();
            let mut info = LineInfo::of(LineStatus::Ok);
            if !had_anything {
                info.notes.push(
                    "nothing above it to reset — this module compiles each tab on its own, so \
                     there is never a previous config left over. Harmless, and kept so the file \
                     still matches what JSM would run."
                        .to_string(),
                );
            }
            info
        }
        "CALCULATE_REAL_WORLD_CALIBRATION" => LineInfo::of(LineStatus::Ignored(
            "FlexInput measures real-world sensitivity in the RWS Aim module — \
             set REAL_WORLD_CALIBRATION here from what it tells you",
        )),
        // On its own line JSM runs this as the config loads, taking however the pad
        // happens to be held right then as the motion stick's centre. Quoted as a
        // binding it is far more useful — press a button to re-centre — and that
        // path runs through `Out::Command`.
        "SET_MOTION_STICK_NEUTRAL" => {
            out.neutral_at_load = true;
            let mut info = LineInfo::of(LineStatus::Ok);
            info.notes.push(
                "runs once, as the config loads, so it takes however the pad is being held                  then. Bind it to a button instead (`HOME = \"SET_MOTION_STICK_NEUTRAL\"`) to                  re-centre whenever you like."
                    .to_string(),
            );
            info
        }
        "RESTART_GYRO_CALIBRATION" | "FINISH_GYRO_CALIBRATION" | "CALIBRATE_TRIGGERS" => {
            LineInfo::of(LineStatus::Ignored(WHY_DEVICE_CARD))
        }
        "RECONNECT_CONTROLLERS" | "MERGE" | "SPLIT" => LineInfo::of(LineStatus::Ignored(
            "FlexInput tracks connected controllers itself",
        )),
        "README" | "HELP" | "CLEAR" | "QUIT" | "SLEEP" => LineInfo::of(LineStatus::Ignored(
            "a JSM console command with nothing to do here",
        )),
        "WHITELIST_SHOW" | "WHITELIST_ADD" | "WHITELIST_REMOVE" => LineInfo::of(
            LineStatus::Ignored("FlexInput hides pads through HidHide, in Settings"),
        ),
        _ => {
            // JSM loads a config by naming its file, and a bare name loads it right
            // there — so everything in it applies as if pasted in. We resolve the
            // name to a tab and fold that tab's compiled result in at this point.
            match layer_target(name, tabs) {
                Some(Ok(tab)) => include_tab(&tab, out, tabs),
                Some(Err(missing)) => LineInfo::of(LineStatus::Error(missing)),
                None => LineInfo::of(LineStatus::Error(format!(
                    "`{name}` isn't a button, setting or command"
                ))),
            }
        }
    }
}

fn setting_line(name: &str, rhs: &str, support: Support, out: &mut Compiled) -> LineInfo {
    apply_setting(
        name,
        rhs,
        support,
        Knobs {
            timings: &mut out.timings,
            settings: &mut out.settings,
            aim: &mut out.aim,
            pad: &mut out.pad,
            motion: &mut out.motion,
            touch: &mut out.touch,
            fb: &mut out.fb,
            cc: &mut out.cc,
        },
    )
}

/// `button,SETTING = value`: the setting takes that value while the button is
/// held. The value is checked here, against a throwaway copy of the settings, so
/// the line can be called good or bad now rather than at run time.
fn modeshift_line(
    chord: Btn,
    name: &str,
    rhs: &str,
    support: Support,
    out: &mut Compiled,
) -> LineInfo {
    let mut t = out.timings;
    let mut a = out.settings;
    let mut m = out.aim;
    let mut p = out.pad;
    let mut mo = out.motion;
    let mut tp = out.touch;
    let mut fbk = out.fb;
    let mut ccs = out.cc;
    let info = apply_setting(
        name,
        rhs,
        support,
        Knobs {
            timings: &mut t,
            settings: &mut a,
            aim: &mut m,
            pad: &mut p,
            motion: &mut mo,
            touch: &mut tp,
            fb: &mut fbk,
            cc: &mut ccs,
        },
    );
    // A setting a later phase owns says so, and the modeshift waits with it.
    if matches!(info.status, LineStatus::Ok) {
        out.modeshifts.push(Modeshift {
            chord,
            support,
            name: name.to_string(),
            value: rhs.to_string(),
        });
    }
    info
}

/// Apply one `SETTING = value` line to a set of settings. The same code serves a
/// plain line (writing the config's base settings) and a modeshift (writing a
/// copy while its chord is held), so a value can only ever be read one way.
/// Everything one setting line might change, borrowed together. A struct rather
/// than a parameter list because each phase adds a group, and six positional
/// `&mut`s at four call sites is how they get passed in the wrong order.
pub(crate) struct Knobs<'a> {
    pub timings: &'a mut Timings,
    pub settings: &'a mut Settings,
    pub aim: &'a mut super::aim::Settings,
    pub pad: &'a mut super::pad::Settings,
    pub motion: &'a mut super::motion::Settings,
    pub touch: &'a mut super::touch::Settings,
    pub fb: &'a mut super::feedback::Settings,
    pub cc: &'a mut super::cc::Settings,
}

pub(crate) fn apply_setting(name: &str, rhs: &str, support: Support, k: Knobs<'_>) -> LineInfo {
    let Knobs { timings, settings, aim, pad, motion, touch, fb, cc } = k;
    match support {
        Support::Analog(which) => analog_setting(name, rhs, which, settings),
        Support::Aim(which) => aim_setting(name, rhs, which, aim),
        Support::Pad(which) => pad_setting(name, rhs, which, pad),
        Support::Motion(which) => {
            motion_setting(name, rhs, which, motion, settings, touch)
        }
        Support::Fb(which) => fb_setting(name, rhs, which, fb),
        Support::Cc(which) => cc_setting(name, rhs, which, cc, motion),
        Support::Timing(which) => {
            let Some(ms) = rhs
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<f32>().ok())
            else {
                return LineInfo::of(LineStatus::Error(format!(
                    "`{name}` wants a number of milliseconds"
                )));
            };
            if ms < 0.0 {
                return LineInfo::of(LineStatus::Error(format!("`{name}` can't be negative")));
            }
            let secs = ms / 1000.0;
            match which {
                TimingId::Hold => timings.hold = secs,
                TimingId::Turbo => timings.turbo = secs,
                TimingId::Sim => timings.sim = secs,
                TimingId::Double => timings.double = secs,
            }
            LineInfo::of(LineStatus::Ok)
        }
        Support::Pending(phase) => LineInfo::of(LineStatus::Pending(phase)),
        Support::Ignored(why) => LineInfo::of(LineStatus::Ignored(why)),
    }
}

fn binding_line(
    first: &str,
    combo: Option<(char, String)>,
    rhs: &str,
    line: usize,
    out: &mut Compiled,
    tabs: Tabs,
) -> LineInfo {
    let Some(btn) = Btn::from_name(first) else {
        return LineInfo::of(LineStatus::Error(format!(
            "`{first}` isn't a button, setting or command"
        )));
    };
    let trigger = match &combo {
        None => Trigger::Simple(btn),
        Some((op, second)) => {
            let Some(other) = Btn::from_name(second) else {
                return LineInfo::of(LineStatus::Error(format!("`{second}` isn't a button")));
            };
            match op {
                ',' if other == btn => Trigger::Double(btn),
                ',' => Trigger::Chord {
                    chord: btn,
                    btn: other,
                },
                '+' => Trigger::Sim(btn, other),
                '*' => Trigger::Diag(btn, other),
                _ => return LineInfo::of(LineStatus::Error(format!("`{op}` isn't a combo"))),
            }
        }
    };

    let (steps, notes) = match parse_mapping(rhs, tabs) {
        Ok(v) => v,
        Err(e) => return LineInfo::of(LineStatus::Error(e)),
    };

    out.mentioned.insert(btn);
    if let Some((_, second)) = &combo {
        if let Some(other) = Btn::from_name(second) {
            out.mentioned.insert(other);
        }
    }

    // Every one of JSM's buttons is readable as of phase 6 — the triggers and
    // sticks, the motion stick, lean, and the touchpad's grid and sticks — so no
    // binding waits on its *source* any more. What a line can still be waiting for
    // is its output, which `unsupported` below covers.
    let pending: Option<&'static str> = None;
    let unsupported: Vec<String> = steps
        .iter()
        .filter_map(|s| match &s.out {
            Out::Unsupported { name, why } => Some(format!("{name}: {why}")),
            _ => None,
        })
        .collect();
    let pending_out: Option<&'static str> = None;
    // Recalibrating mid-game is the device card's job here, so a binding that
    // asks for it still runs — it just doesn't recalibrate.
    let mut notes = notes;
    // A button JSM (or its fork) names that nothing on our bus reports. The binding
    // compiles — the name is real — but it can never fire, and the line says why.
    for btn in [
        match trigger {
            Trigger::Simple(b) | Trigger::Double(b) => Some(b),
            Trigger::Chord { chord, .. } => Some(chord),
            Trigger::Sim(a, _) | Trigger::Diag(a, _) => Some(a),
        },
        match trigger {
            Trigger::Chord { btn, .. } => Some(btn),
            Trigger::Sim(_, b) | Trigger::Diag(_, b) => Some(b),
            _ => None,
        },
    ]
    .into_iter()
    .flatten()
    {
        if let BtnSource::Absent(why) = btn.source() {
            let note = format!("`{}` never fires here — {why}", btn.name());
            if !notes.contains(&note) {
                notes.push(note);
            }
        }
    }
    if steps.iter().any(|s| s.out == Out::Calibrate) {
        notes.push("CALIBRATE does nothing here — the device card owns gyro calibration".into());
    }
    // A console command JSM has and this module doesn't run is not an error — JSM
    // knows the name and so do we — but the line has to say it will do nothing, and
    // why. The reasons are the same ones a bare command line gives.
    let mut inert = 0;
    for step in &steps {
        if let Out::Command(name) = &step.out {
            match command_does_nothing(name) {
                Some(why) => {
                    inert += 1;
                    notes.push(format!("`{name}` does nothing here — {why}"));
                }
                None => {}
            }
        }
    }
    // A pad output goes nowhere unless a virtual pad is wired downstream of this
    // module, and it goes to EVERY pad wired there — `X_` and `PS_` are two names
    // for one pin, as they are in JSM. The module can't see the patch, so the line
    // says both.
    if steps.iter().any(|s| matches!(&s.out, Out::Pin(p) | Out::Pulse(p)
        if super::names::is_pad_pin(p)))
    {
        notes.push(PAD_NOTE.into());
    }

    let status = if let Some(phase) = pending {
        LineStatus::Pending(phase)
    } else if inert > 0 && inert == steps.len() {
        LineStatus::Ignored("every command on this line is one FlexInput owns itself")
    } else if !unsupported.is_empty()
        && steps
            .iter()
            .all(|s| matches!(s.out, Out::Unsupported { .. }))
    {
        LineStatus::Ignored("nothing here reaches our keyboard sink yet")
    } else if let Some(phase) = pending_out {
        LineStatus::Pending(phase)
    } else {
        // A binding is an ASSIGNMENT, so a second one for the same trigger replaces
        // the first — that is what JSM does (`mappings[button] = value`), and it is
        // the whole basis of the "set a default, then override it" shape a config
        // with includes or a `RESET_MAPPINGS` is written in. Appending and taking
        // the first match would silently keep the line the author meant to replace.
        if let Some(prev) = out.bindings.iter().position(|b| b.trigger == trigger) {
            let old_line = out.bindings[prev].line;
            out.bindings.remove(prev);
            // Say so on the line that lost, so a config with two bindings for one
            // button doesn't look like it has two.
            if let Some(info) = out.lines.get_mut(old_line) {
                info.notes.push(format!(
                    "replaced by the binding on line {} — a second one for the same trigger                      wins, as in JSM",
                    line + 1
                ));
            }
        }
        out.bindings.push(Binding {
            trigger,
            steps: steps.clone(),
            line,
        });
        LineStatus::Ok
    };
    LineInfo {
        status,
        notes: notes.into_iter().chain(unsupported).collect(),
    }
}

/// Note, on every line that reads a button this pad doesn't report, that it can't
/// fire here. A config is written for whatever pad the author had, so this is the
/// device's business rather than the config's — hence a note, not a status. Only
/// the buttons this module already reads are checked: the rest say "pending",
/// which is the truer thing to say about them.
///
/// `available` is the pins the device has published. Pass an empty set for "not
/// known yet" and nothing is claimed.
pub fn note_inputs_this_pad_lacks(cfg: &mut Compiled, available: &HashSet<String>) {
    if available.is_empty() {
        return;
    }
    let mut notes: Vec<(usize, String)> = Vec::new();
    for b in &cfg.bindings {
        let buttons = match b.trigger {
            Trigger::Simple(x) | Trigger::Double(x) => vec![x],
            Trigger::Chord { chord, btn } => vec![chord, btn],
            Trigger::Sim(x, y) | Trigger::Diag(x, y) => vec![x, y],
        };
        for btn in buttons {
            let needed: Vec<&str> = match btn.source() {
                BtnSource::Pin(p) => vec![p],
                // Either form will do — a pad with only one of them still works.
                BtnSource::Trigger { analog, digital } => {
                    if available.contains(analog) || available.contains(digital) {
                        continue;
                    }
                    vec![analog, digital]
                }
                BtnSource::TriggerFull { analog } => vec![analog],
                BtnSource::Stick { stick, .. } => match stick {
                    StickId::Left => vec!["left_stick"],
                    StickId::Right => vec!["right_stick"],
                },
                // Sources that aren't live yet already say so on the line.
                _ => continue,
            };
            if needed.iter().any(|p| available.contains(*p)) {
                continue;
            }
            let note = format!(
                "this pad doesn't report `{}`, so `{}` never fires on it",
                needed.join("` or `"),
                btn.name()
            );
            notes.push((b.line, note));
        }
    }
    for (line, note) in notes {
        if let Some(info) = cfg.lines.get_mut(line) {
            if !info.notes.contains(&note) {
                info.notes.push(note);
            }
        }
    }
}

// ── the left-hand side ───────────────────────────────────────────────────────

/// Split a left-hand side into its first token and an optional combo operator
/// plus second token. JSM allows exactly one operator per line, and `+` / `-`
/// are button names themselves, so a token is an optional sign followed by word
/// characters (JSM's `[+-]?\w*`).
fn split_combo(lhs: &str) -> Option<(String, Option<(char, String)>)> {
    let bytes: Vec<char> = lhs.chars().collect();
    let mut i = 0;
    let first = read_token(&bytes, &mut i)?;
    while i < bytes.len() && bytes[i].is_whitespace() {
        i += 1;
    }
    if i >= bytes.len() {
        return Some((first, None));
    }
    let op = bytes[i];
    if !matches!(op, ',' | '+' | '*') {
        return None;
    }
    i += 1;
    while i < bytes.len() && bytes[i].is_whitespace() {
        i += 1;
    }
    let second = read_token(&bytes, &mut i)?;
    while i < bytes.len() && bytes[i].is_whitespace() {
        i += 1;
    }
    if i < bytes.len() {
        return None;
    }
    Some((first, Some((op, second))))
}

fn read_token(chars: &[char], i: &mut usize) -> Option<String> {
    while *i < chars.len() && chars[*i].is_whitespace() {
        *i += 1;
    }
    let mut tok = String::new();
    if *i < chars.len() && matches!(chars[*i], '+' | '-') {
        tok.push(chars[*i]);
        *i += 1;
    }
    while *i < chars.len() && (chars[*i].is_alphanumeric() || chars[*i] == '_') {
        tok.push(chars[*i]);
        *i += 1;
    }
    (!tok.is_empty()).then_some(tok)
}

// ── the right-hand side ──────────────────────────────────────────────────────

/// Parse a binding's value into its steps, plus any notes about lossy names.
///
/// Event modifiers default the way JSM's parser does: the only key of a binding
/// follows the press, the first of several is the tap and the second the hold,
/// and a third needs saying which event it wants.
pub(crate) fn parse_mapping(rhs: &str, tabs: Tabs) -> Result<(Vec<Step>, Vec<String>), String> {
    let chars: Vec<char> = rhs.chars().collect();
    let mut i = 0;
    let mut steps: Vec<Step> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }

        let action = match chars[i] {
            '!' => {
                i += 1;
                ActionMod::Instant
            }
            '^' => {
                i += 1;
                ActionMod::Toggle
            }
            '-' if i + 1 < chars.len() && !chars[i + 1].is_whitespace() => {
                i += 1;
                ActionMod::Release
            }
            _ => ActionMod::None,
        };

        // `^` and `!` are consumed unconditionally, so a value that ends right
        // after one (`S = ^`) would run past the end — name it as an error
        // instead of panicking (the editor compiles on every keystroke).
        if i >= chars.len() {
            return Err("a modifier needs a key after it".into());
        }

        // The key itself: a quoted command, a word, or a single punctuation mark.
        let key: String;
        let mut command = false;
        if chars[i] == '"' {
            let mut j = i + 1;
            let mut inner = String::new();
            while j < chars.len() && chars[j] != '"' {
                inner.push(chars[j]);
                j += 1;
            }
            if j >= chars.len() {
                return Err("a quoted command is missing its closing quote".into());
            }
            i = j + 1;
            key = inner;
            command = true;
        } else if chars[i].is_alphanumeric() || chars[i] == '_' {
            let mut word = String::new();
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                word.push(chars[i]);
                i += 1;
            }
            // JSM's key pattern ends in a digit or a capital, so a trailing '_'
            // is the hold event modifier — hand it back (`RMOUSE_`). Underscores
            // inside a name stay (`CAPS_LOCK`, `GYRO_OFF`, `X_A`).
            if word.len() > 1 && word.ends_with('_') {
                word.pop();
                i -= 1;
            }
            key = word;
        } else {
            key = chars[i].to_string();
            i += 1;
        }

        let explicit_event = match chars.get(i) {
            Some('\\') => {
                i += 1;
                Some(EventMod::Start)
            }
            Some('/') => {
                i += 1;
                Some(EventMod::Release)
            }
            Some('\'') => {
                i += 1;
                Some(EventMod::Tap)
            }
            Some('_') => {
                i += 1;
                Some(EventMod::Hold)
            }
            Some('+') => {
                i += 1;
                Some(EventMod::Turbo)
            }
            _ => None,
        };
        let rest_is_empty = chars[i..].iter().all(|c| c.is_whitespace());

        let mut action = action;
        let out = if command {
            // A console command has no key to release, so it fires and is done.
            if action == ActionMod::None {
                action = ActionMod::Instant;
            }
            if action != ActionMod::Instant {
                return Err("a command in quotes can only be instant".into());
            }
            // A quoted config file name is a layer switch: JSM loads that file,
            // we switch to the tab of that name. Anything else is a console
            // command, run as JSM would run it.
            match layer_target(&key, tabs) {
                Some(Ok(tab)) => Out::Layer(tab),
                Some(Err(missing)) => return Err(missing),
                None if key.eq_ignore_ascii_case("RESET_MAPPINGS") => Out::Reset,
                None => Out::Command(key.clone()),
            }
        } else {
            let Some(found) = out_from_name(&key) else {
                return Err(format!(
                    "`{key}` isn't a key, button or action JSM can bind"
                ));
            };
            if let Some(note) = found.note {
                notes.push(note);
            }
            found.out
        };

        let event = match explicit_event {
            Some(e) => e,
            None => match steps.len() {
                0 if rest_is_empty => EventMod::Start,
                0 => EventMod::Tap,
                1 => EventMod::Hold,
                _ => return Err(
                    "a third key needs an event modifier (\\ press, / release, ' tap, _ hold, + turbo)".into()),
            },
        };
        // JSM: calibration on a tap or release only makes sense as a toggle.
        if out == Out::Calibrate
            && action == ActionMod::None
            && matches!(event, EventMod::Tap | EventMod::Release)
        {
            action = ActionMod::Toggle;
        }
        if event == EventMod::Release && action == ActionMod::None {
            return Err(
                "a key on release needs an action modifier (^ toggle, ! instant, - release)".into(),
            );
        }
        steps.push(Step { out, action, event });
    }
    if steps.is_empty() {
        return Err("this binding has no keys".into());
    }
    Ok((steps, notes))
}

// ── trigger and stick settings ───────────────────────────────────────────────

/// Which analog setting a line sets, and for which side.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum AnalogId {
    Threshold,
    SkipDelay,
    TriggerMode(bool),
    StickMode(Side),
    Ring(Side),
    DeadzoneInner(Side),
    DeadzoneOuter(Side),
    Axis(Side),
    ScrollSens,
    Orientation,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Side {
    Left,
    Right,
    Both,
}

/// Apply one trigger or stick setting. A value a later phase owns leaves the
/// setting alone and says so, rather than reading as an error.
fn analog_setting(name: &str, rhs: &str, which: AnalogId, s: &mut Settings) -> LineInfo {
    let wants = |what: &str| LineInfo::of(LineStatus::Error(format!("`{name}` wants {what}")));
    let each = |s: &mut Settings, side: Side, f: &dyn Fn(&mut super::analog::StickCfg)| {
        if side != Side::Right {
            f(&mut s.left);
        }
        if side != Side::Left {
            f(&mut s.right);
        }
    };
    let value = rhs
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    match which {
        AnalogId::Threshold => match rhs
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<f32>().ok())
        {
            // JSM's own sentinel: anything below zero means hair trigger.
            Some(v) => {
                s.threshold = v;
                LineInfo::of(LineStatus::Ok)
            }
            None => wants("a number between 0 and 1, or -1 for a hair trigger"),
        },
        AnalogId::SkipDelay => match rhs
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<f32>().ok())
        {
            Some(v) if v >= 0.0 => {
                s.skip_delay = v / 1000.0;
                LineInfo::of(LineStatus::Ok)
            }
            _ => wants("a number of milliseconds"),
        },
        AnalogId::TriggerMode(right) => match trigger_mode(&value) {
            Parsed::Run(m) => {
                if right {
                    s.zr = m;
                } else {
                    s.zl = m;
                }
                let mut info = LineInfo::of(LineStatus::Ok);
                if m.pad_side().is_some() {
                    info.notes.push(PAD_NOTE.to_string());
                    info.notes.push(
                        "this trigger's own bindings stop running, as in JSM — its pull goes                          straight to the pad. It still works as a chord for a modeshift."
                            .to_string(),
                    );
                }
                info
            }
            Parsed::Later(phase) => LineInfo::of(LineStatus::Pending(phase)),
            Parsed::Refused(why) => LineInfo::of(LineStatus::Error(why.to_string())),
            Parsed::Unknown => wants(
                "NO_FULL, NO_SKIP, NO_SKIP_EXCLUSIVE, MUST_SKIP, MAY_SKIP, MUST_SKIP_R, \
                 MAY_SKIP_R, X_LT, X_RT, PS_L2 or PS_R2",
            ),
        },
        AnalogId::StickMode(side) => match stick_mode(&value) {
            Parsed::Run((mode, ring)) => {
                each(s, side, &|c| {
                    c.mode = mode;
                    // JSM's INNER_RING / OUTER_RING stick modes set the ring mode too.
                    if let Some(r) = ring {
                        c.ring = r;
                    }
                });
                let mut info = LineInfo::of(LineStatus::Ok);
                if mode.pads() {
                    info.notes.push(PAD_NOTE.to_string());
                }
                info
            }
            Parsed::Later(phase) => {
                // The stick is a mouse or a virtual stick from here on: remember
                // that much, so its directions know they won't fire.
                each(s, side, &|c| c.mode = StickMode::Elsewhere);
                LineInfo::of(LineStatus::Pending(phase))
            }
            Parsed::Refused(why) => LineInfo::of(LineStatus::Error(why.to_string())),
            Parsed::Unknown => wants("a stick mode JSM knows"),
        },
        AnalogId::Ring(side) => match value.as_str() {
            "INNER" => {
                each(s, side, &|c| c.ring = RingMode::Inner);
                LineInfo::of(LineStatus::Ok)
            }
            "OUTER" => {
                each(s, side, &|c| c.ring = RingMode::Outer);
                LineInfo::of(LineStatus::Ok)
            }
            _ => wants("INNER or OUTER"),
        },
        AnalogId::DeadzoneInner(side) | AnalogId::DeadzoneOuter(side) => {
            let inner = matches!(which, AnalogId::DeadzoneInner(_));
            match rhs
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<f32>().ok())
            {
                Some(v) if (0.0..1.0).contains(&v) => {
                    each(s, side, &|c| {
                        if inner {
                            c.inner_dz = v
                        } else {
                            c.outer_dz = v
                        }
                    });
                    LineInfo::of(LineStatus::Ok)
                }
                _ => wants("a number between 0 and 1"),
            }
        }
        AnalogId::Axis(side) => {
            // One sign for both axes, or one each (JSM's `AxisSignPair`).
            let mut signs = rhs.split_whitespace().map(axis_sign);
            let (Some(Some(x)), y) = (signs.next(), signs.next()) else {
                return wants("STANDARD or INVERTED (or 1 / -1), once or twice");
            };
            let y = match y {
                None => x,
                Some(Some(v)) => v,
                Some(None) => return wants("STANDARD or INVERTED (or 1 / -1), once or twice"),
            };
            each(s, side, &|c| {
                c.invert_x = x;
                c.invert_y = y;
            });
            LineInfo::of(LineStatus::Ok)
        }
        AnalogId::ScrollSens => match rhs
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<f32>().ok())
        {
            Some(v) if v > 0.0 => {
                each(s, Side::Both, &|c| c.scroll_sens = v);
                LineInfo::of(LineStatus::Ok)
            }
            _ => wants("a number of degrees per scroll notch"),
        },
        AnalogId::Orientation => match value.as_str() {
            "FORWARD" => {
                s.orientation = Orientation::Forward;
                LineInfo::of(LineStatus::Ok)
            }
            "LEFT" => {
                s.orientation = Orientation::Left;
                LineInfo::of(LineStatus::Ok)
            }
            "RIGHT" => {
                s.orientation = Orientation::Right;
                LineInfo::of(LineStatus::Ok)
            }
            "BACKWARD" => {
                s.orientation = Orientation::Backward;
                LineInfo::of(LineStatus::Ok)
            }
            "JOYCON_SIDEWAYS" => LineInfo::of(LineStatus::Ignored(
                "FlexInput treats each Joy-Con as its own device — say LEFT or RIGHT",
            )),
            _ => wants("FORWARD, LEFT, RIGHT or BACKWARD"),
        },
    }
}

/// A value we run, one a later phase owns, or one JSM doesn't know either.
enum Parsed<T> {
    Run(T),
    Later(&'static str),
    /// Understood, and never going to work here — say why, in full.
    Refused(&'static str),
    Unknown,
}

fn trigger_mode(v: &str) -> Parsed<TriggerMode> {
    use TriggerMode::*;
    Parsed::Run(match v {
        "NO_FULL" => NoFull,
        "NO_SKIP" => NoSkip,
        "NO_SKIP_EXCLUSIVE" => NoSkipExclusive,
        "MUST_SKIP" => MustSkip,
        "MAY_SKIP" => MaySkip,
        "MUST_SKIP_R" => MustSkipR,
        "MAY_SKIP_R" => MaySkipR,
        // These send the trigger's position to a virtual pad's trigger.
        "X_LT" | "PS_L2" => Pad(0),
        "X_RT" | "PS_R2" => Pad(1),
        _ => return Parsed::Unknown,
    })
}

fn stick_mode(v: &str) -> Parsed<(StickMode, Option<RingMode>)> {
    Parsed::Run(match v {
        "NO_MOUSE" => (StickMode::NoMouse, None),
        "SCROLL_WHEEL" => (StickMode::ScrollWheel, None),
        "INNER_RING" => (StickMode::NoMouse, Some(RingMode::Inner)),
        "OUTER_RING" => (StickMode::NoMouse, Some(RingMode::Outer)),
        "AIM" => (StickMode::Aim, None),
        "FLICK" => (StickMode::Flick, None),
        "FLICK_ONLY" => (StickMode::FlickOnly, None),
        "ROTATE_ONLY" => (StickMode::RotateOnly, None),
        "MOUSE_AREA" => (StickMode::MouseArea, None),
        "MOUSE_RING" => return Parsed::Later(PHASE_ABSOLUTE),
        "HYBRID_AIM" => return Parsed::Later(PHASE_HYBRID),
        // ── the virtual pad's own sticks ──────────────────────────────────────
        "LEFT_STICK" => (StickMode::VirtualStick(0), None),
        "RIGHT_STICK" => (StickMode::VirtualStick(1), None),
        "LEFT_ANGLE_TO_X" => (StickMode::AngleToAxis(0, true), None),
        "LEFT_ANGLE_TO_Y" => (StickMode::AngleToAxis(0, false), None),
        "RIGHT_ANGLE_TO_X" => (StickMode::AngleToAxis(1, true), None),
        "RIGHT_ANGLE_TO_Y" => (StickMode::AngleToAxis(1, false), None),
        "LEFT_WIND_X" => (StickMode::Wind(0), None),
        "RIGHT_WIND_X" => (StickMode::Wind(1), None),
        // JSM refuses these on a thumbstick — they are the motion stick's alone,
        // and it says so rather than accepting the line and doing nothing. Saying
        // the same thing here is the honest answer: no later phase will make this
        // work on a thumbstick, so it must not read as pending.
        "LEFT_STEER_X" | "RIGHT_STEER_X" => return Parsed::Refused(
            "only MOTION_STICK_MODE can steer; a thumbstick wants LEFT_WIND_X or ANGLE_TO_X",
        ),
        _ => return Parsed::Unknown,
    })
}

/// JSM writes an inverted axis as `INVERTED` or `-1`.
fn axis_sign(v: &str) -> Option<bool> {
    match v.to_ascii_uppercase().as_str() {
        "STANDARD" | "1" => Some(false),
        "INVERTED" | "-1" => Some(true),
        _ => None,
    }
}

// ── gyro, stick aim and flick settings ───────────────────────────────────────

/// Which aiming setting a line sets.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum AimId {
    /// `GYRO_SENS` sets both ends of the ramp at once.
    GyroSens,
    MinSens,
    MaxSens,
    MinThreshold,
    MaxThreshold,
    /// The sign of one mouse axis (`GYRO_AXIS_X` / `GYRO_AXIS_Y`).
    AxisSign(bool),
    /// Which gyro axes feed one mouse axis.
    FromAxis(bool),
    CutoffSpeed,
    CutoffRecovery,
    SmoothThreshold,
    SmoothTime,
    TrackballDecay,
    RealWorldCalibration,
    InGameSens,
    /// `GYRO_ON` (true) / `GYRO_OFF` (false).
    GyroButton(bool),
    StickSens,
    StickPower,
    StickAccelRate,
    StickAccelCap,
    FlickTime,
    FlickTimeExponent,
    FlickSnapMode,
    FlickSnapStrength,
    FlickDeadzoneAngle,
    RotateSmoothOverride,
    MouseRingRadius,
}

fn aim_setting(name: &str, rhs: &str, which: AimId, s: &mut super::aim::Settings) -> LineInfo {
    let wants = |what: &str| LineInfo::of(LineStatus::Error(format!("`{name}` wants {what}")));
    let num = || {
        rhs.split_whitespace()
            .next()
            .and_then(|v| v.parse::<f32>().ok())
    };
    let value = rhs
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    let ok = LineInfo::of(LineStatus::Ok);
    match which {
        // A pair setting takes one number for both axes, or one each.
        AimId::GyroSens | AimId::MinSens | AimId::MaxSens | AimId::StickSens => {
            let Some(pair) = float_pair(rhs) else {
                return wants("one or two numbers");
            };
            match which {
                AimId::GyroSens => {
                    s.min_sens = pair;
                    s.max_sens = pair;
                }
                AimId::MinSens => s.min_sens = pair,
                AimId::MaxSens => s.max_sens = pair,
                _ => s.stick_sens = pair,
            }
            ok
        }
        AimId::MinThreshold
        | AimId::MaxThreshold
        | AimId::CutoffSpeed
        | AimId::CutoffRecovery
        | AimId::SmoothThreshold
        | AimId::SmoothTime
        | AimId::TrackballDecay
        | AimId::RealWorldCalibration
        | AimId::InGameSens
        | AimId::StickPower
        | AimId::StickAccelRate
        | AimId::StickAccelCap
        | AimId::FlickTime
        | AimId::FlickTimeExponent
        | AimId::FlickSnapStrength
        | AimId::FlickDeadzoneAngle
        | AimId::RotateSmoothOverride
        | AimId::MouseRingRadius => {
            let Some(v) = num() else {
                return wants("a number");
            };
            match which {
                AimId::MinThreshold => s.min_threshold = v,
                AimId::MaxThreshold => s.max_threshold = v,
                AimId::CutoffSpeed => s.cutoff_speed = v,
                AimId::CutoffRecovery => s.cutoff_recovery = v,
                AimId::SmoothThreshold => s.smooth_threshold = v,
                AimId::SmoothTime => s.smooth_time = v,
                AimId::TrackballDecay => s.trackball_decay = v,
                AimId::RealWorldCalibration => {
                    if v <= 0.0 {
                        // It is counts per DEGREE, not per turn — worth getting right
                        // in the message, since a reader who takes it for per-turn is
                        // 360 times out.
                        return wants(
                            "a number above zero — it is how many mouse counts the game turns                              the camera by for each degree the pad rotates",
                        );
                    }
                    s.real_world_calibration = v;
                }
                AimId::InGameSens => s.in_game_sens = v,
                AimId::StickPower => s.stick_power = v,
                AimId::StickAccelRate => s.stick_accel_rate = v,
                AimId::StickAccelCap => s.stick_accel_cap = v,
                AimId::FlickTime => s.flick_time = v,
                AimId::FlickTimeExponent => s.flick_time_exponent = v,
                AimId::FlickSnapStrength => s.flick_snap_strength = v,
                AimId::FlickDeadzoneAngle => s.flick_deadzone_angle = v,
                AimId::RotateSmoothOverride => s.rotate_smooth_override = v,
                _ => s.mouse_ring_radius = v,
            }
            // A calibration measured in-game — by FlexInput's own RWS Aim module, or
            // by JSM's `CALCULATE_REAL_WORLD_CALIBRATION` — already has the in-game
            // sensitivity baked into it, because that is what was on screen while it
            // was measured. Setting `IN_GAME_SENS` as well divides it a second time,
            // and the aim comes out exactly that many times too slow. It is the one
            // place a plain constant factor can appear, so both lines say so.
            if matches!(which, AimId::RealWorldCalibration | AimId::InGameSens) {
                let mut info = LineInfo::of(LineStatus::Ok);
                info.notes.push(
                    "REAL_WORLD_CALIBRATION is mouse counts per DEGREE, and IN_GAME_SENS                      divides it. A calibration you measured in-game already includes your                      in-game sensitivity, so leave IN_GAME_SENS at 1 for it — setting both                      makes the aim exactly IN_GAME_SENS times too slow."
                        .to_string(),
                );
                return info;
            }
            ok
        }
        AimId::AxisSign(is_y) => {
            let Some(inverted) = rhs.split_whitespace().next().and_then(axis_sign) else {
                return wants("STANDARD or INVERTED (or 1 / -1)");
            };
            let sign = if inverted { -1.0 } else { 1.0 };
            if is_y {
                s.axis_y = sign;
            } else {
                s.axis_x = sign;
            }
            ok
        }
        AimId::FromAxis(is_y) => {
            let mask = match value.as_str() {
                "NONE" => AxisMask::default(),
                "X" => AxisMask {
                    x: true,
                    ..Default::default()
                },
                "Y" => AxisMask {
                    y: true,
                    ..Default::default()
                },
                "Z" => AxisMask {
                    z: true,
                    ..Default::default()
                },
                _ => return wants("X, Y, Z or NONE"),
            };
            if is_y {
                s.mouse_y_from = mask;
            } else {
                s.mouse_x_from = mask;
            }
            ok
        }
        AimId::GyroButton(on) => {
            let source = match value.as_str() {
                "NONE" => GyroSource::Never,
                "LEFT_STICK" => GyroSource::LeftStick,
                "RIGHT_STICK" => GyroSource::RightStick,
                _ => match Btn::from_name(&value) {
                    Some(b) => GyroSource::Button(b),
                    None => return wants("a button, LEFT_STICK, RIGHT_STICK or NONE"),
                },
            };
            // `GYRO_ON = X` means off until X is held; `GYRO_OFF = X` the reverse.
            s.gyro_button = Some(GyroButton {
                source,
                always_off: on,
            });
            ok
        }
        AimId::FlickSnapMode => {
            s.flick_snap = match value.as_str() {
                "NONE" => SnapMode::None,
                "4" | "FOUR" => SnapMode::Four,
                "8" | "EIGHT" => SnapMode::Eight,
                _ => return wants("NONE, 4 or 8"),
            };
            ok
        }
    }
}

/// A `FloatXY`: one number for both, or one each.
fn float_pair(rhs: &str) -> Option<(f32, f32)> {
    let mut it = rhs.split_whitespace().map(|v| v.parse::<f32>());
    let x = it.next()?.ok()?;
    match it.next() {
        None => Some((x, x)),
        Some(Ok(y)) => Some((x, y)),
        Some(Err(_)) => None,
    }
}

// ── virtual pad output settings ───────────────────────────────────────────────

/// Which virtual-pad setting a line sets. Where a side appears it is the
/// *virtual* stick's, not the physical one's — a config can cross them over.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum PadId {
    /// `VIRTUAL_STICK_CALIBRATION`.
    Calibration,
    /// `LEFT_STICK_UNDEADZONE_INNER` and its three siblings; `true` is the right
    /// stick, and the `bool` after it distinguishes inner from outer.
    Undeadzone(bool, bool),
    Unpower(bool),
    VirtualScale(bool),
    /// `GYRO_OUTPUT` and `FLICK_STICK_OUTPUT`; `true` for the flick stick.
    Dest(bool),
    /// `ANGLE_TO_AXIS_DEADZONE_INNER` / `_OUTER`.
    AngleDeadzone(bool),
    WindRange,
    WindPower,
    UnwindRate,
}

/// Apply one virtual-pad setting.
fn pad_setting(name: &str, rhs: &str, which: PadId, p: &mut super::pad::Settings) -> LineInfo {
    use super::pad::Dest;
    let wants =
        |what: &str| LineInfo::of(LineStatus::Error(format!("`{name}` wants {what}")));
    let num = || {
        rhs.split_whitespace()
            .next()
            .and_then(|v| v.parse::<f32>().ok())
    };
    // Almost every one of these is a number in a range, so say the range once.
    let in_range = |lo: f32, hi: f32| num().filter(|v| (lo..=hi).contains(v));

    match which {
        PadId::Calibration => match num() {
            Some(v) if v > 0.0 => {
                p.calibration = v;
                LineInfo::of(LineStatus::Ok)
            }
            _ => wants("a camera speed in degrees per second, above zero"),
        },
        PadId::Undeadzone(right, inner) => match in_range(0.0, 1.0) {
            Some(v) => {
                let out = &mut p.out[right as usize];
                if inner {
                    out.undeadzone_inner = v;
                } else {
                    out.undeadzone_outer = v;
                }
                LineInfo::of(LineStatus::Ok)
            }
            None => wants("a fraction of stick travel, 0 to 1"),
        },
        PadId::Unpower(right) => match num() {
            Some(v) if v >= 0.0 => {
                p.out[right as usize].unpower = v;
                LineInfo::of(LineStatus::Ok)
            }
            _ => wants("an exponent of zero or more (zero means no curve)"),
        },
        PadId::VirtualScale(right) => match num() {
            Some(v) if v >= 0.0 => {
                p.out[right as usize].virtual_scale = v;
                LineInfo::of(LineStatus::Ok)
            }
            _ => wants("a multiplier of zero or more"),
        },
        PadId::Dest(flick) => {
            let (dest, note) = match rhs.split_whitespace().next().unwrap_or("").to_ascii_uppercase().as_str() {
                "MOUSE" => (Dest::Mouse, None),
                "LEFT_STICK" => (Dest::LeftStick, None),
                "RIGHT_STICK" => (Dest::RightStick, None),
                "PS_MOTION" => (
                    Dest::PsMotion,
                    Some(
                        "the pad's own motion passes straight through to whatever is wired \
                         downstream, so a DualSense or DS4 sink receives it — as in JSM, \
                         nothing here aims with it"
                            .to_string(),
                    ),
                ),
                _ => return wants("MOUSE, LEFT_STICK, RIGHT_STICK or PS_MOTION"),
            };
            if flick {
                p.flick_dest = dest;
            } else {
                p.gyro_dest = dest;
            }
            let mut info = LineInfo::of(LineStatus::Ok);
            if let Some(n) = note {
                info.notes.push(n);
            }
            // JSM gates its whole mouse move on `GYRO_OUTPUT` being MOUSE, so
            // sending the gyro to a stick stops an `AIM` stick from moving the
            // mouse as well. Reproduced, and said out loud here.
            if !flick && dest != Dest::Mouse {
                info.notes.push(
                    "as in JSM, this also stops a stick in `AIM` or `MOUSE_AREA` from moving \
                     the mouse — the whole mouse output goes quiet, not just the gyro's part"
                        .to_string(),
                );
            }
            if dest.side().is_some() {
                info.notes.push(PAD_NOTE.to_string());
            }
            info
        }
        PadId::AngleDeadzone(inner) => match in_range(0.0, 90.0) {
            Some(v) => {
                if inner {
                    p.angle_dz_inner = v;
                } else {
                    p.angle_dz_outer = v;
                }
                LineInfo::of(LineStatus::Ok)
            }
            None => wants("an angle in degrees, 0 to 90"),
        },
        PadId::WindRange => match num() {
            Some(v) if v > 0.0 => {
                p.wind_range = v;
                LineInfo::of(LineStatus::Ok)
            }
            _ => wants("a number of degrees above zero"),
        },
        PadId::WindPower => match num() {
            Some(v) if v >= 0.0 => {
                p.wind_power = v;
                LineInfo::of(LineStatus::Ok)
            }
            _ => wants("an exponent of zero or more (zero means no curve)"),
        },
        PadId::UnwindRate => match num() {
            Some(v) if v >= 0.0 => {
                p.unwind_rate = v;
                LineInfo::of(LineStatus::Ok)
            }
            _ => wants("a number of degrees per second, zero or more"),
        },
    }
}


// ── gravity and the touchpad ──────────────────────────────────────────────────

/// Which of phase 6's settings a line sets. They land in three places — the gyro
/// space and lean live in `motion`, the motion and touch *sticks* are ordinary
/// stick configs in `settings`, and the touchpad's own knobs are in `touch` — so
/// one handler writes to all three rather than splitting the table three ways.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum MotionId {
    Space,
    LeanThreshold,
    /// The motion stick, in the same settings a thumbstick has.
    MotionStick,
    MotionRing,
    MotionDeadzone(bool),
    MotionAxis,
    /// The touchpad itself.
    TouchpadMode,
    Grid,
    TouchpadSens,
    TouchpadDualStage,
    /// The touch stick.
    TouchStick,
    TouchRing,
    TouchRadius,
    TouchDeadzone,
    TouchAxis,
}

fn motion_setting(
    name: &str,
    rhs: &str,
    which: MotionId,
    m: &mut super::motion::Settings,
    a: &mut Settings,
    t: &mut super::touch::Settings,
) -> LineInfo {
    use super::motion::Space;
    let wants = |what: &str| LineInfo::of(LineStatus::Error(format!("`{name}` wants {what}")));
    let value = rhs.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
    let num = || rhs.split_whitespace().next().and_then(|v| v.parse::<f32>().ok());
    let ok = LineInfo::of(LineStatus::Ok);

    match which {
        MotionId::Space => {
            m.space = match value.as_str() {
                "LOCAL" => Space::Local,
                "PLAYER_TURN" => Space::PlayerTurn,
                "PLAYER_LEAN" => Space::PlayerLean,
                "WORLD_TURN" => Space::WorldTurn,
                "WORLD_LEAN" => Space::WorldLean,
                "YAW_PLUS_ROLL" => Space::YawPlusRoll,
                _ => return wants(
                    "LOCAL, PLAYER_TURN, PLAYER_LEAN, WORLD_TURN, WORLD_LEAN or YAW_PLUS_ROLL",
                ),
            };
            let mut info = ok;
            if m.space.needs_gravity() {
                info.notes.push(GRAVITY_NOTE.to_string());
            }
            info
        }
        MotionId::LeanThreshold => match num() {
            Some(v) if (0.0..=90.0).contains(&v) => {
                m.lean_threshold = v;
                ok
            }
            _ => wants("an angle in degrees, 0 to 90"),
        },
        MotionId::MotionStick | MotionId::TouchStick => {
            let touch = which == MotionId::TouchStick;
            match stick_mode(&value) {
                Parsed::Run((mode, ring)) => {
                    let cfg = if touch { &mut a.touch } else { &mut a.motion };
                    cfg.mode = mode;
                    if let Some(r) = ring {
                        cfg.ring = r;
                    }
                    let mut info = LineInfo::of(LineStatus::Ok);
                    if mode.pads() {
                        info.notes.push(PAD_NOTE.to_string());
                    }
                    if !touch {
                        info.notes.push(GRAVITY_NOTE.to_string());
                    }
                    info
                }
                // `STEER_X` is the one mode that belongs to the motion stick and
                // to nothing else, so here — and only here — it is not an error.
                Parsed::Refused(_) if !touch => {
                    a.motion.mode = match value.as_str() {
                        "LEFT_STEER_X" => StickMode::Steer(0),
                        _ => StickMode::Steer(1),
                    };
                    let mut info = LineInfo::of(LineStatus::Ok);
                    info.notes.push(PAD_NOTE.to_string());
                    info.notes.push(GRAVITY_NOTE.to_string());
                    info
                }
                Parsed::Refused(why) => LineInfo::of(LineStatus::Error(why.to_string())),
                Parsed::Later(phase) => {
                    let cfg = if touch { &mut a.touch } else { &mut a.motion };
                    cfg.mode = StickMode::Elsewhere;
                    LineInfo::of(LineStatus::Pending(phase))
                }
                Parsed::Unknown => wants("a stick mode JSM knows"),
            }
        }
        MotionId::MotionRing | MotionId::TouchRing => {
            let cfg = if which == MotionId::TouchRing { &mut a.touch } else { &mut a.motion };
            match value.as_str() {
                "INNER" => {
                    cfg.ring = RingMode::Inner;
                    ok
                }
                "OUTER" => {
                    cfg.ring = RingMode::Outer;
                    ok
                }
                _ => wants("INNER or OUTER"),
            }
        }
        // JSM states the motion stick's deadzones in DEGREES of tilt against a 180
        // degree range; the stick it builds is a fraction of that half turn, so
        // they are stored here already divided, in the units every other deadzone
        // in this module uses.
        MotionId::MotionDeadzone(inner) => match num() {
            Some(v) if (0.0..=180.0).contains(&v) => {
                if inner {
                    a.motion.inner_dz = v / 180.0;
                } else {
                    a.motion.outer_dz = 1.0 - v / 180.0;
                }
                ok
            }
            _ => wants("an angle of tilt in degrees, 0 to 180"),
        },
        MotionId::TouchDeadzone => match num() {
            Some(v) if (0.0..1.0).contains(&v) => {
                a.touch.inner_dz = v;
                ok
            }
            _ => wants("a fraction of the stick's reach, 0 to 1"),
        },
        MotionId::MotionAxis | MotionId::TouchAxis => {
            let cfg = if which == MotionId::TouchAxis { &mut a.touch } else { &mut a.motion };
            let mut it = rhs.split_whitespace();
            let Some(x) = it.next().and_then(axis_sign) else {
                return wants("STANDARD or INVERTED for each axis");
            };
            let y = match it.next() {
                None => x,
                Some(v) => match axis_sign(v) {
                    Some(y) => y,
                    None => return wants("STANDARD or INVERTED for each axis"),
                },
            };
            cfg.invert_x = x;
            cfg.invert_y = y;
            ok
        }
        MotionId::TouchpadMode => {
            use super::touch::Mode;
            t.mode = match value.as_str() {
                "GRID_AND_STICK" => Mode::GridAndStick,
                "MOUSE" => Mode::Mouse,
                "PS_TOUCHPAD" => Mode::PsTouchpad,
                _ => return wants("GRID_AND_STICK, MOUSE or PS_TOUCHPAD"),
            };
            let mut info = LineInfo::of(LineStatus::Ok);
            if t.mode == Mode::PsTouchpad {
                info.notes.push(
                    "the pad's own touchpad already passes through to whatever is wired \
                     downstream, so a DualSense or DS4 sink receives it — as in JSM, nothing \
                     here reads it"
                        .to_string(),
                );
            }
            info
        }
        MotionId::Grid => {
            let Some((x, y)) = float_pair(rhs) else {
                return wants("a number of columns and rows");
            };
            let (cols, rows) = (x.round(), y.round());
            if cols < 1.0 || rows < 1.0 || cols * rows > 25.0 {
                // The grid buttons are named `T1`…`T25`, so 25 cells is the
                // ceiling — say so rather than clamping in silence.
                return wants("a grid of 1 to 25 cells in all (the buttons are T1 to T25)");
            }
            t.grid = (cols as u8, rows as u8);
            ok
        }
        MotionId::TouchpadSens => match float_pair(rhs) {
            Some((x, y)) => {
                t.sens = (x, y);
                ok
            }
            None => wants("one or two numbers"),
        },
        MotionId::TouchRadius => match num() {
            Some(v) if v > 0.0 => {
                t.stick_radius = v;
                ok
            }
            _ => wants("a distance in touchpad points, above zero"),
        },
        MotionId::TouchpadDualStage => match trigger_mode(&value) {
            Parsed::Run(mode) => match mode.pad_side() {
                // JSM refuses the pass-through modes here, and rightly: there is
                // no analog travel on a touchpad to pass through.
                Some(_) => wants(
                    "a dual-stage mode — a touchpad has no analog travel, so X_LT and X_RT \
                     have nothing to send",
                ),
                None => {
                    a.touchpad_dual_stage = mode;
                    ok
                }
            },
            Parsed::Later(phase) => LineInfo::of(LineStatus::Pending(phase)),
            Parsed::Refused(why) => LineInfo::of(LineStatus::Error(why.to_string())),
            Parsed::Unknown => wants(
                "NO_FULL, NO_SKIP, NO_SKIP_EXCLUSIVE, MUST_SKIP, MAY_SKIP, MUST_SKIP_R or \
                 MAY_SKIP_R",
            ),
        },
    }
}

/// Said on lines that only work once the pad reports its motion sensors.
const GRAVITY_NOTE: &str = "needs the pad's accelerometer: which way is down is worked out by \
                            watching where the pad is pulled, so it takes a moment to settle \
                            after the pad connects";


// ── feedback: rumble, the light bar, the adaptive triggers ────────────────────

/// Which of phase 7's settings a line sets.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum FbId {
    Rumble,
    LightBar,
    Adaptive,
    /// `LEFT_TRIGGER_EFFECT` / `RIGHT_TRIGGER_EFFECT`; `true` is the right one.
    Effect(bool),
}

fn fb_setting(name: &str, rhs: &str, which: FbId, f: &mut super::feedback::Settings) -> LineInfo {
    use super::feedback::Effect;
    let wants = |what: &str| LineInfo::of(LineStatus::Error(format!("`{name}` wants {what}")));
    let ok = LineInfo::of(LineStatus::Ok);
    let mut words = rhs.split_whitespace();
    let first = words.next().unwrap_or("").to_ascii_uppercase();

    match which {
        FbId::Rumble => match on_off(&first) {
            Some(on) => {
                f.rumble = on;
                let mut info = ok;
                if !on {
                    info.notes.push(
                        "the game's rumble stops reaching the pad while this config runs, as in \
                         JSM — a binding can still rumble it (`SMALL_RUMBLE`, `Rhhhh`)"
                            .to_string(),
                    );
                }
                info
            }
            None => wants("ON or OFF"),
        },
        FbId::Adaptive => match on_off(&first) {
            Some(on) => {
                f.adaptive = on;
                ok
            }
            None => wants("ON or OFF"),
        },
        FbId::LightBar => match colour(&first) {
            Some(rgb) => {
                f.light_bar = rgb;
                ok
            }
            None => wants(
                "a colour name (RED, GREEN, BLUE, WHITE, BLACK, YELLOW, CYAN, MAGENTA, ORANGE, \
                 PURPLE, PINK, GREY) or a hex value like xFF8000 — not #FF8000, since `#` \
                 starts a comment and takes the value with it",
            ),
        },
        FbId::Effect(right) => {
            // JSM reads the parameters positionally, in an order that differs per
            // mode, so each arm takes exactly the numbers its mode uses.
            let mut nums = words.map(|w| w.parse::<u32>().ok());
            let mut next = |what: &str| -> Result<u8, LineInfo> {
                match nums.next() {
                    Some(Some(v)) => Ok(v.min(255) as u8),
                    _ => Err(wants(&format!("{what} after `{first}`"))),
                }
            };
            let effect = match first.as_str() {
                "ON" => Effect::Auto,
                "OFF" => Effect::Off,
                "RESISTANCE" => {
                    let (start, force) = match (next("a start zone"), next("a force")) {
                        (Ok(a), Ok(b)) => (a, b),
                        (Err(e), _) | (_, Err(e)) => return e,
                    };
                    Effect::Resistance { start, force }
                }
                "SEMI_AUTOMATIC" => {
                    let (start, end, force) =
                        match (next("a start zone"), next("an end zone"), next("a force")) {
                            (Ok(a), Ok(b), Ok(c)) => (a, b, c),
                            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return e,
                        };
                    Effect::SemiAutomatic { start, end, force }
                }
                "AUTOMATIC" => {
                    let (start, force, frequency) =
                        match (next("a start zone"), next("a force"), next("a frequency")) {
                            (Ok(a), Ok(b), Ok(c)) => (a, b, c),
                            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return e,
                        };
                    Effect::Automatic { start, force, frequency }
                }
                // The three the bus has nowhere to put. Not an error — JSM knows
                // them and so do we — but the line must say what it will actually
                // do, and name the nearest thing that works.
                "BOW" | "GALLOPING" | "MACHINE" => {
                    let nearest = match first.as_str() {
                        "BOW" => "SEMI_AUTOMATIC, which clicks between two zones",
                        "GALLOPING" => "AUTOMATIC, which vibrates from one zone on",
                        _ => "AUTOMATIC, which vibrates from one zone on",
                    };
                    let mut info = LineInfo::of(LineStatus::Ok);
                    f.trigger[right as usize] = Effect::Auto;
                    info.notes.push(format!(
                        "`{first}` needs two forces or a second frequency, and the bus carries \
                         four trigger effects — off, resistance, a click, and vibration. The \
                         trigger is left as the game set it; try {nearest}."
                    ));
                    return info;
                }
                _ => {
                    return wants(
                        "ON, OFF, RESISTANCE, SEMI_AUTOMATIC, AUTOMATIC, BOW, GALLOPING or \
                         MACHINE",
                    )
                }
            };
            f.trigger[right as usize] = effect;
            let mut info = LineInfo::of(LineStatus::Ok);
            if effect != Effect::Auto {
                info.notes.push(TRIGGER_NOTE.to_string());
            }
            info
        }
    }
}

fn on_off(v: &str) -> Option<bool> {
    match v {
        "ON" | "TRUE" | "1" => Some(true),
        "OFF" | "FALSE" | "0" => Some(false),
        _ => None,
    }
}

/// JSM takes a colour name or a hex value, written `xRRGGBB`. A bare `RRGGBB` is
/// taken too, since that is what people type — but `#RRGGBB` cannot be: `#` starts
/// a comment in a JSM config, so the rest of the line is gone before this is
/// reached.
fn colour(v: &str) -> Option<(u8, u8, u8)> {
    let named = match v {
        "BLACK" => 0x000000,
        "WHITE" => 0xFFFFFF,
        "RED" => 0xFF0000,
        "GREEN" => 0x00FF00,
        "BLUE" => 0x0000FF,
        "YELLOW" => 0xFFFF00,
        "CYAN" => 0x00FFFF,
        "MAGENTA" | "PINK" => 0xFF00FF,
        "ORANGE" => 0xFF8000,
        "PURPLE" => 0x8000FF,
        "GREY" | "GRAY" => 0x808080,
        _ => {
            let hex = v.strip_prefix('X').unwrap_or(v);
            if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            u32::from_str_radix(hex, 16).ok()?
        }
    };
    Some((
        ((named >> 16) & 0xFF) as u8,
        ((named >> 8) & 0xFF) as u8,
        (named & 0xFF) as u8,
    ))
}

/// Said on a line that shapes an adaptive trigger.
const TRIGGER_NOTE: &str = "only a DualSense has adaptive triggers; on any other pad this does \
                            nothing, and the pad has to be wired to this module for it to reach \
                            one";


// ── the custom-curve fork ─────────────────────────────────────────────────────

/// Which of the `JSM_custom_curve` fork's settings a line sets.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum CcId {
    Curve,
    NaturalVhalf,
    PowerVref,
    PowerExponent,
    SigmoidMid,
    SigmoidWidth,
    JumpTau,
    DecaySmoothing,
    OneEuroMinCutoff,
    OneEuroSpeedCoeff,
    AngleSnap,
    AngleSnapEase,
    BrakeStrength,
    BrakeThreshold,
    RollContribution,
}

fn cc_setting(
    name: &str,
    rhs: &str,
    which: CcId,
    c: &mut super::cc::Settings,
    m: &super::motion::Settings,
) -> LineInfo {
    use super::cc::Curve;
    let wants = |what: &str| LineInfo::of(LineStatus::Error(format!("`{name}` wants {what}")));
    let value = rhs.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
    let num = || rhs.split_whitespace().next().and_then(|v| v.parse::<f32>().ok());
    let ok = LineInfo::of(LineStatus::Ok);
    // Almost all of these are "a number, at least this big".
    let at_least = |lo: f32, what: &str| match num() {
        Some(v) if v >= lo => Ok(v),
        _ => Err(wants(what)),
    };

    match which {
        CcId::Curve => {
            c.curve = match value.as_str() {
                "LINEAR" => Curve::Linear,
                "NATURAL" => Curve::Natural,
                "POWER" => Curve::Power,
                "QUADRATIC" => Curve::Quadratic,
                "SIGMOID" => Curve::Sigmoid,
                "JUMP" => Curve::Jump,
                _ => return wants("LINEAR, NATURAL, POWER, QUADRATIC, SIGMOID or JUMP"),
            };
            let mut info = LineInfo::of(LineStatus::Ok);
            info.notes.push(FORK_NOTE.to_string());
            if !c.curve.uses_max_threshold() {
                // The trap worth stating out loud: three of the six curves never
                // look at the top threshold, so a config that sets one and then
                // switches curve has a setting that silently stops mattering.
                info.notes.push(format!(
                    "`{value}` takes its shape from its own settings in degrees per second and \
                     never looks at MAX_GYRO_THRESHOLD — only LINEAR, QUADRATIC and JUMP use that"
                ));
            }
            info
        }
        CcId::NaturalVhalf => match at_least(0.0, "a speed in degrees per second") {
            Ok(v) => { c.natural_vhalf = v; ok }
            Err(e) => e,
        },
        CcId::PowerVref => match at_least(0.0, "a reference speed in degrees per second") {
            Ok(v) => { c.power_vref = v; ok }
            Err(e) => e,
        },
        CcId::PowerExponent => match at_least(0.0, "an exponent of zero or more") {
            Ok(v) => { c.power_exponent = v; ok }
            Err(e) => e,
        },
        CcId::SigmoidMid => match at_least(0.0, "a speed in degrees per second") {
            Ok(v) => { c.sigmoid_mid = v; ok }
            Err(e) => e,
        },
        CcId::SigmoidWidth => match at_least(0.0, "a width in degrees per second (larger is gentler)") {
            Ok(v) => { c.sigmoid_width = v; ok }
            Err(e) => e,
        },
        CcId::JumpTau => match at_least(0.0, "a time constant of zero or more (zero is an instant step)") {
            Ok(v) => { c.jump_tau = v; ok }
            Err(e) => e,
        },
        CcId::OneEuroMinCutoff => match at_least(0.0, "a cutoff frequency in Hz") {
            Ok(v) => { c.one_euro_min_cutoff = v; ok }
            Err(e) => e,
        },
        CcId::OneEuroSpeedCoeff => match at_least(0.0, "a coefficient of zero or more") {
            Ok(v) => { c.one_euro_speed_coeff = v; ok }
            Err(e) => e,
        },
        CcId::BrakeStrength => match num() {
            Some(v) if (0.0..=1.0).contains(&v) => {
                c.brake_strength = v;
                let mut info = LineInfo::of(LineStatus::Ok);
                info.notes.push(FORK_NOTE.to_string());
                info
            }
            _ => wants("a strength from 0 to 1"),
        },
        CcId::BrakeThreshold => match at_least(0.0, "a deceleration in degrees per second") {
            Ok(v) => { c.brake_threshold = v; ok }
            Err(e) => e,
        },
        CcId::AngleSnap => match num() {
            Some(v) if (0.0..=45.0).contains(&v) => {
                c.angle_snap = v;
                let mut info = LineInfo::of(LineStatus::Ok);
                info.notes.push(FORK_NOTE.to_string());
                info
            }
            _ => wants("an angle in degrees, 0 to 45"),
        },
        CcId::AngleSnapEase => match on_off(&value) {
            Some(on) => { c.angle_snap_ease = on; ok }
            None => wants("ON or OFF"),
        },
        CcId::DecaySmoothing => match on_off(&value) {
            Some(on) => {
                c.decay_smoothing = on;
                let mut info = LineInfo::of(LineStatus::Ok);
                info.notes.push(FORK_NOTE.to_string());
                if on {
                    info.notes.push(
                        "GYRO_SMOOTH_TIME and GYRO_SMOOTH_THRESHOLD now shape a decaying \
                         smoother instead of a rolling average, so the same numbers feel \
                         different"
                            .to_string(),
                    );
                }
                info
            }
            None => wants("ON or OFF"),
        },
        CcId::RollContribution => match num() {
            Some(v) if (-100.0..=100.0).contains(&v) => {
                c.roll_contribution = v;
                let mut info = LineInfo::of(LineStatus::Ok);
                info.notes.push(FORK_NOTE.to_string());
                if m.space != super::motion::Space::YawPlusRoll {
                    info.notes.push(
                        "only does anything with `GYRO_SPACE = YAW_PLUS_ROLL`".to_string(),
                    );
                }
                info
            }
            _ => wants("a percentage from -100 to 100"),
        },
    }
}

/// Said on a line that comes from the custom-curve fork rather than from JSM itself,
/// so nobody is surprised that a stock JSM build doesn't know it.
const FORK_NOTE: &str = "from the JSM_custom_curve fork, not stock JoyShockMapper — a config using \
                         it won't load in an unmodified JSM";

// ── the settings table ───────────────────────────────────────────────────────

/// Said on every line whose output can only land on a virtual pad. A config full
/// of `X_*` bindings does nothing at all until one is wired up, which is worth
/// saying on the line rather than leaving someone to wonder.
const PAD_NOTE: &str = "needs a virtual pad wired downstream to reach anything; `X_` and `PS_`                         names are the same pin (as in JSM), so wiring decides which pad it                         reaches";

const PHASE_GYRO: &str = "gyro, flick stick and real-world calibration arrive in phase 3";
/// Gravity-referenced gyro spaces land with the motion stick, which needs the
/// same work: our accelerometer frame matched to the one JSM reasons in.
const PHASE_ABSOLUTE: &str =
    "placing the pointer outright needs an absolute mouse pin, which the bus doesn't have yet";
const PHASE_HYBRID: &str = "HYBRID_AIM arrives after the rest of aiming";

const WHY_DEVICE_CARD: &str = "the device card owns calibration in FlexInput";

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum TimingId {
    Hold,
    Turbo,
    Sim,
    Double,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Support {
    Timing(TimingId),
    Analog(AnalogId),
    Aim(AimId),
    Pad(PadId),
    Motion(MotionId),
    Fb(FbId),
    Cc(CcId),
    Pending(&'static str),
    Ignored(&'static str),
}

/// Every setting JSM knows, and what this module does with it today. Keep the
/// names in step with `SettingID` in JSM's `include/JoyShockMapper.h`.
fn setting_support(name: &str) -> Option<Support> {
    use Support::*;
    Some(match name.to_ascii_uppercase().as_str() {
        // Live now.
        "HOLD_PRESS_TIME" => Timing(TimingId::Hold),
        "TURBO_PERIOD" => Timing(TimingId::Turbo),
        "SIM_PRESS_WINDOW" => Timing(TimingId::Sim),
        "DBL_PRESS_WINDOW" => Timing(TimingId::Double),

        // Triggers and sticks.
        "TRIGGER_THRESHOLD" => Analog(AnalogId::Threshold),
        "TRIGGER_SKIP_DELAY" => Analog(AnalogId::SkipDelay),
        "ZL_MODE" => Analog(AnalogId::TriggerMode(false)),
        "ZR_MODE" => Analog(AnalogId::TriggerMode(true)),
        "LEFT_STICK_MODE" => Analog(AnalogId::StickMode(Side::Left)),
        "RIGHT_STICK_MODE" => Analog(AnalogId::StickMode(Side::Right)),
        "LEFT_RING_MODE" => Analog(AnalogId::Ring(Side::Left)),
        "RIGHT_RING_MODE" => Analog(AnalogId::Ring(Side::Right)),
        // The unprefixed deadzones set both sticks, which is how JSM's own
        // command registry wires them.
        "STICK_DEADZONE_INNER" => Analog(AnalogId::DeadzoneInner(Side::Both)),
        "STICK_DEADZONE_OUTER" => Analog(AnalogId::DeadzoneOuter(Side::Both)),
        "LEFT_STICK_DEADZONE_INNER" => Analog(AnalogId::DeadzoneInner(Side::Left)),
        "LEFT_STICK_DEADZONE_OUTER" => Analog(AnalogId::DeadzoneOuter(Side::Left)),
        "RIGHT_STICK_DEADZONE_INNER" => Analog(AnalogId::DeadzoneInner(Side::Right)),
        "RIGHT_STICK_DEADZONE_OUTER" => Analog(AnalogId::DeadzoneOuter(Side::Right)),
        "LEFT_STICK_AXIS" => Analog(AnalogId::Axis(Side::Left)),
        "RIGHT_STICK_AXIS" => Analog(AnalogId::Axis(Side::Right)),
        "SCROLL_SENS" => Analog(AnalogId::ScrollSens),
        "CONTROLLER_ORIENTATION" => Analog(AnalogId::Orientation),
        // These invert the AIM mouse output, not the stick itself.
        "STICK_AXIS_X" | "STICK_AXIS_Y" => Pending(PHASE_GYRO),

        // Gyro, aim and flick.
        "GYRO_SENS" => Aim(AimId::GyroSens),
        "MIN_GYRO_SENS" => Aim(AimId::MinSens),
        "MAX_GYRO_SENS" => Aim(AimId::MaxSens),
        "MIN_GYRO_THRESHOLD" => Aim(AimId::MinThreshold),
        "MAX_GYRO_THRESHOLD" => Aim(AimId::MaxThreshold),
        "GYRO_SPACE" => Motion(MotionId::Space),
        "GYRO_AXIS_X" => Aim(AimId::AxisSign(false)),
        "GYRO_AXIS_Y" => Aim(AimId::AxisSign(true)),
        "MOUSE_X_FROM_GYRO_AXIS" => Aim(AimId::FromAxis(false)),
        "MOUSE_Y_FROM_GYRO_AXIS" => Aim(AimId::FromAxis(true)),
        "GYRO_CUTOFF_SPEED" => Aim(AimId::CutoffSpeed),
        "GYRO_CUTOFF_RECOVERY" => Aim(AimId::CutoffRecovery),
        "GYRO_SMOOTH_THRESHOLD" => Aim(AimId::SmoothThreshold),
        "GYRO_SMOOTH_TIME" => Aim(AimId::SmoothTime),
        "TRACKBALL_DECAY" => Aim(AimId::TrackballDecay),
        "GYRO_OFF" => Aim(AimId::GyroButton(false)),
        "GYRO_ON" => Aim(AimId::GyroButton(true)),
        "REAL_WORLD_CALIBRATION" => Aim(AimId::RealWorldCalibration),
        "IN_GAME_SENS" => Aim(AimId::InGameSens),
        "STICK_SENS" => Aim(AimId::StickSens),
        "STICK_POWER" => Aim(AimId::StickPower),
        "STICK_ACCELERATION_RATE" => Aim(AimId::StickAccelRate),
        "STICK_ACCELERATION_CAP" => Aim(AimId::StickAccelCap),
        "FLICK_TIME" => Aim(AimId::FlickTime),
        "FLICK_TIME_EXPONENT" => Aim(AimId::FlickTimeExponent),
        "FLICK_SNAP_MODE" => Aim(AimId::FlickSnapMode),
        "FLICK_SNAP_STRENGTH" => Aim(AimId::FlickSnapStrength),
        "FLICK_DEADZONE_ANGLE" => Aim(AimId::FlickDeadzoneAngle),
        "ROTATE_SMOOTH_OVERRIDE" => Aim(AimId::RotateSmoothOverride),
        "MOUSE_RING_RADIUS" => Aim(AimId::MouseRingRadius),
        // The pointer-placing modes, and the aim hybrid, wait their turn.
        "SCREEN_RESOLUTION_X" | "SCREEN_RESOLUTION_Y" => Pending(PHASE_ABSOLUTE),
        "STICKLIKE_FACTOR"
        | "MOUSELIKE_FACTOR"
        | "RETURN_DEADZONE_IS_ACTIVE"
        | "RETURN_DEADZONE_ANGLE"
        | "RETURN_DEADZONE_ANGLE_CUTOFF"
        | "EDGE_PUSH_IS_ACTIVE" => Pending(PHASE_HYBRID),
        // Flick and gyro can drive a virtual stick instead of the mouse.
        "FLICK_STICK_OUTPUT" => Pad(PadId::Dest(true)),
        "GYRO_OUTPUT" => Pad(PadId::Dest(false)),
        "VIRTUAL_STICK_CALIBRATION" => Pad(PadId::Calibration),

        // Virtual pad output.
        "LEFT_STICK_UNDEADZONE_INNER" => Pad(PadId::Undeadzone(false, true)),
        "LEFT_STICK_UNDEADZONE_OUTER" => Pad(PadId::Undeadzone(false, false)),
        "RIGHT_STICK_UNDEADZONE_INNER" => Pad(PadId::Undeadzone(true, true)),
        "RIGHT_STICK_UNDEADZONE_OUTER" => Pad(PadId::Undeadzone(true, false)),
        "LEFT_STICK_UNPOWER" => Pad(PadId::Unpower(false)),
        "RIGHT_STICK_UNPOWER" => Pad(PadId::Unpower(true)),
        "LEFT_STICK_VIRTUAL_SCALE" => Pad(PadId::VirtualScale(false)),
        "RIGHT_STICK_VIRTUAL_SCALE" => Pad(PadId::VirtualScale(true)),
        "WIND_STICK_RANGE" => Pad(PadId::WindRange),
        "WIND_STICK_POWER" => Pad(PadId::WindPower),
        "UNWIND_RATE" => Pad(PadId::UnwindRate),
        "ANGLE_TO_AXIS_DEADZONE_INNER" => Pad(PadId::AngleDeadzone(true)),
        "ANGLE_TO_AXIS_DEADZONE_OUTER" => Pad(PadId::AngleDeadzone(false)),

        // Touchpad and motion stick.
        "TOUCHPAD_MODE" => Motion(MotionId::TouchpadMode),
        "GRID_SIZE" => Motion(MotionId::Grid),
        "TOUCHPAD_SENS" => Motion(MotionId::TouchpadSens),
        "TOUCHPAD_DUAL_STAGE_MODE" => Motion(MotionId::TouchpadDualStage),
        "TOUCH_STICK_MODE" => Motion(MotionId::TouchStick),
        "TOUCH_STICK_RADIUS" => Motion(MotionId::TouchRadius),
        "TOUCH_DEADZONE_INNER" => Motion(MotionId::TouchDeadzone),
        "TOUCH_RING_MODE" => Motion(MotionId::TouchRing),
        "TOUCH_STICK_AXIS" => Motion(MotionId::TouchAxis),
        "MOTION_STICK_MODE" => Motion(MotionId::MotionStick),
        "MOTION_RING_MODE" => Motion(MotionId::MotionRing),
        "MOTION_DEADZONE_INNER" => Motion(MotionId::MotionDeadzone(true)),
        "MOTION_DEADZONE_OUTER" => Motion(MotionId::MotionDeadzone(false)),
        "MOTION_STICK_AXIS" => Motion(MotionId::MotionAxis),
        "LEAN_THRESHOLD" => Motion(MotionId::LeanThreshold),

        // The JSM_custom_curve fork's additions. See `cc.rs` and the plan's phase 9.
        "ACCEL_CURVE" => Cc(CcId::Curve),
        "ACCEL_NATURAL_VHALF" => Cc(CcId::NaturalVhalf),
        "ACCEL_POWER_VREF" => Cc(CcId::PowerVref),
        "ACCEL_POWER_EXPONENT" => Cc(CcId::PowerExponent),
        "ACCEL_SIGMOID_MID" => Cc(CcId::SigmoidMid),
        "ACCEL_SIGMOID_WIDTH" => Cc(CcId::SigmoidWidth),
        "ACCEL_JUMP_TAU" => Cc(CcId::JumpTau),
        "GYRO_SMOOTHING_DECAY" => Cc(CcId::DecaySmoothing),
        "ONE_EURO_MIN_CUTOFF" => Cc(CcId::OneEuroMinCutoff),
        "ONE_EURO_SPEED_COEFF" => Cc(CcId::OneEuroSpeedCoeff),
        "GYRO_ANGLE_SNAP" => Cc(CcId::AngleSnap),
        "GYRO_ANGLE_SNAP_EASE" => Cc(CcId::AngleSnapEase),
        "DECEL_BRAKE_STRENGTH" => Cc(CcId::BrakeStrength),
        "DECEL_BRAKE_THRESHOLD" => Cc(CcId::BrakeThreshold),
        "ROLL_CONTRIBUTION" => Cc(CcId::RollContribution),
        // Two the fork has that cannot mean anything here.
        "IGNORE_GYRO_DEVICES" => Ignored(
            "it exists because JSM grabs every pad it can see; this module is handed one device \
             by the patch and cannot see a VID or PID. Don't wire that pad in, or use GYRO_OFF",
        ),
        "TELEMETRY_ENABLED" | "TELEMETRY_PORT" => Ignored(
            "the fork opens a socket so its separate GUI can draw the live curve; the editor here \
             is the GUI",
        ),

        // Feedback.
        "RUMBLE" => Fb(FbId::Rumble),
        "LIGHT_BAR" => Fb(FbId::LightBar),
        "ADAPTIVE_TRIGGER" => Fb(FbId::Adaptive),
        "LEFT_TRIGGER_EFFECT" => Fb(FbId::Effect(false)),
        "RIGHT_TRIGGER_EFFECT" => Fb(FbId::Effect(true)),
        // JSM writes these from its own trigger-calibration routine, in the
        // DualSense's raw 0-255 travel, and uses them to place an effect zone. Our
        // pins are zones along the trigger already, so the physical travel never
        // needs declaring — and calibration is the device card's job here.
        "LEFT_TRIGGER_OFFSET" | "LEFT_TRIGGER_RANGE" | "RIGHT_TRIGGER_OFFSET"
        | "RIGHT_TRIGGER_RANGE" => Ignored(
            "trigger travel is measured by the device card; effect zones here are fractions of \
             it, so there is nothing to declare",
        ),

        // FlexInput's own business.
        "AUTOLOAD" => Ignored("FlexInput loads profiles its own way"),
        "AUTOCONNECT" => Ignored("FlexInput tracks connected controllers itself"),
        "AUTO_CALIBRATE_GYRO" => Ignored(WHY_DEVICE_CARD),
        "JSM_DIRECTORY" => Ignored("configs live in this module's tabs, not in a folder"),
        "TICK_TIME" => Ignored("FlexInput's engine sets its own tick rate, in Settings"),
        "HIDE_MINIMIZED" => Ignored("a JSM window setting"),
        "VIRTUAL_CONTROLLER" => Ignored("wire a virtual pad downstream of this module instead"),
        "COUNTER_OS_MOUSE_SPEED" | "IGNORE_OS_MOUSE_SPEED" => {
            Ignored("our mouse output doesn't go through the OS pointer speed")
        }
        "JOYCON_GYRO_MASK" | "JOYCON_MOTION_MASK" => {
            Ignored("FlexInput treats each Joy-Con as its own device")
        }
        _ => return None,
    })
}
