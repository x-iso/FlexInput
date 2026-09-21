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

/// Rewrite the number on one line, keeping everything else about it — the name as
/// the author spelled it, the spacing, and any trailing comment.
pub fn set_knob(text: &str, line: usize, value: f32, integral: bool) -> String {
    let shown = if integral {
        format!("{}", value.round() as i64)
    } else if (value * 100.0).round() % 100.0 == 0.0 {
        // A whole number reads better without a pointless `.00`.
        format!("{}", value.round() as i64)
    } else {
        let s = format!("{value:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    };
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
pub fn sens_curve(cfg: &Compiled, samples: usize) -> Vec<CurvePoint> {
    let a = &cfg.aim;
    let c = &cfg.cc;
    let (lo, hi) = (a.min_sens.0, a.max_sens.0);
    // A span that shows the shape: past the top threshold, and past whatever the
    // chosen curve's own parameters put the action at — the three curves that
    // ignore the threshold would otherwise be drawn over an arbitrary range.
    let interest = a
        .max_threshold
        .max(c.natural_vhalf)
        .max(c.sigmoid_mid + c.sigmoid_width * 2.0)
        .max(c.power_vref * 4.0);
    let span = (interest * 1.6).max(60.0);
    let n = samples.max(2);
    (0..n)
        .map(|i| {
            let dps = span * i as f32 / (n - 1) as f32;
            // Same reduction the pipeline applies before the curve.
            let magnitude = (dps - a.min_threshold).max(0.0);
            let denom = a.max_threshold - a.min_threshold;
            let t = if denom <= 0.0 {
                if magnitude > 0.0 { 1.0 } else { 0.0 }
            } else {
                (magnitude / denom).min(1.0)
            };
            CurvePoint {
                dps,
                sens: cc::sensitivity(c, magnitude, t, a.max_threshold, lo, hi),
            }
        })
        .collect()
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
        "REAL_WORLD_CALIBRATION" => (1.0, 400.0, false),
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
