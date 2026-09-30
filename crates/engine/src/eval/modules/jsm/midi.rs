//! MIDI in a JSM config: the `MIDI_*` names, on both sides of the `=`, and the
//! settings that fill in what a name leaves unsaid.
//!
//! The names are ours, not JSM's — JSM has no MIDI — and they are taken the way
//! `@` was: no JSM name begins `MIDI_`, so nothing is being taken away. One
//! spelling serves both sides: a note is `MIDI_C4` whether a button plays it or
//! it presses a key. See `docs/JSM_MODULE_PLAN.md`, *Phase 10 — MIDI*.
//!
//! ```text
//! MIDI_C4  MIDI_CS4  MIDI_N60        a note (S is the sharp; N<n> by number)
//! MIDI_CC7  MIDI_CC14_7              a controller, 7- or 14-bit
//! MIDI_NRPN130  MIDI_RPN0            a parameter
//! MIDI_PB  MIDI_CP  MIDI_AT_C4       bend, channel pressure, poly aftertouch
//! MIDI_PC5                           a program change
//! MIDI_START  MIDI_STOP  MIDI_CONTINUE
//! MIDI_BPM  MIDI_PLAYING             inputs only: there is nothing to send
//! …_CH10  …_CHANY                    channel override; ANY on an input only
//! ```

use flexinput_core::midi::{Channel, MidiPin, Transport};

/// What a `MIDI_*` name is.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    Note,
    Cc,
    Cc14,
    Nrpn,
    Rpn,
    Bend,
    Pressure,
    Aftertouch,
    Program,
    Start,
    Stop,
    Continue,
    Bpm,
    Playing,
}

impl Kind {
    /// Messages that live on a channel (and so take a `_CH` suffix).
    fn has_channel(self) -> bool {
        !matches!(self, Kind::Start | Kind::Stop | Kind::Continue | Kind::Bpm | Kind::Playing)
    }

    /// State the port derives, with nothing to send.
    fn input_only(self) -> bool {
        matches!(self, Kind::Bpm | Kind::Playing)
    }

    /// A value rather than a gate: a knob, a bend, a pressure.
    pub fn is_continuous(self) -> bool {
        matches!(
            self,
            Kind::Cc | Kind::Cc14 | Kind::Nrpn | Kind::Rpn | Kind::Bend | Kind::Pressure | Kind::Aftertouch
        )
    }
}

/// The channel a name asks for.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Chan {
    /// Nothing said: `MIDI_CHANNEL` (outputs) or `MIDI_IN_CHANNEL` (inputs).
    Unsaid,
    /// `_CHANY` — any channel. Inputs only.
    Any,
    /// `_CH<n>`, 1..=16.
    Ch(u8),
}

/// One `MIDI_*` name, parsed. `Copy`, so a MIDI input can be a [`super::names::Btn`]
/// and ride every press machine the pad's buttons do.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct MidiName {
    pub kind: Kind,
    /// Note, controller, parameter or program number (0 where there is none).
    pub num: u16,
    pub chan: Chan,
}

/// Parse a `MIDI_*` name. `None` when the name isn't one at all (so the caller
/// tries its other vocabularies); `Some(Err)` when it is plainly meant as one
/// but is malformed — `MIDI_CC200` is worth an explanation, not "isn't a key".
pub fn parse(name: &str) -> Option<Result<MidiName, String>> {
    let upper = name.to_ascii_uppercase();
    let body = upper.strip_prefix("MIDI_")?;
    Some(parse_body(body, name))
}

fn parse_body(body: &str, raw: &str) -> Result<MidiName, String> {
    // The channel suffix comes off first: `_CH10`, `_CHANY`.
    let (body, chan) = match body.rsplit_once("_CH") {
        Some((head, "ANY")) => (head, Chan::Any),
        Some((head, digits)) if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) => {
            match digits.parse::<u8>() {
                Ok(n) if (1..=16).contains(&n) => (head, Chan::Ch(n)),
                _ => return Err(format!("`{raw}` — a MIDI channel is 1 to 16")),
            }
        }
        _ => (body, Chan::Unsaid),
    };
    let num = |digits: &str, max: u16, what: &str| -> Result<u16, String> {
        match digits.parse::<u16>() {
            Ok(n) if n <= max => Ok(n),
            _ => Err(format!("`{raw}` — {what} is 0 to {max}")),
        }
    };
    let mk = |kind: Kind, n: u16| MidiName { kind, num: n, chan };
    let name = match body {
        "PB" => mk(Kind::Bend, 0),
        "CP" => mk(Kind::Pressure, 0),
        "START" => mk(Kind::Start, 0),
        "STOP" => mk(Kind::Stop, 0),
        "CONTINUE" => mk(Kind::Continue, 0),
        "BPM" => mk(Kind::Bpm, 0),
        "PLAYING" => mk(Kind::Playing, 0),
        _ => {
            if let Some(d) = body.strip_prefix("CC14_") {
                mk(Kind::Cc14, num(d, 31, "a 14-bit controller")?)
            } else if let Some(d) = body.strip_prefix("CC") {
                mk(Kind::Cc, num(d, 127, "a controller")?)
            } else if let Some(d) = body.strip_prefix("NRPN") {
                mk(Kind::Nrpn, num(d, 16383, "a parameter")?)
            } else if let Some(d) = body.strip_prefix("RPN") {
                mk(Kind::Rpn, num(d, 16383, "a parameter")?)
            } else if let Some(d) = body.strip_prefix("PC") {
                mk(Kind::Program, num(d, 127, "a program")?)
            } else if let Some(n) = body.strip_prefix("AT_") {
                mk(Kind::Aftertouch, note_number(n).ok_or_else(|| not_a_note(raw))?)
            } else if let Some(n) = note_number(body) {
                mk(Kind::Note, n)
            } else {
                return Err(format!(
                    "`{raw}` isn't a MIDI name — try MIDI_C4, MIDI_N60, MIDI_CC7, MIDI_PB, \
                     MIDI_PC5 or MIDI_START"
                ));
            }
        }
    };
    if name.chan != Chan::Unsaid && !name.kind.has_channel() {
        return Err(format!("`{raw}` — this message has no channel"));
    }
    Ok(name)
}

fn not_a_note(raw: &str) -> String {
    format!("`{raw}` — a note is a name and octave (C4, CS4, BB3 isn't one — use AS3) or N0 to N127")
}

/// `C4`, `CS4`, `N60` → a note number. C4 is 60; `S` is the sharp, since `#` is
/// not a word character in JSM. Octave −1 is reachable only as `N0`…`N11`.
fn note_number(s: &str) -> Option<u16> {
    if let Some(d) = s.strip_prefix('N') {
        if !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()) {
            return d.parse::<u16>().ok().filter(|n| *n <= 127);
        }
    }
    let mut chars = s.chars();
    let letter = chars.next()?;
    let base: i32 = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest: &str = chars.as_str();
    let (sharp, octave) = match rest.strip_prefix('S') {
        Some(o) => (1, o),
        None => (0, rest),
    };
    if octave.is_empty() || !octave.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let octave: i32 = octave.parse().ok()?;
    let n = (octave + 1) * 12 + base + sharp;
    (0..=127).contains(&n).then_some(n as u16)
}

/// A note number as its `MIDI_` word: `C4`, `CS4`, or `N5` below octave 0.
fn spell_note(n: u16) -> String {
    const NAMES: [&str; 12] = ["C", "CS", "D", "DS", "E", "F", "FS", "G", "GS", "A", "AS", "B"];
    if n < 12 {
        return format!("N{n}");
    }
    format!("{}{}", NAMES[(n % 12) as usize], n / 12 - 1)
}

impl MidiName {
    /// The name as the config spells it.
    pub fn spell(&self) -> String {
        let body = match self.kind {
            Kind::Note => spell_note(self.num),
            Kind::Cc => format!("CC{}", self.num),
            Kind::Cc14 => format!("CC14_{}", self.num),
            Kind::Nrpn => format!("NRPN{}", self.num),
            Kind::Rpn => format!("RPN{}", self.num),
            Kind::Bend => "PB".into(),
            Kind::Pressure => "CP".into(),
            Kind::Aftertouch => format!("AT_{}", spell_note(self.num)),
            Kind::Program => format!("PC{}", self.num),
            Kind::Start => "START".into(),
            Kind::Stop => "STOP".into(),
            Kind::Continue => "CONTINUE".into(),
            Kind::Bpm => "BPM".into(),
            Kind::Playing => "PLAYING".into(),
        };
        let chan = match self.chan {
            Chan::Unsaid => String::new(),
            Chan::Any => "_CHANY".into(),
            Chan::Ch(n) => format!("_CH{n}"),
        };
        format!("MIDI_{body}{chan}")
    }

    /// Why this name can't be an output, if it can't.
    pub fn output_problem(&self) -> Option<String> {
        if self.kind.input_only() {
            return Some(format!("`{}` is state the port reports — there is nothing to send", self.spell()));
        }
        if self.chan == Chan::Any {
            return Some(format!(
                "`{}` — an output has to pick one channel; _CHANY is for inputs",
                self.spell()
            ));
        }
        None
    }

    /// Why this name can't be a button, if it can't.
    pub fn input_problem(&self) -> Option<String> {
        (self.kind == Kind::Bpm).then(|| {
            "`MIDI_BPM` is a tempo, not something pressed — it can drive an analog target, \
             not a binding"
                .to_string()
        })
    }

    /// The bus pin this name reads or writes, with `default` (1..=16, or `None`
    /// for any channel) filling a channel the name leaves unsaid.
    pub fn pin(&self, default: Option<u8>) -> MidiPin {
        let ch = match self.chan {
            Chan::Ch(n) => Channel::Ch(n - 1),
            Chan::Any => Channel::Any,
            Chan::Unsaid => match default {
                Some(n) => Channel::Ch(n.clamp(1, 16) - 1),
                None => Channel::Any,
            },
        };
        let n7 = self.num.min(127) as u8;
        match self.kind {
            Kind::Note => MidiPin::Note { ch, note: n7 },
            Kind::Cc => MidiPin::Cc { ch, cc: n7 },
            Kind::Cc14 => MidiPin::Cc14 { ch, cc: self.num.min(31) as u8 },
            Kind::Nrpn => MidiPin::Nrpn { ch, param: self.num },
            Kind::Rpn => MidiPin::Rpn { ch, param: self.num },
            Kind::Bend => MidiPin::PitchBend { ch },
            Kind::Pressure => MidiPin::ChannelPressure { ch },
            Kind::Aftertouch => MidiPin::PolyAftertouch { ch, note: n7 },
            Kind::Program => MidiPin::ProgramChange { ch, program: n7 },
            Kind::Start => MidiPin::Transport(Transport::Start),
            Kind::Stop => MidiPin::Transport(Transport::Stop),
            Kind::Continue => MidiPin::Transport(Transport::Continue),
            Kind::Bpm => MidiPin::Bpm,
            Kind::Playing => MidiPin::Playing,
        }
    }

    /// Every pin an input name could arrive on: the one it names, and — for an
    /// any-channel input — each channel's own pin as well, so claiming it
    /// silences the message whichever channel it came in on.
    pub fn input_pins(&self, in_channel: Option<u8>) -> Vec<String> {
        let pin = self.pin(in_channel);
        let mut v = vec![pin.to_id()];
        if pin.channel() == Some(Channel::Any) {
            for c in 1..=16u8 {
                v.push(MidiName { chan: Chan::Ch(c), ..*self }.pin(None).to_id());
            }
        }
        v
    }
}

/// A continuous source a config can point at a MIDI value, one axis at a time.
///
/// A thumbstick, the motion stick and the gyro only send once their mode says
/// so (`LEFT_STICK_MODE = MIDI`, `GYRO_OUTPUT = MIDI`, …), which is what hands
/// the source over from whatever it was doing. The touchpad is the same through
/// `TOUCHPAD_MODE = MIDI`. The accelerometer has no mode — it is only ever read
/// here, never turned into anything else — so setting a target is all it takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    LeftX,
    LeftY,
    RightX,
    RightY,
    MotionX,
    MotionY,
    TouchX,
    TouchY,
    GyroX,
    GyroY,
    GyroZ,
    AccelX,
    AccelY,
    AccelZ,
}

/// Every source, with the setting that names its target.
pub(crate) const SOURCES: [(&str, Source); 14] = [
    ("LEFT_MIDI_X", Source::LeftX),
    ("LEFT_MIDI_Y", Source::LeftY),
    ("RIGHT_MIDI_X", Source::RightX),
    ("RIGHT_MIDI_Y", Source::RightY),
    ("MOTION_MIDI_X", Source::MotionX),
    ("MOTION_MIDI_Y", Source::MotionY),
    ("TOUCH_MIDI_X", Source::TouchX),
    ("TOUCH_MIDI_Y", Source::TouchY),
    ("GYRO_MIDI_X", Source::GyroX),
    ("GYRO_MIDI_Y", Source::GyroY),
    ("GYRO_MIDI_Z", Source::GyroZ),
    ("ACCEL_MIDI_X", Source::AccelX),
    ("ACCEL_MIDI_Y", Source::AccelY),
    ("ACCEL_MIDI_Z", Source::AccelZ),
];

impl Source {
    /// Centred at rest (a stick, a rate, a force) rather than starting at the
    /// bottom (a finger's place on the pad).
    pub fn two_sided(self) -> bool {
        !matches!(self, Source::TouchX | Source::TouchY)
    }
}

/// Where a source's reading lands in its target's range.
///
/// A one-sided reading (0..1) fills the value from bottom to top. A two-sided
/// one (−1..1) is centred: 64 at rest, 0 and 127 at the ends. A bend is
/// two-sided in the protocol too, so it takes a two-sided reading as it stands
/// and a one-sided one as a push up from its centre.
pub fn value_for(target: &MidiPin, reading: f32, two_sided: bool) -> f32 {
    let bend = matches!(target, MidiPin::PitchBend { .. });
    match (bend, two_sided) {
        (true, _) => reading.clamp(-1.0, 1.0),
        (false, true) => (0.5 + 0.5 * reading).clamp(0.0, 1.0),
        (false, false) => reading.clamp(0.0, 1.0),
    }
}

/// The MIDI settings, as the config (or a held chord) has them.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Settings {
    /// `MIDI_CHANNEL`: an output's channel when its name doesn't say, 1..=16.
    pub channel: u8,
    /// `MIDI_IN_CHANNEL`: an input's, or `None` for any channel.
    pub in_channel: Option<u8>,
    /// `MIDI_VELOCITY`: note-on velocity for a button-played note, 1..=127.
    pub velocity: u8,
    /// `MIDI_IN_THRESHOLD`: where a continuous input counts as pressed.
    pub in_threshold: f32,
    /// `GYRO_MIDI_SCALE`: deg/s that reaches the end of a MIDI value.
    pub gyro_scale: f32,
    /// `ACCEL_MIDI_SCALE`: g that reaches the end of a MIDI value.
    pub accel_scale: f32,
    /// Each [`Source`]'s target (`LEFT_MIDI_X = MIDI_CC1`), by `Source as usize`.
    pub targets: [Option<MidiName>; SOURCES.len()],
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            channel: 1,
            in_channel: None,
            velocity: 100,
            in_threshold: 0.5,
            gyro_scale: 360.0,
            accel_scale: 2.0,
            targets: [None; SOURCES.len()],
        }
    }
}

impl Settings {
    /// The target `source` sends to, if it has one.
    pub fn target(&self, source: Source) -> Option<MidiName> {
        self.targets[source as usize]
    }
}

/// Which MIDI setting a line sets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MidiId {
    Channel,
    InChannel,
    Velocity,
    InThreshold,
    GyroScale,
    AccelScale,
    /// Where one continuous source sends (`LEFT_MIDI_X` …).
    Target(Source),
}

impl MidiId {
    /// A channel decides which pin a name IS. Changing it under a held chord
    /// would start a note on one channel and stop it on another, leaving the
    /// first hanging — so the two channel settings belong to the config, not
    /// to a chord.
    pub(crate) fn chordable(self) -> bool {
        !matches!(self, MidiId::Channel | MidiId::InChannel)
    }
}

/// Apply one MIDI setting line.
pub(crate) fn apply(name: &str, rhs: &str, which: MidiId, s: &mut Settings) -> Result<(), String> {
    let word = rhs.split_whitespace().next().unwrap_or("");
    let number = || word.parse::<f32>().ok();
    match which {
        MidiId::Channel | MidiId::InChannel => {
            if which == MidiId::InChannel && word.eq_ignore_ascii_case("ANY") {
                s.in_channel = None;
                return Ok(());
            }
            let Some(n) = word.parse::<u8>().ok().filter(|n| (1..=16).contains(n)) else {
                return Err(if which == MidiId::InChannel {
                    format!("`{name}` wants a channel, 1 to 16, or ANY")
                } else {
                    format!("`{name}` wants a channel, 1 to 16")
                });
            };
            if which == MidiId::Channel {
                s.channel = n;
            } else {
                s.in_channel = Some(n);
            }
        }
        MidiId::Velocity => {
            let Some(n) = word.parse::<u8>().ok().filter(|n| (1..=127).contains(n)) else {
                return Err(format!("`{name}` wants a velocity, 1 to 127"));
            };
            s.velocity = n;
        }
        MidiId::InThreshold => {
            let Some(v) = number().filter(|v| (0.0..=1.0).contains(v)) else {
                return Err(format!("`{name}` wants a number from 0 to 1"));
            };
            s.in_threshold = v;
        }
        MidiId::GyroScale | MidiId::AccelScale => {
            let Some(v) = number().filter(|v| *v > 0.0) else {
                return Err(format!(
                    "`{name}` wants a positive number ({})",
                    if which == MidiId::GyroScale { "degrees per second" } else { "g" }
                ));
            };
            if which == MidiId::GyroScale {
                s.gyro_scale = v;
            } else {
                s.accel_scale = v;
            }
        }
        MidiId::Target(source) => {
            if word.eq_ignore_ascii_case("NONE") {
                s.targets[source as usize] = None;
                return Ok(());
            }
            let m = match parse(word) {
                Some(Ok(m)) => m,
                Some(Err(why)) => return Err(why),
                None => {
                    return Err(format!(
                        "`{name}` wants a MIDI controller, bend or pressure (`MIDI_CC1`, \
                         `MIDI_PB`, `MIDI_CP` …), or NONE"
                    ))
                }
            };
            if let Some(why) = m.output_problem() {
                return Err(why);
            }
            // A note or a program change is a gate; a stick sweeping through
            // one would retrigger it on every move.
            if !m.kind.is_continuous() {
                return Err(format!(
                    "`{name}` follows a moving source, so it wants a value — a controller, bend \
                     or pressure, not `{}`",
                    m.spell()
                ));
            }
            s.targets[source as usize] = Some(m);
        }
    }
    Ok(())
}

/// Is `value` (a MIDI input pin's bus value) pressed? A gate is itself; a
/// continuous value crosses at `threshold` — both halves of a bend count.
pub fn value_pressed(kind: Kind, value: flexinput_core::Signal, threshold: f32) -> bool {
    if kind.is_continuous() {
        let v = value.as_float();
        let v = if kind == Kind::Bend { v.abs() } else { v };
        v >= threshold.max(f32::EPSILON)
    } else {
        value.as_bool()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> MidiName {
        parse(s).expect("a MIDI name").expect("a valid one")
    }

    #[test]
    fn names_parse_and_spell_back() {
        for s in [
            "MIDI_C4", "MIDI_CS4", "MIDI_N5", "MIDI_G9", "MIDI_CC7", "MIDI_CC14_7",
            "MIDI_NRPN130", "MIDI_RPN0", "MIDI_PB", "MIDI_CP", "MIDI_AT_C4", "MIDI_PC5",
            "MIDI_START", "MIDI_STOP", "MIDI_CONTINUE", "MIDI_BPM", "MIDI_PLAYING",
            "MIDI_C4_CH10", "MIDI_CC7_CHANY", "MIDI_PB_CH16",
        ] {
            assert_eq!(p(s).spell(), s, "{s}");
        }
        // Case, and a note written by number, come back in the canonical form.
        assert_eq!(p("midi_c4").spell(), "MIDI_C4");
        assert_eq!(p("MIDI_N60").spell(), "MIDI_C4");
    }

    #[test]
    fn notes_land_where_a_keyboard_puts_them() {
        assert_eq!(p("MIDI_C4").num, 60);
        assert_eq!(p("MIDI_CS4").num, 61);
        assert_eq!(p("MIDI_A4").num, 69);
        assert_eq!(p("MIDI_C0").num, 12);
        assert_eq!(p("MIDI_G9").num, 127);
        assert!(parse("MIDI_GS9").unwrap().is_err(), "past 127");
    }

    #[test]
    fn a_malformed_midi_name_says_why_and_others_are_left_alone() {
        assert!(parse("SPACE").is_none());
        assert!(parse("X_A").is_none());
        for bad in ["MIDI_CC200", "MIDI_C4_CH17", "MIDI_START_CH2", "MIDI_H4", "MIDI_CC14_40"] {
            assert!(parse(bad).unwrap().is_err(), "{bad}");
        }
    }

    #[test]
    fn a_pin_takes_the_default_channel_only_when_the_name_is_silent() {
        assert_eq!(p("MIDI_C4").pin(Some(3)).to_id(), "midi:note:3:60");
        assert_eq!(p("MIDI_C4_CH10").pin(Some(3)).to_id(), "midi:note:10:60");
        assert_eq!(p("MIDI_CC7").pin(None).to_id(), "midi:cc:*:7");
        assert_eq!(p("MIDI_START").pin(Some(3)).to_id(), "midi:rt:start");
    }

    #[test]
    fn outputs_and_inputs_refuse_what_they_cannot_be() {
        assert!(p("MIDI_BPM").output_problem().is_some());
        assert!(p("MIDI_PLAYING").output_problem().is_some());
        assert!(p("MIDI_C4_CHANY").output_problem().is_some());
        assert!(p("MIDI_C4").output_problem().is_none());
        assert!(p("MIDI_BPM").input_problem().is_some());
        assert!(p("MIDI_PLAYING").input_problem().is_none());
    }

    #[test]
    fn a_reading_lands_centred_or_from_the_bottom() {
        let cc = MidiPin::Cc { ch: Channel::Ch(0), cc: 1 };
        let pb = MidiPin::PitchBend { ch: Channel::Ch(0) };
        assert_eq!(value_for(&cc, 0.0, true), 0.5, "a centred stick sits at 64");
        assert_eq!(value_for(&cc, -1.0, true), 0.0);
        assert_eq!(value_for(&cc, 1.0, true), 1.0);
        assert_eq!(value_for(&cc, 0.25, false), 0.25, "a finger fills from the bottom");
        assert_eq!(value_for(&pb, -0.5, true), -0.5, "a bend is two-sided already");
        assert_eq!(value_for(&pb, 0.5, false), 0.5, "a one-sided reading pushes up from centre");
        assert_eq!(value_for(&cc, 3.0, true), 1.0, "past the scale it pins at the end");
    }

    #[test]
    fn a_target_wants_a_value_it_can_send() {
        let mut s = Settings::default();
        assert!(apply("LEFT_MIDI_X", "MIDI_CC1", MidiId::Target(Source::LeftX), &mut s).is_ok());
        assert_eq!(s.target(Source::LeftX), Some(p("MIDI_CC1")));
        assert!(apply("LEFT_MIDI_X", "NONE", MidiId::Target(Source::LeftX), &mut s).is_ok());
        assert_eq!(s.target(Source::LeftX), None);
        for bad in ["MIDI_C4", "MIDI_PC3", "MIDI_BPM", "MIDI_CC1_CHANY", "SPACE"] {
            assert!(apply("LEFT_MIDI_X", bad, MidiId::Target(Source::LeftX), &mut s).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_continuous_input_crosses_at_the_threshold_and_a_bend_both_ways() {
        use flexinput_core::Signal;
        assert!(!value_pressed(Kind::Cc, Signal::Float(0.4), 0.5));
        assert!(value_pressed(Kind::Cc, Signal::Float(0.5), 0.5));
        assert!(value_pressed(Kind::Bend, Signal::Float(-0.8), 0.5));
        assert!(value_pressed(Kind::Note, Signal::Bool(true), 0.5));
        assert!(!value_pressed(Kind::Cc, Signal::Float(0.0), 0.0), "rest never presses");
    }
}
