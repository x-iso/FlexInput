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
                &mut r.timings,
                &mut r.settings,
                &mut r.aim,
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

/// Compile a whole config.
pub fn compile(text: &str) -> Compiled {
    let mut out = Compiled {
        lines: Vec::new(),
        bindings: Vec::new(),
        timings: Timings::default(),
        settings: Settings::default(),
        aim: super::aim::Settings::default(),
        modeshifts: Vec::new(),
        mentioned: HashSet::new(),
    };
    for (n, line) in text.lines().enumerate() {
        let info = compile_line(line, n, &mut out);
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

fn compile_line(raw: &str, n: usize, out: &mut Compiled) -> LineInfo {
    // '#' starts a comment to end of line (JSM cuts it before parsing, so a '#'
    // inside a quoted command ends the line there too).
    let line = raw.split('#').next().unwrap_or("").trim();
    if line.is_empty() {
        return LineInfo::of(LineStatus::Blank);
    }

    // A line with no value: RESET_MAPPINGS, a config file name, the console-only ones.
    let Some((lhs, rhs)) = line.split_once('=') else {
        return command_line(line, out);
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
    binding_line(&first, combo, rhs, n, out)
}

// ── line kinds ───────────────────────────────────────────────────────────────

fn command_line(name: &str, out: &mut Compiled) -> LineInfo {
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
        "RESET_MAPPINGS" => LineInfo::of(LineStatus::Pending(PHASE_LAYERS)),
        "CALCULATE_REAL_WORLD_CALIBRATION" => LineInfo::of(LineStatus::Ignored(
            "FlexInput measures real-world sensitivity in the RWS Aim module — \
             set REAL_WORLD_CALIBRATION here from what it tells you",
        )),
        "SET_MOTION_STICK_NEUTRAL" => LineInfo::of(LineStatus::Pending(PHASE_TOUCH)),
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
            // JSM loads a config by naming its file; we resolve that to a tab.
            if upper.ends_with(".TXT") || name.contains('/') || name.contains('\\') {
                LineInfo::of(LineStatus::Pending(PHASE_LAYERS))
            } else {
                LineInfo::of(LineStatus::Error(format!(
                    "`{name}` isn't a button, setting or command"
                )))
            }
        }
    }
}

fn setting_line(name: &str, rhs: &str, support: Support, out: &mut Compiled) -> LineInfo {
    apply_setting(name, rhs, support, &mut out.timings, &mut out.settings, &mut out.aim)
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
    let (mut timings, mut settings, mut aim) = (out.timings, out.settings, out.aim);
    let info = apply_setting(name, rhs, support, &mut timings, &mut settings, &mut aim);
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
pub(crate) fn apply_setting(
    name: &str,
    rhs: &str,
    support: Support,
    timings: &mut Timings,
    settings: &mut Settings,
    aim: &mut super::aim::Settings,
) -> LineInfo {
    match support {
        Support::Analog(which) => analog_setting(name, rhs, which, settings),
        Support::Aim(which) => aim_setting(name, rhs, which, aim),
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

    let (steps, notes) = match parse_mapping(rhs) {
        Ok(v) => v,
        Err(e) => return LineInfo::of(LineStatus::Error(e)),
    };

    out.mentioned.insert(btn);
    if let Some((_, second)) = &combo {
        if let Some(other) = Btn::from_name(second) {
            out.mentioned.insert(other);
        }
    }

    // A button we can read is live now; the derived ones wait for their phase.
    let pending = match trigger {
        Trigger::Simple(b) | Trigger::Double(b) => pending_source(b),
        Trigger::Chord { chord, btn } => pending_source(chord).or_else(|| pending_source(btn)),
        Trigger::Sim(a, b) | Trigger::Diag(a, b) => pending_source(a).or_else(|| pending_source(b)),
    };
    let unsupported: Vec<String> = steps
        .iter()
        .filter_map(|s| match &s.out {
            Out::Unsupported { name, why } => Some(format!("{name}: {why}")),
            _ => None,
        })
        .collect();
    let pending_out = steps.iter().find_map(|s| match &s.out {
        Out::Rumble { .. } => Some(PHASE_FEEDBACK),
        Out::Command(_) => Some(PHASE_LAYERS),
        _ => None,
    });
    // Recalibrating mid-game is the device card's job here, so a binding that
    // asks for it still runs — it just doesn't recalibrate.
    let mut notes = notes;
    if steps.iter().any(|s| s.out == Out::Calibrate) {
        notes.push("CALIBRATE does nothing here — the device card owns gyro calibration".into());
    }
    // A pad output goes nowhere unless a virtual pad is wired downstream of this
    // module, and it goes to EVERY pad wired there — `X_` and `PS_` are two names
    // for one pin, as they are in JSM. The module can't see the patch, so the line
    // says both.
    if steps.iter().any(|s| matches!(&s.out, Out::Pin(p) | Out::Pulse(p)
        if super::names::is_pad_pin(p)))
    {
        notes.push(
            "needs a virtual pad wired downstream to reach anything; `X_` and `PS_` names are \
             the same pin (as in JSM), so wiring decides which pad it reaches"
                .into(),
        );
    }

    let status = if let Some(phase) = pending {
        LineStatus::Pending(phase)
    } else if !unsupported.is_empty()
        && steps
            .iter()
            .all(|s| matches!(s.out, Out::Unsupported { .. }))
    {
        LineStatus::Ignored("nothing here reaches our keyboard sink yet")
    } else if let Some(phase) = pending_out {
        LineStatus::Pending(phase)
    } else {
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

/// The phase that will make a button readable, or `None` when it already is.
fn pending_source(btn: Btn) -> Option<&'static str> {
    use super::names::BtnSource as S;
    match btn.source() {
        S::Pin(_) | S::Trigger { .. } | S::TriggerFull { .. } | S::Stick { .. } => None,
        S::Motion { .. } | S::Lean { .. } => Some(PHASE_TOUCH),
        S::Touch | S::TouchZone { .. } => Some(PHASE_TOUCH),
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
pub(crate) fn parse_mapping(rhs: &str) -> Result<(Vec<Step>, Vec<String>), String> {
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
            Out::Command(key.clone())
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
                LineInfo::of(LineStatus::Ok)
            }
            Parsed::Later(phase) => LineInfo::of(LineStatus::Pending(phase)),
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
                LineInfo::of(LineStatus::Ok)
            }
            Parsed::Later(phase) => {
                // The stick is a mouse or a virtual stick from here on: remember
                // that much, so its directions know they won't fire.
                each(s, side, &|c| c.mode = StickMode::Elsewhere);
                LineInfo::of(LineStatus::Pending(phase))
            }
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
        "X_LT" | "X_RT" | "PS_L2" | "PS_R2" => return Parsed::Later(PHASE_PAD),
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
        "LEFT_STICK" | "RIGHT_STICK" | "LEFT_ANGLE_TO_X" | "LEFT_ANGLE_TO_Y"
        | "RIGHT_ANGLE_TO_X" | "RIGHT_ANGLE_TO_Y" | "LEFT_STEER_X" | "RIGHT_STEER_X"
        | "LEFT_WIND_X" | "RIGHT_WIND_X" => return Parsed::Later(PHASE_PAD),
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
    Space,
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
                        return wants(
                            "a number above zero — it is how many mouse counts make a full turn",
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
        AimId::Space => match value.as_str() {
            "LOCAL" => ok,
            "PLAYER_TURN" | "PLAYER_LEAN" | "WORLD_TURN" | "WORLD_LEAN" => {
                LineInfo::of(LineStatus::Pending(PHASE_GRAVITY))
            }
            _ => wants("LOCAL, PLAYER_TURN, PLAYER_LEAN, WORLD_TURN or WORLD_LEAN"),
        },
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

// ── the settings table ───────────────────────────────────────────────────────

const PHASE_GYRO: &str = "gyro, flick stick and real-world calibration arrive in phase 3";
/// Gravity-referenced gyro spaces land with the motion stick, which needs the
/// same work: our accelerometer frame matched to the one JSM reasons in.
const PHASE_GRAVITY: &str =
    "gyro spaces measured against gravity arrive with the motion stick in phase 6";
const PHASE_ABSOLUTE: &str =
    "placing the pointer outright needs an absolute mouse pin, which the bus doesn't have yet";
const PHASE_HYBRID: &str = "HYBRID_AIM arrives after the rest of aiming";
const PHASE_MODESHIFT: &str = "settings that change while a button is held arrive in phase 4";
const PHASE_PAD: &str = "virtual pad output arrives in phase 5";
const PHASE_TOUCH: &str = "the touchpad and motion stick arrive in phase 6";
const PHASE_FEEDBACK: &str = "rumble, light bar and adaptive triggers arrive in phase 7";
const PHASE_LAYERS: &str = "loading another config arrives in phase 8";

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
        "GYRO_SPACE" => Aim(AimId::Space),
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
        "FLICK_STICK_OUTPUT" | "VIRTUAL_STICK_CALIBRATION" => Pending(PHASE_PAD),

        // Virtual pad output.
        "GYRO_OUTPUT"
        | "LEFT_STICK_UNDEADZONE_INNER"
        | "LEFT_STICK_UNDEADZONE_OUTER"
        | "LEFT_STICK_UNPOWER"
        | "RIGHT_STICK_UNDEADZONE_INNER"
        | "RIGHT_STICK_UNDEADZONE_OUTER"
        | "RIGHT_STICK_UNPOWER"
        | "LEFT_STICK_VIRTUAL_SCALE"
        | "RIGHT_STICK_VIRTUAL_SCALE"
        | "WIND_STICK_RANGE"
        | "WIND_STICK_POWER"
        | "UNWIND_RATE"
        | "ANGLE_TO_AXIS_DEADZONE_INNER"
        | "ANGLE_TO_AXIS_DEADZONE_OUTER" => Pending(PHASE_PAD),

        // Touchpad and motion stick.
        "TOUCHPAD_MODE"
        | "GRID_SIZE"
        | "TOUCHPAD_SENS"
        | "TOUCHPAD_DUAL_STAGE_MODE"
        | "TOUCH_STICK_MODE"
        | "TOUCH_STICK_RADIUS"
        | "TOUCH_DEADZONE_INNER"
        | "TOUCH_RING_MODE"
        | "TOUCH_STICK_AXIS"
        | "MOTION_STICK_MODE"
        | "MOTION_RING_MODE"
        | "MOTION_DEADZONE_INNER"
        | "MOTION_DEADZONE_OUTER"
        | "MOTION_STICK_AXIS"
        | "LEAN_THRESHOLD" => Pending(PHASE_TOUCH),

        // Feedback.
        "RUMBLE"
        | "LIGHT_BAR"
        | "ADAPTIVE_TRIGGER"
        | "LEFT_TRIGGER_EFFECT"
        | "RIGHT_TRIGGER_EFFECT"
        | "LEFT_TRIGGER_OFFSET"
        | "LEFT_TRIGGER_RANGE"
        | "RIGHT_TRIGGER_OFFSET"
        | "RIGHT_TRIGGER_RANGE" => Pending(PHASE_FEEDBACK),

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
        "RESET_MAPPINGS" => Pending(PHASE_LAYERS),
        _ => return None,
    })
}
