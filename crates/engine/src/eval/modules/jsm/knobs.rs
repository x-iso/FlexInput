//! What the editor needs to put a slider on a setting, and to draw the curve a
//! config describes.
//!
//! Both are read off the config **text**, which stays the only source of truth: a
//! slider is a nicer way to type a number, so dragging one rewrites the number on
//! its line and the parser sees the edit like any other. Nothing here holds state.
//!
//! Only a line with exactly one number gets a slider. Several of JSM's settings
//! take a pair (`GRID_SIZE = 2 3`, `MIN_GYRO_SENS = 2 3`), and one slider cannot
//! honestly stand for two numbers — so those are left to the keyboard rather than
//! given a control that would silently drop half the line.

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
}

impl Knob {
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
        let Some((lo, hi, integral)) = range(&upper) else { continue };
        // Exactly one number, or nothing doing — see the note at the top.
        let mut words = rhs.split_whitespace();
        let Some(first) = words.next() else { continue };
        if words.next().is_some() {
            continue;
        }
        let Ok(value) = first.parse::<f32>() else { continue };
        out.push(Knob { line, name: name.to_string(), value, lo, hi, integral });
    }
    out
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
    let upper = name.to_ascii_uppercase();
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

        "TRIGGER_THRESHOLD" | "TRIGGER_SKIP_DELAY" => Feel::Triggers,

        // Aiming and flick settings that don't say which stick: the one aiming.
        "STICK_DEADZONE_INNER" | "STICK_DEADZONE_OUTER" | "STICK_POWER"
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
/// A flat step of 1 would put half of JSM's settings out of a pad's reach: a
/// deadzone lives in 0..1, where whole numbers offer "off" and "all of it".
/// Twenty steps across the setting's own range, rounded to a size a person
/// would have picked (1, 2 or 5 of some power of ten), lands on numbers that
/// read like numbers — 0.05 for a deadzone, 1 for gyro sensitivity, 50ms for a
/// hold time.
///
/// Deliberately coarse. This gesture exists to get a number THERE; the moment
/// one is, the setting has a fader in the Tune pane, and that is the tool for
/// finding the value you actually want.
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
    step
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
    let bounds = range(&name.to_ascii_uppercase());
    let (lo, hi, integral) = bounds.unwrap_or((f32::MIN, f32::MAX, false));
    let next = match current {
        Some(v) => v + dir as f32 * if bounds.is_some() { step_for(lo, hi, integral) } else { 1.0 },
        // Nothing there yet, so the first deflection puts a number in the slot
        // rather than stepping one. Zero — pulled inside the setting's range,
        // because seeding a value the setting cannot take would be its own small
        // lie — and the direction steers it from there.
        None => 0.0,
    };
    Some(cursor::replace(text, cur, &shown(next.clamp(lo, hi), integral)))
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
        // A slider range, not a limit — the parser takes any positive number, and
        // a config that writes one outside this keeps it until the slider is
        // dragged. Real configs live at the low end, so a range up to 400 gave
        // the whole usable span about two pixels of travel.
        "REAL_WORLD_CALIBRATION" => (0.0, 10.0, false),
        "IN_GAME_SENS" => (0.1, 10.0, false),
        // Stick aiming and flick.
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
        _ => return None,
    };
    Some(r)
}
