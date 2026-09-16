//! MIDI bus pin vocabulary and wire-message codec.
//!
//! Shared by the device backend (decode incoming bytes into pins, encode pins
//! into outgoing bytes), the engine (route pins across the AutoMap bus) and the
//! UI (learn, chips, pickers), so the three can never disagree on what an id
//! means.
//!
//! # Pin ids
//!
//! Every id is persisted in patches, so the grammar is stable:
//!
//! | id                              | signal | meaning                              |
//! |---------------------------------|--------|--------------------------------------|
//! | `midi:note:<ch>:<n>`            | Bool   | note gate                            |
//! | `midi:vel:<ch>:<n>`             | Float  | velocity of the held note, 0..1      |
//! | `midi:pat:<ch>:<n>`             | Float  | poly aftertouch, 0..1                |
//! | `midi:cc:<ch>:<n>`              | Float  | 7-bit CC, 0..1                       |
//! | `midi:cc14:<ch>:<n>`            | Float  | 14-bit CC (MSB n 0..31, LSB n+32)    |
//! | `midi:nrpn:<ch>:<p>`            | Float  | NRPN data entry, 14-bit, 0..1        |
//! | `midi:rpn:<ch>:<p>`             | Float  | RPN data entry, 14-bit, 0..1         |
//! | `midi:pb:<ch>`                  | Float  | pitch bend, -1..1                    |
//! | `midi:cp:<ch>`                  | Float  | channel pressure, 0..1               |
//! | `midi:pc:<ch>:<n>`              | Bool   | program change (pulse)               |
//! | `midi:rt:start` / `stop` / `continue` | Bool | transport (pulse)               |
//! | `midi:rt:playing`               | Bool   | transport state (input only)         |
//! | `midi:rt:bpm`                   | Float  | tempo derived from clock (input only)|
//! | `midi:sx:<HEX>`                 | Bool   | exact SysEx message (pulse)          |
//!
//! `<ch>` is `1..16`, or `*` for "any channel". `*` only makes sense when
//! READING a device (it matches whichever channel is active); an output always
//! needs a concrete channel.
//!
//! The pre-AutoMap MIDI nodes used `cc_<n>` and `pitch_bend` with no channel.
//! Those ids stay valid on existing nodes and mean "any channel" — see
//! [`legacy_pin`].

use crate::SignalType;

/// Prefix shared by every MIDI bus pin.
pub const PIN_PREFIX: &str = "midi:";

/// Longest SysEx message (including `F0`/`F7`) that can become a pin. Pin ids
/// carry the bytes as hex, and a bulk dump is never something a user maps.
pub const SYSEX_MAX_BYTES: usize = 64;

/// Legacy (pre-AutoMap) pitch bend pin id.
pub const LEGACY_PITCH_BEND: &str = "pitch_bend";

// ── Channel ───────────────────────────────────────────────────────────────────

/// A MIDI channel as it appears in a pin id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Channel {
    /// Matches every channel. Input side only.
    Any,
    /// Zero-based channel number, 0..=15 (displayed and persisted as 1..=16).
    Ch(u8),
}

impl Channel {
    /// Zero-based channel from a status byte's low nibble.
    pub fn from_status(status: u8) -> Self {
        Channel::Ch(status & 0x0F)
    }

    /// The zero-based channel number, or `None` for [`Channel::Any`].
    pub fn index(self) -> Option<u8> {
        match self {
            Channel::Any => None,
            Channel::Ch(c) => Some(c),
        }
    }

    fn to_id(self) -> String {
        match self {
            Channel::Any => "*".to_string(),
            Channel::Ch(c) => (c + 1).to_string(),
        }
    }

    fn parse_id(s: &str) -> Option<Self> {
        if s == "*" {
            return Some(Channel::Any);
        }
        let n: u8 = s.parse().ok()?;
        (1..=16).contains(&n).then(|| Channel::Ch(n - 1))
    }

    /// Short human label: "ch 1" … "ch 16", or "any ch".
    pub fn label(self) -> String {
        match self {
            Channel::Any => "any ch".to_string(),
            Channel::Ch(c) => format!("ch {}", c + 1),
        }
    }
}

// ── Pin ───────────────────────────────────────────────────────────────────────

/// Transport messages that are mapped as pulses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Transport {
    Start,
    Stop,
    Continue,
}

/// A parsed MIDI bus pin.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MidiPin {
    Note { ch: Channel, note: u8 },
    Velocity { ch: Channel, note: u8 },
    PolyAftertouch { ch: Channel, note: u8 },
    Cc { ch: Channel, cc: u8 },
    /// `cc` is the MSB controller, 0..=31; its LSB is `cc + 32`.
    Cc14 { ch: Channel, cc: u8 },
    /// 14-bit parameter number, 0..=16383.
    Nrpn { ch: Channel, param: u16 },
    Rpn { ch: Channel, param: u16 },
    PitchBend { ch: Channel },
    ChannelPressure { ch: Channel },
    ProgramChange { ch: Channel, program: u8 },
    Transport(Transport),
    /// Derived from Start/Stop/Continue. Input only.
    Playing,
    /// Derived from Timing Clock. Input only.
    Bpm,
    /// Complete message including `F0` and `F7`.
    SysEx(Vec<u8>),
}

impl MidiPin {
    /// The channel this pin lives on, if it is a channel message.
    pub fn channel(&self) -> Option<Channel> {
        use MidiPin::*;
        match self {
            Note { ch, .. } | Velocity { ch, .. } | PolyAftertouch { ch, .. }
            | Cc { ch, .. } | Cc14 { ch, .. } | Nrpn { ch, .. } | Rpn { ch, .. }
            | PitchBend { ch } | ChannelPressure { ch } | ProgramChange { ch, .. } => Some(*ch),
            Transport(_) | Playing | Bpm | SysEx(_) => None,
        }
    }

    /// The same pin on another channel. Non-channel pins are returned as-is.
    pub fn with_channel(&self, new_ch: Channel) -> MidiPin {
        use MidiPin::*;
        match self.clone() {
            Note { note, .. } => Note { ch: new_ch, note },
            Velocity { note, .. } => Velocity { ch: new_ch, note },
            PolyAftertouch { note, .. } => PolyAftertouch { ch: new_ch, note },
            Cc { cc, .. } => Cc { ch: new_ch, cc },
            Cc14 { cc, .. } => Cc14 { ch: new_ch, cc },
            Nrpn { param, .. } => Nrpn { ch: new_ch, param },
            Rpn { param, .. } => Rpn { ch: new_ch, param },
            PitchBend { .. } => PitchBend { ch: new_ch },
            ChannelPressure { .. } => ChannelPressure { ch: new_ch },
            ProgramChange { program, .. } => ProgramChange { ch: new_ch, program },
            other => other,
        }
    }

    /// Bus signal type carried by this pin.
    pub fn signal_type(&self) -> SignalType {
        use MidiPin::*;
        match self {
            Note { .. } | ProgramChange { .. } | Transport(_) | Playing | SysEx(_) => SignalType::Bool,
            Velocity { .. } | PolyAftertouch { .. } | Cc { .. } | Cc14 { .. }
            | Nrpn { .. } | Rpn { .. } | PitchBend { .. } | ChannelPressure { .. } | Bpm => SignalType::Float,
        }
    }

    /// Pins that fire momentarily on a single message (no "off" message).
    pub fn is_pulse(&self) -> bool {
        matches!(self, MidiPin::ProgramChange { .. } | MidiPin::Transport(_) | MidiPin::SysEx(_))
    }

    /// Pins held for as long as a key is down.
    pub fn is_gate(&self) -> bool {
        matches!(self, MidiPin::Note { .. } | MidiPin::Playing)
    }

    /// Pins carrying a continuously varying value (a knob, fader, bender…).
    /// Learn treats these as "active while moving" rather than "while held".
    pub fn is_continuous(&self) -> bool {
        matches!(
            self,
            MidiPin::PolyAftertouch { .. } | MidiPin::Cc { .. } | MidiPin::Cc14 { .. }
                | MidiPin::Nrpn { .. } | MidiPin::Rpn { .. } | MidiPin::PitchBend { .. }
                | MidiPin::ChannelPressure { .. } | MidiPin::Bpm
        )
    }

    /// Whether a mapping may WRITE this pin to a MIDI output. Derived state and
    /// "any channel" matchers can only be read.
    pub fn is_output_capable(&self) -> bool {
        if self.channel() == Some(Channel::Any) {
            return false;
        }
        !matches!(self, MidiPin::Playing | MidiPin::Bpm)
    }

    /// Resting value: what a pin reads when nothing is happening on it. The
    /// device backend omits pins at rest, so readers substitute this.
    pub fn rest_value(&self) -> crate::Signal {
        match self.signal_type() {
            SignalType::Bool => crate::Signal::Bool(false),
            _ => crate::Signal::Float(0.0),
        }
    }

    /// Stable persisted id.
    pub fn to_id(&self) -> String {
        use MidiPin::*;
        match self {
            Note { ch, note } => format!("midi:note:{}:{note}", ch.to_id()),
            Velocity { ch, note } => format!("midi:vel:{}:{note}", ch.to_id()),
            PolyAftertouch { ch, note } => format!("midi:pat:{}:{note}", ch.to_id()),
            Cc { ch, cc } => format!("midi:cc:{}:{cc}", ch.to_id()),
            Cc14 { ch, cc } => format!("midi:cc14:{}:{cc}", ch.to_id()),
            Nrpn { ch, param } => format!("midi:nrpn:{}:{param}", ch.to_id()),
            Rpn { ch, param } => format!("midi:rpn:{}:{param}", ch.to_id()),
            PitchBend { ch } => format!("midi:pb:{}", ch.to_id()),
            ChannelPressure { ch } => format!("midi:cp:{}", ch.to_id()),
            ProgramChange { ch, program } => format!("midi:pc:{}:{program}", ch.to_id()),
            Transport(t) => format!("midi:rt:{}", match t {
                self::Transport::Start => "start",
                self::Transport::Stop => "stop",
                self::Transport::Continue => "continue",
            }),
            Playing => "midi:rt:playing".to_string(),
            Bpm => "midi:rt:bpm".to_string(),
            SysEx(bytes) => {
                let mut s = String::with_capacity(8 + bytes.len() * 2);
                s.push_str("midi:sx:");
                for b in bytes {
                    s.push_str(&format!("{b:02X}"));
                }
                s
            }
        }
    }

    /// Full human-readable name, e.g. "C4 · ch 1", "CC 7 Volume · ch 2".
    pub fn display_name(&self) -> String {
        use MidiPin::*;
        let on = |ch: &Channel| ch.label();
        match self {
            Note { ch, note } => format!("{} · {}", note_name(*note), on(ch)),
            Velocity { ch, note } => format!("{} velocity · {}", note_name(*note), on(ch)),
            PolyAftertouch { ch, note } => format!("{} aftertouch · {}", note_name(*note), on(ch)),
            Cc { ch, cc } => format!("{} · {}", cc_display_name(*cc), on(ch)),
            Cc14 { ch, cc } => match cc_label(*cc) {
                Some(l) => format!("CC {cc} (14-bit) {l} · {}", on(ch)),
                None => format!("CC {cc} (14-bit) · {}", on(ch)),
            },
            Nrpn { ch, param } => format!("NRPN {param} · {}", on(ch)),
            Rpn { ch, param } => match rpn_label(*param) {
                Some(l) => format!("RPN {param} {l} · {}", on(ch)),
                None => format!("RPN {param} · {}", on(ch)),
            },
            PitchBend { ch } => format!("Pitch Bend · {}", on(ch)),
            ChannelPressure { ch } => format!("Channel Pressure · {}", on(ch)),
            ProgramChange { ch, program } => format!("Program {program} · {}", on(ch)),
            Transport(self::Transport::Start) => "MIDI Start".to_string(),
            Transport(self::Transport::Stop) => "MIDI Stop".to_string(),
            Transport(self::Transport::Continue) => "MIDI Continue".to_string(),
            Playing => "MIDI Playing".to_string(),
            Bpm => "MIDI Clock BPM".to_string(),
            SysEx(bytes) => {
                let head: Vec<String> = bytes.iter().take(4).map(|b| format!("{b:02X}")).collect();
                let more = if bytes.len() > 4 { " …" } else { "" };
                format!("SysEx {}{more}", head.join(" "))
            }
        }
    }

    /// Compact label for chips: "C4", "CC7", "PB", "Start", "SysEx".
    pub fn short_label(&self) -> String {
        use MidiPin::*;
        match self {
            Note { note, .. } => note_name(*note),
            Velocity { note, .. } => format!("{} vel", note_name(*note)),
            PolyAftertouch { note, .. } => format!("{} AT", note_name(*note)),
            Cc { cc, .. } => format!("CC{cc}"),
            Cc14 { cc, .. } => format!("CC{cc}·14"),
            Nrpn { param, .. } => format!("NRPN{param}"),
            Rpn { param, .. } => format!("RPN{param}"),
            PitchBend { .. } => "PB".to_string(),
            ChannelPressure { .. } => "CP".to_string(),
            ProgramChange { program, .. } => format!("PC{program}"),
            Transport(self::Transport::Start) => "Start".to_string(),
            Transport(self::Transport::Stop) => "Stop".to_string(),
            Transport(self::Transport::Continue) => "Continue".to_string(),
            Playing => "Playing".to_string(),
            Bpm => "BPM".to_string(),
            SysEx(_) => "SysEx".to_string(),
        }
    }
}

/// Whether `pin` uses the MIDI bus grammar (does not validate it).
pub fn is_midi_pin(pin: &str) -> bool {
    pin.starts_with(PIN_PREFIX)
}

/// Parse a MIDI bus pin id. Returns `None` for anything malformed or out of
/// range, including ids from other namespaces.
pub fn parse_pin(pin: &str) -> Option<MidiPin> {
    let rest = pin.strip_prefix(PIN_PREFIX)?;
    let mut parts = rest.split(':');
    let kind = parts.next()?;
    let a = parts.next();
    let b = parts.next();
    if parts.next().is_some() {
        return None;
    }
    let ch = |s: Option<&str>| s.and_then(Channel::parse_id);
    let u7 = |s: Option<&str>| s.and_then(|s| s.parse::<u8>().ok()).filter(|v| *v <= 127);
    let u14 = |s: Option<&str>| s.and_then(|s| s.parse::<u16>().ok()).filter(|v| *v <= 16383);
    let none = |s: Option<&str>| s.is_none();
    Some(match kind {
        "note" => MidiPin::Note { ch: ch(a)?, note: u7(b)? },
        "vel" => MidiPin::Velocity { ch: ch(a)?, note: u7(b)? },
        "pat" => MidiPin::PolyAftertouch { ch: ch(a)?, note: u7(b)? },
        "cc" => MidiPin::Cc { ch: ch(a)?, cc: u7(b)? },
        "cc14" => MidiPin::Cc14 { ch: ch(a)?, cc: u7(b).filter(|c| *c <= 31)? },
        "nrpn" => MidiPin::Nrpn { ch: ch(a)?, param: u14(b)? },
        "rpn" => MidiPin::Rpn { ch: ch(a)?, param: u14(b)? },
        "pb" if none(b) => MidiPin::PitchBend { ch: ch(a)? },
        "cp" if none(b) => MidiPin::ChannelPressure { ch: ch(a)? },
        "pc" => MidiPin::ProgramChange { ch: ch(a)?, program: u7(b)? },
        "rt" if none(b) => match a? {
            "start" => MidiPin::Transport(Transport::Start),
            "stop" => MidiPin::Transport(Transport::Stop),
            "continue" => MidiPin::Transport(Transport::Continue),
            "playing" => MidiPin::Playing,
            "bpm" => MidiPin::Bpm,
            _ => return None,
        },
        "sx" if none(b) => MidiPin::SysEx(parse_sysex_hex(a?)?),
        _ => return None,
    })
}

/// Map a legacy (pre-AutoMap) MIDI node pin id to its bus pin. Legacy ids had
/// no channel, and always behaved as "any channel" on input.
pub fn legacy_pin(pin: &str) -> Option<MidiPin> {
    if pin == LEGACY_PITCH_BEND {
        return Some(MidiPin::PitchBend { ch: Channel::Any });
    }
    let cc: u8 = pin.strip_prefix("cc_")?.parse().ok()?;
    (cc <= 127).then_some(MidiPin::Cc { ch: Channel::Any, cc })
}

/// Resting value for a MIDI pin id in either grammar (bus or legacy), or
/// `None` if `pin` isn't a MIDI pin. The device backend omits pins at rest, so
/// a reader that finds a MIDI pin missing on a MIDI device substitutes this.
pub fn rest_value_for_id(pin: &str) -> Option<crate::Signal> {
    parse_pin(pin).or_else(|| legacy_pin(pin)).map(|p| p.rest_value())
}

/// Legacy CC pin id (`cc_<n>`).
pub fn legacy_cc_id(cc: u8) -> String {
    format!("cc_{cc}")
}

fn parse_sysex_hex(hex: &str) -> Option<Vec<u8>> {
    if hex.len() % 2 != 0 || hex.len() / 2 > SYSEX_MAX_BYTES {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect();
    let bytes = bytes?;
    is_valid_sysex(&bytes).then_some(bytes)
}

/// A complete, mappable SysEx message: `F0 … F7`, data bytes 7-bit, within
/// [`SYSEX_MAX_BYTES`].
pub fn is_valid_sysex(bytes: &[u8]) -> bool {
    bytes.len() >= 2
        && bytes.len() <= SYSEX_MAX_BYTES
        && bytes[0] == 0xF0
        && bytes[bytes.len() - 1] == 0xF7
        && bytes[1..bytes.len() - 1].iter().all(|b| *b < 0x80)
}

// ── Names ─────────────────────────────────────────────────────────────────────

/// Note name with C4 = 60 ("C4", "F#2", "C-1").
pub fn note_name(note: u8) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    let octave = (note as i32) / 12 - 1;
    format!("{}{}", NAMES[(note % 12) as usize], octave)
}

fn cc_label(cc: u8) -> Option<&'static str> {
    Some(match cc {
        0 => "Bank Select",
        1 => "Modulation",
        2 => "Breath",
        4 => "Foot",
        5 => "Portamento Time",
        6 => "Data Entry MSB",
        7 => "Volume",
        8 => "Balance",
        10 => "Pan",
        11 => "Expression",
        12 => "Effect 1",
        13 => "Effect 2",
        38 => "Data Entry LSB",
        64 => "Sustain",
        65 => "Portamento",
        66 => "Sostenuto",
        67 => "Soft Pedal",
        68 => "Legato",
        69 => "Hold 2",
        91 => "Reverb",
        92 => "Tremolo",
        93 => "Chorus",
        94 => "Detune",
        95 => "Phaser",
        96 => "Data Increment",
        97 => "Data Decrement",
        98 => "NRPN LSB",
        99 => "NRPN MSB",
        100 => "RPN LSB",
        101 => "RPN MSB",
        120 => "All Sound Off",
        121 => "Reset Controllers",
        123 => "All Notes Off",
        _ => return None,
    })
}

fn rpn_label(param: u16) -> Option<&'static str> {
    Some(match param {
        0 => "Bend Range",
        1 => "Fine Tuning",
        2 => "Coarse Tuning",
        5 => "Modulation Depth",
        _ => return None,
    })
}

/// "CC 7 – Volume" / "CC 3". Used by the legacy per-CC node bodies.
pub fn cc_display_name(cc: u8) -> String {
    match cc_label(cc) {
        Some(name) => format!("CC {cc} – {name}"),
        None => format!("CC {cc}"),
    }
}

// ── Value scaling ─────────────────────────────────────────────────────────────

/// 0..1 → 0..127.
pub fn unit_to_7bit(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 127.0).round() as u8
}

/// 0..1 → 0..16383.
pub fn unit_to_14bit(v: f32) -> u16 {
    (v.clamp(0.0, 1.0) * 16383.0).round() as u16
}

/// -1..1 → 0..16383 with 0.0 at the centre (8192).
pub fn bend_to_14bit(v: f32) -> u16 {
    ((v.clamp(-1.0, 1.0) * 8192.0).round() as i32 + 8192).clamp(0, 16383) as u16
}

/// 0..16383 → -1..1 (8192 = 0.0; the top of the range is 8191/8192).
pub fn bend_from_14bit(raw: u16) -> f32 {
    (raw.min(16383) as f32 - 8192.0) / 8192.0
}

// ── Wire messages ─────────────────────────────────────────────────────────────

/// A decoded MIDI 1.0 message. Channels are zero-based.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MidiMessage {
    NoteOff { ch: u8, note: u8, vel: u8 },
    NoteOn { ch: u8, note: u8, vel: u8 },
    PolyAftertouch { ch: u8, note: u8, value: u8 },
    ControlChange { ch: u8, cc: u8, value: u8 },
    ProgramChange { ch: u8, program: u8 },
    ChannelPressure { ch: u8, value: u8 },
    /// 0..16383, centre 8192.
    PitchBend { ch: u8, value: u16 },
    SysEx(Vec<u8>),
    TimingClock,
    Start,
    Continue,
    Stop,
    ActiveSensing,
    Reset,
    /// Anything else that parsed structurally (MTC, song position, tune
    /// request…). Not mapped.
    Other(Vec<u8>),
}

impl MidiMessage {
    /// Parse one message. `running_status` carries the last channel status
    /// byte across calls so a status-less data message still decodes. Returns
    /// `None` for truncated or malformed input.
    pub fn parse(bytes: &[u8], running_status: &mut Option<u8>) -> Option<MidiMessage> {
        let first = *bytes.first()?;
        let (status, data): (u8, &[u8]) = if first & 0x80 == 0 {
            (running_status.filter(|s| *s < 0xF0)?, bytes)
        } else {
            (first, &bytes[1..])
        };
        if status < 0xF0 {
            *running_status = Some(status);
        } else if status < 0xF8 {
            // System common cancels running status; realtime does not.
            *running_status = None;
        }
        let ch = status & 0x0F;
        let d = |i: usize| data.get(i).copied().filter(|b| *b < 0x80);
        Some(match status & 0xF0 {
            0x80 => MidiMessage::NoteOff { ch, note: d(0)?, vel: d(1)? },
            0x90 => {
                let (note, vel) = (d(0)?, d(1)?);
                if vel == 0 {
                    MidiMessage::NoteOff { ch, note, vel: 0 }
                } else {
                    MidiMessage::NoteOn { ch, note, vel }
                }
            }
            0xA0 => MidiMessage::PolyAftertouch { ch, note: d(0)?, value: d(1)? },
            0xB0 => MidiMessage::ControlChange { ch, cc: d(0)?, value: d(1)? },
            0xC0 => MidiMessage::ProgramChange { ch, program: d(0)? },
            0xD0 => MidiMessage::ChannelPressure { ch, value: d(0)? },
            0xE0 => MidiMessage::PitchBend { ch, value: ((d(1)? as u16) << 7) | d(0)? as u16 },
            _ => match status {
                0xF0 => MidiMessage::SysEx(bytes.to_vec()),
                0xF8 => MidiMessage::TimingClock,
                0xFA => MidiMessage::Start,
                0xFB => MidiMessage::Continue,
                0xFC => MidiMessage::Stop,
                0xFE => MidiMessage::ActiveSensing,
                0xFF => MidiMessage::Reset,
                _ => MidiMessage::Other(bytes.to_vec()),
            },
        })
    }

    /// Wire bytes (always with an explicit status byte).
    pub fn to_bytes(&self) -> Vec<u8> {
        use MidiMessage::*;
        let c = |base: u8, ch: u8| base | (ch & 0x0F);
        match self {
            NoteOff { ch, note, vel } => vec![c(0x80, *ch), note & 0x7F, vel & 0x7F],
            NoteOn { ch, note, vel } => vec![c(0x90, *ch), note & 0x7F, vel & 0x7F],
            PolyAftertouch { ch, note, value } => vec![c(0xA0, *ch), note & 0x7F, value & 0x7F],
            ControlChange { ch, cc, value } => vec![c(0xB0, *ch), cc & 0x7F, value & 0x7F],
            ProgramChange { ch, program } => vec![c(0xC0, *ch), program & 0x7F],
            ChannelPressure { ch, value } => vec![c(0xD0, *ch), value & 0x7F],
            PitchBend { ch, value } => {
                let v = (*value).min(16383);
                vec![c(0xE0, *ch), (v & 0x7F) as u8, (v >> 7) as u8]
            }
            SysEx(bytes) | Other(bytes) => bytes.clone(),
            TimingClock => vec![0xF8],
            Start => vec![0xFA],
            Continue => vec![0xFB],
            Stop => vec![0xFC],
            ActiveSensing => vec![0xFE],
            Reset => vec![0xFF],
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn all_kinds() -> Vec<MidiPin> {
        let ch = Channel::Ch(9);
        vec![
            MidiPin::Note { ch, note: 60 },
            MidiPin::Note { ch: Channel::Any, note: 0 },
            MidiPin::Velocity { ch, note: 127 },
            MidiPin::PolyAftertouch { ch, note: 61 },
            MidiPin::Cc { ch: Channel::Ch(0), cc: 7 },
            MidiPin::Cc14 { ch, cc: 31 },
            MidiPin::Nrpn { ch, param: 16383 },
            MidiPin::Rpn { ch, param: 0 },
            MidiPin::PitchBend { ch: Channel::Ch(15) },
            MidiPin::ChannelPressure { ch: Channel::Any },
            MidiPin::ProgramChange { ch, program: 5 },
            MidiPin::Transport(Transport::Start),
            MidiPin::Transport(Transport::Stop),
            MidiPin::Transport(Transport::Continue),
            MidiPin::Playing,
            MidiPin::Bpm,
            MidiPin::SysEx(vec![0xF0, 0x43, 0x10, 0x4C, 0x00, 0xF7]),
        ]
    }

    #[test]
    fn every_pin_kind_round_trips_through_its_id() {
        for pin in all_kinds() {
            let id = pin.to_id();
            assert!(is_midi_pin(&id));
            assert_eq!(parse_pin(&id), Some(pin.clone()), "id {id}");
        }
    }

    #[test]
    fn ids_are_the_documented_persisted_spelling() {
        assert_eq!(MidiPin::Note { ch: Channel::Ch(0), note: 60 }.to_id(), "midi:note:1:60");
        assert_eq!(MidiPin::Cc { ch: Channel::Any, cc: 7 }.to_id(), "midi:cc:*:7");
        assert_eq!(MidiPin::PitchBend { ch: Channel::Ch(15) }.to_id(), "midi:pb:16");
        assert_eq!(MidiPin::SysEx(vec![0xF0, 0x7E, 0xF7]).to_id(), "midi:sx:F07EF7");
    }

    #[test]
    fn malformed_and_out_of_range_ids_are_rejected() {
        for bad in [
            "midi:note:0:60", "midi:note:17:60", "midi:note:1:128", "midi:note:1",
            "midi:note:1:60:9", "midi:cc14:1:32", "midi:nrpn:1:16384", "midi:pb:1:3",
            "midi:rt:pause", "midi:sx:F07E", "midi:sx:F0F7F", "midi:sx:F080F7",
            "midi:bogus:1:1", "cc_7", "macro:abc", "",
        ] {
            assert_eq!(parse_pin(bad), None, "{bad} should not parse");
        }
        let too_long = format!("midi:sx:F0{}F7", "00".repeat(SYSEX_MAX_BYTES - 1));
        assert_eq!(parse_pin(&too_long), None);
    }

    #[test]
    fn legacy_ids_map_to_any_channel_pins() {
        assert_eq!(legacy_pin("cc_7"), Some(MidiPin::Cc { ch: Channel::Any, cc: 7 }));
        assert_eq!(legacy_pin("pitch_bend"), Some(MidiPin::PitchBend { ch: Channel::Any }));
        assert_eq!(legacy_pin("cc_128"), None);
        assert_eq!(legacy_pin("midi:cc:1:7"), None);
    }

    #[test]
    fn outputs_need_a_concrete_channel_and_real_messages() {
        assert!(MidiPin::Note { ch: Channel::Ch(0), note: 1 }.is_output_capable());
        assert!(!MidiPin::Note { ch: Channel::Any, note: 1 }.is_output_capable());
        assert!(!MidiPin::Bpm.is_output_capable());
        assert!(!MidiPin::Playing.is_output_capable());
        assert!(MidiPin::Transport(Transport::Start).is_output_capable());
    }

    #[test]
    fn note_names_use_c4_as_middle_c() {
        assert_eq!(note_name(60), "C4");
        assert_eq!(note_name(0), "C-1");
        assert_eq!(note_name(127), "G9");
        assert_eq!(note_name(66), "F#4");
    }

    #[test]
    fn bend_scaling_centres_and_clamps() {
        assert_eq!(bend_to_14bit(0.0), 8192);
        assert_eq!(bend_to_14bit(-1.0), 0);
        assert_eq!(bend_to_14bit(1.0), 16383);
        assert_eq!(bend_from_14bit(8192), 0.0);
        assert_eq!(bend_from_14bit(0), -1.0);
        assert_eq!(bend_to_14bit(bend_from_14bit(12000)), 12000);
    }

    #[test]
    fn channel_messages_round_trip_through_bytes() {
        let msgs = [
            MidiMessage::NoteOff { ch: 3, note: 60, vel: 64 },
            MidiMessage::NoteOn { ch: 15, note: 1, vel: 127 },
            MidiMessage::PolyAftertouch { ch: 0, note: 5, value: 9 },
            MidiMessage::ControlChange { ch: 2, cc: 74, value: 100 },
            MidiMessage::ProgramChange { ch: 1, program: 42 },
            MidiMessage::ChannelPressure { ch: 4, value: 33 },
            MidiMessage::PitchBend { ch: 9, value: 12345 },
            MidiMessage::SysEx(vec![0xF0, 0x01, 0xF7]),
            MidiMessage::TimingClock,
            MidiMessage::Start,
            MidiMessage::Continue,
            MidiMessage::Stop,
        ];
        for m in msgs {
            let mut rs = None;
            assert_eq!(MidiMessage::parse(&m.to_bytes(), &mut rs), Some(m.clone()));
        }
    }

    #[test]
    fn note_on_with_zero_velocity_is_a_note_off() {
        let mut rs = None;
        assert_eq!(
            MidiMessage::parse(&[0x92, 60, 0], &mut rs),
            Some(MidiMessage::NoteOff { ch: 2, note: 60, vel: 0 })
        );
    }

    #[test]
    fn running_status_decodes_data_only_messages() {
        let mut rs = None;
        MidiMessage::parse(&[0xB1, 7, 100], &mut rs).unwrap();
        assert_eq!(
            MidiMessage::parse(&[10, 64], &mut rs),
            Some(MidiMessage::ControlChange { ch: 1, cc: 10, value: 64 })
        );
        // Realtime keeps running status; system common clears it.
        MidiMessage::parse(&[0xF8], &mut rs).unwrap();
        assert!(MidiMessage::parse(&[11, 1], &mut rs).is_some());
        MidiMessage::parse(&[0xF6], &mut rs).unwrap();
        assert_eq!(MidiMessage::parse(&[11, 1], &mut rs), None);
    }

    #[test]
    fn truncated_messages_do_not_parse() {
        let mut rs = None;
        assert_eq!(MidiMessage::parse(&[0x90, 60], &mut rs), None);
        assert_eq!(MidiMessage::parse(&[0xE0, 0x7F], &mut rs), None);
        assert_eq!(MidiMessage::parse(&[], &mut rs), None);
    }
}
