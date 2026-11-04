//! The Steam Controller (2026, "Triton"), read straight off its HID reports.
//!
//! SDL read it before, but SDL's driver turns the pad's lizard mode off every 3 s
//! from inside the update that reads every SDL pad, with a feature report that
//! waits for the pad's answer with no timeout, and the puck can hold one for
//! seconds: the pad froze for as long. Here each pad slot has a reader thread that
//! does nothing but read, and a keeper thread that sends the pad its settings and
//! waits as long as the puck takes, while the reader goes on. SDL is told to leave
//! the pad alone (`sdl_backend`'s `try_init`).
//!
//! What comes out is what SDL made of the same reports: the same pins, scales and
//! frame, its button mapping and its trigger arithmetic, under the id SDL gave the
//! pad, `sdl:generic:<USB serial>`, so a saved patch and calibration carry over.
//! Where SDL's C casts wrap (a stick at full down, a trigger under zero), the
//! values here clamp. Report layouts and bits are SDL's
//! (`SDL_hidapi_steam_triton.c`, `steam/controller_structs.h`).

use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use glam::Vec2;
use hidapi::{DeviceInfo, HidApi, HidDevice};

use flexinput_core::Signal;

use crate::gyro::ImuAverage;
use crate::identification::ControllerKind;
use crate::sdl_backend::{sanitize_inst, sdl_accel_normalized, sdl_gyro_normalized};
use crate::spike::SpikeFilter;
use crate::steam_haptics::{self, is_steam_controller, SteamHaptics};
use crate::{layouts, DeviceBackend, PhysicalDevice};

const VALVE: u16 = 0x28DE;
/// The wireless dongles (Proteus, Nereid). Their pad slots are interfaces 2–5.
const PUCKS: [u16; 2] = [0x1304, 0x1305];
const PUCK_SLOTS: std::ops::RangeInclusive<i32> = 2..=5;
const VENDOR_FIRST_USAGE_PAGE: u16 = 0xFF00;

// Input reports.
const REPORT_STATE: u8 = 0x42;
const REPORT_STATE_BLE: u8 = 0x45;
const REPORT_STATE_TIMESTAMP: u8 = 0x47;
const REPORT_LINK: u8 = 0x79;
const REPORT_LINK_X: u8 = 0x46;
const LINK_DOWN: u8 = 1;
const LINK_UP: u8 = 2;
/// Both state reports' length past their id.
const STATE_LEN: usize = 45;

// Settings the keeper sends.
const SETTING_LIZARD_MODE: u8 = 9;
const LIZARD_MODE_OFF: i16 = 0;
const SETTING_IMU_MODE: u8 = 48;
/// Raw accel and raw gyro, as SDL's driver asks for them.
const IMU_RAW: i16 = 0x08 | 0x10;
/// Lizard mode comes back on by itself; SDL's driver turns it off this often.
const LIZARD_EVERY: Duration = Duration::from_secs(3);
/// An IMU whose timestamp hasn't moved on for this long, on a pad that reports,
/// is off: Steam Input resets it as it takes the pad and lets it go.
const IMU_STALL: Duration = Duration::from_millis(500);
/// And the IMU setting goes out again no more often than this.
const IMU_RETRY: Duration = Duration::from_secs(1);
/// A setting that takes this long gets a line in the log.
const SLOW_SETTING: Duration = Duration::from_millis(50);

/// A pad on sends hundreds of reports a second. Silence this long on a connected
/// slot is a handle that stopped delivering, which is opened again; silence as
/// long after that, a pad gone without saying.
const STALL_REOPEN: Duration = Duration::from_secs(2);
/// A slot with no pad on it is opened again this often: a handle opened before
/// HidHide cloaked the dongle would never see a pad arrive.
const IDLE_REOPEN: Duration = Duration::from_secs(5);
/// More than any poll gap should see.
const MAX_SAMPLES: usize = 256;

/// The report's button bits (SDL's `TRITON_*`).
mod bit {
    pub const A: u32 = 0x0000_0001;
    pub const B: u32 = 0x0000_0002;
    pub const X: u32 = 0x0000_0004;
    pub const Y: u32 = 0x0000_0008;
    pub const QAM: u32 = 0x0000_0010;
    pub const R3: u32 = 0x0000_0020;
    pub const VIEW: u32 = 0x0000_0040;
    pub const R4: u32 = 0x0000_0080;
    pub const R5: u32 = 0x0000_0100;
    pub const R: u32 = 0x0000_0200;
    pub const DPAD_DOWN: u32 = 0x0000_0400;
    pub const DPAD_RIGHT: u32 = 0x0000_0800;
    pub const DPAD_LEFT: u32 = 0x0000_1000;
    pub const DPAD_UP: u32 = 0x0000_2000;
    pub const MENU: u32 = 0x0000_4000;
    pub const L3: u32 = 0x0000_8000;
    pub const STEAM: u32 = 0x0001_0000;
    pub const L4: u32 = 0x0002_0000;
    pub const L5: u32 = 0x0004_0000;
    pub const L: u32 = 0x0008_0000;
    pub const RIGHT_STICK_TOUCH: u32 = 0x0010_0000;
    pub const RIGHT_PAD_TOUCH: u32 = 0x0020_0000;
    pub const RIGHT_PAD_CLICK: u32 = 0x0040_0000;
    pub const LEFT_STICK_TOUCH: u32 = 0x0100_0000;
    pub const LEFT_PAD_TOUCH: u32 = 0x0200_0000;
    pub const LEFT_PAD_CLICK: u32 = 0x0400_0000;
    pub const RIGHT_GRIP_TOUCH: u32 = 0x1000_0000;
    pub const LEFT_GRIP_TOUCH: u32 = 0x2000_0000;
}

/// Each button pin and its bit, as SDL mapped them: its driver sends Menu as
/// Back and View as Start, and its Triton gamepad mapping puts the Steam (QAM)
/// button, the right pad's click and the stick and grip touches on Misc1–6, the
/// back buttons on the paddles, and the left pad's click on the touchpad button.
const BUTTON_PINS: [(&str, u32); 26] = [
    ("btn_south", bit::A),
    ("btn_east", bit::B),
    ("btn_west", bit::X),
    ("btn_north", bit::Y),
    ("btn_lb", bit::L),
    ("btn_rb", bit::R),
    ("btn_ls", bit::L3),
    ("btn_rs", bit::R3),
    ("btn_start", bit::VIEW),
    ("btn_back", bit::MENU),
    ("btn_guide", bit::STEAM),
    ("dpad_up", bit::DPAD_UP),
    ("dpad_down", bit::DPAD_DOWN),
    ("dpad_left", bit::DPAD_LEFT),
    ("dpad_right", bit::DPAD_RIGHT),
    ("btn_paddle_l1", bit::L4),
    ("btn_paddle_r1", bit::R4),
    ("btn_paddle_l2", bit::L5),
    ("btn_paddle_r2", bit::R5),
    ("btn_misc1", bit::QAM),
    ("btn_misc2", bit::RIGHT_PAD_CLICK),
    ("btn_misc3", bit::LEFT_STICK_TOUCH),
    ("btn_misc4", bit::RIGHT_STICK_TOUCH),
    ("btn_misc5", bit::LEFT_GRIP_TOUCH),
    ("btn_misc6", bit::RIGHT_GRIP_TOUCH),
    ("btn_touchpad", bit::LEFT_PAD_CLICK),
];

// ── Reports ───────────────────────────────────────────────────────────────────

/// One state report's IMU reading, raw.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct Imu {
    stamp: u32,
    accel: [i16; 3],
    gyro: [i16; 3],
}

impl Imu {
    /// As SDL handed it over (its frame, rad/s and m/s²), then as the SDL path
    /// normalized it: gyro `[x, z, −y]` of ±2000 °/s, accel `[x, z, −y]` of ±2 g.
    fn normalized(&self) -> [f32; 6] {
        const GYRO: f32 = 2000.0 * (std::f32::consts::PI / 180.0);
        const ACCEL: f32 = 2.0 * 9.806_65;
        let g = self.gyro.map(f32::from);
        let a = self.accel.map(f32::from);
        let gyro = [g[0] / 32768.0 * GYRO, g[2] / 32768.0 * GYRO, -g[1] / 32768.0 * GYRO];
        let accel = [a[0] / 32768.0 * ACCEL, a[2] / 32768.0 * ACCEL, -a[1] / 32768.0 * ACCEL];
        let mut out = [0.0; 6];
        out[..3].copy_from_slice(&sdl_gyro_normalized(gyro));
        out[3..].copy_from_slice(&sdl_accel_normalized(accel));
        out
    }
}

/// One state report.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct Report {
    buttons: u32,
    triggers: [i16; 2],
    /// Left x, left y, right x, right y.
    sticks: [i16; 4],
    /// Each pad's x and y, left then right.
    pads: [[i16; 2]; 2],
    /// Each pad's pressure, left then right: 32768 is SDL's 1.
    pressure: [u16; 2],
    imu: Imu,
}

/// What a report says.
#[derive(Debug, PartialEq)]
enum Input {
    State(Report),
    /// The radio link to the pad came up (`true`) or went down.
    Link(bool),
}

/// A report off the handle, its id first; `None` for any other kind.
fn parse(buf: &[u8]) -> Option<Input> {
    let (&id, d) = buf.split_first()?;
    match id {
        REPORT_STATE | REPORT_STATE_BLE | REPORT_STATE_TIMESTAMP if d.len() >= STATE_LEN => {
            let i16_at = |o: usize| i16::from_le_bytes([d[o], d[o + 1]]);
            let u16_at = |o: usize| u16::from_le_bytes([d[o], d[o + 1]]);
            // The timestamped report has the trackpads' own timestamp ahead of
            // them, and a 16-bit IMU timestamp in place of 32: the IMU itself
            // sits where it does in the other.
            let stamped = id == REPORT_STATE_TIMESTAMP;
            let pad = if stamped { 19 } else { 17 };
            let stamp = if stamped {
                u32::from(u16::from_le_bytes([d[31], d[32]]))
            } else {
                u32::from_le_bytes([d[29], d[30], d[31], d[32]])
            };
            Some(Input::State(Report {
                buttons: u32::from_le_bytes([d[1], d[2], d[3], d[4]]),
                triggers: [i16_at(5), i16_at(7)],
                sticks: [i16_at(9), i16_at(11), i16_at(13), i16_at(15)],
                pads: [[i16_at(pad), i16_at(pad + 2)], [i16_at(pad + 6), i16_at(pad + 8)]],
                pressure: [u16_at(pad + 4), u16_at(pad + 10)],
                imu: Imu {
                    stamp,
                    accel: [i16_at(33), i16_at(35), i16_at(37)],
                    gyro: [i16_at(39), i16_at(41), i16_at(43)],
                },
            }))
        }
        REPORT_LINK | REPORT_LINK_X => match d.first()? {
            &LINK_UP => Some(Input::Link(true)),
            &LINK_DOWN => Some(Input::Link(false)),
            _ => None,
        },
        _ => None,
    }
}

/// A trigger as SDL handed it over: its joystick axis (`2v − 32768`), mapped by
/// the gamepad layer onto 0..32767 in float, as SDL does, then over 32767.
fn sdl_trigger(v: i16) -> f32 {
    let axis = (i32::from(v) * 2 - 32768).clamp(-32768, 32767);
    let normalized = (axis + 32768) as f32 / 65535.0;
    let value = (normalized * 32767.0) as i32;
    (value as f32 / 32767.0).clamp(0.0, 1.0)
}

/// A stick axis as the SDL path read it: `v / 32767`, with SDL's Y negated into
/// its own axis and back, so up is +1.
fn sdl_sticks(sticks: [i16; 4]) -> [f32; 4] {
    let norm = |v: i32| (v as f32 / 32767.0).clamp(-1.0, 1.0);
    let y = |v: i16| -norm((-i32::from(v)).clamp(-32768, 32767));
    [norm(sticks[0].into()), y(sticks[1]), norm(sticks[2].into()), y(sticks[3])]
}

/// Everything but the IMU and the clicks, as the SDL path published it, onto
/// `out`: `buttons` are those down in any report since the last poll.
fn state_pins(out: &mut Vec<(String, String, Signal)>, dev: &str, report: &Report, buttons: u32) {
    let mut push = |pin: &str, s: Signal| out.push((dev.to_owned(), pin.to_owned(), s));
    let [lx, ly, rx, ry] = sdl_sticks(report.sticks);
    push("left_stick_x", Signal::Float(lx));
    push("left_stick_y", Signal::Float(ly));
    push("right_stick_x", Signal::Float(rx));
    push("right_stick_y", Signal::Float(ry));
    push("left_stick", Signal::Vec2(Vec2::new(lx, ly)));
    push("right_stick", Signal::Vec2(Vec2::new(rx, ry)));
    let [lt, rt] = report.triggers.map(sdl_trigger);
    push("left_trigger", Signal::Float(lt));
    push("right_trigger", Signal::Float(rt));
    // Past a small threshold, as XInput's ~30/255.
    push("btn_lt_dig", Signal::Bool(lt > 0.117));
    push("btn_rt_dig", Signal::Bool(rt > 0.117));
    for (pin, b) in BUTTON_PINS {
        push(pin, Signal::Bool(buttons & b != 0));
    }
    let on = |b: u32| buttons & b != 0;
    let dx = if on(bit::DPAD_RIGHT) { 1.0f32 } else if on(bit::DPAD_LEFT) { -1.0 } else { 0.0 };
    let dy = if on(bit::DPAD_UP) { 1.0f32 } else if on(bit::DPAD_DOWN) { -1.0 } else { 0.0 };
    let (dx, dy) = if dx != 0.0 && dy != 0.0 {
        (dx * std::f32::consts::FRAC_1_SQRT_2, dy * std::f32::consts::FRAC_1_SQRT_2)
    } else {
        (dx, dy)
    };
    push("dpad_x", Signal::Float(dx));
    push("dpad_y", Signal::Float(dy));
    push("dpad", Signal::Vec2(Vec2::new(dx, dy)));
    // Each pad a field: SDL's 0..1 (its y down), here -1..1 with +y up. Contact
    // is the latest report's, as SDL kept it.
    let fields = [
        (["touch1_x", "touch1_y", "touch1_active"], report.pads[0], bit::LEFT_PAD_TOUCH),
        (["touch2_x", "touch2_y", "touch2_active"], report.pads[1], bit::RIGHT_PAD_TOUCH),
    ];
    for ([px, py, active], [x, y], touch) in fields {
        if report.buttons & touch == 0 {
            push(active, Signal::Bool(false));
            continue;
        }
        let sx = (f32::from(x) / 65536.0 + 0.5).clamp(0.0, 1.0);
        let sy = (-f32::from(y) / 65536.0 + 0.5).clamp(0.0, 1.0);
        push(px, Signal::Float(sx * 2.0 - 1.0));
        push(py, Signal::Float(1.0 - sy * 2.0));
        push(active, Signal::Bool(true));
    }
}

// ── Trackpad tracking ─────────────────────────────────────────────────────────

/// Stabilisation's filter: an adaptive low-pass (the 1€ filter) that smooths a
/// slow or resting finger and opens up as it moves, so jitter goes while a swipe
/// keeps up. Its cutoff at rest, in Hz, at the least and the most stabilisation.
const STABILIZE_CUTOFF_HZ: (f32, f32) = (25.0, 1.0);
/// How much the cutoff opens per pad unit a second of finger speed.
const STABILIZE_BETA: f32 = 4.0;
/// The speed estimate's own cutoff.
const STABILIZE_SPEED_HZ: f32 = 1.0;
/// A click of the set force lets go below this share of it.
const CLICK_RELEASE: f32 = 0.85;
/// Click stabilisation keeps the pointer held this long past the press easing
/// off, while the finger settles.
const CLICK_SETTLE: Duration = Duration::from_millis(80);
/// How far, in pad units (1 = half the pad), a press may roll the finger and
/// still be held: past it the move is meant, and the hold lets go, so a firm
/// swipe or a click-drag moves.
const HOLD_REACH: f32 = 0.15;
/// The finger travel, in pad units, over which the gap a hold leaves between
/// the point and the finger closes by e⁻¹: the point catches up as the finger
/// moves on, and still reaches every edge.
const CATCH_UP: f32 = 0.15;

/// The 1€ filter over a 2-D position, in pad units.
#[derive(Clone, Copy, Debug)]
struct Stabilizer {
    out: Vec2,
    raw: Vec2,
    speed: Vec2,
}

impl Stabilizer {
    fn new(at: Vec2) -> Self {
        Self { out: at, raw: at, speed: Vec2::ZERO }
    }

    fn step(&mut self, raw: Vec2, dt: f32, min_cutoff: f32) -> Vec2 {
        let alpha = |cutoff: f32| {
            let tau = 1.0 / (std::f32::consts::TAU * cutoff);
            1.0 / (1.0 + tau / dt)
        };
        let speed = (raw - self.raw) / dt;
        self.raw = raw;
        self.speed += (speed - self.speed) * alpha(STABILIZE_SPEED_HZ);
        self.out += (raw - self.out) * alpha(min_cutoff + STABILIZE_BETA * self.speed.length());
        self.out
    }
}

/// The trackpads' tracking settings, as the engine sends them
/// (`automap::TRACKPAD_TRACKING`).
#[derive(Clone, Copy, Debug, PartialEq)]
struct TrackingConfig {
    stabilize: f32,
    /// Each pad's, left then right.
    click_force: [f32; 2],
    click_hold: f32,
}

impl Default for TrackingConfig {
    fn default() -> Self {
        use flexinput_core::automap::{TRACKPAD_CLICK_FORCE, TRACKPAD_CLICK_HOLD, TRACKPAD_STABILIZE};
        Self { stabilize: TRACKPAD_STABILIZE, click_force: [TRACKPAD_CLICK_FORCE; 2], click_hold: TRACKPAD_CLICK_HOLD }
    }
}

impl TrackingConfig {
    /// Takes a pin of these; false for any other.
    fn set(&mut self, pin: &str, v: f32) -> bool {
        let v = v.clamp(0.0, 1.0);
        match pin {
            "tp_stabilize" => self.stabilize = v,
            "tp_click_force_l" => self.click_force[0] = v,
            "tp_click_force_r" => self.click_force[1] = v,
            "tp_click_hold" => self.click_hold = v,
            _ => return false,
        }
        true
    }
}

/// One trackpad between reports.
#[derive(Clone, Copy, Debug, Default)]
struct PadTrack {
    filter: Option<Stabilizer>,
    /// The position last handed on, in pad units.
    out: Option<Vec2>,
    /// What is handed on less the filtered finger, after a hold: the pointer
    /// goes on from where the hold kept it, rather than jump, and closes the gap
    /// as the finger moves on ([`CATCH_UP`]).
    offset: Vec2,
    /// Held here while the press builds, until `settle` after it eases; and
    /// the finger when the hold began.
    held: Option<Vec2>,
    hold_from: Option<Vec2>,
    settle: Option<Instant>,
    /// The finger moved past [`HOLD_REACH`] in this press: no hold until it eases.
    moved_on: bool,
    /// The filtered finger last report.
    last_smooth: Option<Vec2>,
    /// Down by the set force.
    clicked: bool,
    /// The pressure the pad's own click was seen to set at, learnt from its
    /// clicks, and whether its click was down last report.
    own_force: Option<f32>,
    own_down: bool,
    last: Option<Instant>,
}

/// Both trackpads' tracking: what [`TrackingConfig`] makes of each report's
/// pads, in place, before anything reads them.
#[derive(Clone, Copy, Debug, Default)]
struct Tracking {
    config: TrackingConfig,
    pads: [PadTrack; 2],
}

impl Tracking {
    /// A pad's press: its pressure over the force it clicks at, the set one or
    /// its own click's once seen (until then the sensor's whole scale), and 1
    /// from there on.
    fn press(&self, i: usize, pressure: f32) -> f32 {
        let set = self.config.click_force[i];
        let force = if set > 0.0 { Some(set) } else { self.pads[i].own_force };
        (pressure / force.unwrap_or(1.0).max(0.01)).clamp(0.0, 1.0)
    }

    fn process(&mut self, r: &mut Report, now: Instant) {
        const BITS: [(u32, u32); 2] = [(bit::LEFT_PAD_TOUCH, bit::LEFT_PAD_CLICK), (bit::RIGHT_PAD_TOUCH, bit::RIGHT_PAD_CLICK)];
        let c = self.config;
        for (i, (touch, click)) in BITS.into_iter().enumerate() {
            let t = &mut self.pads[i];
            let p = f32::from(r.pressure[i]) / 32768.0;
            // The pad's own click: where it sets, learnt, so click stabilisation
            // has a force to go by when no force is set.
            let own = r.buttons & click != 0;
            if own && !t.own_down {
                t.own_force = Some(t.own_force.map_or(p, |f| f + (p - f) * 0.3));
            }
            t.own_down = own;
            // A set force takes the click over from the pad's own.
            let set_force = c.click_force[i];
            let clicked = if set_force > 0.0 {
                t.clicked = p >= if t.clicked { set_force * CLICK_RELEASE } else { set_force };
                r.buttons = if t.clicked { r.buttons | click } else { r.buttons & !click };
                t.clicked
            } else {
                own
            };
            if r.buttons & touch == 0 {
                *t = PadTrack { own_force: t.own_force, own_down: t.own_down, ..Default::default() };
                continue;
            }
            let raw = Vec2::new(f32::from(r.pads[i][0]), f32::from(r.pads[i][1])) / 32768.0;
            let dt = t.last.map_or(0.004, |l| now.duration_since(l).as_secs_f32()).clamp(0.0005, 0.025);
            t.last = Some(now);
            let smooth = if c.stabilize > 0.0 {
                let (least, most) = STABILIZE_CUTOFF_HZ;
                let min_cutoff = least * (most / least).powf(c.stabilize);
                t.filter.get_or_insert(Stabilizer::new(raw)).step(raw, dt, min_cutoff)
            } else {
                raw
            };
            // The gap a hold left, closing as the finger moves.
            let moved = t.last_smooth.map_or(0.0, |l| (smooth - l).length());
            t.last_smooth = Some(smooth);
            t.offset *= (-moved / CATCH_UP).exp();
            // Click stabilisation: still from the set share of the click's force,
            // within reach of where the press began.
            let force = if set_force > 0.0 { Some(set_force) } else { t.own_force };
            let pressing = c.click_hold > 0.0 && (clicked || force.is_some_and(|f| p >= f * c.click_hold));
            if !pressing {
                t.moved_on = false;
            }
            if pressing && !t.moved_on {
                t.settle = Some(now + CLICK_SETTLE);
            }
            if t.settle.is_some_and(|s| now < s)
                && (smooth - *t.hold_from.get_or_insert(smooth)).length() > HOLD_REACH
            {
                t.moved_on = pressing;
                t.settle = None;
            }
            let out = if t.settle.is_some_and(|s| now < s) {
                *t.held.get_or_insert(t.out.unwrap_or(smooth + t.offset))
            } else {
                if let Some(h) = t.held.take() {
                    t.offset = h - smooth;
                }
                t.hold_from = None;
                smooth + t.offset
            };
            t.out = Some(out);
            let raw_of = |v: f32| (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
            r.pads[i] = [raw_of(out.x), raw_of(out.y)];
        }
    }
}

// ── Slots ─────────────────────────────────────────────────────────────────────

/// One pad slot: a dongle has four, a wired pad one. Shared by the slot's reader
/// and keeper threads and the backend.
struct Slot {
    path: String,
    serial: String,
    interface: i32,
    pid: u16,
    state: Mutex<SlotState>,
    /// The reader has ended: the slot is gone.
    ended: AtomicBool,
}

/// What the reader has made of the reports so far.
#[derive(Default)]
struct SlotState {
    connected: bool,
    report: Report,
    /// Buttons down in any report since the last poll, so a tap between two
    /// polls still shows.
    held: u32,
    /// The IMU samples since the last poll, normalized.
    samples: Vec<[f32; 6]>,
    /// The newest sample. A report whose IMU timestamp is the last one's
    /// carries no new reading; SDL started from 0 too.
    imu: Option<[f32; 6]>,
    imu_stamp: u32,
    /// When the IMU timestamp last moved on, for the keeper.
    imu_moved: Option<Instant>,
    /// State reports since the last poll.
    reports: u32,
    /// The trackpads' tracking, applied to each report as it comes.
    tracking: Tracking,
}

impl Slot {
    fn new(path: String, serial: String, interface: i32, pid: u16) -> Self {
        Self { path, serial, interface, pid, state: Mutex::default(), ended: AtomicBool::new(false) }
    }

    fn lock(&self) -> MutexGuard<'_, SlotState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn connected(&self) -> bool {
        self.lock().connected
    }

    /// One report off the handle.
    fn take(&self, buf: &[u8]) {
        let Some(input) = parse(buf) else { return };
        let mut s = self.lock();
        match input {
            Input::Link(up) => s.connected = up,
            Input::State(mut r) => {
                s.tracking.process(&mut r, Instant::now());
                s.connected = true;
                s.held |= r.buttons;
                s.reports += 1;
                if r.imu.stamp != s.imu_stamp {
                    s.imu_stamp = r.imu.stamp;
                    s.imu_moved = Some(Instant::now());
                    let sample = r.imu.normalized();
                    s.imu = Some(sample);
                    if s.samples.len() < MAX_SAMPLES {
                        s.samples.push(sample);
                    }
                }
                s.report = r;
            }
        }
    }
}

/// Opens on the slot's thread: `HidApi::new` walks every HID device first.
fn open_path(path: &str) -> Option<HidDevice> {
    let api = HidApi::new().ok()?;
    let path = CString::new(path).ok()?;
    #[cfg(target_os = "linux")]
    let path = crate::linux_hide::reachable(&path);
    api.open_path(&path).ok()
}

/// The slot's reader: reads, and nothing else, until the slot is gone.
fn read(slot: Arc<Slot>) {
    let mut buf = [0u8; 64];
    // Opened again for silence since the last report.
    let mut reopened = false;
    'open: while let Some(dev) = open_path(&slot.path) {
        let opened = Instant::now();
        let mut last = opened;
        loop {
            match dev.read_timeout(&mut buf, 100) {
                Ok(0) => {
                    if !slot.connected() {
                        if opened.elapsed() >= IDLE_REOPEN {
                            continue 'open;
                        }
                        continue;
                    }
                    if last.elapsed() < STALL_REOPEN {
                        continue;
                    }
                    if reopened {
                        // Silent on a fresh handle too: the pad is gone.
                        slot.lock().connected = false;
                        continue;
                    }
                    // A handle HidHide cloaks while it is open reads nothing,
                    // with no error, from then on.
                    reopened = true;
                    continue 'open;
                }
                Ok(n) => {
                    last = Instant::now();
                    reopened = false;
                    slot.take(&buf[..n]);
                }
                Err(_) => break 'open,
            }
        }
    }
    slot.lock().connected = false;
    slot.ended.store(true, Ordering::Relaxed);
}

/// The slot's keeper: while a pad is on it, lizard mode off every
/// [`LIZARD_EVERY`], and the IMU on as the pad comes and whenever its readings
/// stop. Each waits on the pad's answer, which the puck can hold for seconds;
/// the reader goes on meanwhile.
fn keep(slot: Arc<Slot>) {
    let mut dev: Option<HidDevice> = None;
    let mut lizard_sent: Option<Instant> = None;
    let mut imu_sent: Option<Instant> = None;
    while !slot.ended.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(100));
        let (connected, imu_moved) = {
            let s = slot.lock();
            (s.connected, s.imu_moved)
        };
        if !connected {
            (lizard_sent, imu_sent) = (None, None);
            continue;
        }
        let now = Instant::now();
        let lizard_due = lizard_sent.is_none_or(|t| now.duration_since(t) >= LIZARD_EVERY);
        let imu_due = imu_sent.is_none_or(|t| {
            now.duration_since(t) >= IMU_RETRY && imu_moved.is_none_or(|m| now.duration_since(m) >= IMU_STALL)
        });
        if !lizard_due && !imu_due {
            continue;
        }
        if dev.is_none() {
            dev = open_path(&slot.path);
        }
        let Some(d) = dev.as_ref() else { continue };
        let mut ok = true;
        if lizard_due {
            lizard_sent = Some(now);
            ok = send_setting(d, SETTING_LIZARD_MODE, LIZARD_MODE_OFF, "lizard mode off");
        }
        if ok && imu_due {
            imu_sent = Some(Instant::now());
            ok = send_setting(d, SETTING_IMU_MODE, IMU_RAW, "IMU on");
        }
        if !ok {
            dev = None; // opened again next time
        }
    }
}

fn send_setting(dev: &HidDevice, setting: u8, value: i16, what: &str) -> bool {
    let started = Instant::now();
    let ok = steam_haptics::set_setting(dev, setting, value);
    let took = started.elapsed();
    if (took >= SLOW_SETTING || !ok) && steam_haptics::diag_on() {
        steam_haptics::diag_note(&format!(
            "{what}: the pad took {} ms{}", took.as_millis(), if ok { "" } else { " and refused it" }));
    }
    ok
}

/// A pad slot's collection: the vendor one (the haptic reports' too), and on a
/// dongle one of its four slots, not its other vendor interface.
fn is_slot(info: &DeviceInfo) -> bool {
    is_steam_controller(Some(info.vendor_id()), Some(info.product_id()))
        && info.usage_page() >= VENDOR_FIRST_USAGE_PAGE
        && info.usage() == 1
        && (!PUCKS.contains(&info.product_id()) || PUCK_SLOTS.contains(&info.interface_number()))
}

/// Finds the pads' slots every 2 s and starts a reader and a keeper on each new
/// one. Ends with the backend.
fn discover(slots: Weak<Mutex<Vec<Arc<Slot>>>>) {
    let mut api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            eprintln!("[steam-controller] hidapi: {e}");
            return;
        }
    };
    loop {
        let Some(all) = slots.upgrade() else { return };
        {
            let mut list = all.lock().unwrap_or_else(|e| e.into_inner());
            list.retain(|s| !s.ended.load(Ordering::Relaxed));
            for info in api.device_list().filter(|i| is_slot(i)) {
                let Ok(path) = info.path().to_str() else { continue };
                if list.iter().any(|s| s.path == path) {
                    continue;
                }
                let serial = info.serial_number().unwrap_or_default().to_owned();
                let slot = Arc::new(Slot::new(path.to_owned(), serial, info.interface_number(), info.product_id()));
                let (reader, keeper) = (Arc::clone(&slot), Arc::clone(&slot));
                let spawned = std::thread::Builder::new()
                    .name("steam-controller-read".into())
                    .spawn(move || read(reader))
                    .and_then(|_| std::thread::Builder::new()
                        .name("steam-controller-keep".into())
                        .spawn(move || keep(keeper)));
                if spawned.is_ok() {
                    list.push(slot);
                }
            }
        }
        drop(all);
        std::thread::sleep(Duration::from_secs(2));
        let _ = api.refresh_devices();
    }
}

// ── Backend ───────────────────────────────────────────────────────────────────

/// A pad on a slot.
struct Pad {
    dev_id: String,
    slot: Arc<Slot>,
    haptics: Option<SteamHaptics>,
    /// Outlier spike filter over the IMU, born and dropped with the pad.
    spike: SpikeFilter,
    capacitive_mask: u8,
    /// This poll's IMU samples, the buffer kept from poll to poll.
    samples: Vec<[f32; 6]>,
}

pub struct SteamControllerBackend {
    slots: Arc<Mutex<Vec<Arc<Slot>>>>,
    pads: Vec<Pad>,
    counts: Vec<(String, u32)>,
}

impl SteamControllerBackend {
    pub fn new() -> Self {
        let slots = Arc::new(Mutex::new(Vec::new()));
        let weak = Arc::downgrade(&slots);
        if let Err(e) = std::thread::Builder::new().name("steam-controller".into()).spawn(move || discover(weak)) {
            eprintln!("[steam-controller] no discovery thread ({e})");
        }
        Self { slots, pads: Vec::new(), counts: Vec::new() }
    }

    /// A pad for each slot a controller is on, and none for the rest.
    fn sync_pads(&mut self) {
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner()).clone();
        self.pads.retain(|p| p.slot.connected() && slots.iter().any(|s| Arc::ptr_eq(s, &p.slot)));
        for slot in slots {
            if !slot.connected() || self.pads.iter().any(|p| Arc::ptr_eq(&p.slot, &slot)) {
                continue;
            }
            // SDL's id: its kind's slug and the pad's USB serial, the dongle's
            // for a pad on one. A second pad on the same dongle gets its slot.
            let inst = match sanitize_inst(&slot.serial) {
                s if s.is_empty() => format!("i{}", slot.interface),
                s => s,
            };
            let base = format!("sdl:{}:{inst}", ControllerKind::Generic.id_slug());
            let dev_id = if self.pads.iter().any(|p| p.dev_id == base) {
                format!("{base}m{}", slot.interface)
            } else {
                base
            };
            eprintln!("[steam-controller] {dev_id}: a pad on interface {}", slot.interface);
            let haptics = SteamHaptics::spawn(Some(slot.path.clone()), Some(VALVE), Some(slot.pid));
            self.pads.push(Pad {
                dev_id,
                haptics,
                spike: SpikeFilter::new(),
                capacitive_mask: flexinput_core::automap::capacitive_misc_mask(VALVE, slot.pid),
                samples: Vec::new(),
                slot,
            });
        }
    }
}

impl Default for SteamControllerBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceBackend for SteamControllerBackend {
    fn name(&self) -> &'static str {
        "Steam Controller"
    }

    fn enumerate(&mut self) -> Vec<PhysicalDevice> {
        self.sync_pads();
        self.pads.iter()
            .map(|p| {
                let (vid, pid) = (Some(VALVE), Some(p.slot.pid));
                PhysicalDevice {
                    id: p.dev_id.clone(),
                    display_name: "Steam Controller".into(),
                    kind: ControllerKind::Generic,
                    outputs: layouts::outputs_for_pad(ControllerKind::Generic, vid, pid),
                    inputs: layouts::inputs_for_pad(ControllerKind::Generic, vid, pid),
                    instance_path: None,
                    vid,
                    pid,
                }
            })
            .collect()
    }

    fn poll(&mut self) -> Vec<(String, String, Signal)> {
        let mut out = Vec::new();
        for pad in &mut self.pads {
            let (report, buttons, imu, reports, own_force, pressure, press) = {
                let mut s = pad.slot.lock();
                if !s.connected {
                    continue;
                }
                pad.samples.clear();
                std::mem::swap(&mut pad.samples, &mut s.samples);
                let buttons = std::mem::take(&mut s.held) | s.report.buttons;
                let own_force = s.tracking.pads.map(|t| t.own_force);
                // A pad untouched presses nothing, whatever its sensor says.
                let pressure: [f32; 2] = std::array::from_fn(|i| {
                    let touched = s.report.buttons & [bit::LEFT_PAD_TOUCH, bit::RIGHT_PAD_TOUCH][i] != 0;
                    if touched { f32::from(s.report.pressure[i]) / 32768.0 } else { 0.0 }
                });
                let press = [0, 1].map(|i| s.tracking.press(i, pressure[i]));
                (s.report, buttons, s.imu, std::mem::take(&mut s.reports), own_force, pressure, press)
            };
            let dev = pad.dev_id.as_str();
            if reports > 0 {
                self.counts.push((dev.to_owned(), reports));
            }
            state_pins(&mut out, dev, &report, buttons);
            if let Some(h) = pad.haptics.as_mut() {
                let [lt, rt] = report.triggers.map(sdl_trigger);
                h.poll_triggers(lt, rt);
                h.poll_pad_clicks(buttons & bit::LEFT_PAD_CLICK != 0, buttons & bit::RIGHT_PAD_CLICK != 0);
            }
            if pad.capacitive_mask != 0 {
                out.push((dev.to_owned(), flexinput_core::automap::CAPACITIVE_MASK_PIN.into(),
                          Signal::Float(f32::from(pad.capacitive_mask))));
            }
            // Its touch fields are its two trackpads, not two fingers on one.
            out.push((dev.to_owned(), flexinput_core::automap::TOUCH_PADS_PIN.into(), Signal::Float(2.0)));
            // The trackpads' press; and their pressure and where their own click
            // sets, for the Calibration window.
            for i in 0..2 {
                use flexinput_core::automap::{TOUCH_OWN_FORCE_PINS, TOUCH_PRESSURE_PINS, TOUCH_RAW_PRESSURE_PINS};
                out.push((dev.to_owned(), TOUCH_PRESSURE_PINS[i].into(), Signal::Float(press[i])));
                out.push((dev.to_owned(), TOUCH_RAW_PRESSURE_PINS[i].into(), Signal::Float(pressure[i])));
                if let Some(f) = own_force[i] {
                    out.push((dev.to_owned(), TOUCH_OWN_FORCE_PINS[i].into(), Signal::Float(f)));
                }
            }
            // Each report since the last poll through the spike filter as one,
            // and the poll gets their mean, as the SDL path did.
            if let Some(latest) = imu {
                let imu = if pad.samples.len() > 1 {
                    let mut average = ImuAverage::default();
                    for &s in &pad.samples {
                        average.add(pad.spike.push(s));
                    }
                    average.mean().unwrap_or(latest)
                } else {
                    pad.spike.push(latest)
                };
                for (pin, v) in ["gyro_x", "gyro_y", "gyro_z", "accel_x", "accel_y", "accel_z"].iter().zip(imu) {
                    out.push((dev.to_owned(), (*pin).to_owned(), Signal::Float(v)));
                }
            }
        }
        out
    }

    fn send(&mut self, device_id: &str, pin_id: &str, signal: Signal) {
        let Some(pad) = self.pads.iter_mut().find(|p| p.dev_id == device_id) else { return };
        // The trackpads' tracking, which the reader applies to each report.
        if pin_id.starts_with("tp_") && pad.slot.lock().tracking.config.set(pin_id, signal.as_float()) {
            return;
        }
        if let Some(h) = pad.haptics.as_mut() {
            // Full resolution: a trigger click's position is raw travel, and its
            // gain is in dB.
            let _ = h.set(pin_id, signal.as_float());
        }
    }

    fn take_event_counts(&mut self) -> Vec<(String, u32)> {
        std::mem::take(&mut self.counts)
    }

    fn set_spike_filter(&mut self, device_id: &str, enabled: bool, sensitivity_pct: f32, window: u8) {
        if let Some(pad) = self.pads.iter_mut().find(|p| p.dev_id == device_id) {
            pad.spike.bind_capture(device_id);
            pad.spike.set_config(enabled, sensitivity_pct, window);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 0x42 report: id, then the 45-byte state with `fill` setting fields.
    fn state(id: u8, fill: impl FnOnce(&mut [u8])) -> Vec<u8> {
        let mut b = vec![0u8; 1 + STATE_LEN];
        b[0] = id;
        fill(&mut b[1..]);
        b
    }

    fn put(d: &mut [u8], at: usize, v: i16) {
        d[at..at + 2].copy_from_slice(&v.to_le_bytes());
    }

    #[test]
    fn a_state_report_parses_at_sdls_offsets() {
        let b = state(REPORT_STATE, |d| {
            d[1..5].copy_from_slice(&(bit::A | bit::LEFT_PAD_TOUCH).to_le_bytes());
            put(d, 5, 100);
            put(d, 7, 200);
            put(d, 9, -300);
            put(d, 11, 400);
            put(d, 17, 1000);
            put(d, 19, -2000);
            put(d, 23, 3000);
            put(d, 25, -4000);
            d[29..33].copy_from_slice(&7u32.to_le_bytes());
            put(d, 33, 11);
            put(d, 43, -13);
        });
        let Some(Input::State(r)) = parse(&b) else { panic!("not a state report") };
        assert_eq!(r.buttons, bit::A | bit::LEFT_PAD_TOUCH);
        assert_eq!(r.triggers, [100, 200]);
        assert_eq!(r.sticks[..2], [-300, 400]);
        assert_eq!(r.pads, [[1000, -2000], [3000, -4000]]);
        assert_eq!(r.imu, Imu { stamp: 7, accel: [11, 0, 0], gyro: [0, 0, -13] });
        // The timestamped kind: pads two bytes on, a 16-bit IMU timestamp.
        let b = state(REPORT_STATE_TIMESTAMP, |d| {
            put(d, 19, 1000);
            put(d, 27, -4000);
            d[31..33].copy_from_slice(&9u16.to_le_bytes());
            put(d, 33, 11);
        });
        let Some(Input::State(r)) = parse(&b) else { panic!("not a state report") };
        assert_eq!(r.pads, [[1000, 0], [0, -4000]]);
        assert_eq!((r.imu.stamp, r.imu.accel[0]), (9, 11));
        assert_eq!(parse(&b[..20]), None, "too short");
        assert_eq!(parse(&[REPORT_LINK, LINK_DOWN]), Some(Input::Link(false)));
        assert_eq!(parse(&[REPORT_LINK_X, LINK_UP]), Some(Input::Link(true)));
        assert_eq!(parse(&[0x44, 0, 4]), None);
    }

    // SDL's trigger: the joystick axis, then the gamepad's float rescale.
    #[test]
    fn a_trigger_reads_as_sdl_handed_it_over() {
        assert_eq!(sdl_trigger(0), 0.0);
        assert_eq!(sdl_trigger(32767), 32766.0 / 32767.0);
        assert_eq!(sdl_trigger(16384), ((32768.0f32 / 65535.0 * 32767.0) as i32) as f32 / 32767.0);
        assert_eq!(sdl_trigger(-5), 0.0, "clamped where SDL's cast wraps");
        assert_eq!(sdl_sticks([32767, 32767, -32767, -32768]), [1.0, 1.0, -1.0, -1.0], "y up is +1");
    }

    fn pins(report: &Report, buttons: u32) -> Vec<(String, String, Signal)> {
        let mut out = Vec::new();
        state_pins(&mut out, "sdl:generic:x", report, buttons);
        out
    }

    fn pin(out: &[(String, String, Signal)], name: &str) -> Option<Signal> {
        out.iter().find(|(_, p, _)| p == name).map(|(_, _, s)| *s)
    }

    // SDL's mapping: Misc1–6 and the paddles, the left pad's click on the
    // touchpad button, Menu as Back.
    #[test]
    fn buttons_land_on_sdls_pins() {
        let out = pins(&Report::default(), bit::QAM | bit::RIGHT_PAD_CLICK | bit::L4 | bit::R5 | bit::LEFT_PAD_CLICK
            | bit::MENU | bit::RIGHT_GRIP_TOUCH | bit::DPAD_UP | bit::DPAD_RIGHT);
        for (name, want) in [("btn_misc1", true), ("btn_misc2", true), ("btn_paddle_l1", true), ("btn_paddle_r2", true),
                             ("btn_paddle_r1", false), ("btn_touchpad", true), ("btn_back", true), ("btn_start", false),
                             ("btn_misc6", true), ("btn_misc5", false), ("dpad_up", true)] {
            assert_eq!(pin(&out, name), Some(Signal::Bool(want)), "{name}");
        }
        let half = std::f32::consts::FRAC_1_SQRT_2;
        assert_eq!(pin(&out, "dpad"), Some(Signal::Vec2(Vec2::new(half, half))));
    }

    #[test]
    fn a_touched_pad_is_a_field_with_y_up() {
        let report = Report { buttons: bit::RIGHT_PAD_TOUCH, pads: [[0, 0], [16384, 16384]], ..Default::default() };
        let out = pins(&report, report.buttons);
        assert_eq!(pin(&out, "touch1_active"), Some(Signal::Bool(false)));
        assert_eq!(pin(&out, "touch1_x"), None);
        assert_eq!(pin(&out, "touch2_active"), Some(Signal::Bool(true)));
        assert_eq!(pin(&out, "touch2_x"), Some(Signal::Float(0.5)));
        assert_eq!(pin(&out, "touch2_y"), Some(Signal::Float(0.5)), "the report's +y is up");
    }

    // The IMU in SDL's frame and scale, then the SDL path's normalization.
    #[test]
    fn the_imu_reads_as_the_sdl_path_normalized_it() {
        let imu = Imu { stamp: 1, accel: [0, -16384, 0], gyro: [16384, 0, 0] };
        let n = imu.normalized();
        let rad = 1000.0f32.to_radians();
        assert_eq!(n[..3], sdl_gyro_normalized([rad, 0.0, 0.0]));
        assert_eq!(n[3..], sdl_accel_normalized([0.0, 0.0, 9.806_65]));
    }

    /// A report with the left pad touched at `x` (pad units) and `pressure`.
    fn touched(x: f32, pressure: f32, own_click: bool) -> Report {
        let buttons = bit::LEFT_PAD_TOUCH | if own_click { bit::LEFT_PAD_CLICK } else { 0 };
        Report {
            buttons,
            pads: [[(x * 32768.0) as i16, 0], [0, 0]],
            pressure: [(pressure * 32768.0) as u16, 0],
            ..Default::default()
        }
    }

    /// Runs `reports` 4 ms apart: each one's left pad x and click bit after.
    fn track(t: &mut Tracking, reports: &[Report]) -> Vec<(f32, bool)> {
        let start = Instant::now();
        reports.iter().enumerate().map(|(i, r)| {
            let mut r = *r;
            t.process(&mut r, start + Duration::from_millis(4 * i as u64));
            (f32::from(r.pads[0][0]) / 32768.0, r.buttons & bit::LEFT_PAD_CLICK != 0)
        }).collect()
    }

    // Stabilisation takes the jitter off a resting finger and keeps up with a swipe.
    #[test]
    fn stabilisation_steadies_a_resting_finger_and_follows_a_swipe() {
        let mut t = Tracking { config: TrackingConfig { stabilize: 1.0, click_force: [0.0; 2], click_hold: 0.0 }, ..Default::default() };
        let jitter: Vec<_> = (0..200).map(|i| touched(if i % 2 == 0 { 0.02 } else { -0.02 }, 0.1, false)).collect();
        let out = track(&mut t, &jitter);
        let spread = out[100..].iter().map(|(x, _)| x.abs()).fold(0.0f32, f32::max);
        assert!(spread < 0.005, "±0.02 of jitter held to {spread}");
        let mut t = Tracking { config: TrackingConfig { stabilize: 1.0, ..Default::default() }, ..Default::default() };
        let swipe: Vec<_> = (0..60).map(|i| touched(-0.6 + i as f32 * 0.02, 0.1, false)).collect();
        let (x, _) = *track(&mut t, &swipe).last().unwrap();
        assert!((x - 0.58).abs() < 0.08, "a fast swipe keeps up: {x}");
        let mut off = Tracking { config: TrackingConfig { stabilize: 0.0, click_hold: 0.0, ..Default::default() }, ..Default::default() };
        assert!((track(&mut off, &[touched(0.3, 0.1, false)])[0].0 - 0.3).abs() < 1e-4, "off: as it comes");
    }

    // A set force takes the click from the pad's own, and lets go a little under.
    #[test]
    fn a_set_click_force_clicks_by_pressure() {
        let mut t = Tracking { config: TrackingConfig { stabilize: 0.0, click_force: [0.5; 2], click_hold: 0.0 }, ..Default::default() };
        let clicks: Vec<bool> = track(&mut t, &[
            touched(0.0, 0.3, true), touched(0.0, 0.55, false), touched(0.0, 0.45, false), touched(0.0, 0.4, false),
        ]).into_iter().map(|(_, c)| c).collect();
        assert_eq!(clicks, [false, true, true, false], "the pad's own bit is ignored; under 85% lets go");
        // Unset, the pad's own click stands, and where it set is learnt.
        let mut t = Tracking { config: TrackingConfig { click_force: [0.0; 2], ..Default::default() }, ..Default::default() };
        let clicks: Vec<bool> = track(&mut t, &[touched(0.0, 0.7, true)]).into_iter().map(|(_, c)| c).collect();
        assert_eq!(clicks, [true]);
        assert!(t.pads[0].own_force.is_some_and(|f| (f - 0.7).abs() < 1e-3), "{:?}", t.pads[0].own_force);
    }

    // The press is 1 at the click: the set force, else the pad's own once seen,
    // else the sensor's whole scale.
    #[test]
    fn the_press_reaches_one_where_the_pad_clicks() {
        let mut t = Tracking { config: TrackingConfig { click_force: [0.4, 0.0], ..Default::default() }, ..Default::default() };
        assert!((t.press(0, 0.2) - 0.5).abs() < 1e-6);
        assert_eq!(t.press(0, 0.6), 1.0, "past the click, held at 1");
        assert!((t.press(1, 0.2) - 0.2).abs() < 1e-6, "the right pad's force is its own");
        assert!(t.config.set("tp_click_force_r", 0.8));
        assert!((t.press(1, 0.2) - 0.25).abs() < 1e-6);
        t.config.click_force = [0.0; 2];
        assert!((t.press(0, 0.2) - 0.2).abs() < 1e-6, "no click seen yet");
        t.pads[0].own_force = Some(0.8);
        assert!((t.press(0, 0.2) - 0.25).abs() < 1e-6);
        assert!((t.press(1, 0.2) - 0.2).abs() < 1e-6, "each pad its own");
    }

    // Click stabilisation holds the pointer as the press builds, then goes on
    // from there, without a jump, once it has eased off and settled.
    #[test]
    fn click_stabilisation_holds_the_pointer_through_a_press() {
        let mut t = Tracking { config: TrackingConfig { stabilize: 0.0, click_force: [0.5; 2], click_hold: 0.6 }, ..Default::default() };
        let mut reports = vec![touched(0.0, 0.1, false)];
        // Pressing from 0.3 (60% of 0.5): the finger rolls 0.1 under the force.
        reports.extend((0..10).map(|i| touched(i as f32 * 0.01, 0.6, false)));
        // Eased off, still rolling, then 80 ms of settle and on.
        reports.extend((0..40).map(|i| touched(0.1 + i as f32 * 0.001, 0.1, false)));
        let out = track(&mut t, &reports);
        assert!(out[1..11].iter().all(|(x, _)| x.abs() < 1e-4), "held at 0 through the press");
        assert!(out[11..30].iter().all(|(x, _)| x.abs() < 1e-4), "and through the settle");
        let (last, _) = out[49];
        assert!(last > 0.0 && last < 0.06, "then on from there, not from 0.1: {last}");
    }

    // A move past the hold's reach is meant: a firm swipe moves, the gap the
    // hold left closes as the finger goes on, and every edge is still reached.
    #[test]
    fn a_meant_move_breaks_the_hold_and_the_point_catches_up() {
        let mut t = Tracking { config: TrackingConfig { stabilize: 0.0, click_force: [0.5; 2], click_hold: 0.6 }, ..Default::default() };
        let mut reports = vec![touched(0.0, 0.1, false)];
        // A firm press, then a swipe right while still pressing.
        reports.extend((0..60).map(|i| touched(i as f32 * 0.02, 0.4, false)));
        let out = track(&mut t, &reports);
        assert!(out[1..6].iter().all(|(x, _)| x.abs() < 1e-4), "held while within reach");
        let (x, _) = out[30];
        assert!(x > 0.3, "the swipe moves the point: {x}");
        let (x, _) = out[60];
        assert!((x - 1.0).abs() < 0.03, "and it reaches the edge with the finger: {x}");
    }

    // Presses between polls are kept, the link reports turn the slot on and off,
    // and a repeated IMU timestamp is no new sample.
    #[test]
    fn a_slot_keeps_what_the_poll_needs() {
        let slot = Slot::new(String::new(), String::new(), 2, 0x1304);
        let report = |buttons: u32, stamp: u32| state(REPORT_STATE, |d| {
            d[1..5].copy_from_slice(&buttons.to_le_bytes());
            d[29..33].copy_from_slice(&stamp.to_le_bytes());
        });
        assert!(!slot.connected());
        slot.take(&report(bit::A, 0));
        slot.take(&report(0, 1));
        slot.take(&report(0, 1));
        let s = slot.lock();
        assert!(s.connected);
        assert_eq!((s.held, s.report.buttons, s.reports), (bit::A, 0, 3));
        assert_eq!(s.samples.len(), 1, "timestamp 0 is SDL's start, and 1 came twice");
        drop(s);
        slot.take(&[REPORT_LINK, LINK_DOWN]);
        assert!(!slot.connected());
    }
}
