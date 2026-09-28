//! What the editor needs to put a slider on a setting, and to draw the curve a
//! config describes.
//!
//! Both are read off the config **text**, which stays the only source of truth: a
//! slider is a nicer way to type a number, so dragging one rewrites the number on
//! its line and the parser sees the edit like any other. Nothing here holds state.
//!
//! A line with one number gets one slider. Several of JSM's settings take a pair
//! (`GRID_SIZE = 2 3`, `MIN_GYRO_SENS = 2 3`) — one number for both axes, or one
//! each — and a pair line gets TWO sliders, one per number, because one slider
//! cannot honestly stand for two numbers. Whether a setting is split is read off
//! the text like everything else: `split_pair` / `join_pair` rewrite `2` as `2 2`
//! and back, and the faders follow. Any other line with more than one number is
//! left to the keyboard.

use super::cc;
use super::cursor::{self, Cursor, TokenKind};
use super::parse::{setting_support, Compiled, Support};

/// A numeric setting line a slider can drive.
#[derive(Clone, PartialEq, Debug)]
pub struct Knob {
    /// Which line of the tab's text, 0-based.
    pub line: usize,
    /// The setting's name as written, so the label reads like the config.
    pub name: String,
    pub value: f32,
    pub lo: f32,
    pub hi: f32,
    /// Whole numbers only — a millisecond count, a zone, a grid dimension.
    pub integral: bool,
    /// Which word after the `=` this fader drives: `None` for a line with one
    /// number, `Some(0)` / `Some(1)` for the two a pair line gets, and for a
    /// trigger effect `Some(1)`… for the numbers after its mode word.
    pub part: Option<u8>,
    /// What the fader is labelled, where the setting's name alone wouldn't say
    /// which number it is — a trigger effect's `snap force (R BOW)`.
    pub caption: Option<String>,
}

/// The two settings whose line is a mode word followed by that mode's numbers.
fn is_trigger_effect(upper: &str) -> bool {
    matches!(upper, "LEFT_TRIGGER_EFFECT" | "RIGHT_TRIGGER_EFFECT")
}

/// Settings that take one number for both axes, or one each (`FloatXY` in JSM).
pub fn pairable(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        "GYRO_SENS" | "MIN_GYRO_SENS" | "MAX_GYRO_SENS" | "STICK_SENS" | "TOUCHPAD_SENS"
            | "GRID_SIZE"
    )
}

/// What each number of a pair means, for the fader labels.
fn part_names(name: &str) -> (&'static str, &'static str) {
    match name.to_ascii_uppercase().as_str() {
        "GRID_SIZE" => ("cols", "rows"),
        "TOUCHPAD_SENS" => ("X", "Y"),
        // The sensitivities: JSM's second number is the vertical.
        _ => ("H", "V"),
    }
}

/// The setting a knob key names. A fader past a line's first number is keyed
/// `NAME/n` — `n` counting words after the `=` from 1, so a pair's second fader
/// is `NAME/2` — while the first keeps the bare name, so a fader pinned before a
/// split still finds its (horizontal) half.
pub fn key_setting(key: &str) -> &str {
    match key.rsplit_once('/') {
        Some((name, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => key,
    }
}

impl Knob {
    /// This fader's identity for pinning and gamepad nav. Unique per fader: the
    /// numbers of one line share a name, so the name alone can't be the key.
    pub fn key(&self) -> String {
        match self.part {
            Some(i) if i > 0 => format!("{}/{}", self.name, i + 1),
            _ => self.name.clone(),
        }
    }

    /// What the fader is labelled: the name, plus which number it is where the
    /// line has several.
    pub fn label(&self) -> String {
        if let Some(c) = &self.caption {
            return c.clone();
        }
        match self.part {
            None => self.name.clone(),
            Some(i) => {
                let (a, b) = part_names(&self.name);
                format!("{} {}", self.name, if i == 0 { a } else { b })
            }
        }
    }

    /// Where the handle sits, 0..1.
    pub fn t(&self) -> f32 {
        if self.hi <= self.lo {
            return 0.0;
        }
        ((self.value - self.lo) / (self.hi - self.lo)).clamp(0.0, 1.0)
    }

    /// The value at handle position `t`, rounded if this setting is whole-numbered.
    pub fn at(&self, t: f32) -> f32 {
        let v = self.lo + t.clamp(0.0, 1.0) * (self.hi - self.lo);
        if self.integral {
            v.round()
        } else {
            // Two decimals is as fine as any of these settings are worth writing,
            // and it keeps the rewritten line readable.
            (v * 100.0).round() / 100.0
        }
    }
}

/// Every numeric setting the text sets, in the order they are written.
pub fn knobs(text: &str) -> Vec<Knob> {
    let mut out = Vec::new();
    for (line, raw) in text.lines().enumerate() {
        // Comments go first, as the parser does it.
        let stripped = raw.split('#').next().unwrap_or("");
        let Some((lhs, rhs)) = stripped.split_once('=') else { continue };
        let (name, rhs) = (lhs.trim(), rhs.trim());
        // A chorded setting (`ZL,GYRO_SENS = 4`) is a modeshift; the slider would
        // have to know which chord it belonged to, so leave those to the keyboard.
        if name.contains(',') || name.contains('+') || name.contains('*') {
            continue;
        }
        let upper = name.to_ascii_uppercase();
        // Only settings, and only ones this module actually runs — a slider on a
        // line that does nothing would be a lie.
        match setting_support(&upper) {
            Some(Support::Pending(_)) | Some(Support::Ignored(_)) | None => continue,
            Some(_) => {}
        }
        if is_trigger_effect(&upper) {
            out.extend(trigger_effect_knobs(line, name, &upper, rhs));
            continue;
        }
        let Some((lo, hi, integral)) = range(&upper) else { continue };
        // One number, or a pair where the setting takes one — see the note at the top.
        let words: Vec<&str> = rhs.split_whitespace().collect();
        let Ok(nums) = words.iter().map(|w| w.parse::<f32>()).collect::<Result<Vec<_>, _>>()
        else {
            continue;
        };
        let knob = |value: f32, part: Option<u8>| Knob {
            line,
            name: name.to_string(),
            value,
            lo,
            hi,
            integral,
            part,
            caption: None,
        };
        match nums.as_slice() {
            [v] => out.push(knob(*v, None)),
            [a, b] if pairable(&upper) => {
                out.push(knob(*a, Some(0)));
                out.push(knob(*b, Some(1)));
            }
            _ => {}
        }
    }
    out
}

/// A trigger effect's faders: one per number its mode takes, each labelled with
/// what that number does — first, since a narrow panel trims a label from its
/// end: `snap force (L BOW)` — and held to JSM's range for it.
/// A number the line hasn't got yet gets no fader — the line says it is
/// missing, and picking the mode with Select fills it in.
fn trigger_effect_knobs(line: usize, name: &str, upper: &str, rhs: &str) -> Vec<Knob> {
    let words: Vec<&str> = rhs.split_whitespace().collect();
    let Some(mode) = words.first() else { return Vec::new() };
    let Some(params) = super::feedback::effect_params(mode) else { return Vec::new() };
    let side = if upper.starts_with("LEFT") { "L" } else { "R" };
    let mode = mode.to_ascii_uppercase();
    params
        .iter()
        .enumerate()
        .filter_map(|(i, prm)| {
            let value = words.get(i + 1)?.parse::<f32>().ok()?;
            Some(Knob {
                line,
                name: name.to_string(),
                value,
                lo: prm.lo as f32,
                hi: prm.hi as f32,
                integral: true,
                // Word 0 is the mode; its numbers are words 1…
                part: Some(i as u8 + 1),
                caption: Some(format!("{} ({side} {mode})", prm.name)),
            })
        })
        .collect()
}

/// Give a trigger effect line exactly the numbers its mode takes.
///
/// Changing the mode word leaves the old mode's numbers behind — too many for
/// `RESISTANCE` after `MACHINE`, too few the other way. This rewrites them to
/// the new mode's list: a number the two modes share by name (the start zone,
/// the force, the frequency…) carries over, held to the new mode's range; one
/// the new mode adds gets its default; one it doesn't take goes. Where the new
/// mode needs its numbers in order (an end past its start, a second foot after
/// the first) the later one is moved on, or the earlier back, until it is.
///
/// `was` is the mode word the numbers were written for — the one just replaced.
/// Without it (or if it isn't an effect) they are read against the line's own
/// mode.
///
/// A line that isn't a trigger effect, or whose mode word isn't one, comes back
/// as it was. The name, the spacing before the value, and a trailing comment
/// are kept.
pub fn settle_trigger_effect(text: &str, line: usize, was: Option<&str>) -> String {
    let Some(raw) = text.lines().nth(line) else { return text.to_string() };
    let body = raw.split('#').next().unwrap_or("");
    let Some((lhs, rhs)) = body.split_once('=') else { return text.to_string() };
    if !is_trigger_effect(&lhs.trim().to_ascii_uppercase()) {
        return text.to_string();
    }
    let words: Vec<&str> = rhs.split_whitespace().collect();
    let Some(&mode) = words.first() else { return text.to_string() };
    let Some(params) = super::feedback::effect_params(mode) else { return text.to_string() };
    // What the line says now, by name — the numbers read against the mode they
    // were written for. A number with no name there has nothing to carry into.
    let old = was.and_then(super::feedback::effect_params).unwrap_or(params);
    let had: Vec<(&str, u32)> = old
        .iter()
        .zip(words.iter().skip(1).map_while(|w| w.parse::<u32>().ok()))
        .map(|(p, n)| (p.name, n))
        .collect();
    let mut v: Vec<u8> = params
        .iter()
        .map(|prm| {
            had.iter()
                .find(|(n, _)| *n == prm.name)
                .map(|(_, x)| (*x).clamp(prm.lo as u32, prm.hi as u32) as u8)
                .unwrap_or(prm.default)
        })
        .collect();
    for &(early, late) in super::feedback::effect_orderings(mode) {
        if v[late] <= v[early] {
            v[late] = (v[early] + 1).min(params[late].hi);
            if v[late] <= v[early] {
                v[early] = v[late].saturating_sub(1).max(params[early].lo);
            }
        }
    }
    let mut value = mode.to_string();
    for n in &v {
        value.push(' ');
        value.push_str(&n.to_string());
    }
    rewrite_rhs(text, line, |rhs| {
        let lead: String = rhs.chars().take_while(|c| c.is_whitespace()).collect();
        let trail: String = {
            let t: String = rhs.chars().rev().take_while(|c| c.is_whitespace()).collect();
            t.chars().rev().collect()
        };
        format!("{lead}{value}{trail}")
    })
}

/// Replace the word under the cursor, as the pad's editing does — and when that
/// word is a trigger effect's MODE, give the line the new mode's numbers
/// (`settle_trigger_effect`). A number typed over a number is left exactly as
/// typed: clamping it would be the editor second-guessing a value you chose,
/// and the line already says when one is out of range.
pub fn replace_word(text: &str, cur: Cursor, with: &str) -> String {
    let was = cursor::selection(text, cur).map(|(_, s, e)| text[s..e].to_string());
    let out = cursor::replace(text, cur, with);
    let on_mode = cursor::tokens_at(text, cur.line)
        .iter()
        .position(|t| t.kind == TokenKind::Value)
        == Some(cur.token);
    if on_mode {
        settle_trigger_effect(&out, cur.line, was.as_deref())
    } else {
        out
    }
}

/// How the editor writes a number back into the config.
///
/// Two decimals is as fine as any of these settings are worth writing, and a
/// whole number reads better without a pointless `.00`.
fn shown(value: f32, integral: bool) -> String {
    if integral || (value * 100.0).round() % 100.0 == 0.0 {
        format!("{}", value.round() as i64)
    } else {
        let s = format!("{value:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Rewrite one line's value part through `f`, keeping the name as the author
/// spelled it and any trailing comment. `f` gets the text after the `=` and
/// returns its replacement.
fn rewrite_rhs(text: &str, line: usize, f: impl FnOnce(&str) -> String) -> String {
    let mut f = Some(f);
    let mut out: Vec<String> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        if i != line {
            out.push(raw.to_string());
            continue;
        }
        let (body, comment) = match raw.find('#') {
            Some(at) => (&raw[..at], &raw[at..]),
            None => (raw, ""),
        };
        let Some((lhs, rhs)) = body.split_once('=') else {
            out.push(raw.to_string());
            continue;
        };
        let rhs = (f.take().expect("one line"))(rhs);
        out.push(format!("{lhs}={rhs}{comment}"));
    }
    let joined = out.join("\n");
    // `lines()` drops a trailing newline; put it back.
    if text.ends_with('\n') { joined + "\n" } else { joined }
}

/// The byte span of each whitespace-separated word in `s`.
fn word_spans(s: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    for (i, c) in s.char_indices() {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(i),
            (true, Some(st)) => {
                spans.push((st, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(st) = start {
        spans.push((st, s.len()));
    }
    spans
}

/// Rewrite the number a fader drives — the line's first number, or for a pair's
/// second fader its second — and nothing else about the line.
pub fn set_knob_part(text: &str, line: usize, part: Option<u8>, value: f32, integral: bool) -> String {
    let shown = shown(value, integral);
    let which = part.unwrap_or(0) as usize;
    rewrite_rhs(text, line, |rhs| match word_spans(rhs).get(which) {
        Some(&(a, b)) => format!("{}{shown}{}", &rhs[..a], &rhs[b..]),
        None => rhs.to_string(),
    })
}

/// Give a one-number pair setting its second number, equal to the first:
/// `GYRO_SENS = 2` becomes `GYRO_SENS = 2 2`, which means the same thing — so the
/// split changes how it is tuned, never how it aims.
pub fn split_pair(text: &str, line: usize) -> String {
    rewrite_rhs(text, line, |rhs| match word_spans(rhs).as_slice() {
        [(a, b)] => format!("{} {}{}", &rhs[..*b], &rhs[*a..*b], &rhs[*b..]),
        _ => rhs.to_string(),
    })
}

/// Drop a pair line's second number, keeping the first for both axes:
/// `GYRO_SENS = 2 3` becomes `GYRO_SENS = 2`.
pub fn join_pair(text: &str, line: usize) -> String {
    rewrite_rhs(text, line, |rhs| match word_spans(rhs).as_slice() {
        [(_, b), (_, d)] => format!("{}{}", &rhs[..*b], &rhs[*d..]),
        _ => rhs.to_string(),
    })
}

/// Split a single-number pair setting into two faders, or join a pair back into
/// one — whichever this fader's line is now. `None` for a setting that can't
/// take a pair.
pub fn toggle_pair(text: &str, knob: &Knob) -> Option<String> {
    if !pairable(&knob.name) {
        return None;
    }
    Some(match knob.part {
        None => split_pair(text, knob.line),
        Some(_) => join_pair(text, knob.line),
    })
}

/// Rewrite the number on one line, keeping everything else about it — the name as
/// the author spelled it, the spacing, and any trailing comment.
pub fn set_knob(text: &str, line: usize, value: f32, integral: bool) -> String {
    let shown = shown(value, integral);
    let mut out: Vec<String> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        if i != line {
            out.push(raw.to_string());
            continue;
        }
        // Split the comment off, rewrite the value, put the comment back.
        let (body, comment) = match raw.find('#') {
            Some(at) => (&raw[..at], &raw[at..]),
            None => (raw, ""),
        };
        let Some((lhs, rhs)) = body.split_once('=') else {
            out.push(raw.to_string());
            continue;
        };
        // Replace the number token and nothing else: the spacing either side of it
        // is the author's, and a slider quietly reformatting their file every time
        // it moves would be its own small insult.
        let lead: String = rhs.chars().take_while(|c| c.is_whitespace()).collect();
        let rest = &rhs[lead.len()..];
        let token = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let trail = &rest[token..];
        out.push(format!("{lhs}={lead}{shown}{trail}{comment}"));
    }
    let joined = out.join("\n");
    // `lines()` drops a trailing newline; put it back so editing the last line
    // doesn't quietly reflow the file.
    if text.ends_with('\n') {
        joined + "\n"
    } else {
        joined
    }
}

/// Which line of `text` sets `name`, if any.
///
/// Matches the way the parser does — leading whitespace ignored, the name
/// case-insensitive, a `#` comment stripped first — so a line this finds is the
/// same line the config is actually running.
pub fn line_of(text: &str, name: &str) -> Option<usize> {
    text.lines().position(|raw| {
        let body = match raw.find('#') {
            Some(at) => &raw[..at],
            None => raw,
        };
        body.split_once('=')
            .is_some_and(|(lhs, _)| lhs.trim().eq_ignore_ascii_case(name))
    })
}

/// Set `name` to `value`, rewriting the line the config already has for it, or
/// appending one when it has none.
///
/// A calibration writes its answer here rather than into a node param, because
/// the config text is this module's only source of truth: the measured value
/// then shows up on its own fader, undoes with the rest of the edit, and travels
/// with the config the way a hand-typed one does.
pub fn set_setting(text: &str, name: &str, value: f32, integral: bool) -> String {
    if let Some(line) = line_of(text, name) {
        return set_knob(text, line, value, integral);
    }
    let mut out = text.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("{name} = {}\n", shown(value, integral)));
    out
}

/// One point of the sensitivity curve: how fast the pad is turning, and the
/// sensitivity the config gives at that speed.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CurvePoint {
    pub dps: f32,
    pub sens: f32,
}

/// The curve a config describes, as samples across a speed range worth looking at
/// — what the custom-curve fork's GUI draws, and the reason its telemetry socket is
/// not needed here: the editor and the engine are the same process.
///
/// The horizontal (yaw) sensitivities are used, since that is the axis JSM treats
/// as the reference and the one a player calibrates against.
/// `warp` is the exponent the speed axis is drawn with: 1 is linear, below 1
/// stretches the slow end (where a deadzone and the noise floor live), above 1
/// stretches the fast end. Samples are distributed the same way, or a stretched
/// slow end would be drawn from two or three points and the very detail it was
/// stretched to show would be a straight line between them.
pub fn sens_curve_warped(cfg: &Compiled, samples: usize, warp: f32) -> Vec<CurvePoint> {
    curve(cfg, samples, warp.clamp(0.05, 20.0))
}

/// The curve on a plain linear axis.
pub fn sens_curve(cfg: &Compiled, samples: usize) -> Vec<CurvePoint> {
    curve(cfg, samples, 1.0)
}

fn curve(cfg: &Compiled, samples: usize, warp: f32) -> Vec<CurvePoint> {
    let a = &cfg.aim;
    let c = &cfg.cc;
    let (lo, hi) = (a.min_sens.0, a.max_sens.0);
    // The same speed axis the custom-curve fork's graph uses: a fixed 500°/s
    // unless a setting puts the action further out. Matching it means a curve
    // drawn here and the same curve drawn there are the same picture — which is
    // the point of drawing it at all, since the fork's graph is what people have
    // been tuning against.
    const MAX_OMEGA: f32 = 500.0;
    let span = MAX_OMEGA
        .max(a.max_threshold)
        .max(c.power_vref)
        .max(c.sigmoid_mid + c.sigmoid_width)
        // The fork leaves NATURAL's half-speed out of its axis, which is fine
        // while that is under 500 — as it is in any config anyone writes, since
        // it is the speed you are half way to full sensitivity at. Past that its
        // graph flattens into a useless corner, so this one keeps following it.
        // Agrees with the fork everywhere the fork is readable.
        .max(c.natural_vhalf);
    let n = samples.max(2);
    (0..n)
        .map(|i| {
            // Placed where the axis will draw them: evenly on screen, which is
            // unevenly in degrees per second whenever the axis is warped.
            let p = i as f32 / (n - 1) as f32;
            let dps = span * p.powf(1.0 / warp);
            // The cutoff scales the VELOCITY before the ramp reads it, exactly as
            // the pipeline does — so a config's gyro deadzone shows here as the
            // curve falling to nothing below it, instead of being invisible.
            let eff = dps * super::aim::cutoff_factor(a, dps);
            let magnitude = (eff - a.min_threshold).max(0.0);
            let denom = a.max_threshold - a.min_threshold;
            let t = if denom <= 0.0 {
                if magnitude > 0.0 { 1.0 } else { 0.0 }
            } else {
                (magnitude / denom).min(1.0)
            };
            // Folding the cutoff into the sensitivity keeps `dps × sens` the true
            // camera speed, so the output curve needs no separate knowledge of it.
            let scale = if dps > 0.0 { eff / dps } else { 1.0 };
            CurvePoint {
                dps,
                sens: scale * cc::sensitivity(c, magnitude, t, a.max_threshold, lo, hi),
            }
        })
        .collect()
}

/// One of the pad's two sticks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hand {
    Left,
    Right,
}

/// Which of the pad's inputs a setting changes the feel of.
///
/// Tuning is done by feel: a sensitivity is right when the aim is right, and you
/// cannot tell with the pad taken away. So while a setting is being adjusted, the
/// input it governs should still reach the game — and, just as importantly,
/// everything else should not, or you would be shooting while you tune.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Feel {
    Gyro,
    Stick(Hand),
    Triggers,
    /// Nothing to feel while dragging it — a press timing, a binding window.
    /// Everything stays blocked.
    Nothing,
}

/// Is this stick mode one that aims?
fn aims(mode: super::analog::StickMode) -> bool {
    use super::analog::StickMode as M;
    matches!(mode, M::Aim | M::Flick | M::FlickOnly | M::RotateOnly | M::MouseArea)
}

/// The stick a config actually aims with, for the settings that don't say which
/// they mean (`STICK_POWER`, the flick settings…). Right unless only the left is
/// set up to aim — which is the way round almost every config has it, but read
/// from the config rather than assumed.
fn aiming_hand(cfg: &Compiled) -> Hand {
    if aims(cfg.settings.right.mode) {
        Hand::Right
    } else if aims(cfg.settings.left.mode) {
        Hand::Left
    } else {
        Hand::Right
    }
}

/// What to let through to the game while this setting is being tuned.
///
/// `cfg` is the config as it currently stands, because several settings don't
/// name the input they act on — `STICK_POWER` shapes whichever stick aims, and a
/// virtual-pad setting shapes whatever was routed to that stick, which may be the
/// gyro. Guessing would hand the game the wrong input at the exact moment the
/// user is judging feel.
pub fn feel_of(cfg: &Compiled, name: &str) -> Feel {
    // A pair's second fader is keyed `NAME/2`; it feels like the setting it is.
    let upper = key_setting(name).to_ascii_uppercase();
    // A setting that names its side means that side.
    if let Some(rest) = upper.strip_prefix("LEFT_STICK_") {
        return virtual_or_stick(cfg, Hand::Left, rest);
    }
    if let Some(rest) = upper.strip_prefix("RIGHT_STICK_") {
        return virtual_or_stick(cfg, Hand::Right, rest);
    }
    match upper.as_str() {
        // The gyro, including everything the custom-curve fork added to it, and
        // the motion settings — which are the accelerometer, on the same wire.
        "GYRO_SENS" | "MIN_GYRO_SENS" | "MAX_GYRO_SENS" | "MIN_GYRO_THRESHOLD"
        | "MAX_GYRO_THRESHOLD" | "GYRO_SMOOTH_THRESHOLD" | "GYRO_SMOOTH_TIME"
        | "GYRO_CUTOFF_SPEED" | "GYRO_CUTOFF_RECOVERY" | "TRACKBALL_DECAY"
        | "REAL_WORLD_CALIBRATION" | "IN_GAME_SENS"
        | "ACCEL_NATURAL_VHALF" | "ACCEL_POWER_VREF" | "ACCEL_POWER_EXPONENT"
        | "ACCEL_SIGMOID_MID" | "ACCEL_SIGMOID_WIDTH" | "ACCEL_JUMP_TAU"
        | "ONE_EURO_MIN_CUTOFF" | "ONE_EURO_SPEED_COEFF" | "GYRO_ANGLE_SNAP"
        | "DECEL_BRAKE_STRENGTH" | "DECEL_BRAKE_THRESHOLD" | "ROLL_CONTRIBUTION"
        | "LOCAL_AXIS_OFFSET"
        | "LEAN_THRESHOLD" | "MOTION_DEADZONE_INNER" | "MOTION_DEADZONE_OUTER" => Feel::Gyro,

        // The winding settings shape whatever was routed to a virtual stick,
        // which is usually the gyro — so ask the config rather than assume.
        "WIND_STICK_RANGE" | "WIND_STICK_POWER" | "UNWIND_RATE"
        | "ANGLE_TO_AXIS_DEADZONE_INNER" | "ANGLE_TO_AXIS_DEADZONE_OUTER"
        | "VIRTUAL_STICK_CALIBRATION" => {
            if cfg.pad.gyro_dest == super::pad::Dest::Mouse {
                Feel::Stick(aiming_hand(cfg))
            } else {
                Feel::Gyro
            }
        }

        // A trigger effect is felt by pulling the trigger it is on.
        "TRIGGER_THRESHOLD" | "TRIGGER_SKIP_DELAY" | "LEFT_TRIGGER_EFFECT"
        | "RIGHT_TRIGGER_EFFECT" => Feel::Triggers,

        // Aiming and flick settings that don't say which stick: the one aiming.
        "STICK_DEADZONE_INNER" | "STICK_DEADZONE_OUTER" | "STICK_SENS" | "STICK_POWER"
        | "STICK_ACCELERATION_RATE" | "STICK_ACCELERATION_CAP" | "SCROLL_SENS"
        | "FLICK_TIME" | "FLICK_TIME_EXPONENT" | "FLICK_SNAP_STRENGTH"
        | "FLICK_DEADZONE_ANGLE" | "ROTATE_SMOOTH_OVERRIDE" | "MOUSE_RING_RADIUS" => {
            Feel::Stick(aiming_hand(cfg))
        }

        // A press timing has nothing to feel by holding an axis, and the buttons
        // that would show it are the ones you must NOT send to a live game.
        _ => Feel::Nothing,
    }
}

/// A `LEFT_STICK_*` / `RIGHT_STICK_*` setting: its own stick, unless it shapes
/// that stick as a virtual-pad OUTPUT, in which case what you want to feel is
/// whatever drives it.
fn virtual_or_stick(cfg: &Compiled, hand: Hand, rest: &str) -> Feel {
    let is_output = matches!(
        rest,
        "UNDEADZONE_INNER" | "UNDEADZONE_OUTER" | "UNPOWER" | "VIRTUAL_SCALE"
    );
    if !is_output {
        return Feel::Stick(hand);
    }
    let dest = match hand {
        Hand::Left => super::pad::Dest::LeftStick,
        Hand::Right => super::pad::Dest::RightStick,
    };
    if cfg.pad.gyro_dest == dest {
        Feel::Gyro
    } else {
        Feel::Stick(hand)
    }
}

/// How far one deflection of the stick moves a setting.
///
/// A whole unit per push, which is what anyone watching a number expects — but
/// never MORE than one, and less where a whole unit is too blunt. A deadzone
/// lives in 0..1, where stepping by 1 offers "off" and "all of it" and nothing
/// between; so the floor is twenty steps across the setting's own range,
/// rounded to a size a person would have picked (1, 2 or 5 of some power of
/// ten). That gives 0.05 for a deadzone and 0.1 for a trigger threshold.
///
/// The cap is the half that was missing. Twenty steps across
/// `GYRO_CUTOFF_SPEED`'s 0..50 is a step of 2, walking 0, 2, 4 straight past
/// every odd number — and that setting is the gyro's deadzone, so it is exactly
/// the one somebody nudges while watching the curve. A stepper that skips half
/// the numbers reads as a bug, whatever the reasoning behind it.
///
/// Whole-numbered settings keep the coarse step. They are millisecond timings
/// in a 0..1000 range, where 1ms is below anything you can feel and stepping by
/// it would be two hundred pushes to reach a hold time.
///
/// Even so this is the rough tool: it exists to get a number THERE, and the
/// moment one is, the setting has a fader in the Tune pane for finding the
/// value you actually want.
fn step_for(lo: f32, hi: f32, integral: bool) -> f32 {
    let target = (hi - lo) / 20.0;
    let mut step = if integral { 1.0 } else { 0.01 };
    for k in -2..=4 {
        for m in [1.0, 2.0, 5.0] {
            let c = m * 10f32.powi(k);
            if c <= target && c > step {
                step = c;
            }
        }
    }
    if integral {
        step
    } else {
        step.min(1.0)
    }
}

/// Walk the number under the cursor, or put one there. `dir` is one deflection
/// of the stick: +1 up, -1 down.
///
/// This is how a pad types a number, and the only way it can — the command list
/// offers names, and names are all it can offer. Without this, every numeric
/// setting a pad wrote would be left reading `GYRO_SENS = ?`.
///
/// It touches a token only if that token is already a number or an empty slot.
/// A binding's key (`S = SPACE`) is left alone: turning SPACE into 0 because a
/// thumb brushed the stick is a worse outcome than making you delete it first,
/// and delete is one button away.
pub fn scrub(text: &str, cur: Cursor, dir: i32) -> Option<String> {
    let (tok, s, e) = cursor::selection(text, cur)?;
    // Right of the `=` only. A setting's name is never a number, and a line that
    // starts with one is not a line JSM has any use for.
    if tok.kind != TokenKind::Value {
        return None;
    }
    let current = text[s..e].parse::<f32>().ok();
    if current.is_none() && &text[s..e] != cursor::SLOT {
        return None;
    }
    // What this line sets, so the number can be kept inside what the setting
    // takes. A modeshift (`ZL,GYRO_SENS = 4`) sets the same thing it always
    // does, so the chord in front of the name doesn't change what a number means
    // here.
    let (ls, le) = cursor::line_span(text, cur.line);
    let line = &text[ls..le];
    let name = cursor::tokenize(line)
        .first()
        .filter(|t| t.kind == TokenKind::Name)
        .map(|t| t.text(line))
        .unwrap_or("");
    let name = name.rsplit([',', '+', '*']).next().unwrap_or(name);
    let upper = name.to_ascii_uppercase();
    let bounds = if is_trigger_effect(&upper) {
        // A trigger effect's numbers each have their own range, by position
        // after the mode word.
        let values: Vec<_> =
            cursor::tokenize(line).into_iter().filter(|t| t.kind == TokenKind::Value).collect();
        let at = cursor::tokenize(line).get(cur.token).map(|t| t.start);
        let i = values.iter().position(|t| Some(t.start) == at);
        values
            .first()
            .and_then(|m| super::feedback::effect_params(m.text(line)))
            .zip(i.and_then(|i| i.checked_sub(1)))
            .and_then(|(ps, i)| ps.get(i))
            .map(|p| (p.lo as f32, p.hi as f32, true))
    } else {
        range(&upper)
    };
    let (lo, hi, integral) = bounds.unwrap_or((f32::MIN, f32::MAX, false));
    let step = if bounds.is_some() { step_for(lo, hi, integral) } else { 1.0 };
    let (next, floor, ceiling) = match current {
        Some(v) => {
            // Land on multiples of the step. Walking a number should give 1, 2,
            // 3 rather than 0.7, 1.7, 2.7 — so a value typed by hand is tidied
            // onto the grid by the first push instead of carrying its offset
            // through every one after it.
            //
            // The slack is float arithmetic, not taste. A grid value divided
            // by its own step lands either side of the integer in f32:
            // 0.65 / 0.05 is 12.999999, so `floor + 1` gives back 0.65 and the
            // stick looks dead pushing UP; 0.18 / 0.02 is 9.0000009, so
            // `ceil - 1` gives back 0.18 and it looks dead pushing DOWN. Both
            // are ordinary numbers to find on a deadzone line.
            const SLACK: f32 = 1e-4;
            let n = v / step;
            let stepped = if dir > 0 {
                ((n + SLACK).floor() + 1.0) * step
            } else {
                ((n - SLACK).ceil() - 1.0) * step
            };
            // The range is a fence, but a config already sitting outside it is
            // not dragged back over in one push: `GYRO_SENS = 99` walks down to
            // 98 rather than snapping to 32 and swallowing what was written.
            (stepped, lo.min(v), hi.max(v))
        }
        // Nothing there yet, so the first deflection puts a number in the slot
        // rather than stepping one. Zero — pulled inside the setting's range,
        // because seeding a value the setting cannot take would be its own small
        // lie — and the direction steers it from there.
        None => (0.0, lo, hi),
    };
    Some(cursor::replace(text, cur, &shown(next.clamp(floor, ceiling), integral)))
}

/// The slider range for a setting, and whether it is whole-numbered.
///
/// These are for a control to feel right in, not the parser's limits — the parser
/// still decides what is legal, and typing a number outside a slider's range is
/// fine. Where a setting has a natural bound (a fraction of travel, an angle) the
/// range is that bound; otherwise it is the span real configs live in.
fn range(upper: &str) -> Option<(f32, f32, bool)> {
    let r = match upper {
        // Timings, in milliseconds.
        "HOLD_PRESS_TIME" | "DBL_PRESS_WINDOW" | "SIM_PRESS_WINDOW" | "TURBO_PERIOD" => {
            (0.0, 1000.0, true)
        }
        // Triggers. The threshold goes negative for JSM's hair trigger.
        "TRIGGER_THRESHOLD" => (-1.0, 1.0, false),
        "TRIGGER_SKIP_DELAY" => (0.0, 1000.0, true),
        // Sticks.
        "LEFT_STICK_DEADZONE_INNER" | "LEFT_STICK_DEADZONE_OUTER"
        | "RIGHT_STICK_DEADZONE_INNER" | "RIGHT_STICK_DEADZONE_OUTER"
        | "STICK_DEADZONE_INNER" | "STICK_DEADZONE_OUTER" => (0.0, 1.0, false),
        "SCROLL_SENS" => (1.0, 180.0, false),
        // Gyro.
        "GYRO_SENS" | "MIN_GYRO_SENS" | "MAX_GYRO_SENS" => (0.0, 32.0, false),
        "MIN_GYRO_THRESHOLD" | "MAX_GYRO_THRESHOLD" => (0.0, 200.0, false),
        "GYRO_SMOOTH_THRESHOLD" => (0.0, 100.0, false),
        "GYRO_SMOOTH_TIME" => (0.0, 0.5, false),
        "GYRO_CUTOFF_SPEED" | "GYRO_CUTOFF_RECOVERY" => (0.0, 50.0, false),
        "TRACKBALL_DECAY" => (0.0, 10.0, false),
        // Counts per degree, and it spans three orders of magnitude because it
        // absorbs each game's own sensitivity scale. JSM's own numbers: a 2D
        // cursor game calibrates around 1 to 5.4 (its Desktop.txt ships
        // 5.3333), a 3D camera game starts from a first guess of 40, the
        // README's worked example lands on 151.5, and GyroWiki has Control at
        // roughly 300 on default sensitivity. A thousand is about three times
        // the highest real value anyone has written down, which leaves room
        // for a game whose sensitivity scale is smaller still.
        //
        // A slider range, not a limit: the parser takes any positive number, a
        // config that writes a bigger one keeps it, and the stick still walks
        // it down from there.
        "REAL_WORLD_CALIBRATION" => (0.0, 1000.0, false),
        "IN_GAME_SENS" => (0.1, 10.0, false),
        // Stick aiming and flick.
        // Degrees per second at full deflection (JSM's default is 360) — the same
        // scale as VIRTUAL_STICK_CALIBRATION, which is that rate the other way round.
        "STICK_SENS" => (0.0, 1080.0, false),
        "STICK_POWER" => (0.0, 4.0, false),
        "STICK_ACCELERATION_RATE" => (0.0, 10.0, false),
        "STICK_ACCELERATION_CAP" => (1.0, 100.0, false),
        "FLICK_TIME" => (0.0, 0.5, false),
        "FLICK_TIME_EXPONENT" => (0.0, 2.0, false),
        "FLICK_SNAP_STRENGTH" => (0.0, 1.0, false),
        "FLICK_DEADZONE_ANGLE" => (0.0, 30.0, false),
        "ROTATE_SMOOTH_OVERRIDE" => (-1.0, 10.0, false),
        "MOUSE_RING_RADIUS" => (0.0, 512.0, false),
        // Virtual pad output.
        "VIRTUAL_STICK_CALIBRATION" => (30.0, 1080.0, false),
        "LEFT_STICK_UNDEADZONE_INNER" | "LEFT_STICK_UNDEADZONE_OUTER"
        | "RIGHT_STICK_UNDEADZONE_INNER" | "RIGHT_STICK_UNDEADZONE_OUTER" => (0.0, 1.0, false),
        "LEFT_STICK_UNPOWER" | "RIGHT_STICK_UNPOWER" => (0.0, 4.0, false),
        "LEFT_STICK_VIRTUAL_SCALE" | "RIGHT_STICK_VIRTUAL_SCALE" => (0.0, 4.0, false),
        "WIND_STICK_RANGE" => (90.0, 1800.0, false),
        "WIND_STICK_POWER" => (0.0, 4.0, false),
        "UNWIND_RATE" => (0.0, 3600.0, false),
        "ANGLE_TO_AXIS_DEADZONE_INNER" | "ANGLE_TO_AXIS_DEADZONE_OUTER" => (0.0, 90.0, false),
        // Gravity, the motion stick and the touchpad.
        "LEAN_THRESHOLD" => (0.0, 90.0, false),
        "MOTION_DEADZONE_INNER" | "MOTION_DEADZONE_OUTER" => (0.0, 180.0, false),
        "TOUCH_DEADZONE_INNER" => (0.0, 1.0, false),
        "TOUCH_STICK_RADIUS" => (20.0, 600.0, false),
        // Mouse counts per touchpad point; JSM's default is 1.
        "TOUCHPAD_SENS" => (0.0, 5.0, false),
        // Columns and rows. Five each keeps every combination the faders can
        // reach inside JSM's 25-cell ceiling, so a drag can never write an error.
        "GRID_SIZE" => (1.0, 5.0, true),
        // The custom-curve fork.
        "ACCEL_NATURAL_VHALF" => (0.0, 1000.0, false),
        "ACCEL_POWER_VREF" => (0.0, 10.0, false),
        "ACCEL_POWER_EXPONENT" => (0.0, 4.0, false),
        "ACCEL_SIGMOID_MID" => (0.0, 200.0, false),
        "ACCEL_SIGMOID_WIDTH" => (0.1, 100.0, false),
        "ACCEL_JUMP_TAU" => (0.0, 50.0, false),
        "ONE_EURO_MIN_CUTOFF" => (0.0, 30.0, false),
        "ONE_EURO_SPEED_COEFF" => (0.0, 5.0, false),
        "GYRO_ANGLE_SNAP" => (0.0, 45.0, false),
        "DECEL_BRAKE_STRENGTH" => (0.0, 1.0, false),
        "DECEL_BRAKE_THRESHOLD" => (0.0, 200.0, false),
        "ROLL_CONTRIBUTION" => (-100.0, 100.0, false),
        // A neutral hold's pitch: the whole band the parser takes, since a grip
        // anywhere from flat to well past 45 degrees is ordinary.
        "LOCAL_AXIS_OFFSET" => (-90.0, 90.0, false),
        _ => return None,
    };
    Some(r)
}
