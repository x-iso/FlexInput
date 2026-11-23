//! Steam Controller (2026, "Triton") haptics, over a HID handle of our own.
//!
//! `steam_controller` reads the pad; a worker thread here opens the same
//! collection on a handle of its own and writes every haptic report: the rumble
//! (report 0x80, as SDL's driver sent it) and all the reports SDL had no way to.
//!
//! The pad has four actuators, and each is driven the way that was found to work
//! on the hardware (2026-10-31, over the puck):
//!
//!   * grip motors: a PCM stream, because only a waveform plays both carriers of
//!     a `grip_*` / `grip2_*` pair. Report 0x86 sets the stream up for both motors,
//!     then each 0x88 carries both sides' samples. 0x44 reports back how each
//!     buffer is doing, and the sending rate trims itself by it, a little.
//!   * trackpads: tones (report 0x83), which need no pacing. A stream reaches
//!     each pad alone as well (see [`PAD_STREAM`]), which a soft click uses.
//!   * trackpad clicks: the firmware's click command (report 0x82).
//!
//! The trigger click is ours as well. The pad plays none of its own: the click
//! felt with Steam open is Steam Input's, played when the pad's click bit sets.
//! So [`TriggerClick`] watches the analog value here, and while it is on the
//! pad's threshold (setting 68) sits above reach, which keeps that bit, and so
//! Steam's click, from firing as well. Nothing else here reads that bit.
//! [`PadClickFeedback`] does the like for a press on either trackpad.

use std::ffi::CString;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use flexinput_core::automap::{click_envelope, PitchScale, TriggerCrossing, TriggerEdges};
use hidapi::{HidApi, HidDevice};

/// The Steam Controller and its dongles (`automap::capacitive_misc_mask` knows
/// them by the same range).
pub fn is_steam_controller(vid: Option<u16>, pid: Option<u16>) -> bool {
    matches!((vid, pid), (Some(0x28DE), Some(0x1302..=0x1305)))
}

const VENDOR_FIRST_USAGE_PAGE: u16 = 0xFF00;

// Reports.
const REPORT_TONE: u8 = 0x83;
const REPORT_COMMAND: u8 = 0x82;
const REPORT_STREAM_OP: u8 = 0x86;
/// Samples for one actuator: a trackpad's burst.
const REPORT_STREAM_PUSH: u8 = 0x87;
/// Samples for both grips in one report, `[0x88, n, left × 31, right × 31]`.
const REPORT_STREAM_PUSH_GRIPS: u8 = 0x88;
const REPORT_STREAM_STATUS: u8 = 0x44;
/// Feature report 1 carries settings as `[1, op, len, id, value_le16]`.
const FEATURE_REPORT: u8 = 1;
const OP_SET_SETTING: u8 = 0x87;
const OP_GET_SETTING: u8 = 0x89;

// Tone report sides.
const SIDE_PAD_L: u8 = 0;
const SIDE_GRIP_L: u8 = 3;
const SIDE_GRIP_R: u8 = 4;

const COMMAND_STOP: u8 = 0;
const COMMAND_CLICK: u8 = 1;
const COMMAND_CLICK_STRONG: u8 = 2;

// Stream setup and targets.
const STREAM_STOP: u8 = 1;
const STREAM_START: u8 = 2;
const STREAM_BOTH_GRIPS: u8 = 2;
/// 4 kHz µ-law (G.711), which both the grips and the trackpads take. Heard on
/// the pad over the puck (2026-11-23, `scbuzz` probe): clean through 0x88, as
/// was 8-bit linear (format 5); at 8 kHz (format 8) the puck doesn't carry
/// enough reports and the stream keeps running dry, a galloping stutter.
const STREAM_FORMAT: u8 = 9;
const STREAM_RATE: f32 = 4000.0;
/// Samples in a trackpad's 0x87 report.
const STREAM_CHUNK: usize = 62;
/// Samples per grip in a 0x88 report. The grips' samples went out in 0x87s first,
/// each grip its own, and every tone rattled under the probe, even with the
/// right grip sent nothing; through 0x88 the same tone is clean.
const GRIP_SLOT: usize = 31;
/// Each trackpad's stream, `(0x86 target, 0x87 target)`. Set up alone, the right
/// pad takes id 5, which otherwise writes both.
const PAD_STREAM: [(u8, u8); 2] = [(3, 3), (4, 5)];
/// Silence after a pad's burst before stopping its stream, in samples, in case the
/// pad holds back the tail.
const PAD_BURST_TAIL: u32 = 2 * STREAM_CHUNK as u32;

// Stream status bits, per motor.
const STATUS_OVERRUN: u8 = 0x01;
const STATUS_STOPPED: u8 = 0x02;
const STATUS_NEEDS_MORE: u8 = 0x04;
const STATUS_HAS_ENOUGH: u8 = 0x08;
const STATUS_ALREADY_RUNNING: u8 = 0x40;

/// A stream with nothing to play ends this long after going quiet, so that a
/// signal fading in and out doesn't set it up again every time.
const STREAM_IDLE: Duration = Duration::from_millis(250);
/// Ceiling on samples owed to the grips, so a stalled thread doesn't send a burst.
const MAX_OWED: f32 = STREAM_RATE * 0.1;
/// What a stream starts with in hand, and is topped up by when the pad says it
/// ran dry: ~46 ms, so a late wake doesn't leave it nothing to play. 120 ms
/// overran the pad's buffer under the probe: a thump each time.
const STREAM_PRIME: f32 = 6.0 * GRIP_SLOT as f32;
/// How long after it starts a stream's "ran dry" is the start's, not a real one.
const STREAM_SETTLE: Duration = Duration::from_millis(200);
/// How far the sending rate trims itself each time a buffer reports low or full,
/// and how far it goes at most. Others streaming to this pad found it follows
/// the pace samples arrive at (sent slow, a tone "plays longer and lower"), so a
/// rate that swung by an eighth, as this one did, would swing the pitch two
/// semitones back and forth.
const TRIM_STEP: f32 = 0.001;
const TRIM_MAX: f32 = 0.006;

/// How fast a grip's carriers follow a new strength and pitch: about 3 ms, so a
/// change that lands between two chunks glides rather than steps (a step every
/// chunk, ~65 times a second, is a rattle of its own).
const GLIDE_S: f32 = 0.003;
/// The highest pitch the grips' stream plays: past it the 4 kHz stream has too
/// few samples a cycle to make a tone. The trackpads' tones aren't sampled and go
/// to the scale's top.
const GRIP_STREAM_MAX_HZ: f32 = 1600.0;

const SILENCE: f32 = 1.0 / 255.0;
/// Where a carrier with an amplitude but no frequency sits: near the resonance.
const DEFAULT_HZ: f32 = 185.0;
const GAIN_DB_MIN: f32 = -23.0;

/// How long a tone lasts unless renewed, so a lost update can't hold it on.
const TONE_MS: u16 = 250;
const TONE_RENEW: Duration = Duration::from_millis(150);
/// Tones restart when changed, so changes wait this long between them.
const TONE_MIN_GAP: Duration = Duration::from_millis(30);

/// A one-shot click pin counts as up above this.
const CLICK_PIN_MIN: f32 = 0.02;
/// A one-shot's tone report would take anything; these keep it something an
/// actuator can play: the wide scale. As samples it goes no higher than
/// [`GRIP_STREAM_MAX_HZ`].
const CLICK_HZ_RANGE: (f32, f32) = flexinput_core::automap::WIDE_HZ;
const CLICK_MS_RANGE: (f32, f32) = (1.0, 200.0);
/// A trackpad's tone click lets go at half the strength it pressed with.
const PAD_RELEASE_DB: i8 = -6;

/// Setting 68: where the pad sets its click bit, in percent of travel. The bit
/// sets about 5 above it and clears about 5 below, so 99 never sets.
const SETTING_THRESHOLD: u8 = 68;
const THRESHOLD_OUT_OF_REACH: i16 = 99;
const THRESHOLD_DEFAULT: i16 = 90;

/// Settings 34 and 35: the grip sensors' touch level and release level, each with
/// its range. The same two Steam's Grip Sensors page writes. The raw reading they
/// are compared with stays in the firmware: the host only ever sees a bit per grip.
const GRIP_SETTINGS: [(&str, u8, i16, i16); 2] = [
    ("grip_touch", 34, 25, 400),
    ("grip_release", 35, 25, 100),
];
/// A dragged slider writes once it has rested this long.
const GRIP_WRITE_DELAY: Duration = Duration::from_millis(150);

// ── Targets ───────────────────────────────────────────────────────────────────

/// The analog pins the worker reads, in the order of [`Shared::targets`].
const TARGET_PINS: [&str; 14] = [
    "grip_l_amp", "grip_l_freq", "grip_r_amp", "grip_r_freq",
    "grip2_l_amp", "grip2_l_freq", "grip2_r_amp", "grip2_r_freq",
    "tp_l_amp", "tp_l_freq", "tp_r_amp", "tp_r_freq",
    "rumble_strong", "rumble_weak",
];

/// Each side's offset into [`TARGET_PINS`] from carrier 1, carrier 2 and pad.
const GRIP: usize = 0;
const GRIP2: usize = 4;
const TP: usize = 8;
/// The two rumble levels, strong (left) then weak (right).
const RUMBLE: usize = 12;

/// The rumble report: `[0x80, type, intensity_le16, left speed_le16, left gain,
/// right speed_le16, right gain]`, as SDL's driver sends it.
const REPORT_RUMBLE: u8 = 0x80;
/// The pad stops a rumble about 50 ms after the last report (SDL's driver),
/// so a lasting one goes out again this often.
const RUMBLE_RESEND: Duration = Duration::from_millis(40);

#[derive(Default)]
struct Shared {
    /// Levels as `f32` bits, so the I/O thread never waits on the worker.
    targets: [AtomicU32; TARGET_PINS.len()],
}

impl Shared {
    fn get(&self, i: usize) -> f32 {
        f32::from_bits(self.targets[i].load(Ordering::Relaxed))
    }

    /// One carrier: `(amp, normalized freq)`, on `side` (0 left, 1 right).
    fn carrier(&self, base: usize, side: usize) -> (f32, f32) {
        (self.get(base + side * 2), self.get(base + side * 2 + 1))
    }
}

/// A frequency pin's pitch, on the wide scale; 0 (unset) is [`DEFAULT_HZ`].
fn carrier_hz(freq: f32) -> f32 {
    if freq <= 0.0 { DEFAULT_HZ } else { PitchScale::Wide.hz(freq) }
}

/// An amplitude as the dB the tone report takes.
fn amp_to_gain_db(amp: f32) -> i8 {
    (20.0 * amp.clamp(SILENCE, 1.0).log10()).max(GAIN_DB_MIN).round() as i8
}

// ── Trigger click ─────────────────────────────────────────────────────────────

/// What a one-shot plays: a sine's pitch and length, and how much of it fades in
/// and out. The firmware's tone starts and stops dead (its LFO settings shape nothing
/// usable), so `soft` above 0 goes out as samples, and at 0 as the tone.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ClickShape {
    hz: u16,
    ms: u16,
    soft: f32,
}

impl ClickShape {
    fn new(hz: f32, ms: f32, soft: f32) -> Self {
        Self {
            hz: hz.clamp(CLICK_HZ_RANGE.0, CLICK_HZ_RANGE.1).round() as u16,
            ms: ms.clamp(CLICK_MS_RANGE.0, CLICK_MS_RANGE.1).round() as u16,
            soft: soft.clamp(0.0, 1.0),
        }
    }

    fn trigger_default() -> Self {
        use flexinput_core::automap::{TRIGGER_CLICK_HZ, TRIGGER_CLICK_MS};
        Self::new(TRIGGER_CLICK_HZ, TRIGGER_CLICK_MS, 0.0)
    }

    fn played_as_samples(self) -> bool {
        self.soft > 0.0
    }

    /// Its length in samples on the grip stream.
    fn samples(self) -> u32 {
        self.ms as u32 * STREAM_RATE as u32 / 1000
    }
}

/// A one-shot in progress, in samples: on a grip's stream or a pad's own.
#[derive(Default, Clone, Copy, Debug, PartialEq)]
struct ClickBurst {
    at: u32,
    total: u32,
    level: f32,
    hz: f32,
    soft: f32,
}

impl ClickBurst {
    fn new(shape: ClickShape, gain_db: i8) -> Self {
        Self {
            at: 0,
            total: shape.samples(),
            level: 10f32.powf(gain_db as f32 / 20.0).min(1.0),
            hz: (shape.hz as f32).min(GRIP_STREAM_MAX_HZ),
            soft: shape.soft,
        }
    }

    fn playing(&self) -> bool {
        self.at < self.total
    }

    /// The next sample, or silence once it has ended.
    fn next(&mut self) -> f32 {
        if !self.playing() {
            return 0.0;
        }
        let i = self.at as f32;
        self.at += 1;
        let env = click_envelope(i / self.total as f32, self.soft);
        self.level * env * (i / STREAM_RATE * self.hz * std::f32::consts::TAU).sin()
    }
}

/// What one edge of a trigger plays: strength in dB, pitch, length, softness.
#[derive(Clone, Copy, Debug, PartialEq)]
struct EdgeClick {
    gain_db: f32,
    hz: f32,
    ms: f32,
    soft: f32,
}

impl EdgeClick {
    fn command(self, side: usize) -> Cmd {
        Cmd::TriggerClick {
            side,
            gain_db: self.gain_db.clamp(GAIN_DB_MIN, 24.0).round() as i8,
            shape: ClickShape::new(self.hz, self.ms, self.soft),
        }
    }
}

/// Where each trigger clicks and lets go again, on its raw 0–1 travel, and how
/// long it stays under the band before it lets go, as the engine sends them
/// (`feedback::apply_trigger_click`, from `automap::trigger_edges`), and what it
/// plays on the way in and on the way back out, each where asked. A point at 0
/// = no click.
struct TriggerClick {
    edges: [TriggerEdges; 2],
    hold: [f32; 2],
    press: [EdgeClick; 2],
    press_on: [bool; 2],
    release: [EdgeClick; 2],
    release_on: [bool; 2],
    crossing: [TriggerCrossing; 2],
}

impl Default for TriggerClick {
    fn default() -> Self {
        use flexinput_core::automap::{TRIGGER_CLICK_DB, TRIGGER_CLICK_HZ, TRIGGER_CLICK_MS, TRIGGER_RELEASE_DB};
        let press = EdgeClick { gain_db: TRIGGER_CLICK_DB, hz: TRIGGER_CLICK_HZ, ms: TRIGGER_CLICK_MS, soft: 0.0 };
        Self {
            edges: Default::default(),
            hold: [0.0; 2],
            press: [press; 2],
            press_on: [true; 2],
            release: [EdgeClick { gain_db: TRIGGER_RELEASE_DB, ..press }; 2],
            release_on: [false; 2],
            crossing: Default::default(),
        }
    }
}

impl TriggerClick {
    fn on(&self) -> bool {
        self.edges.iter().any(|e| e.press > 0.0)
    }

    /// One poll's trigger values, `dt` seconds after the last: what each side
    /// plays, if anything.
    fn step(&mut self, travel: [f32; 2], dt: f32) -> [Option<Cmd>; 2] {
        let mut out = [None, None];
        for side in 0..2 {
            out[side] = match self.crossing[side].step(&self.edges[side], travel[side], dt, self.hold[side]) {
                Some(true) if self.press_on[side] => Some(self.press[side].command(side)),
                Some(false) if self.release_on[side] => Some(self.release[side].command(side)),
                _ => None,
            };
        }
        out
    }

    /// Takes a pin of this family; false for any other.
    fn set(&mut self, pin: &str, level: f32) -> bool {
        let Some((name, side)) = pin.strip_suffix("_l").map(|n| (n, 0))
            .or_else(|| pin.strip_suffix("_r").map(|n| (n, 1)))
        else {
            return false;
        };
        let (edges, press, release) = (&mut self.edges[side], &mut self.press[side], &mut self.release[side]);
        match name {
            "trig_click" => edges.press = level.max(0.0),
            "trig_top" => edges.again = level.max(0.0),
            "trig_rearm" => edges.release = level.max(0.0),
            "trig_rest" => edges.rest = level.max(0.0),
            "trig_hold" => self.hold[side] = level.max(0.0),
            "trig_press" => self.press_on[side] = level > 0.5,
            "trig_click_gain" => press.gain_db = level,
            "trig_click_hz" => press.hz = level,
            "trig_click_ms" => press.ms = level,
            "trig_click_soft" => press.soft = level,
            "trig_release" => self.release_on[side] = level > 0.5,
            "trig_release_gain" => release.gain_db = level,
            "trig_release_hz" => release.hz = level,
            "trig_release_ms" => release.ms = level,
            "trig_release_soft" => release.soft = level,
            _ => return false,
        }
        true
    }
}

// ── One-shot click pins ───────────────────────────────────────────────────────

/// The `automap::CLICK_TONE_PINS`. A click is due as its pin rises, as strong as it
/// rose to: a wired button reads 1, full strength. It goes out on the next poll, as
/// the pins of one pass arrive in no particular order and its shape may be behind it.
#[derive(Default)]
struct ClickPins {
    up: [bool; 4],
    due: [Option<f32>; 4],
    /// Each pin's `[hz, ms, soft]`; no pitch = the pad's plain click.
    tone: [[f32; 3]; 4],
}

impl ClickPins {
    /// Takes a pin of this family; false for any other.
    fn set(&mut self, pin: &str, level: f32) -> bool {
        use flexinput_core::automap::CLICK_TONE_PINS;
        for (i, (click, tone)) in CLICK_TONE_PINS.iter().enumerate() {
            if pin == *click {
                let down = level > CLICK_PIN_MIN;
                if down && !self.up[i] {
                    self.due[i] = Some(level);
                }
                self.up[i] = down;
                return true;
            }
            if let Some(field) = tone.iter().position(|p| *p == pin) {
                self.tone[i][field] = level;
                return true;
            }
        }
        false
    }

    /// The clicks due now.
    fn take_due(&mut self) -> Vec<Cmd> {
        let mut out = Vec::new();
        for i in 0..self.due.len() {
            let Some(level) = self.due[i].take() else { continue };
            let (side, grip) = (i % 2, i >= 2);
            let gain_db = amp_to_gain_db(level);
            let [hz, ms, soft] = self.tone[i];
            let shape = (hz > 0.0).then(|| ClickShape::new(hz, ms, soft));
            out.push(match (grip, shape) {
                (true, shape) => Cmd::TriggerClick {
                    side, gain_db, shape: shape.unwrap_or_else(ClickShape::trigger_default),
                },
                (false, Some(shape)) => Cmd::PadTone { side, gain_db, shape },
                (false, None) => Cmd::PadClick { side, command: COMMAND_CLICK, gain_db },
            });
        }
        out
    }
}

// ── Pad click feedback ────────────────────────────────────────────────────────

/// A click on each trackpad as it is pressed, and optionally released, set in the
/// Calibration window, each pad its own.
#[derive(Default)]
struct PadClickFeedback {
    sides: [PadClickSide; 2],
    held: [bool; 2],
}

/// One trackpad's click feedback. `mode` is the `automap::PAD_CLICK_*` bits,
/// 0 = off.
#[derive(Clone, Copy)]
struct PadClickSide {
    mode: u8,
    gain_db: f32,
    hz: f32,
    ms: f32,
    soft: f32,
}

impl Default for PadClickSide {
    fn default() -> Self {
        use flexinput_core::automap::{PAD_CLICK_HZ, PAD_CLICK_MS};
        Self { mode: 0, gain_db: 0.0, hz: PAD_CLICK_HZ, ms: PAD_CLICK_MS, soft: 0.0 }
    }
}

impl PadClickFeedback {
    /// Takes a pad click pin (`automap::PAD_CLICK_PINS`); false for any other.
    fn set(&mut self, pin: &str, level: f32) -> bool {
        use flexinput_core::automap::PAD_CLICK_PINS;
        let Some((side, field)) = PAD_CLICK_PINS.iter().enumerate()
            .find_map(|(side, pins)| pins.iter().position(|p| *p == pin).map(|f| (side, f))) else {
            return false;
        };
        let s = &mut self.sides[side];
        match field {
            0 => s.mode = level as u8,
            1 => s.gain_db = level,
            2 => s.hz = level,
            3 => s.ms = level,
            _ => s.soft = level,
        }
        true
    }

    /// One poll's pad clicks: what each side plays, if any. Keeps track while
    /// off, so turning it on with a pad held doesn't click.
    fn step(&mut self, down: [bool; 2]) -> [Option<Cmd>; 2] {
        use flexinput_core::automap::{PAD_CLICK_ON, PAD_CLICK_ON_RELEASE, PAD_CLICK_STRONG, PAD_CLICK_TONE};
        let mut out = [None, None];
        for side in 0..2 {
            let s = self.sides[side];
            let gain_db = s.gain_db.clamp(GAIN_DB_MIN, 24.0).round() as i8;
            let was = std::mem::replace(&mut self.held[side], down[side]);
            if s.mode & PAD_CLICK_ON == 0 || was == down[side] {
                continue;
            }
            if !down[side] && s.mode & PAD_CLICK_ON_RELEASE == 0 {
                continue;
            }
            out[side] = Some(if s.mode & PAD_CLICK_TONE != 0 {
                let gain_db = if down[side] { gain_db } else { gain_db.saturating_add(PAD_RELEASE_DB) };
                Cmd::PadTone { side, gain_db, shape: ClickShape::new(s.hz, s.ms, s.soft) }
            } else {
                let strong = down[side] && s.mode & PAD_CLICK_STRONG != 0;
                let command = if strong { COMMAND_CLICK_STRONG } else { COMMAND_CLICK };
                Cmd::PadClick { side, command, gain_db }
            });
        }
        out
    }
}

// ── Handle ────────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
enum Cmd {
    /// A trigger click on the side's grip motor.
    TriggerClick { side: usize, gain_db: i8, shape: ClickShape },
    /// A firmware click on the side's trackpad.
    PadClick { side: usize, command: u8, gain_db: i8 },
    /// A tone click on the side's trackpad.
    PadTone { side: usize, gain_db: i8, shape: ClickShape },
    /// Keep the pad's click bit from setting, or let it again.
    HoldBit(bool),
    /// A grip sensor setting: its index in [`GRIP_SETTINGS`], and the value.
    Grip(usize, i16),
}

/// One pad's haptics: lives on its `steam_controller` pad, and dropping it ends
/// the worker.
pub(crate) struct SteamHaptics {
    shared: Arc<Shared>,
    cmds: Sender<Cmd>,
    click: TriggerClick,
    /// When the triggers were last read, which the click's debounce counts from.
    triggers_read: Option<Instant>,
    pad_feedback: PadClickFeedback,
    bit_held: bool,
    click_pins: ClickPins,
    grip_sent: [Option<i16>; GRIP_SETTINGS.len()],
}

impl SteamHaptics {
    pub(crate) fn spawn(path: Option<String>, vid: Option<u16>, pid: Option<u16>) -> Option<Self> {
        let shared = Arc::new(Shared::default());
        let (cmds, rx) = mpsc::channel();
        let worker = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("steam-haptics".into())
            .spawn(move || {
                let Some(dev) = open(path.as_deref(), vid, pid) else {
                    eprintln!("[steam-haptics] no HID handle — rumble only");
                    return;
                };
                Worker::new(dev, worker).run(rx);
            })
            .ok()?;
        Some(Self {
            shared,
            cmds,
            click: TriggerClick::default(),
            triggers_read: None,
            pad_feedback: PadClickFeedback::default(),
            bit_held: false,
            click_pins: ClickPins::default(),
            grip_sent: Default::default(),
        })
    }

    /// Takes a pin this module owns; false for any other, which the caller routes
    /// as for every pad. Called from the I/O thread: never waits.
    pub(crate) fn set(&mut self, pin: &str, level: f32) -> bool {
        if let Some(i) = TARGET_PINS.iter().position(|p| *p == pin) {
            self.shared.targets[i].store(level.to_bits(), Ordering::Relaxed);
            return true;
        }
        if self.click_pins.set(pin, level) {
            return true;
        }
        if self.click.set(pin, level) {
            let on = self.click.on();
            if on != self.bit_held {
                self.bit_held = on;
                let _ = self.cmds.send(Cmd::HoldBit(on));
            }
            return true;
        }
        if self.pad_feedback.set(pin, level) {
            return true;
        }
        let Some(i) = GRIP_SETTINGS.iter().position(|s| s.0 == pin) else {
            return false;
        };
        if let Some(value) = grip_value(i, level) {
            if self.grip_sent[i] != Some(value) {
                self.grip_sent[i] = Some(value);
                let _ = self.cmds.send(Cmd::Grip(i, value));
            }
        }
        true
    }

    /// This poll's trigger travel (raw 0–1), left and right.
    pub(crate) fn poll_triggers(&mut self, left: f32, right: f32) {
        for cmd in self.click_pins.take_due() {
            let _ = self.cmds.send(cmd);
        }
        let now = Instant::now();
        let dt = self.triggers_read.replace(now).map_or(0.0, |t| now.duration_since(t).as_secs_f32());
        for cmd in self.click.step([left, right], dt).into_iter().flatten() {
            let _ = self.cmds.send(cmd);
        }
    }

    /// This poll's pad clicks, left and right.
    pub(crate) fn poll_pad_clicks(&mut self, left: bool, right: bool) {
        for cmd in self.pad_feedback.step([left, right]).into_iter().flatten() {
            let _ = self.cmds.send(cmd);
        }
    }
}

/// A level on a grip pin as the setting's value. `None` for the 0 zero-on-drop
/// sends when the pin stops, which leaves the pad as it is.
fn grip_value(setting: usize, level: f32) -> Option<i16> {
    let (_, _, min, max) = GRIP_SETTINGS[setting];
    (level > 0.0).then(|| (level.round() as i16).clamp(min, max))
}

// ── Opening ───────────────────────────────────────────────────────────────────

/// SDL's path if it reaches the collection with the haptic reports, else the
/// vendor collection that is streaming state: a puck lists one per pad slot, and
/// the empty ones answer nothing.
fn open(path: Option<&str>, vid: Option<u16>, pid: Option<u16>) -> Option<HidDevice> {
    let api = HidApi::new().map_err(|e| eprintln!("[steam-haptics] hidapi: {e}")).ok()?;
    if let Some(dev) = path.and_then(|p| open_path(&api, p)) {
        return Some(dev);
    }
    let live = api.device_list()
        .filter(|i| {
            Some(i.vendor_id()) == vid
                && Some(i.product_id()) == pid
                && i.usage_page() >= VENDOR_FIRST_USAGE_PAGE
                && i.usage() == 1
        })
        .filter_map(|i| i.open_device(&api).ok())
        .find(streams_state);
    live
}

fn open_path(api: &HidApi, path: &str) -> Option<HidDevice> {
    let path = CString::new(path).ok()?;
    #[cfg(target_os = "linux")]
    let path = crate::linux_hide::reachable(&path);
    let dev = api.open_path(&path).ok()?;
    let mut desc = [0u8; 1024];
    let n = dev.get_report_descriptor(&mut desc).ok()?;
    // The Report ID item for the stream's push report.
    desc[..n].windows(2).any(|w| w == [0x85, REPORT_STREAM_PUSH]).then_some(dev)
}

fn streams_state(dev: &HidDevice) -> bool {
    let mut buf = [0u8; 64];
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(400) {
        if matches!(dev.read_timeout(&mut buf, 50), Ok(n) if n > 0 && matches!(buf[0], 0x42 | 0x45 | 0x47)) {
            return true;
        }
    }
    false
}

// ── Worker ────────────────────────────────────────────────────────────────────

/// A trackpad's burst, and how its stream is doing: from idle it sets up, sends in
/// real time with a little silence after, and stops, the way it was proved on the pad.
#[derive(Clone, Copy)]
struct PadBurst {
    burst: ClickBurst,
    /// Samples sent since the burst began, silence included.
    sent: u32,
    owed: f32,
    last: Instant,
}

/// Both grip motors' stream, the two sides in step.
struct Stream {
    running: bool,
    started: Instant,
    last_sound: Instant,
    last_pump: Instant,
    owed: f32,
    /// The sending rate, as a share above or below [`STREAM_RATE`].
    trim: f32,
    /// Carrier 1 and 2 phase per side.
    phase: [[f32; 2]; 2],
    /// Carrier 1 and 2 strength and phase step per side as they play, gliding to
    /// the targets.
    level: [[f32; 2]; 2],
    step: [[f32; 2]; 2],
    /// What a click adds to a side's stream: the tone report isn't known to play
    /// over a stream on the same motor, and a soft one needs a stream anyway.
    click: [ClickBurst; 2],
}

impl Stream {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            running: false,
            started: now,
            last_sound: now,
            last_pump: now,
            owed: 0.0,
            trim: 0.0,
            phase: [[0.0; 2]; 2],
            level: [[0.0; 2]; 2],
            step: [[0.0; 2]; 2],
            click: Default::default(),
        }
    }

    /// Both sides' next samples, from the current targets, as one 0x88 report.
    fn report(&mut self, shared: &Shared) -> [u8; 2 + 2 * GRIP_SLOT] {
        let mut out = [0u8; 2 + 2 * GRIP_SLOT];
        out[0] = REPORT_STREAM_PUSH_GRIPS;
        out[1] = GRIP_SLOT as u8;
        for side in 0..2 {
            let at = 2 + side * GRIP_SLOT;
            self.fill(side, shared, &mut out[at..at + GRIP_SLOT]);
        }
        out
    }

    /// One side's next samples.
    fn fill(&mut self, side: usize, shared: &Shared, out: &mut [u8]) {
        let targets = [shared.carrier(GRIP, side), shared.carrier(GRIP2, side)]
            .map(|(a, f)| (a.clamp(0.0, 1.0), carrier_hz(f).min(GRIP_STREAM_MAX_HZ) / STREAM_RATE));
        let (phase, level, step, click) =
            (&mut self.phase[side], &mut self.level[side], &mut self.step[side], &mut self.click[side]);
        for (k, (_, to)) in targets.into_iter().enumerate() {
            // A carrier starting from silence starts at its pitch, not gliding up
            // from wherever it last was.
            if level[k] <= SILENCE {
                step[k] = to;
            }
        }
        let glide = 1.0 - (-1.0 / (STREAM_RATE * GLIDE_S)).exp();
        for s in out {
            let mut v = click.next();
            for (k, (amp, to)) in targets.into_iter().enumerate() {
                level[k] += (amp - level[k]) * glide;
                step[k] += (to - step[k]) * glide;
                v += level[k] * (phase[k] * std::f32::consts::TAU).sin();
                phase[k] = (phase[k] + step[k]).fract();
            }
            *s = mulaw(soft_limit(v));
        }
    }
}

/// Over 0.8, eased into full scale rather than cut off at it: two carriers and a
/// click can add up past it, and a clipped wave buzzes.
fn soft_limit(v: f32) -> f32 {
    const KNEE: f32 = 0.8;
    let a = v.abs();
    if a <= KNEE {
        v
    } else {
        v.signum() * (KNEE + (1.0 - KNEE) * ((a - KNEE) / (1.0 - KNEE)).tanh())
    }
}

/// A sample (−1..1) as G.711 µ-law, the stream's format.
fn mulaw(v: f32) -> u8 {
    const BIAS: i32 = 0x84;
    const CLIP: i32 = 32635;
    let s = (v.clamp(-1.0, 1.0) * 32767.0) as i32;
    let sign = if s < 0 { 0x80 } else { 0 };
    let mag = s.abs().min(CLIP) + BIAS;
    let exponent = (31 - mag.leading_zeros() as i32 - 7).clamp(0, 7);
    let mantissa = (mag >> (exponent + 3)) & 0x0F;
    !(sign | (exponent << 4) | mantissa) as u8
}

// ── Diagnostics ───────────────────────────────────────────────────────────────

/// Input this long apart is a gap worth a line.
const DIAG_INPUT_GAP: Duration = Duration::from_millis(100);
/// A write this slow is worth a line.
const DIAG_SLOW_WRITE: Duration = Duration::from_millis(20);
/// The log stops growing here.
const DIAG_MAX_BYTES: u64 = 5 * 1024 * 1024;
// The puck's report that the controller's radio link came up or went down
// (SDL's `ID_TRITON_WIRELESS_STATUS` / `_X`), its first byte 1 = down, 2 = up.
const REPORT_WIRELESS: [u8; 2] = [0x79, 0x46];
// The controller's state reports.
const REPORT_STATE: [u8; 3] = [0x42, 0x45, 0x47];

/// Whether the diagnostic log was asked for.
pub fn diag_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("FLEXINPUT_STEAM_LOG").is_ok_and(|v| !v.is_empty()))
}

/// Where the diagnostic log goes: `FLEXINPUT_STEAM_LOG=on` for the logs
/// folder's `steam-controller.log`, or a file path.
fn diag_path() -> Option<std::path::PathBuf> {
    let want = std::env::var("FLEXINPUT_STEAM_LOG").ok().filter(|v| !v.is_empty())?;
    if want.eq_ignore_ascii_case("on") {
        Some(flexinput_core::paths::log_dir()?.join("steam-controller.log"))
    } else {
        Some(std::path::PathBuf::from(want))
    }
}

/// A line in the diagnostic log from outside any pad's worker (the I/O loop, the
/// SDL backend), on the same clock, so a stall there lines up with what the pad
/// was doing. Nothing without the log on.
pub fn diag_note(text: &str) {
    use std::io::Write;
    static FILE: std::sync::OnceLock<Option<std::sync::Mutex<std::fs::File>>> = std::sync::OnceLock::new();
    if !diag_on() {
        return;
    }
    let file = FILE.get_or_init(|| {
        let path = diag_path()?;
        std::fs::OpenOptions::new().create(true).append(true).open(path).ok().map(std::sync::Mutex::new)
    });
    if let Some(Ok(mut f)) = file.as_ref().map(|f| f.lock()) {
        let _ = f.write_all(format!("{} {:>11} {text}\n", utc_clock(), "").as_bytes());
    }
}

/// A heartbeat line this often, with how many reports the pad sent meanwhile.
const DIAG_HEARTBEAT: Duration = Duration::from_secs(5);

/// One setting operation in feature report 1: `op` on setting `id` with `value`.
/// Waits on the pad's answer, which the puck can hold for seconds.
fn feature(dev: &HidDevice, op: u8, id: u8, value: i16) -> bool {
    let mut b = [0u8; 65];
    let v = value.to_le_bytes();
    b[..6].copy_from_slice(&[FEATURE_REPORT, op, 3, id, v[0], v[1]]);
    dev.send_feature_report(&b).is_ok()
}

/// Sets setting `id` to `value` (see [`feature`]).
pub(crate) fn set_setting(dev: &HidDevice, id: u8, value: i16) -> bool {
    feature(dev, OP_SET_SETTING, id, value)
}

/// The time of day, UTC, for a log line someone will match against a clock.
fn utc_clock() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let s = t.as_secs() % 86_400;
    format!("{:02}:{:02}:{:02}.{:03}Z", s / 3600, s / 60 % 60, s % 60, t.subsec_millis())
}

/// Opt-in diagnostics, `FLEXINPUT_STEAM_LOG=on` (or a file path): the puck's
/// radio link reports, gaps in the controller's input, slow or failed writes,
/// and once a second while the grips stream, how the stream is doing. Enough to
/// tell "the pad drops out while it plays haptics" apart: the radio going down,
/// the pad going quiet on a live link, or neither (the gap is past this handle).
struct Diag {
    file: std::fs::File,
    written: u64,
    start: Instant,
    last_input: Option<Instant>,
    /// Since the last summary.
    sent: [u32; 2],
    low: u32,
    enough: u32,
    overruns: u32,
    write_errors: u32,
    slowest_write: Duration,
    summary_at: Instant,
    /// State reports since the last heartbeat.
    reports: u32,
    heartbeat_at: Instant,
}

impl Diag {
    fn open() -> Option<Self> {
        let path = diag_path()?;
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok()?;
        let now = Instant::now();
        let mut d = Self {
            file, written: 0, start: now, last_input: None, sent: [0; 2], low: 0, enough: 0,
            overruns: 0, write_errors: 0, slowest_write: Duration::ZERO, summary_at: now,
            reports: 0, heartbeat_at: now,
        };
        d.line(format_args!("started: the pad is on (log {})", path.display()));
        Some(d)
    }

    fn line(&mut self, what: std::fmt::Arguments) {
        use std::io::Write;
        if self.written >= DIAG_MAX_BYTES {
            return;
        }
        let text = format!("{} {:>10.3}s {what}\n", utc_clock(), self.start.elapsed().as_secs_f32());
        if self.file.write_all(text.as_bytes()).is_ok() {
            self.written += text.len() as u64;
        }
    }

    /// One report off the handle, as it is read.
    fn report(&mut self, buf: &[u8], streaming: bool) {
        let now = Instant::now();
        if REPORT_WIRELESS.contains(&buf[0]) && buf.len() > 1 {
            let state = match buf[1] { 1 => "DOWN", 2 => "up", _ => "?" };
            self.line(format_args!("radio link {state} (report {:#04x} {:#04x}), streaming: {streaming}", buf[0], buf[1]));
        }
        if REPORT_STATE.contains(&buf[0]) {
            if let Some(gap) = self.last_input.map(|t| now.duration_since(t)).filter(|g| *g >= DIAG_INPUT_GAP) {
                self.line(format_args!("no input for {} ms, streaming: {streaming}", gap.as_millis()));
            }
            self.last_input = Some(now);
            self.reports += 1;
        }
    }

    fn write_done(&mut self, report: u8, took: Duration, ok: bool) {
        if !ok {
            self.write_errors += 1;
        }
        if took >= DIAG_SLOW_WRITE {
            self.line(format_args!("write {report:#04x} took {} ms{}", took.as_millis(), if ok { "" } else { ", failed" }));
        }
        self.slowest_write = self.slowest_write.max(took);
    }

    /// The heartbeat, and the once-a-second line while the grips stream.
    fn tick(&mut self, streaming: bool) {
        let since = self.heartbeat_at.elapsed();
        if since >= DIAG_HEARTBEAT {
            let reports = self.reports;
            self.line(format_args!("alive: the pad sent {reports} reports in {:.1} s", since.as_secs_f32()));
            self.reports = 0;
            self.heartbeat_at = Instant::now();
        }
        if self.summary_at.elapsed() < Duration::from_secs(1) {
            return;
        }
        if streaming {
            let (sent, low, enough, overruns, errors) = (self.sent, self.low, self.enough, self.overruns, self.write_errors);
            let slowest = self.slowest_write.as_secs_f32() * 1000.0;
            self.line(format_args!(
                "stream: chunks L {} R {}, needs-more {low} enough {enough} overrun {overruns}, \
                 write errors {errors}, slowest write {slowest:.1} ms",
                sent[0], sent[1],
            ));
        }
        self.sent = [0; 2];
        (self.low, self.enough, self.overruns, self.write_errors) = (0, 0, 0, 0);
        self.slowest_write = Duration::ZERO;
        self.summary_at = Instant::now();
    }
}

/// How often the worker sends what is due while a stream plays.
const PUMP_EVERY: Duration = Duration::from_millis(2);

/// A wait that lands within a fraction of a millisecond. On Windows a
/// high-resolution waitable timer (Win10 1803+), which doesn't raise the global
/// timer resolution; elsewhere a plain sleep already does.
struct Waiter {
    #[cfg(windows)]
    timer: windows_sys::Win32::Foundation::HANDLE,
}

impl Waiter {
    fn new() -> Self {
        // SAFETY: FFI with null names and attributes; a null result is handled in
        // `wait`.
        #[cfg(windows)]
        let timer = unsafe {
            use windows_sys::Win32::System::Threading::{
                CreateWaitableTimerExW, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS,
            };
            CreateWaitableTimerExW(std::ptr::null(), std::ptr::null(), CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS)
        };
        Self {
            #[cfg(windows)]
            timer,
        }
    }

    fn wait(&self, dur: Duration) {
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Threading::{SetWaitableTimer, WaitForSingleObject};
            let due: i64 = -((dur.as_nanos() / 100).clamp(1, i64::MAX as u128) as i64);
            // SAFETY: FFI on our own timer handle.
            let armed = !self.timer.is_null()
                && unsafe { SetWaitableTimer(self.timer, &due, 0, None, std::ptr::null(), 0) } != 0;
            if armed {
                // SAFETY: as above.
                unsafe { WaitForSingleObject(self.timer, dur.as_millis() as u32 + 50) };
                return;
            }
        }
        std::thread::sleep(dur);
    }
}

#[cfg(windows)]
impl Drop for Waiter {
    fn drop(&mut self) {
        if !self.timer.is_null() {
            // SAFETY: our handle, unused after this.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(self.timer) };
        }
    }
}

/// One trackpad's tone, as last sent.
#[derive(Default, Clone, Copy)]
struct PadTone {
    sent: Option<(i8, u16)>,
    at: Option<Instant>,
}

struct Worker {
    dev: HidDevice,
    shared: Arc<Shared>,
    stream: Stream,
    pads: [PadTone; 2],
    pad_bursts: [Option<PadBurst>; 2],
    /// While holding the click bit: the threshold to put back.
    held_threshold: Option<i16>,
    logged_overrun: bool,
    /// Grip settings waiting to be written, and when the last one came in.
    grip_pending: [Option<i16>; GRIP_SETTINGS.len()],
    grip_changed: Instant,
    /// The rumble speeds last sent, and when.
    rumble: ([u16; 2], Option<Instant>),
    diag: Option<Diag>,
}

impl Worker {
    fn new(dev: HidDevice, shared: Arc<Shared>) -> Self {
        let _ = dev.set_blocking_mode(false);
        Self {
            dev,
            shared,
            stream: Stream::new(),
            pads: Default::default(),
            pad_bursts: [None, None],
            held_threshold: None,
            logged_overrun: false,
            grip_pending: Default::default(),
            grip_changed: Instant::now(),
            rumble: ([0; 2], None),
            diag: Diag::open(),
        }
    }

    /// The rumble motors from `rumble_strong` / `rumble_weak`: sent on a change,
    /// and again every [`RUMBLE_RESEND`] while either runs.
    fn pump_rumble(&mut self) {
        let speed = |i: usize| (self.shared.get(RUMBLE + i).clamp(0.0, 1.0) * 65535.0).round() as u16;
        let want = [speed(0), speed(1)];
        let (sent, at) = self.rumble;
        let due = want != sent
            || (want != [0, 0] && at.is_none_or(|t| t.elapsed() >= RUMBLE_RESEND));
        if !due || (want == [0, 0] && at.is_none()) {
            return;
        }
        let [l, r] = want.map(u16::to_le_bytes);
        self.write(&[REPORT_RUMBLE, 0, 0, 0, l[0], l[1], 0, r[0], r[1], 0]);
        self.rumble = (want, (want != [0, 0]).then(Instant::now));
    }

    fn run(mut self, cmds: mpsc::Receiver<Cmd>) {
        // A run that was killed while holding the bit left the threshold there.
        if self.setting(SETTING_THRESHOLD) == Some(THRESHOLD_OUT_OF_REACH) {
            self.set_setting(SETTING_THRESHOLD, THRESHOLD_DEFAULT);
        }
        let waiter = Waiter::new();
        'run: loop {
            // While samples go out, a wait that lands when asked: a plain one rounds
            // up to the system's ~15.6 ms tick on Windows, and the chunks would go
            // out in clumps.
            if self.stream.running || self.pad_bursts.iter().any(Option::is_some) {
                waiter.wait(PUMP_EVERY);
                loop {
                    match cmds.try_recv() {
                        Ok(cmd) => self.command(cmd),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => break 'run,
                    }
                }
            } else {
                match cmds.recv_timeout(PUMP_EVERY) {
                    Ok(cmd) => self.command(cmd),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            // With diagnostics on, the input is watched while idle too, for a
            // baseline; streaming reads it anyway.
            if self.diag.is_some() && !self.stream.running {
                self.drain_status();
            }
            let streaming = self.stream.running;
            if let Some(d) = self.diag.as_mut() {
                d.tick(streaming);
            }
            self.pump_stream();
            self.pump_pads();
            self.pump_pad_bursts();
            self.pump_rumble();
            if self.grip_changed.elapsed() >= GRIP_WRITE_DELAY {
                self.write_grip_settings();
            }
        }
        self.write_grip_settings();
        for side in 0..2 {
            if self.pad_bursts[side].take().is_some() {
                self.write(&[REPORT_STREAM_OP, STREAM_STOP, PAD_STREAM[side].0, 0]);
            }
        }
        self.write(&[REPORT_COMMAND, 2, COMMAND_STOP, 0]);
        if self.stream.running {
            self.write(&[REPORT_STREAM_OP, STREAM_STOP, STREAM_BOTH_GRIPS, 0]);
        }
        self.hold_bit(false);
        // The pad is gone: it went off or out of reach (a new worker opens when
        // it's back), or FlexInput is closing.
        if let Some(d) = self.diag.as_mut() {
            d.line(format_args!("stopped: the pad went away, or FlexInput is closing"));
        }
    }

    fn command(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::TriggerClick { side, gain_db, shape } => {
                if self.stream.running || shape.played_as_samples() {
                    self.stream.click[side] = ClickBurst::new(shape, gain_db);
                    self.stream.last_sound = Instant::now();
                } else {
                    let grip = [SIDE_GRIP_L, SIDE_GRIP_R][side];
                    self.tone(grip, gain_db, shape.hz, shape.ms);
                }
            }
            Cmd::PadTone { side, gain_db, shape } => {
                if shape.played_as_samples() {
                    self.start_pad_burst(side, ClickBurst::new(shape, gain_db));
                } else {
                    self.tone(SIDE_PAD_L + side as u8, gain_db, shape.hz, shape.ms);
                }
            }
            Cmd::PadClick { side, command, gain_db } => {
                self.write(&[REPORT_COMMAND, SIDE_PAD_L + side as u8, command, gain_db as u8]);
            }
            Cmd::HoldBit(hold) => self.hold_bit(hold),
            Cmd::Grip(i, value) => {
                self.grip_pending[i] = Some(value);
                self.grip_changed = Instant::now();
            }
        }
    }

    /// A pad's burst, from idle or over the one it is still playing.
    fn start_pad_burst(&mut self, side: usize, burst: ClickBurst) {
        let now = Instant::now();
        let (owed, last) = match &self.pad_bursts[side] {
            Some(p) => (p.owed, p.last),
            None => {
                self.write(&[REPORT_STREAM_OP, STREAM_START, PAD_STREAM[side].0, STREAM_FORMAT]);
                // A chunk goes out at once rather than after a chunk's worth of wait.
                (STREAM_CHUNK as f32, now)
            }
        };
        self.pad_bursts[side] = Some(PadBurst { burst, sent: 0, owed, last });
    }

    fn pump_pad_bursts(&mut self) {
        let now = Instant::now();
        for side in 0..2 {
            let Some(mut p) = self.pad_bursts[side] else { continue };
            let dt = now.duration_since(p.last).as_secs_f32();
            p.last = now;
            p.owed = (p.owed + STREAM_RATE * dt).min(MAX_OWED);
            while p.owed >= STREAM_CHUNK as f32 {
                let mut report = [0u8; STREAM_CHUNK + 2];
                report[0] = REPORT_STREAM_PUSH;
                report[1] = PAD_STREAM[side].1;
                for s in &mut report[2..] {
                    *s = mulaw(soft_limit(p.burst.next()));
                }
                self.write(&report);
                p.owed -= STREAM_CHUNK as f32;
                p.sent += STREAM_CHUNK as u32;
            }
            if p.sent >= p.burst.total + PAD_BURST_TAIL {
                self.write(&[REPORT_STREAM_OP, STREAM_STOP, PAD_STREAM[side].0, 0]);
                self.pad_bursts[side] = None;
            } else {
                self.pad_bursts[side] = Some(p);
            }
        }
    }

    fn write_grip_settings(&mut self) {
        for (i, (_, id, _, _)) in GRIP_SETTINGS.iter().enumerate() {
            if let Some(value) = self.grip_pending[i].take() {
                self.set_setting(*id, value);
            }
        }
    }

    fn hold_bit(&mut self, hold: bool) {
        match (hold, self.held_threshold) {
            (true, None) => {
                let was = self.setting(SETTING_THRESHOLD)
                    .filter(|v| *v != THRESHOLD_OUT_OF_REACH)
                    .unwrap_or(THRESHOLD_DEFAULT);
                self.held_threshold = Some(was);
                self.set_setting(SETTING_THRESHOLD, THRESHOLD_OUT_OF_REACH);
            }
            (false, Some(was)) => {
                self.held_threshold = None;
                self.set_setting(SETTING_THRESHOLD, was);
            }
            _ => {}
        }
    }

    fn pump_stream(&mut self) {
        let now = Instant::now();
        let shared = Arc::clone(&self.shared);
        let sounding = [GRIP, GRIP2].iter()
            .any(|&b| (0..2).any(|side| shared.carrier(b, side).0 > SILENCE));
        let clicking = self.stream.click.iter().any(ClickBurst::playing);
        if sounding || clicking {
            self.stream.last_sound = now;
        }
        if !self.stream.running {
            if !sounding && !clicking {
                return;
            }
            self.drain_status();
            self.write(&[REPORT_STREAM_OP, STREAM_START, STREAM_BOTH_GRIPS, STREAM_FORMAT]);
            self.stream.running = true;
            if let Some(d) = self.diag.as_mut() {
                d.line(format_args!("grip stream started"));
            }
            self.stream.last_pump = now;
            self.stream.started = now;
            self.stream.trim = 0.0;
            self.stream.level = [[0.0; 2]; 2];
            // Some in hand at once rather than after a report's worth of wait.
            self.stream.owed = STREAM_PRIME;
        } else if now.duration_since(self.stream.last_sound) >= STREAM_IDLE {
            self.write(&[REPORT_STREAM_OP, STREAM_STOP, STREAM_BOTH_GRIPS, 0]);
            self.stream.running = false;
            if let Some(d) = self.diag.as_mut() {
                d.line(format_args!("grip stream stopped, quiet"));
            }
            return;
        }
        self.drain_status();
        if !self.stream.running {
            return; // the pad refused it; set up again on the next pump
        }
        let dt = now.duration_since(self.stream.last_pump).as_secs_f32();
        self.stream.last_pump = now;
        let rate = STREAM_RATE * (1.0 + self.stream.trim);
        self.stream.owed = (self.stream.owed + rate * dt).min(MAX_OWED);
        while self.stream.owed >= GRIP_SLOT as f32 {
            let report = self.stream.report(&shared);
            self.write(&report);
            self.stream.owed -= GRIP_SLOT as f32;
            if let Some(d) = self.diag.as_mut() {
                d.sent = d.sent.map(|n| n + 1);
            }
        }
    }

    fn drain_status(&mut self) {
        let mut buf = [0u8; 64];
        // Bounded: this handle also receives every state report.
        for _ in 0..32 {
            let Ok(n) = self.dev.read(&mut buf) else { return };
            if n == 0 {
                return;
            }
            let streaming = self.stream.running;
            if let Some(d) = self.diag.as_mut() {
                d.report(&buf[..n], streaming);
            }
            // Statuses answer a stream's ops: only ours, while it runs. Before it
            // starts they're stale, and while idle they may be Steam's.
            if buf[0] != REPORT_STREAM_STATUS || n < 3 || !streaming {
                continue;
            }
            let Some(side) = [0u8, 1].iter().position(|m| *m == buf[1]) else { continue };
            let status = buf[2];
            // Both grips are fed in step, so their buffers fill alike: the left's
            // status paces the stream, and either counts for the log.
            if side == 0 {
                let trim = &mut self.stream.trim;
                if status & STATUS_OVERRUN != 0 {
                    // Too far ahead: a report's worth held back.
                    self.stream.owed -= GRIP_SLOT as f32;
                    *trim = (*trim - TRIM_STEP).max(-TRIM_MAX);
                    if !self.logged_overrun {
                        self.logged_overrun = true;
                        eprintln!("[steam-haptics] the pad's stream buffer overran");
                    }
                }
                // Ran dry: some in hand again, as at the start. Not as it starts,
                // when the pad says so before the first samples land (the probe
                // saw one on every stream): topped up then, it holds twice as
                // much, near where it overruns.
                if status & STATUS_STOPPED != 0 && self.stream.started.elapsed() >= STREAM_SETTLE {
                    self.stream.owed = self.stream.owed.max(STREAM_PRIME);
                }
                if status & (STATUS_STOPPED | STATUS_NEEDS_MORE) != 0 {
                    *trim = (*trim + TRIM_STEP).min(TRIM_MAX);
                }
                if status & STATUS_HAS_ENOUGH != 0 {
                    *trim = (*trim - TRIM_STEP).max(-TRIM_MAX);
                }
            }
            if let Some(d) = self.diag.as_mut() {
                d.overruns += u32::from(status & STATUS_OVERRUN != 0);
                d.low += u32::from(status & (STATUS_STOPPED | STATUS_NEEDS_MORE) != 0);
                d.enough += u32::from(status & STATUS_HAS_ENOUGH != 0);
            }
            if status & STATUS_ALREADY_RUNNING != 0 {
                // Someone's stream, or one of ours from before a crash.
                self.write(&[REPORT_STREAM_OP, STREAM_STOP, STREAM_BOTH_GRIPS, 0]);
                self.stream.running = false;
                if let Some(d) = self.diag.as_mut() {
                    d.line(format_args!("grip stream refused: one was already running; stopped it"));
                }
            }
        }
    }

    fn pump_pads(&mut self) {
        for side in 0..2 {
            let (amp, freq) = self.shared.carrier(TP, side);
            let now = Instant::now();
            let pad = self.pads[side];
            let side_byte = SIDE_PAD_L + side as u8;
            if amp <= SILENCE {
                if pad.sent.is_some() {
                    self.write(&[REPORT_COMMAND, side_byte, COMMAND_STOP, 0]);
                    self.pads[side] = PadTone::default();
                }
                continue;
            }
            let want = (amp_to_gain_db(amp), carrier_hz(freq).round() as u16);
            let since = pad.at.map_or(Duration::MAX, |at| now.duration_since(at));
            let due = since >= TONE_RENEW || (pad.sent != Some(want) && since >= TONE_MIN_GAP);
            if due {
                self.tone(side_byte, want.0, want.1, TONE_MS);
                self.pads[side] = PadTone { sent: Some(want), at: Some(now) };
            }
        }
    }

    fn tone(&mut self, side: u8, gain_db: i8, hz: u16, ms: u16) {
        let (f, d) = (hz.to_le_bytes(), ms.to_le_bytes());
        self.write(&[REPORT_TONE, side, gain_db as u8, f[0], f[1], d[0], d[1], 0, 0, 0]);
    }

    fn write(&mut self, report: &[u8]) {
        let started = Instant::now();
        let result = self.dev.write(report);
        if let Some(d) = self.diag.as_mut() {
            d.write_done(report[0], started.elapsed(), result.is_ok());
        }
        if let Err(e) = result {
            #[cfg(debug_assertions)]
            eprintln!("[steam-haptics] write {:#04x}: {e}", report[0]);
            let _ = e;
        }
    }

    fn feature(&self, op: u8, id: u8, value: i16) -> bool {
        feature(&self.dev, op, id, value)
    }

    fn set_setting(&self, id: u8, value: i16) {
        if !self.feature(OP_SET_SETTING, id, value) {
            eprintln!("[steam-haptics] couldn't set setting {id}");
        }
    }

    /// Asks, then polls for the answer: over the puck it takes a round trip.
    fn setting(&self, id: u8) -> Option<i16> {
        if !self.feature(OP_GET_SETTING, id, 0) {
            return None;
        }
        for _ in 0..25 {
            std::thread::sleep(Duration::from_millis(20));
            let mut r = [0u8; 65];
            r[0] = FEATURE_REPORT;
            if matches!(self.dev.get_feature_report(&mut r), Ok(n) if n >= 6)
                && r[1] == OP_GET_SETTING
                && r[3] == id
            {
                return Some(i16::from_le_bytes([r[4], r[5]]));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_steam_controller_is_known_by_its_usb_ids() {
        for pid in 0x1302..=0x1305 {
            assert!(is_steam_controller(Some(0x28DE), Some(pid)));
        }
        assert!(!is_steam_controller(Some(0x28DE), Some(0x1205)), "Steam Deck");
        assert!(!is_steam_controller(Some(0x054C), Some(0x1302)));
        assert!(!is_steam_controller(None, None));
    }

    fn clicked(out: [Option<Cmd>; 2]) -> [bool; 2] {
        out.map(|c| c.is_some())
    }

    const MS: f32 = 0.001;

    /// A point at 0.5 with a band from 0.47 to 0.53.
    fn edges() -> TriggerEdges {
        TriggerEdges { press: 0.5, again: 0.53, release: 0.47, rest: 0.01 }
    }

    #[test]
    fn a_trigger_clicks_once_per_pull_and_rearms_past_the_hysteresis() {
        let mut click = TriggerClick { edges: [edges(), TriggerEdges::default()], ..Default::default() };
        assert_eq!(clicked(click.step([0.49, 0.9], MS)), [false, false], "below it, and R has no click");
        assert_eq!(click.step([0.5, 0.0], MS),
            [Some(Cmd::TriggerClick { side: 0, gain_db: -5, shape: ClickShape::trigger_default() }), None],
            "reaching it clicks, at the stock strength");
        assert_eq!(clicked(click.step([0.9, 0.0], MS)), [false, false], "no second click further in");
        assert_eq!(clicked(click.step([0.48, 0.0], MS)), [false, false], "still inside the hysteresis");
        assert_eq!(clicked(click.step([0.5, 0.0], MS)), [false, false], "so jitter here is silent");
        assert_eq!(clicked(click.step([0.46, 0.0], MS)), [false, false], "released, with no release click");
        assert_eq!(clicked(click.step([0.5, 0.0], MS)), [false, false], "short of rest, the point holds still");
        assert_eq!(clicked(click.step([0.53, 0.0], MS)), [true, false], "the band's top clicks");
        assert_eq!(clicked(click.step([0.0, 0.0], MS)), [false, false]);
        assert_eq!(clicked(click.step([0.5, 0.0], MS)), [true, false], "and a pull from rest at the point");
    }

    // A release click plays its own shape as the trigger comes back past the
    // point, once it has stayed under the band for the hold.
    #[test]
    fn a_release_click_plays_on_the_way_back_with_its_own_shape() {
        let mut click = TriggerClick::default();
        for (pin, v) in [("trig_click_r", 0.5), ("trig_top_r", 0.53), ("trig_rearm_r", 0.47), ("trig_rest_r", 0.01),
                         ("trig_hold_r", 0.004), ("trig_release_r", 1.0), ("trig_release_gain_r", -9.0),
                         ("trig_release_hz_r", 90.0), ("trig_release_ms_r", 40.0)] {
            assert!(click.set(pin, v), "{pin}");
        }
        assert!(!click.set("hd_r_amp", 1.0), "not one of these");
        assert_eq!(clicked(click.step([0.0, 0.9], MS)), [false, true]);
        for _ in 0..4 {
            assert_eq!(clicked(click.step([0.0, 0.1], MS)), [false, false], "under the band, not 4 ms yet");
        }
        assert_eq!(click.step([0.0, 0.1], MS),
            [None, Some(Cmd::TriggerClick { side: 1, gain_db: -9, shape: ClickShape::new(90.0, 40.0, 0.0) })]);
        click.set("trig_release_r", 0.0);
        click.set("trig_hold_r", 0.0);
        assert_eq!(clicked(click.step([0.0, 0.9], MS)), [false, true]);
        assert_eq!(clicked(click.step([0.0, 0.1], MS)), [false, false], "release off again");
    }

    // Press off with release on: the trigger is still followed, and only lets go.
    #[test]
    fn a_side_with_press_off_plays_only_its_release() {
        let mut click = TriggerClick { edges: [edges(); 2], release_on: [true; 2], ..Default::default() };
        assert!(click.set("trig_press_l", 0.0));
        assert_eq!(clicked(click.step([0.9, 0.9], MS)), [false, true]);
        assert_eq!(clicked(click.step([0.0, 0.0], MS)), [true, true]);
    }

    #[test]
    fn turning_a_click_off_forgets_where_the_trigger_was() {
        let mut click = TriggerClick { edges: [edges(); 2], ..Default::default() };
        assert!(click.on());
        assert_eq!(clicked(click.step([0.9, 0.9], MS)), [true, true]);
        click.edges[0] = TriggerEdges::default();
        assert!(click.on());
        assert_eq!(clicked(click.step([0.9, 0.9], MS)), [false, false]);
        click.edges[0] = edges();
        assert_eq!(clicked(click.step([0.9, 0.9], MS)), [true, false], "L starts afresh; R stayed down");
    }

    /// Each pin owned here reaches the slot `Shared::carrier` reads it from.
    #[test]
    fn carrier_slots_match_the_pin_table() {
        let index = |pin: &str| TARGET_PINS.iter().position(|p| *p == pin).unwrap();
        for (base, prefix) in [(GRIP, "grip"), (GRIP2, "grip2"), (TP, "tp")] {
            for (side, letter) in ["l", "r"].iter().enumerate() {
                assert_eq!(index(&format!("{prefix}_{letter}_amp")), base + side * 2);
                assert_eq!(index(&format!("{prefix}_{letter}_freq")), base + side * 2 + 1);
            }
        }
    }

    #[test]
    fn a_pad_clicks_on_press_and_only_on_release_when_asked() {
        use flexinput_core::automap::{PAD_CLICK_ON, PAD_CLICK_ON_RELEASE, PAD_CLICK_STRONG};
        let firmware = |side, command| Some(Cmd::PadClick { side, command, gain_db: 0 });
        let mut fb = PadClickFeedback::default();
        let mode = |fb: &mut PadClickFeedback, m: u8| {
            assert!(fb.set("pad_click_mode_l", f32::from(m)) && fb.set("pad_click_mode_r", f32::from(m)));
        };
        assert_eq!(fb.step([true, false]), [None, None], "off, and it keeps track");
        mode(&mut fb, PAD_CLICK_ON);
        assert_eq!(fb.step([true, false]), [None, None], "held since before it came on");
        assert_eq!(fb.step([false, true]), [None, firmware(1, COMMAND_CLICK)]);
        assert_eq!(fb.step([false, true]), [None, None], "once per press");
        mode(&mut fb, PAD_CLICK_ON | PAD_CLICK_ON_RELEASE | PAD_CLICK_STRONG);
        assert_eq!(fb.step([true, false]), [firmware(0, COMMAND_CLICK_STRONG), firmware(1, COMMAND_CLICK)],
            "a press takes the strength, a release is always light");
        // Each pad its own: the right one off.
        assert!(fb.set("pad_click_mode_r", 0.0));
        assert_eq!(fb.step([false, true]), [firmware(0, COMMAND_CLICK), None]);
        assert!(!fb.set("pad_click_mode", 1.0), "no shared pin");
    }

    #[test]
    fn a_pad_set_to_tone_plays_its_pitch_and_length_and_lets_go_softer() {
        use flexinput_core::automap::{PAD_CLICK_ON, PAD_CLICK_ON_RELEASE, PAD_CLICK_TONE};
        let mut fb = PadClickFeedback::default();
        for (pin, v) in [
            ("pad_click_mode_l", f32::from(PAD_CLICK_ON | PAD_CLICK_ON_RELEASE | PAD_CLICK_TONE)),
            ("pad_click_gain_l", -3.0), ("pad_click_hz_l", 320.0), ("pad_click_ms_l", 8.0),
        ] {
            assert!(fb.set(pin, v));
        }
        let shape = ClickShape { hz: 320, ms: 8, soft: 0.0 };
        assert_eq!(fb.step([true, false]), [Some(Cmd::PadTone { side: 0, gain_db: -3, shape }), None]);
        assert_eq!(fb.step([false, false]), [Some(Cmd::PadTone { side: 0, gain_db: -9, shape }), None]);
    }

    /// A click's shape counts whichever order a pass's pins arrive in, and without
    /// one a click pin plays what it always did.
    #[test]
    fn a_click_pin_takes_its_shape_from_the_same_pass() {
        let mut pins = ClickPins::default();
        assert!(pins.set("tp_r_click", 0.5), "the click first");
        assert!(pins.set("tp_r_click_hz", 300.0));
        assert!(pins.set("tp_r_click_ms", 30.0));
        assert!(pins.set("tp_r_click_soft", 0.5));
        let shape = ClickShape { hz: 300, ms: 30, soft: 0.5 };
        assert_eq!(pins.take_due(), [Cmd::PadTone { side: 1, gain_db: -6, shape }]);
        assert_eq!(pins.take_due(), [], "once, for as long as it stays up");
        assert!(pins.set("tp_r_click", 0.5));
        assert_eq!(pins.take_due(), []);

        // Dropped, all back to 0: the next is the plain click.
        for pin in ["tp_r_click", "tp_r_click_hz", "tp_r_click_ms", "tp_r_click_soft"] {
            assert!(pins.set(pin, 0.0));
        }
        assert!(pins.set("tp_r_click", 1.0));
        assert_eq!(pins.take_due(), [Cmd::PadClick { side: 1, command: COMMAND_CLICK, gain_db: 0 }]);

        assert!(pins.set("grip_l_click", 1.0));
        assert_eq!(pins.take_due(),
            [Cmd::TriggerClick { side: 0, gain_db: 0, shape: ClickShape::trigger_default() }]);
        assert!(!pins.set("grip_touch", 100.0), "a setting, not one of these");
    }

    #[test]
    fn a_click_shape_stays_something_an_actuator_can_play() {
        assert_eq!(ClickShape::new(0.0, 0.0, -1.0), ClickShape { hz: 40, ms: 1, soft: 0.0 });
        assert_eq!(ClickShape::new(9000.0, 9000.0, 2.0), ClickShape { hz: 2000, ms: 200, soft: 1.0 });
        assert_eq!(ClickBurst::new(ClickShape::new(2000.0, 10.0, 0.5), 0).hz, GRIP_STREAM_MAX_HZ, "as samples, capped");
        assert_eq!(ClickShape::new(170.4, 20.0, 0.0).samples(), 80);
    }

    /// Sharp holds from the first sample to the last; fully soft is silent at both
    /// ends and full only at the middle; between, it fades that share and holds.
    #[test]
    fn softness_is_how_much_of_a_click_fades() {
        for t in [0.0, 0.1, 0.5, 0.9, 0.999] {
            assert_eq!(click_envelope(t, 0.0), 1.0, "sharp at {t}");
        }
        assert_eq!(click_envelope(0.0, 1.0), 0.0);
        assert_eq!(click_envelope(1.0, 1.0), 0.0);
        assert_eq!(click_envelope(0.5, 1.0), 1.0);
        assert!((click_envelope(0.25, 1.0) - 0.5).abs() < 1e-5);
        assert_eq!(click_envelope(0.5, 0.5), 1.0);
        assert_eq!(click_envelope(0.3, 0.5), 1.0, "past its fade, it holds");
        assert!(click_envelope(0.1, 0.5) < 1.0);
    }

    /// A burst plays its length at its level, then silence; a soft one starts from
    /// nothing where a sharp one is at once at full swing.
    #[test]
    fn a_burst_plays_its_length_then_goes_quiet() {
        let peak = |soft: f32, range: std::ops::Range<usize>| {
            let mut b = ClickBurst::new(ClickShape { hz: 500, ms: 20, soft }, 0);
            let all: Vec<f32> = (0..90).map(|_| b.next()).collect();
            assert_eq!(b.at, b.total);
            assert!(all[80..].iter().all(|s| *s == 0.0), "quiet after its 80 samples");
            all[range].iter().fold(0.0f32, |m, s| m.max(s.abs()))
        };
        assert!(peak(0.0, 0..8) > 0.9, "sharp swings fully right away");
        assert!(peak(1.0, 0..8) < 0.4, "soft eases in");
        assert!(peak(1.0, 36..44) > 0.9, "and reaches full in the middle");
        let quiet = ClickBurst::new(ClickShape { hz: 200, ms: 10, soft: 0.0 }, -6);
        assert!((quiet.level - 0.5).abs() < 0.01);
    }

    #[test]
    fn a_grip_setting_stays_in_its_range_and_zero_touches_nothing() {
        assert_eq!(grip_value(0, 100.4), Some(100));
        assert_eq!(grip_value(0, 900.0), Some(400));
        assert_eq!(grip_value(0, 3.0), Some(25));
        assert_eq!(grip_value(1, 900.0), Some(100));
        assert_eq!(grip_value(0, 0.0), None);
    }

    /// G.711's decoder, to check the encoder against.
    fn from_mulaw(b: u8) -> f32 {
        let b = !b;
        let exponent = i32::from((b >> 4) & 0x07);
        let mantissa = i32::from(b & 0x0F);
        let mag = (((mantissa << 3) + 0x84) << exponent) - 0x84;
        (if b & 0x80 != 0 { -mag } else { mag }) as f32 / 32767.0
    }

    /// Silence, the ends, and every level in between back within µ-law's step,
    /// which is finest near silence.
    #[test]
    fn samples_are_mulaw() {
        assert_eq!(mulaw(0.0), 0xFF, "silence");
        assert_eq!(mulaw(1.0), 0x80);
        assert_eq!(mulaw(-1.0), 0x00);
        assert_eq!(mulaw(3.0), 0x80, "past full scale holds there rather than wrapping");
        for i in -100..=100 {
            let v = i as f32 / 100.0;
            let back = from_mulaw(mulaw(v));
            assert!((back - v).abs() <= 0.02 + v.abs() * 0.07, "{v} came back {back}");
        }
        assert!((from_mulaw(mulaw(0.003)) - 0.003).abs() < 0.0005, "a quiet tone keeps its shape");
    }

    #[test]
    fn the_limiter_is_straight_to_its_knee_then_eases_into_full() {
        assert_eq!(soft_limit(0.5), 0.5);
        assert_eq!(soft_limit(-0.8), -0.8);
        assert!(soft_limit(1.0) > 0.9 && soft_limit(1.0) < 1.0);
        assert!(soft_limit(2.0) < 1.0 && soft_limit(2.0) > soft_limit(1.0));
        assert_eq!(soft_limit(-2.0), -soft_limit(2.0));
    }

    #[test]
    fn amplitude_maps_onto_the_reports_gain_range() {
        assert_eq!(amp_to_gain_db(1.0), 0);
        assert_eq!(amp_to_gain_db(0.5), -6);
        assert_eq!(amp_to_gain_db(0.0), GAIN_DB_MIN as i8);
    }

    #[test]
    fn a_carrier_without_a_frequency_sits_near_the_resonance() {
        assert_eq!(carrier_hz(0.0), DEFAULT_HZ);
        assert!((carrier_hz(1.0) - 2000.0).abs() < 0.5, "the wide scale's top");
        assert!((carrier_hz(0.5) - 282.8).abs() < 0.5);
    }

    const SILENT: u8 = 0xFF;

    fn left(r: &[u8]) -> &[u8] {
        &r[2..2 + GRIP_SLOT]
    }

    fn right(r: &[u8]) -> &[u8] {
        &r[2 + GRIP_SLOT..]
    }

    /// Each report carries both grips' next samples, the left's first.
    #[test]
    fn a_stream_report_carries_both_grips() {
        let shared = Shared::default();
        shared.targets[0].store(1.0f32.to_bits(), Ordering::Relaxed);
        let mut stream = Stream::new();
        let r = stream.report(&shared);
        assert_eq!(r.len(), 64);
        assert_eq!((r[0], r[1]), (REPORT_STREAM_PUSH_GRIPS, GRIP_SLOT as u8));
        assert!(left(&r).iter().any(|s| *s != SILENT), "left carries the tone");
        assert!(right(&r).iter().all(|s| *s == SILENT), "right is silent");
    }

    /// A steady tone runs on from one report into the next with no seam, and a new
    /// strength or pitch glides in rather than stepping at the report's edge.
    #[test]
    fn a_tone_runs_on_across_reports_and_glides_to_a_change() {
        let shared = Shared::default();
        let set = |i: usize, v: f32| shared.targets[i].store(v.to_bits(), Ordering::Relaxed);
        set(0, 0.5);
        set(1, PitchScale::Wide.value(200.0));
        let mut stream = Stream::new();
        let mut wave: Vec<f32> = Vec::new();
        for _ in 0..8 {
            wave.extend(left(&stream.report(&shared)).iter().map(|b| from_mulaw(*b)));
        }
        // 200 Hz at 4 kHz: neighbours differ by at most 0.5·sin(2π/20) ≈ 0.155.
        let worst = wave.windows(2).skip(60).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        assert!(worst < 0.18, "a seam: {worst}");
        // Twice as strong: the first samples after the change are still near the old.
        set(0, 1.0);
        let next: Vec<f32> = [stream.report(&shared), stream.report(&shared)].iter()
            .flat_map(|r| left(r).iter().map(|b| from_mulaw(*b)).collect::<Vec<_>>())
            .collect();
        assert!(next[0].abs() < 0.62, "{}", next[0]);
        assert!(next.iter().any(|s| s.abs() > 0.85), "and it gets there within two reports");
    }

    /// The grips' stream plays no higher than it can make a tone of.
    #[test]
    fn the_grip_stream_tops_out_where_its_samples_still_make_a_tone() {
        let shared = Shared::default();
        shared.targets[0].store(1.0f32.to_bits(), Ordering::Relaxed);
        shared.targets[1].store(1.0f32.to_bits(), Ordering::Relaxed);
        let mut stream = Stream::new();
        let _ = stream.report(&shared);
        assert!((stream.step[0][0] * STREAM_RATE - GRIP_STREAM_MAX_HZ).abs() < 1.0);
    }

    #[test]
    fn a_click_mixes_into_a_running_stream_for_its_length() {
        let shared = Shared::default();
        let mut stream = Stream::new();
        // 80 samples: it spans three reports.
        let shape = ClickShape::new(170.0, 20.0, 0.0);
        let samples = shape.samples();
        stream.click[1] = ClickBurst::new(shape, 0);
        let first = stream.report(&shared);
        assert!(right(&first).iter().any(|s| *s != SILENT));
        assert!(left(&first).iter().all(|s| *s == SILENT), "on its own side only");
        assert_eq!(stream.click[1].total - stream.click[1].at, samples - GRIP_SLOT as u32);
        let _ = stream.report(&shared);
        let _ = stream.report(&shared);
        assert!(!stream.click[1].playing());
        assert!(right(&stream.report(&shared)).iter().all(|s| *s == SILENT), "and then stops");
    }
}
