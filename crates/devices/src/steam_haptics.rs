//! Steam Controller (2026, "Triton") haptics, over a HID handle of our own.
//!
//! SDL's driver reads the pad and plays its rumble (report 0x80), but has no way
//! to send the other haptic reports, so a worker thread opens the collection SDL
//! reads and writes them itself. Windows and Linux both let the two share it.
//!
//! The pad has four actuators, and each is driven the way that was found to work
//! on the hardware (2026-10-31, over the puck):
//!
//!   * grip motors: a PCM stream, because only a waveform plays both carriers of
//!     an `hd_*` / `hd2_*` pair. Report 0x86 sets the stream up for both motors,
//!     then 0x87 pushes each side's samples. 0x44 reports back how each buffer
//!     is doing, and the sending rate leans on it.
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
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use flexinput_core::automap::{click_envelope, TriggerCrossing, TriggerEdges};
use flexinput_core::haptic_pcm::{FREQ_MAX_HZ, FREQ_MIN_HZ};
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
const REPORT_STREAM_PUSH: u8 = 0x87;
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
/// 4 kHz, signed 8-bit: a tenth of the puck's report rate for the two sides, and
/// well above the highest carrier.
const STREAM_FORMAT: u8 = 5;
const STREAM_RATE: f32 = 4000.0;
const STREAM_CHUNK: usize = 62;
/// 0x87's target for each motor. Not the sides the tone report uses, and id 1
/// is ignored, so the right motor can't be reached as "1".
const PUSH_TARGET: [u8; 2] = [0, 4];
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
/// Ceiling on samples owed to a motor, so a stalled thread doesn't send a burst.
const MAX_OWED: f32 = STREAM_RATE * 0.1;
/// How far to lean on the sending rate while a buffer reports low or high.
const FLOW_LEAN: f32 = 1.0 / 8.0;
const OVERRUN_BACKOFF: f32 = STREAM_RATE * 0.004;

const SILENCE: f32 = 1.0 / 255.0;
/// Where a carrier with an amplitude but no frequency sits: near the resonance.
const DEFAULT_FREQ_NORM: f32 = 0.25;
const GAIN_DB_MIN: f32 = -23.0;

/// How long a tone lasts unless renewed, so a lost update can't hold it on.
const TONE_MS: u16 = 250;
const TONE_RENEW: Duration = Duration::from_millis(150);
/// Tones restart when changed, so changes wait this long between them.
const TONE_MIN_GAP: Duration = Duration::from_millis(30);

/// A one-shot click pin counts as up above this.
const CLICK_PIN_MIN: f32 = 0.02;
/// A one-shot's tone report would take anything; these keep it something an
/// actuator can play and a finger can feel.
const CLICK_HZ_RANGE: (f32, f32) = (40.0, 1000.0);
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
const TARGET_PINS: [&str; 12] = [
    "hd_l_amp", "hd_l_freq", "hd_r_amp", "hd_r_freq",
    "hd2_l_amp", "hd2_l_freq", "hd2_r_amp", "hd2_r_freq",
    "tp_l_amp", "tp_l_freq", "tp_r_amp", "tp_r_freq",
];

/// Each side's offset into [`TARGET_PINS`] from carrier 1, carrier 2 and pad.
const HD: usize = 0;
const HD2: usize = 4;
const TP: usize = 8;

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

fn carrier_hz(freq_norm: f32) -> f32 {
    let f = if freq_norm <= 0.0 { DEFAULT_FREQ_NORM } else { freq_norm.min(1.0) };
    FREQ_MIN_HZ + f * (FREQ_MAX_HZ - FREQ_MIN_HZ)
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
            hz: shape.hz as f32,
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
        use flexinput_core::automap::{TRIGGER_CLICK_HZ, TRIGGER_CLICK_MS, TRIGGER_RELEASE_DB};
        let press = EdgeClick { gain_db: 0.0, hz: TRIGGER_CLICK_HZ, ms: TRIGGER_CLICK_MS, soft: 0.0 };
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
/// Calibration window. `mode` is the `automap::PAD_CLICK_*` bits, 0 = off.
struct PadClickFeedback {
    mode: u8,
    gain_db: f32,
    hz: f32,
    ms: f32,
    soft: f32,
    held: [bool; 2],
}

impl Default for PadClickFeedback {
    fn default() -> Self {
        use flexinput_core::automap::{PAD_CLICK_HZ, PAD_CLICK_MS};
        Self { mode: 0, gain_db: 0.0, hz: PAD_CLICK_HZ, ms: PAD_CLICK_MS, soft: 0.0, held: [false; 2] }
    }
}

impl PadClickFeedback {
    /// One poll's pad clicks: what each side plays, if any. Keeps track while
    /// off, so turning it on with a pad held doesn't click.
    fn step(&mut self, down: [bool; 2]) -> [Option<Cmd>; 2] {
        use flexinput_core::automap::{PAD_CLICK_ON, PAD_CLICK_ON_RELEASE, PAD_CLICK_STRONG, PAD_CLICK_TONE};
        let gain_db = self.gain_db.clamp(GAIN_DB_MIN, 24.0).round() as i8;
        let mut out = [None, None];
        for side in 0..2 {
            let was = std::mem::replace(&mut self.held[side], down[side]);
            if self.mode & PAD_CLICK_ON == 0 || was == down[side] {
                continue;
            }
            if !down[side] && self.mode & PAD_CLICK_ON_RELEASE == 0 {
                continue;
            }
            out[side] = Some(if self.mode & PAD_CLICK_TONE != 0 {
                let gain_db = if down[side] { gain_db } else { gain_db.saturating_add(PAD_RELEASE_DB) };
                Cmd::PadTone { side, gain_db, shape: ClickShape::new(self.hz, self.ms, self.soft) }
            } else {
                let strong = down[side] && self.mode & PAD_CLICK_STRONG != 0;
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

/// One pad's haptics: lives on its `OpenPad`, and dropping it ends the worker.
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
        match pin {
            "pad_click_mode" => self.pad_feedback.mode = level as u8,
            "pad_click_gain" => self.pad_feedback.gain_db = level,
            "pad_click_hz" => self.pad_feedback.hz = level,
            "pad_click_ms" => self.pad_feedback.ms = level,
            "pad_click_soft" => self.pad_feedback.soft = level,
            _ => {
                let Some(i) = GRIP_SETTINGS.iter().position(|s| s.0 == pin) else {
                    return false;
                };
                if let Some(value) = grip_value(i, level) {
                    if self.grip_sent[i] != Some(value) {
                        self.grip_sent[i] = Some(value);
                        let _ = self.cmds.send(Cmd::Grip(i, value));
                    }
                }
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

/// Both grip motors' stream.
struct Stream {
    running: bool,
    last_sound: Instant,
    last_pump: Instant,
    owed: [f32; 2],
    low: [bool; 2],
    /// Carrier 1 and 2 phase per side.
    phase: [[f32; 2]; 2],
    /// What a click adds to a side's stream: the tone report isn't known to play
    /// over a stream on the same motor, and a soft one needs a stream anyway.
    click: [ClickBurst; 2],
}

impl Stream {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            running: false,
            last_sound: now,
            last_pump: now,
            owed: [0.0; 2],
            low: [true; 2],
            phase: [[0.0; 2]; 2],
            click: Default::default(),
        }
    }

    /// One side's next samples, from the current targets.
    fn chunk(&mut self, side: usize, shared: &Shared) -> [u8; STREAM_CHUNK + 2] {
        let (a1, f1) = shared.carrier(HD, side);
        let (a2, f2) = shared.carrier(HD2, side);
        let tau = std::f32::consts::TAU;
        let steps = [
            carrier_hz(f1) / STREAM_RATE,
            carrier_hz(f2) / STREAM_RATE,
        ];
        let mut out = [0u8; STREAM_CHUNK + 2];
        out[0] = REPORT_STREAM_PUSH;
        out[1] = PUSH_TARGET[side];
        let (phase, click) = (&mut self.phase[side], &mut self.click[side]);
        for s in &mut out[2..] {
            let v = a1.clamp(0.0, 1.0) * (phase[0] * tau).sin()
                + a2.clamp(0.0, 1.0) * (phase[1] * tau).sin()
                + click.next();
            phase[0] = (phase[0] + steps[0]).fract();
            phase[1] = (phase[1] + steps[1]).fract();
            *s = pcm_s8(v);
        }
        out
    }
}

fn pcm_s8(v: f32) -> u8 {
    (v.clamp(-1.0, 1.0) * 127.0) as i8 as u8
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
        }
    }

    fn run(mut self, cmds: mpsc::Receiver<Cmd>) {
        // A run that was killed while holding the bit left the threshold there.
        if self.setting(SETTING_THRESHOLD) == Some(THRESHOLD_OUT_OF_REACH) {
            self.set_setting(SETTING_THRESHOLD, THRESHOLD_DEFAULT);
        }
        loop {
            match cmds.recv_timeout(Duration::from_millis(2)) {
                Ok(cmd) => self.command(cmd),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.pump_stream();
            self.pump_pads();
            self.pump_pad_bursts();
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
                    *s = pcm_s8(p.burst.next());
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
        let sounding = [HD, HD2].iter()
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
            self.stream.last_pump = now;
            self.stream.low = [true; 2];
            // A chunk goes out at once rather than after a chunk's worth of wait.
            self.stream.owed = [STREAM_CHUNK as f32; 2];
        } else if now.duration_since(self.stream.last_sound) >= STREAM_IDLE {
            self.write(&[REPORT_STREAM_OP, STREAM_STOP, STREAM_BOTH_GRIPS, 0]);
            self.stream.running = false;
            return;
        }
        self.drain_status();
        if !self.stream.running {
            return; // the pad refused it; set up again on the next pump
        }
        let dt = now.duration_since(self.stream.last_pump).as_secs_f32();
        self.stream.last_pump = now;
        for side in 0..2 {
            let lean = if self.stream.low[side] { 1.0 + FLOW_LEAN } else { 1.0 - FLOW_LEAN };
            self.stream.owed[side] = (self.stream.owed[side] + STREAM_RATE * dt * lean).min(MAX_OWED);
            while self.stream.owed[side] >= STREAM_CHUNK as f32 {
                let chunk = self.stream.chunk(side, &shared);
                self.write(&chunk);
                self.stream.owed[side] -= STREAM_CHUNK as f32;
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
            if buf[0] != REPORT_STREAM_STATUS || n < 3 {
                continue;
            }
            let Some(side) = [0u8, 1].iter().position(|m| *m == buf[1]) else { continue };
            let status = buf[2];
            if status & STATUS_OVERRUN != 0 {
                self.stream.owed[side] -= OVERRUN_BACKOFF;
                if !self.logged_overrun {
                    self.logged_overrun = true;
                    eprintln!("[steam-haptics] the pad's stream buffer overran");
                }
            }
            if status & (STATUS_STOPPED | STATUS_NEEDS_MORE) != 0 {
                self.stream.low[side] = true;
            }
            if status & STATUS_HAS_ENOUGH != 0 {
                self.stream.low[side] = false;
            }
            if status & STATUS_ALREADY_RUNNING != 0 {
                // Someone's stream, or one of ours from before a crash.
                self.write(&[REPORT_STREAM_OP, STREAM_STOP, STREAM_BOTH_GRIPS, 0]);
                self.stream.running = false;
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
        if let Err(e) = self.dev.write(report) {
            #[cfg(debug_assertions)]
            eprintln!("[steam-haptics] write {:#04x}: {e}", report[0]);
            let _ = e;
        }
    }

    fn feature(&self, op: u8, id: u8, value: i16) -> bool {
        let mut b = [0u8; 65];
        let v = value.to_le_bytes();
        b[..6].copy_from_slice(&[FEATURE_REPORT, op, 3, id, v[0], v[1]]);
        self.dev.send_feature_report(&b).is_ok()
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
            [Some(Cmd::TriggerClick { side: 0, gain_db: 0, shape: ClickShape::trigger_default() }), None],
            "reaching it clicks");
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
        for (base, prefix) in [(HD, "hd"), (HD2, "hd2"), (TP, "tp")] {
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
        assert_eq!(fb.step([true, false]), [None, None], "off, and it keeps track");
        fb.mode = PAD_CLICK_ON;
        assert_eq!(fb.step([true, false]), [None, None], "held since before it came on");
        assert_eq!(fb.step([false, true]), [None, firmware(1, COMMAND_CLICK)]);
        assert_eq!(fb.step([false, true]), [None, None], "once per press");
        fb.mode = PAD_CLICK_ON | PAD_CLICK_ON_RELEASE | PAD_CLICK_STRONG;
        assert_eq!(fb.step([true, false]), [firmware(0, COMMAND_CLICK_STRONG), firmware(1, COMMAND_CLICK)],
            "a press takes the strength, a release is always light");
    }

    #[test]
    fn a_pad_set_to_tone_plays_its_pitch_and_length_and_lets_go_softer() {
        use flexinput_core::automap::{PAD_CLICK_ON, PAD_CLICK_ON_RELEASE, PAD_CLICK_TONE};
        let mut fb = PadClickFeedback::default();
        fb.mode = PAD_CLICK_ON | PAD_CLICK_ON_RELEASE | PAD_CLICK_TONE;
        (fb.gain_db, fb.hz, fb.ms) = (-3.0, 320.0, 8.0);
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
        assert_eq!(ClickShape::new(9000.0, 9000.0, 2.0), ClickShape { hz: 1000, ms: 200, soft: 1.0 });
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

    #[test]
    fn samples_are_signed() {
        assert_eq!(pcm_s8(0.0), 0);
        assert_eq!(pcm_s8(1.0), 127);
        assert_eq!(pcm_s8(-1.0) as i8, -127);
        assert_eq!(pcm_s8(3.0), 127, "two loud carriers clip rather than wrap");
    }

    #[test]
    fn amplitude_maps_onto_the_reports_gain_range() {
        assert_eq!(amp_to_gain_db(1.0), 0);
        assert_eq!(amp_to_gain_db(0.5), -6);
        assert_eq!(amp_to_gain_db(0.0), GAIN_DB_MIN as i8);
    }

    #[test]
    fn a_carrier_without_a_frequency_sits_near_the_resonance() {
        assert_eq!(carrier_hz(0.0), carrier_hz(DEFAULT_FREQ_NORM));
        assert_eq!(carrier_hz(1.0), FREQ_MAX_HZ);
    }

    #[test]
    fn a_stream_chunk_fills_one_report_for_its_side() {
        let shared = Shared::default();
        shared.targets[0].store(1.0f32.to_bits(), Ordering::Relaxed);
        let mut stream = Stream::new();
        let left = stream.chunk(0, &shared);
        let right = stream.chunk(1, &shared);
        assert_eq!((left[0], left[1]), (REPORT_STREAM_PUSH, 0));
        assert_eq!((right[0], right[1]), (REPORT_STREAM_PUSH, 4));
        assert!(left[2..].iter().any(|s| *s != 0), "left carries the tone");
        assert!(right[2..].iter().all(|s| *s == 0), "right is silent");
    }

    #[test]
    fn a_click_mixes_into_a_running_stream_for_its_length() {
        let shared = Shared::default();
        let mut stream = Stream::new();
        let samples = ClickShape::trigger_default().samples();
        stream.click[1] = ClickBurst::new(ClickShape::trigger_default(), 0);
        let first = stream.chunk(1, &shared);
        assert!(first[2..].iter().any(|s| *s != 0));
        assert_eq!(stream.click[1].total - stream.click[1].at, samples - STREAM_CHUNK as u32);
        let _ = stream.chunk(1, &shared);
        assert!(!stream.click[1].playing());
        assert!(stream.chunk(1, &shared)[2..].iter().all(|s| *s == 0), "and then stops");
    }
}
