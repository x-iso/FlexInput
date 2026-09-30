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
//! MIDI_PB  MIDI_CP                   bend, channel pressure
//!                                    (a note's aftertouch is part of the note:
//!                                    as a value, MIDI_C4 reads it)
//! MIDI_PB_UP  MIDI_PB_DOWN           one half of the bend: which way a trigger or
//!                                    button pushes it, or which way presses
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
            Kind::Cc | Kind::Cc14 | Kind::Nrpn | Kind::Rpn | Kind::Bend | Kind::Pressure
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
    /// For a bend, which half: 0 the whole wheel, [`BEND_UP`] or [`BEND_DOWN`].
    pub num: u16,
    pub chan: Chan,
}

/// `MIDI_PB_UP` / `MIDI_PB_DOWN`, in a bend's [`MidiName::num`].
pub const BEND_UP: u16 = 1;
pub const BEND_DOWN: u16 = 2;

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
        "PB_UP" => mk(Kind::Bend, BEND_UP),
        "PB_DOWN" => mk(Kind::Bend, BEND_DOWN),
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
                // Aftertouch is part of its note, not a message of its own.
                let note = note_number(n).map(spell_note).unwrap_or_else(|| n.to_string());
                return Err(format!(
                    "`{raw}` — a note's aftertouch is part of the note: `MIDI_{note}` reads it \
                     as its live value (its aftertouch, else the channel's pressure, else its \
                     velocity), and plays it when a trigger or stick drives the note"
                ));
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
            Kind::Bend => match self.num {
                BEND_UP => "PB_UP".into(),
                BEND_DOWN => "PB_DOWN".into(),
                _ => "PB".into(),
            },
            Kind::Pressure => "CP".into(),
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

    /// For `MIDI_PB_UP` / `MIDI_PB_DOWN`, which way (+1 up, −1 down); `None`
    /// for the whole wheel and for anything that isn't a bend.
    pub fn bend_dir(&self) -> Option<f32> {
        match (self.kind, self.num) {
            (Kind::Bend, BEND_UP) => Some(1.0),
            (Kind::Bend, BEND_DOWN) => Some(-1.0),
            _ => None,
        }
    }

    /// The whole bend wheel, with no half named.
    pub fn is_whole_bend(&self) -> bool {
        self.kind == Kind::Bend && self.bend_dir().is_none()
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

/// How a bus pin a MIDI picker built is written in a config — the editor's
/// "MIDI…" row inserts this. `None` for what a config can't name (a velocity
/// twin, derived state it has no word for).
///
/// A picker's "any channel" becomes a name with no channel at all, so it takes
/// the config's own `MIDI_CHANNEL` / `MIDI_IN_CHANNEL` — which is what "I don't
/// mind which" means once the config says. SysEx has no word spelling; it goes
/// behind `@` as the pin id itself.
pub fn tag_for_pin(pin_id: &str) -> Option<String> {
    let pin = flexinput_core::midi::parse_pin(pin_id)?;
    let chan = match pin.channel() {
        Some(Channel::Ch(c)) => Chan::Ch(c + 1),
        _ => Chan::Unsaid,
    };
    let (kind, num) = match pin {
        MidiPin::Note { note, .. } => (Kind::Note, note as u16),
        MidiPin::Cc { cc, .. } => (Kind::Cc, cc as u16),
        MidiPin::Cc14 { cc, .. } => (Kind::Cc14, cc as u16),
        MidiPin::Nrpn { param, .. } => (Kind::Nrpn, param),
        MidiPin::Rpn { param, .. } => (Kind::Rpn, param),
        MidiPin::PitchBend { .. } => (Kind::Bend, 0),
        MidiPin::ChannelPressure { .. } => (Kind::Pressure, 0),
        // A note's companions are written as the note itself.
        MidiPin::PolyAftertouch { note, .. } => (Kind::Note, note as u16),
        MidiPin::ProgramChange { program, .. } => (Kind::Program, program as u16),
        MidiPin::Transport(Transport::Start) => (Kind::Start, 0),
        MidiPin::Transport(Transport::Stop) => (Kind::Stop, 0),
        MidiPin::Transport(Transport::Continue) => (Kind::Continue, 0),
        MidiPin::Bpm => (Kind::Bpm, 0),
        MidiPin::Playing => (Kind::Playing, 0),
        MidiPin::SysEx(_) => return Some(format!("@\"{pin_id}\"")),
        MidiPin::Velocity { .. } => return None,
    };
    Some(MidiName { kind, num, chan }.spell())
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
/// two-sided in the protocol too: the whole wheel takes a two-sided reading as
/// it stands, and a named half (`bend_dir`, from `MIDI_PB_UP` / `_DOWN`) takes a
/// one-sided reading — or a two-sided one's upper half — as a push that way
/// from the centre.
pub fn value_for(target: &MidiPin, bend_dir: Option<f32>, reading: f32, two_sided: bool) -> f32 {
    let bend = matches!(target, MidiPin::PitchBend { .. });
    match (bend, bend_dir, two_sided) {
        (true, Some(dir), true) => dir * reading.clamp(0.0, 1.0),
        (true, Some(dir), false) => dir * reading.abs().clamp(0.0, 1.0),
        (true, None, _) => reading.clamp(-1.0, 1.0),
        (false, _, true) => (0.5 + 0.5 * reading).clamp(0.0, 1.0),
        (false, _, false) => reading.clamp(0.0, 1.0),
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
            // A finger's place starts at the bottom and goes one way, so on a
            // wheel that goes both it has to say which.
            if !source.two_sided() && m.is_whole_bend() {
                return Err(format!(
                    "`{name}` reads one way from the bottom, and a bend goes both — say which \
                     half: MIDI_PB_UP or MIDI_PB_DOWN"
                ));
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
/// continuous value crosses at `threshold` — both halves of the whole bend
/// wheel count, and `MIDI_PB_UP` / `_DOWN` only their own.
pub fn value_pressed(name: MidiName, value: flexinput_core::Signal, threshold: f32) -> bool {
    if name.kind.is_continuous() {
        let v = name.reading(value.as_float());
        v >= threshold.max(f32::EPSILON)
    } else {
        value.as_bool()
    }
}

impl MidiName {
    /// An input's continuous value as this name reads it: a named bend half
    /// only its own side (0..1), the whole wheel its size either way, anything
    /// else as it stands.
    pub fn reading(&self, v: f32) -> f32 {
        match self.bend_dir() {
            Some(dir) => (v * dir).max(0.0),
            None if self.kind == Kind::Bend => v.abs(),
            None => v,
        }
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
            "MIDI_NRPN130", "MIDI_RPN0", "MIDI_PB", "MIDI_CP", "MIDI_PC5",
            "MIDI_PB_UP", "MIDI_PB_DOWN_CH3",
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
        for bad in ["MIDI_CC200", "MIDI_C4_CH17", "MIDI_START_CH2", "MIDI_H4", "MIDI_CC14_40", "MIDI_AT_C4"] {
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
    fn a_picked_pin_is_written_the_way_the_parser_reads_it() {
        assert_eq!(tag_for_pin("midi:note:*:60").as_deref(), Some("MIDI_C4"), "any = the config's own");
        assert_eq!(tag_for_pin("midi:note:10:61").as_deref(), Some("MIDI_CS4_CH10"));
        assert_eq!(tag_for_pin("midi:cc14:2:7").as_deref(), Some("MIDI_CC14_7_CH2"));
        assert_eq!(tag_for_pin("midi:rt:start").as_deref(), Some("MIDI_START"));
        assert_eq!(tag_for_pin("midi:sx:F07E7F0601F7").as_deref(), Some("@\"midi:sx:F07E7F0601F7\""));
        assert_eq!(tag_for_pin("midi:vel:1:60"), None);
        // A note's aftertouch is written as the note it belongs to.
        assert_eq!(tag_for_pin("midi:pat:2:60").as_deref(), Some("MIDI_C4_CH2"));
        // And every word it writes parses back to the same pin.
        for id in ["midi:note:3:0", "midi:nrpn:1:300", "midi:note:16:127", "midi:pc:4:5", "midi:pb:9"] {
            let tag = tag_for_pin(id).unwrap();
            assert_eq!(p(&tag).pin(None).to_id(), id, "{tag}");
        }
    }

    #[test]
    fn a_reading_lands_centred_or_from_the_bottom() {
        let cc = MidiPin::Cc { ch: Channel::Ch(0), cc: 1 };
        let pb = MidiPin::PitchBend { ch: Channel::Ch(0) };
        assert_eq!(value_for(&cc, None, 0.0, true), 0.5, "a centred stick sits at 64");
        assert_eq!(value_for(&cc, None, -1.0, true), 0.0);
        assert_eq!(value_for(&cc, None, 1.0, true), 1.0);
        assert_eq!(value_for(&cc, None, 0.25, false), 0.25, "a finger fills from the bottom");
        assert_eq!(value_for(&pb, None, -0.5, true), -0.5, "the whole wheel is two-sided already");
        assert_eq!(value_for(&pb, Some(-1.0), 0.5, false), -0.5, "a named half pushes its way");
        assert_eq!(value_for(&pb, Some(1.0), -0.5, true), 0.0, "and takes a stick's upper half");
        assert_eq!(value_for(&cc, None, 3.0, true), 1.0, "past the scale it pins at the end");
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
        // A finger reads one way, so it has to name a half of the bend.
        assert!(apply("TOUCH_MIDI_X", "MIDI_PB", MidiId::Target(Source::TouchX), &mut s).is_err());
        assert!(apply("TOUCH_MIDI_X", "MIDI_PB_DOWN", MidiId::Target(Source::TouchX), &mut s).is_ok());
        assert!(apply("LEFT_MIDI_X", "MIDI_PB", MidiId::Target(Source::LeftX), &mut s).is_ok());
    }

    #[test]
    fn a_continuous_input_crosses_at_the_threshold_and_a_bend_both_ways() {
        use flexinput_core::Signal;
        assert!(!value_pressed(p("MIDI_CC1"), Signal::Float(0.4), 0.5));
        assert!(value_pressed(p("MIDI_CC1"), Signal::Float(0.5), 0.5));
        assert!(value_pressed(p("MIDI_PB"), Signal::Float(-0.8), 0.5));
        assert!(value_pressed(p("MIDI_PB_DOWN"), Signal::Float(-0.8), 0.5));
        assert!(!value_pressed(p("MIDI_PB_UP"), Signal::Float(-0.8), 0.5), "the other half");
        assert!(value_pressed(p("MIDI_C4"), Signal::Bool(true), 0.5));
        assert!(!value_pressed(p("MIDI_CC1"), Signal::Float(0.0), 0.0), "rest never presses");
    }
}
