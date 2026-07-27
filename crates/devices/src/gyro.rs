use std::collections::{HashMap, HashSet};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use hidapi::{HidApi, HidDevice};

#[cfg(windows)]
use crate::dualsense_haptic;

const SONY_VID: u16           = 0x054C;
const DS4_PIDS: &[u16]       = &[0x05C4, 0x09CC];
const DUALSENSE_PIDS: &[u16] = &[0x0CE6, 0x0DF2]; // standard + DualSense Edge
const SWITCH_VID: u16     = 0x057E;
const SWITCH_PRO_PID: u16 = 0x2009;

const USAGE_PAGE_GENERIC_DESKTOP: u16 = 0x0001;
const USAGE_GAMEPAD: u16 = 0x0005;

// ── IMU sensitivity constants ─────────────────────────────────────────────────
// Normalized signal graph values: ±1.0 means ±GYRO_REF_DPS deg/s or ±ACCEL_REF_G G.
// Virtual devices that consume gyro/accel signals assume this common reference so
// they can rescale to their own hardware spec without knowing the source device.

/// ±1.0 in the signal graph corresponds to this many degrees per second.
pub const GYRO_REF_DPS: f32 = 2000.0;
/// ±1.0 in the signal graph corresponds to this many standard gravity units.
pub const ACCEL_REF_G: f32 = 8.0;

// Per-device sensitivities (physical deg/s or G per raw sensor LSB).
// DS4 / DualSense: factory ±2000 dps gyro, ±8 G accel.
const DS4_GYRO_DPS_PER_LSB: f32 = 2000.0 / 32767.0;
const DS4_ACCEL_G_PER_LSB: f32  = 8.0   / 32767.0;
// Switch Pro (ICM-20689): ±2000 dps, matching DS4/DualSense so a physical
// rotation produces the SAME normalized value on every known controller (the
// 3D orientation view confirmed the old 4000-based scale read exactly 2× too
// large — it needed a 0.5 display scale to track reality). Uniformity beats
// preserving old patches' feel per user decision 2026-07-17.
const SWITCH_GYRO_DPS_PER_LSB: f32 = 2000.0 / 32767.0;
const SWITCH_ACCEL_G_PER_LSB: f32  = 8.0   / 32767.0;

// Retry open no more than once per N seconds to avoid hammering HidHide.
const RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// A stalled handle (open, reads succeed, but no report parses) is logged at 2s
/// but only auto-reopened after this longer window — comfortably past the Switch
/// Pro's known transient ~3s Bluetooth input gaps, which recover on their own.
/// Past this, the stall is the permanent "HidHide cloaked the pad after we
/// opened its raw-HID handle" freeze, so we drop the handle and let the open
/// worker re-open it (now with the cloak in place) — what a manual reconnect does.
const STALL_REOPEN: Duration = Duration::from_millis(5000);

/// Monotonic epoch for the Switch HD-rumble amplitude-modulation phase. Set on the
/// first rumble flush; the AM flutter phase is `elapsed_secs * 2π·rate_hz` so it
/// stays smooth and continuous regardless of when individual packets are sent.
static SWITCH_AM_EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

#[derive(Clone, Copy, Default, Debug)]
pub struct TouchPoint {
    /// Active flag — touchpad currently sees this finger.
    pub active: bool,
    /// Normalized X in roughly [-1, 1] (left edge = -1, right edge = +1).
    pub x: f32,
    /// Normalized Y in roughly [-1, 1] (top edge = -1, bottom edge = +1).
    pub y: f32,
}

/// Full DualSense input state parsed directly from the raw HID input report.
/// Used to override gilrs, which mis-maps axes on the Windows HID path.
#[derive(Clone, Copy, Default, Debug)]
pub struct DualSenseState {
    // Sticks: normalized [-1, 1]
    pub lx: f32, pub ly: f32,
    pub rx: f32, pub ry: f32,
    // Triggers: normalized [0, 1]
    pub l2: f32, pub r2: f32,
    // Face buttons
    pub btn_south: bool, pub btn_east: bool, pub btn_west: bool, pub btn_north: bool,
    // Shoulder / trigger digital
    pub btn_l1: bool, pub btn_r1: bool,
    pub btn_l2: bool, pub btn_r2: bool,
    // Stick clicks
    pub btn_ls: bool, pub btn_rs: bool,
    // Menu / special
    pub btn_options: bool, pub btn_create: bool, pub btn_ps: bool,
    // DPad
    pub dpad_up: bool, pub dpad_down: bool, pub dpad_left: bool, pub dpad_right: bool,
}

/// Switch Pro button state read directly from input report 0x30 bytes 3/4/5.
/// Used to bypass gilrs's WGI backend which loses diagonal D-Pad positions and
/// has unreliable Home/Capture/Plus/Minus mapping in BT mode.
/// Naming uses Nintendo's physical labels (A=east, B=south, X=north, Y=west).
#[derive(Clone, Copy, Default, Debug)]
pub struct SwitchProButtons {
    pub btn_a: bool, pub btn_b: bool, pub btn_x: bool, pub btn_y: bool,
    pub btn_l: bool, pub btn_r: bool, pub btn_zl: bool, pub btn_zr: bool,
    pub btn_lstick: bool, pub btn_rstick: bool,
    pub btn_minus: bool, pub btn_plus: bool,
    pub btn_home: bool, pub btn_capture: bool,
    pub dpad_up: bool, pub dpad_down: bool, pub dpad_left: bool, pub dpad_right: bool,
    /// L-Stick analog X normalized to [-1, 1] (raw 12-bit calibrated value).
    pub lstick_x: f32,
    pub lstick_y: f32,
    pub rstick_x: f32,
    pub rstick_y: f32,
}

/// # Canonical IMU frame (the AutoMap contract)
///
/// Every device's parser MUST fill `gyro_*` / `accel_*` in ONE shared body
/// frame, so that nothing downstream — the 3DOF module, AutoMap, the canvas —
/// ever has to know which pad produced a value. This is the entire point of
/// AutoMap; a device that deviates here is a bug at the source, not something
/// for a consumer to special-case.
///
/// Axes (right = the player's right, holding the pad normally):
/// - **x = forward / longitudinal.** `accel_x > 0` when the nose (USB port)
///   tilts up. `gyro_x` is ROLL (rotation about this axis), + clockwise.
/// - **y = side / lateral.** `accel_y > 0` when the right grip drops.
///   `gyro_y` is PITCH (about this axis), + nose-up.
/// - **z = vertical.** `accel_z > 0` when the pad lies flat, face up
///   (gravity ≈ +1 g). `gyro_z` is YAW (about this axis), + clockwise.
///
/// Accel and gyro therefore share the same axis assignment: roll is about the
/// forward axis, pitch about the side axis, yaw about vertical. The reference
/// device is the Switch Pro, whose raw report already lands in this frame; the
/// Sony parser permutes and signs its raw accel to match (see `build`).
#[derive(Clone, Copy, Default, Debug)]
pub struct HidReading {
    pub gyro_x: f32,
    pub gyro_y: f32,
    pub gyro_z: f32,
    pub accel_x: f32,
    pub accel_y: f32,
    pub accel_z: f32,
    /// True when the source device exposes a touchpad (DS4 / DualSense).
    pub has_touchpad: bool,
    pub touch1: TouchPoint,
    pub touch2: TouchPoint,
    /// Touchpad click (the whole touchpad is also a button on DS4 / DualSense).
    /// Read straight from the HID report because gilrs's Windows backend doesn't
    /// expose it reliably.
    pub touchpad_click: bool,
    /// Microphone mute button (DualSense only).
    pub mic_button: bool,
    /// Switch Pro full button state, parsed from raw HID. None for non-Switch devices.
    pub switch_buttons: Option<SwitchProButtons>,
    /// DualSense full input state, parsed from raw HID. None for non-DualSense devices.
    pub dualsense: Option<DualSenseState>,
    /// Battery charge as a fraction 0.0..1.0, decoded from the DS4/DualSense
    /// report (`None` for devices we don't decode battery for, e.g. Switch Pro).
    /// Surfaced as the `battery` source pin so FlexInput can show the PHYSICAL
    /// pad's real charge. (Virtual pads always report 100% — see encode.rs — so
    /// this read path is purely informational and never feeds a virtual report.)
    pub battery: Option<f32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Connection { Usb, Bt }

/// Per-axis stick calibration. center is the raw resting value; min/max are the
/// raw extents at full deflection (each ~1500 LSB away from center, but varies
/// per controller and per side). Values normalize raw → [-1, 1] as:
///   raw < center: (raw - center) / (center - min)
///   raw > center: (raw - center) / (max - center)
#[derive(Clone, Copy, Default, Debug)]
struct AxisCalib { min: u16, center: u16, max: u16 }

#[derive(Clone, Copy, Default, Debug)]
struct SwitchProCalib {
    l_x: AxisCalib,
    l_y: AxisCalib,
    r_x: AxisCalib,
    r_y: AxisCalib,
}

enum DeviceKind {
    Ds4,
    DualSense {
        connection: Option<Connection>,
        /// 4-bit sequence counter for BT output reports (low 4 bits used).
        bt_seq: u8,
        /// Lazily-initialized WASAPI haptic stream (USB only). `None` while
        /// the audio endpoint hasn't been resolved yet or on non-Windows builds.
        #[cfg(windows)]
        haptic: Option<dualsense_haptic::HapticStream>,
    },
    SwitchPro {
        initialized: bool,
        packet_counter: u8,
        /// Stick calibration read from SPI flash during init. None until the
        /// SPI read succeeds; parsing falls back to a centered 12-bit identity
        /// (center=2048, half-range=1500) when None so sticks still work even
        /// if calibration read failed.
        calib: Option<SwitchProCalib>,
    },
}

struct HidEntry {
    device: HidDevice,
    kind: DeviceKind,
    last: HidReading,
    out: OutputState,
    /// Output state at the time of the last successful HID write. If `out`
    /// matches this, the controller already has the current state and we
    /// can skip the USB write entirely — the I/O thread runs at hundreds
    /// of Hz but real-world rumble / lightbar / trigger updates happen at
    /// dozens of Hz at most, so 99 %+ of writes are redundant. Sending
    /// only on change drops gyro_flush_outputs from ~8 ms/iter to a few
    /// microseconds when nothing's changing. Also kinder to USB bandwidth.
    /// `None` forces the first write after connect / option change.
    last_sent: Option<OutputState>,
    /// Last Instant we successfully wrote to the device. Used together
    /// with `last_sent` to enforce a minimum heartbeat (~1 Hz) — some
    /// firmware (DS4) drops back to safe defaults if it doesn't see a
    /// host output report for a while, so we re-send the current state
    /// every second even when unchanged.
    last_sent_at: Option<std::time::Instant>,
    output_active: bool,
    /// Number of HID input reports successfully parsed since the last drain
    /// of `take_event_count`. Used to feed the per-device polling-rate readout
    /// so gyro-only devices (and IMU activity in general) show real Hz.
    event_count: u32,
    /// Per-axis snap-back spike filter state.
    /// `spike_anchor` holds the IMU values currently being emitted to the
    /// engine. `spike_pending` holds the most recently parsed packet,
    /// deferred by one packet so we can judge whether it was a snap-back
    /// outlier when the NEXT packet arrives. The filter only acts on the
    /// six IMU axes; all other fields of `last` are written straight from
    /// the freshest packet (button state, touchpad, etc).
    spike_enabled: bool,
    spike_sensitivity: f32,
    spike_anchor: Option<HidReading>,
    spike_pending: Option<HidReading>,
    /// Diagnostics for the "frozen until reconnect" report (a Switch Pro paired
    /// over Bluetooth BEFORE launch comes up dead). `last_report_at` is the last
    /// time a report actually parsed; `stalled` latches so the stall onset /
    /// recovery is logged once, not every poll.
    last_report_at: std::time::Instant,
    stalled: bool,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct OutputState {
    /// Big / heavy / low-frequency motor (DS4 "left", DualSense `motor_left`).
    rumble_strong: u8,
    /// Small / light / high-frequency motor (DS4 "right", DualSense `motor_right`).
    rumble_weak: u8,
    lightbar_r: u8,
    lightbar_g: u8,
    lightbar_b: u8,
    // DualSense adaptive trigger — right
    // mode: 0=Off, 1=Feedback, 2=Weapon, 3=Vibration
    trigger_r_mode:     u8,
    trigger_r_start:    u8, // zone 0–9 (stored as raw zone; caller passes 0–9 already scaled)
    trigger_r_end:      u8, // zone 0–9 (Weapon mode only)
    trigger_r_strength: u8, // force 0–7
    trigger_r_freq:     u8, // 0–255 (Vibration mode only)
    // DualSense adaptive trigger — left
    trigger_l_mode:     u8,
    trigger_l_start:    u8,
    trigger_l_end:      u8,
    trigger_l_strength: u8,
    trigger_l_freq:     u8,
    // DualSense LEDs
    // player_led: 0=off, 1=P1(0x04), 2=P2(0x0A), 3=P3(0x15), 4=P4(0x1B)
    player_led: u8,
    // mic_led: 0=off, 1=on(orange), 2=pulsing
    mic_led:    u8,
    // HD Rumble carrier 1 (LF) — per-side amplitude + frequency (0–255 each, mapped at encode).
    // hd_l/r_amp:  0=silent, 255=max safe; perceptual power-law curve applied at encode.
    // hd_l/r_freq: 0–255 mapped logarithmically over safe range 82–1253 Hz (indices 32–159).
    // Device-agnostic: a Switch Pro packs this into its HD-rumble packet; a USB
    // DualSense synthesizes it as a PCM sine on the LRA.
    hd_l_amp:  u8,
    hd_l_freq: u8,
    hd_r_amp:  u8,
    hd_r_freq: u8,
    // HD Rumble carrier 2 (HF) — the SECOND simultaneous carrier per actuator.
    // Both LRA families play two carriers at once that mix on the actuator. Zero
    // amplitude ⇒ single-carrier behavior (carrier 1 only). The legacy ds_*_amp/freq
    // pins are routed to carrier 1 (hd_*) in set_output_byte.
    hd2_l_amp:  u8,
    hd2_l_freq: u8,
    hd2_r_amp:  u8,
    hd2_r_freq: u8,
}

pub struct GyroManager {
    /// Shared with the gilrs backend's background classification worker — the
    /// ONLY place allowed to call the slow (~110-200 ms on Windows)
    /// `refresh_devices()`. Everything on the I/O thread reads the cached list
    /// under a short lock and never refreshes inline.
    api: Option<Arc<Mutex<HidApi>>>,
    // key: (vid, pid, instance_index)
    devices: HashMap<(u16, u16, usize), HidEntry>,
    // tracks the last failed open attempt to rate-limit retries
    failed_opens: HashMap<(u16, u16, usize), Instant>,
    /// Set when an open retry would previously have refreshed the device list
    /// inline (a ~110-200 ms stall ON the I/O loop, every RETRY_INTERVAL
    /// while a gyro-capable pad persistently failed to open — the "variable
    /// gaps" polling freeze). Drained by the gilrs backend, which turns it into
    /// an async refresh+reclassify job on the worker instead.
    refresh_wanted: bool,
    /// Job side of the "hid-open" worker: device opens AND the Switch Pro init
    /// handshake run off-thread. The handshake is the ~600 ms io-stall
    /// fingerprint over Bluetooth (100 ms USB-probe timeout + subcommand acks
    /// + four 50 ms×N SPI calibration reads) — a BT pad that hiccups every few
    /// seconds used to freeze ALL input for that long on every reopen.
    open_tx: Option<mpsc::Sender<(u16, u16, usize)>>,
    /// Result side: `(key, opened)` — `None` payload = open/init failed
    /// (stamped into `failed_opens` for the normal rate-limited retry).
    open_rx: Option<mpsc::Receiver<((u16, u16, usize), Option<OpenedDevice>)>>,
    /// Keys with an open in flight, so `read()` doesn't re-request every tick.
    opens_pending: HashSet<(u16, u16, usize)>,
}

/// The `Send`-safe parts of a freshly opened device, produced by the
/// "hid-open" worker. `HidEntry` itself is assembled on the I/O thread because
/// `DeviceKind::DualSense`'s lazily-created WASAPI haptic stream must never
/// cross threads — the worker only ever ships the raw handle + init results.
struct OpenedDevice {
    device: HidDevice,
    kind_tag: KindTag,
    /// Switch Pro only: SPI calibration + subcommand counter from the init
    /// handshake (run to completion on the worker).
    calib: Option<SwitchProCalib>,
    packet_counter: u8,
}

/// Background worker loop: performs `open_and_init` (device-list read + open
/// under a short api lock, then the slow Switch Pro handshake with the lock
/// RELEASED) and ships the result back. One job at a time — opens are rare
/// (connect/reconnect) and ordering doesn't matter.
fn open_worker(
    api: Arc<Mutex<HidApi>>,
    jobs: mpsc::Receiver<(u16, u16, usize)>,
    done: mpsc::Sender<((u16, u16, usize), Option<OpenedDevice>)>,
) {
    while let Ok((vid, pid, idx)) = jobs.recv() {
        let opened = open_and_init(&api, vid, pid, idx);
        if done.send(((vid, pid, idx), opened)).is_err() {
            break; // manager dropped — exit the thread
        }
    }
}

impl GyroManager {
    pub fn new() -> Self {
        let api = HidApi::new().ok().map(|a| Arc::new(Mutex::new(a)));
        let mut open_tx = None;
        let mut open_rx = None;
        if let Some(api) = api.clone() {
            let (jtx, jrx) = mpsc::channel();
            let (dtx, drx) = mpsc::channel();
            let spawned = std::thread::Builder::new()
                .name("hid-open".into())
                .spawn(move || open_worker(api, jrx, dtx))
                .is_ok();
            if spawned {
                open_tx = Some(jtx);
                open_rx = Some(drx);
            }
        }
        Self {
            api,
            devices: HashMap::new(),
            failed_opens: HashMap::new(),
            refresh_wanted: false,
            open_tx,
            open_rx,
            opens_pending: HashSet::new(),
        }
    }

    /// Adopt finished opens from the worker: successes become live entries
    /// (unless the key raced back in), failures get the normal rate-limited
    /// retry stamp.
    fn drain_finished_opens(&mut self) {
        let Some(rx) = &self.open_rx else { return };
        while let Ok((key, opened)) = rx.try_recv() {
            self.opens_pending.remove(&key);
            match opened {
                Some(o) if !self.devices.contains_key(&key) => {
                    let kind = match o.kind_tag {
                        KindTag::Ds4 => DeviceKind::Ds4,
                        KindTag::DualSense => DeviceKind::DualSense {
                            connection: None,
                            bt_seq: 0,
                            #[cfg(windows)]
                            haptic: None,
                        },
                        KindTag::SwitchPro => DeviceKind::SwitchPro {
                            initialized: true, // handshake ran on the worker
                            packet_counter: o.packet_counter,
                            calib: o.calib,
                        },
                    };
                    self.devices.insert(key, HidEntry {
                        device: o.device,
                        kind,
                        last: HidReading::default(),
                        out: OutputState::default(),
                        last_sent: None,
                        last_sent_at: None,
                        output_active: false,
                        event_count: 0,
                        spike_enabled: true,
                        spike_sensitivity: 50.0,
                        spike_anchor: None,
                        spike_pending: None,
                        last_report_at: Instant::now(),
                        stalled: false,
                    });
                }
                Some(_) => {} // key already live again — drop the duplicate handle
                None => { self.failed_opens.insert(key, Instant::now()); }
            }
        }
    }

    /// Handle to the shared hidapi instance for the background refresh/classify
    /// worker. `None` when hidapi failed to initialize (classification then
    /// stays empty and gyro reads are unavailable, same as before).
    pub(crate) fn shared_api(&self) -> Option<Arc<Mutex<HidApi>>> {
        self.api.clone()
    }

    /// True once per request: an open retry wanted a fresh device list. The
    /// gilrs backend drains this in `enumerate()` and kicks the async worker.
    pub(crate) fn take_refresh_wanted(&mut self) -> bool {
        std::mem::take(&mut self.refresh_wanted)
    }

    /// Returns the latest IMU + touchpad reading for the Nth physical device with this VID/PID.
    pub fn read(&mut self, vid: u16, pid: u16, idx: usize) -> Option<HidReading> {
        puffin::profile_function!();
        if classify(vid, pid).is_none() {
            return None;
        }

        if !self.devices.contains_key(&(vid, pid, idx)) {
            // Opens run on the "hid-open" worker: open_path on a flaky BT link
            // plus the Switch Pro init handshake cost ~600 ms, and doing them
            // here froze ALL input on every reconnect. Adopt any finished
            // opens, then (rate-limited) request one if the key is still dark.
            self.drain_finished_opens();
            let key = (vid, pid, idx);
            if !self.devices.contains_key(&key) {
                let should_try = !self.opens_pending.contains(&key)
                    && self.failed_opens.get(&key)
                        .map_or(true, |t| t.elapsed() >= RETRY_INTERVAL);
                if should_try {
                    // Also ask the classify worker for a fresh device list —
                    // never inline (the ~110-200 ms call). The open job uses
                    // the current cached list; if the device only shows up in
                    // the fresh one, the next retry picks it up.
                    self.refresh_wanted = true;
                    match &self.open_tx {
                        Some(tx) if tx.send(key).is_ok() => {
                            self.opens_pending.insert(key);
                        }
                        _ => { self.failed_opens.insert(key, Instant::now()); }
                    }
                }
                return None; // nothing to read until the worker delivers
            }
        }

        let entry = self.devices.get_mut(&(vid, pid, idx))?;
        // (Switch Pro init used to run lazily right here, blocking the I/O
        // thread — it now completes on the worker before the entry exists.)

        // If reading fails (device disconnected), drop and retry next cycle.
        let ok = drain_reports(entry);
        if !ok {
            self.devices.remove(&(vid, pid, idx));
            return None;
        }
        // Stall handling: the handle is open and reads succeed (no error), but no
        // report has parsed for a while → the "frozen until reconnect" signature.
        // Because the read returns 0 bytes rather than erroring, the device is
        // never dropped, so input just silently stops. Prime suspect (confirmed on
        // this machine): HidHide cloaking the pad AFTER we opened its raw-HID
        // handle (the init log shows 0x30 reports flowing, then HidHide applies).
        let elapsed = entry.last_report_at.elapsed();
        if !entry.stalled && elapsed >= Duration::from_millis(2000) {
            entry.stalled = true;
            eprintln!(
                "[gyro] {vid:04X}:{pid:04X} idx={idx} STALLED: no HID reports for {:.1}s \
                 but the handle is still open (reads return 0 bytes, no error). Suspect \
                 HidHide cloaking the pad while we hold its raw-HID handle; will auto-reopen \
                 if it doesn't recover.",
                elapsed.as_secs_f32()
            );
        }
        let last = entry.last; // ends the &mut borrow so we can drop the entry
        // Past STALL_REOPEN it's not one of the Switch Pro's transient BT gaps —
        // drop the handle so the open worker re-opens it with the cloak already
        // in place (whitelisted), automating what a manual reconnect does.
        if elapsed >= STALL_REOPEN {
            eprintln!(
                "[gyro] {vid:04X}:{pid:04X} idx={idx} auto-reopening after {:.1}s stall \
                 (HidHide-cloak-after-open recovery)",
                elapsed.as_secs_f32()
            );
            self.devices.remove(&(vid, pid, idx));
            return None;
        }
        Some(last)
    }

    pub fn remove(&mut self, vid: u16, pid: u16, idx: usize) {
        self.devices.remove(&(vid, pid, idx));
    }

    /// Configure the per-device snap-back spike filter applied at the
    /// HID polling layer. `sensitivity_pct` is 0..100 (50 = default).
    /// Has no effect if the device hasn't been opened yet — callers
    /// should retry on subsequent frames or just call every UI frame.
    pub fn set_spike_filter(&mut self, vid: u16, pid: u16, idx: usize, on: bool, sensitivity_pct: f32) {
        if let Some(e) = self.devices.get_mut(&(vid, pid, idx)) {
            let s = sensitivity_pct.clamp(0.0, 100.0);
            let changed = e.spike_enabled != on || (e.spike_sensitivity - s).abs() > f32::EPSILON;
            if changed {
                e.spike_enabled = on;
                e.spike_sensitivity = s;
                if !on {
                    e.spike_anchor = None;
                    e.spike_pending = None;
                }
            }
        }
    }

    /// Drains the count of parsed HID reports for `(vid, pid, idx)` since the
    /// previous call. Used by the I/O thread to attribute IMU activity to the
    /// per-device polling-rate readout.
    pub fn take_event_count(&mut self, vid: u16, pid: u16, idx: usize) -> u32 {
        self.devices.get_mut(&(vid, pid, idx))
            .map(|e| std::mem::take(&mut e.event_count))
            .unwrap_or(0)
    }


    /// Stage one byte of an output report (rumble/lightbar) for the Nth physical
    /// device with this VID/PID. Has no effect if the device isn't open. Call
    /// `flush_outputs()` once per frame to actually transmit.
    pub fn set_output_byte(&mut self, vid: u16, pid: u16, idx: usize, pin_id: &str, byte: u8) {
        let entry = match self.devices.get_mut(&(vid, pid, idx)) {
            Some(e) => e,
            None => return,
        };
        let updated = match pin_id {
            "rumble_strong" => { entry.out.rumble_strong = byte; true }
            "rumble_weak"   => { entry.out.rumble_weak   = byte; true }
            // Legacy amplitude-only HD rumble pins — route to per-side amp field.
            "hd_rumble_l" => { entry.out.hd_l_amp = byte; true }
            "hd_rumble_r" => { entry.out.hd_r_amp = byte; true }
            // HD rumble carrier 1 (LF) — amplitude + frequency per side. Shared by
            // Switch Pro (table encode) and DualSense (PCM synth).
            "hd_l_amp"  => { entry.out.hd_l_amp  = byte; true }
            "hd_l_freq" => { entry.out.hd_l_freq  = byte; true }
            "hd_r_amp"  => { entry.out.hd_r_amp   = byte; true }
            "hd_r_freq" => { entry.out.hd_r_freq  = byte; true }
            // HD rumble carrier 2 (HF) — the second simultaneous carrier.
            "hd2_l_amp"  => { entry.out.hd2_l_amp  = byte; true }
            "hd2_l_freq" => { entry.out.hd2_l_freq = byte; true }
            "hd2_r_amp"  => { entry.out.hd2_r_amp  = byte; true }
            "hd2_r_freq" => { entry.out.hd2_r_freq = byte; true }
            // DualSense HD haptics legacy aliases — amplitude + frequency per side
            // (USB only). Route to carrier 1 so old patches keep working.
            "ds_l_amp"  => { entry.out.hd_l_amp  = byte; true }
            "ds_l_freq" => { entry.out.hd_l_freq = byte; true }
            "ds_r_amp"  => { entry.out.hd_r_amp  = byte; true }
            "ds_r_freq" => { entry.out.hd_r_freq = byte; true }
            "lightbar_r"    => { entry.out.lightbar_r = byte; true }
            "lightbar_g"    => { entry.out.lightbar_g = byte; true }
            "lightbar_b"    => { entry.out.lightbar_b = byte; true }
            // Adaptive trigger pins — caller passes Float 0–1 already scaled to
            // the appropriate range before calling set_output_byte:
            //   mode:     0–3  (0=Off,1=Feedback,2=Weapon,3=Vibration)
            //   start/end: 0–9 (zone index along trigger travel)
            //   strength:  0–7 (force level)
            //   freq:      0–255 (vibration frequency, Vibration mode only)
            "trigger_r_mode"     => { entry.out.trigger_r_mode     = byte; true }
            "trigger_r_start"    => { entry.out.trigger_r_start    = byte; true }
            "trigger_r_end"      => { entry.out.trigger_r_end      = byte; true }
            "trigger_r_strength" => { entry.out.trigger_r_strength = byte; true }
            "trigger_r_freq"     => { entry.out.trigger_r_freq     = byte; true }
            "trigger_l_mode"     => { entry.out.trigger_l_mode     = byte; true }
            "trigger_l_start"    => { entry.out.trigger_l_start    = byte; true }
            "trigger_l_end"      => { entry.out.trigger_l_end      = byte; true }
            "trigger_l_strength" => { entry.out.trigger_l_strength = byte; true }
            "trigger_l_freq"     => { entry.out.trigger_l_freq     = byte; true }
            // DualSense LEDs — caller passes scaled byte
            "player_led" => { entry.out.player_led = byte; true }
            "mic_led"    => { entry.out.mic_led    = byte; true }
            _ => false,
        };
        if updated { entry.output_active = true; }
    }

    /// Stage a signed float output value for AC rumble pins (hd_l_ac / hd_r_ac).
    /// Send pending output reports for every device that has been driven at
    /// least once. Call once per frame.
    pub fn flush_outputs(&mut self) {
        // Heartbeat interval: re-send the current output state at least
        // this often even if unchanged. DS4 firmware drops to defaults if
        // it doesn't see a host report for ~5 s; 1 s is conservative.
        const HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(1);
        let now = std::time::Instant::now();
        for entry in self.devices.values_mut() {
            if !entry.output_active { continue; }
            // Skip the USB write when the output state hasn't changed since
            // the last successful write AND we're within the heartbeat
            // window. This turns polling-rate-many redundant writes per
            // second per device (500/s at the default rate) into ~1 —
            // gyro_flush_outputs goes from ~8 ms/iter to a few µs in the
            // steady state.
            let unchanged = entry.last_sent == Some(entry.out);
            let within_heartbeat = entry.last_sent_at
                .map(|t| now.duration_since(t) < HEARTBEAT)
                .unwrap_or(false);
            if unchanged && within_heartbeat { continue; }
            // Snapshot the output state we're about to send so we can stamp
            // `last_sent` after the match without re-borrowing `entry`.
            let to_send = entry.out;
            // Set by the Switch arm when amplitude modulation is active: the emitted
            // packet then changes every tick even with steady inputs, so we must NOT
            // let the unchanged-skip suppress the next packet (it would freeze the
            // flutter). Stamping `last_sent = None` forces a resend next tick.
            let mut force_continuous = false;
            let HidEntry { device, kind, out, .. } = entry;
            match kind {
                DeviceKind::Ds4 => {
                    hid_write(device, &build_ds4_usb_out(out));
                }
                DeviceKind::DualSense { connection, bt_seq, #[cfg(windows)] haptic } => {
                    match connection {
                        Some(Connection::Bt) => {
                            let pkt = build_dualsense_bt_out(out, *bt_seq);
                            *bt_seq = bt_seq.wrapping_add(1) & 0x0F;
                            hid_write(device, &pkt);
                        }
                        // USB (or unknown — default to USB report which is what
                        // the firmware accepts when the device is on interface 3).
                        _ => {
                            hid_write(device, &build_dualsense_usb_out(out));
                            #[cfg(windows)]
                            {
                                // HD haptics over the USB audio endpoint: synthesize
                                // BOTH carriers (LF = hd_*, HF = hd2_*) as PCM sines
                                // on the LRAs. Open the stream lazily the first time
                                // any carrier is non-zero.
                                let want_haptic = out.hd_l_amp != 0 || out.hd_r_amp != 0
                                    || out.hd2_l_amp != 0 || out.hd2_r_amp != 0;
                                if want_haptic && haptic.is_none() {
                                    *haptic = dualsense_haptic::HapticStream::open();
                                }
                                if let Some(h) = haptic.as_mut() {
                                    h.set_targets_dual(
                                        out.hd_l_amp  as f32 / 255.0, out.hd_l_freq  as f32 / 255.0,
                                        out.hd2_l_amp as f32 / 255.0, out.hd2_l_freq as f32 / 255.0,
                                        out.hd_r_amp  as f32 / 255.0, out.hd_r_freq  as f32 / 255.0,
                                        out.hd2_r_amp as f32 / 255.0, out.hd2_r_freq as f32 / 255.0,
                                    );
                                }
                            }
                        }
                    }
                }
                DeviceKind::SwitchPro { initialized, packet_counter, .. } => {
                    if !*initialized { continue; }
                    // The Switch Pro voice coil is driven by amplitude+frequency
                    // (HD rumble). It has no classic ERM motors, so a value wired
                    // to the legacy `rumble_strong/weak` pins would otherwise be
                    // silent. When no explicit HD amplitude is set, synthesize it
                    // from the legacy pins (strong→left, weak→right) so legacy
                    // rumble — and the game's classic rumble routed via AutoMap —
                    // is audible. Explicit HD wiring always wins: if either
                    // hd_*_amp is non-zero we use the HD pins verbatim and ignore
                    // legacy, so the two paths never fight.
                    // Both carriers count as "HD set" — carrier 2 (hd2_*) alone is
                    // enough to take the HD path.
                    let hd_set = out.hd_l_amp != 0 || out.hd_r_amp != 0
                        || out.hd2_l_amp != 0 || out.hd2_r_amp != 0;
                    // Per side: carrier 1 (LF) + carrier 2 (HF). In legacy fallback
                    // (no HD wiring) the classic rumble pins drive carrier 1 only at
                    // the firmware-default ~320 Hz, with carrier 2 silent.
                    let (l_lf_a, l_lf_f, l_hf_a, l_hf_f,
                         r_lf_a, r_lf_f, r_hf_a, r_hf_f) = if hd_set {
                        (
                            out.hd_l_amp  as f32 / 255.0, out.hd_l_freq  as f32 / 255.0,
                            out.hd2_l_amp as f32 / 255.0, out.hd2_l_freq as f32 / 255.0,
                            out.hd_r_amp  as f32 / 255.0, out.hd_r_freq  as f32 / 255.0,
                            out.hd2_r_amp as f32 / 255.0, out.hd2_r_freq as f32 / 255.0,
                        )
                    } else {
                        const DEFAULT_FREQ: f32 = 0.6;
                        let l = out.rumble_strong as f32 / 255.0;
                        let r = out.rumble_weak as f32 / 255.0;
                        (
                            l, if l > 0.0 { DEFAULT_FREQ } else { 0.0 }, 0.0, 0.0,
                            r, if r > 0.0 { DEFAULT_FREQ } else { 0.0 }, 0.0, 0.0,
                        )
                    };
                    // HF "texture" = amplitude modulation of the LF carrier. Advance
                    // each side's modulator phase from a monotonic process clock (so
                    // the flutter is smooth regardless of when packets actually go
                    // out): phase = elapsed_secs * (2π · mod_rate_hz). hd2_*_amp is the
                    // mod DEPTH, hd2_*_freq the mod RATE.
                    let t = SWITCH_AM_EPOCH.get_or_init(std::time::Instant::now)
                        .elapsed().as_secs_f32();
                    let l_phase = t * am_rate_rad_per_sec(l_hf_f);
                    let r_phase = t * am_rate_rad_per_sec(r_hf_f);
                    let left  = switch_rumble_encode_am(l_lf_a, l_lf_f, l_hf_a, l_hf_f, l_phase);
                    let right = switch_rumble_encode_am(r_lf_a, r_lf_f, r_hf_a, r_hf_f, r_phase);
                    let pkt = build_switch_rumble_only(*packet_counter, left, right);
                    *packet_counter = packet_counter.wrapping_add(1);
                    hid_write(device, &pkt);
                    // When AM is active the packet changes every tick even with steady
                    // inputs, so force continuous re-send (bypass the unchanged-skip).
                    force_continuous = l_hf_a > 0.0 || r_hf_a > 0.0;
                }
            }
            // Record what we just wrote so the next iteration can skip
            // if nothing's changed. If the hid_write failed (debug logs
            // it; release silently swallows) we still record — re-trying
            // every 2 ms buys nothing, and the heartbeat will re-send
            // within a second anyway.
            entry.last_sent = if force_continuous { None } else { Some(to_send) };
            entry.last_sent_at = Some(now);
        }
    }
}

/// Classify a hidapi device instance path as a FlexInput-emulated (virtual)
/// HIDMaestro device vs a real controller.
///
/// HIDMaestro devices are root-enumerated, so their HID PDO path looks like
/// `\\?\HID#HIDCLASS#1&...` — note the `HIDCLASS` enumerator token and the
/// absence of a `VID_xxxx&PID_xxxx` hardware token. A real USB controller's
/// path is `\\?\HID#VID_054C&PID_0CE6&MI_03#...` and a Bluetooth one carries a
/// `{bth-guid}` / `BTHENUM` segment. We key on the positive virtual signal
/// (`HIDCLASS` without a `VID_` token) so a real device is never mis-flagged.
/// Ordered device-info list for the Nth-device addressing scheme used
/// throughout this module: the gamepad interface for `(vid, pid)`, with the
/// same primary/fallback selection as `open_device`. A free function over a
/// caller-held `&HidApi` so the I/O-thread readers (short lock on the shared
/// api) and the background refresh/classify worker (holds the lock across its
/// `refresh_devices()`) index into an identical ordering and can never drift.
pub(crate) fn gamepad_device_list_of(api: &HidApi, vid: u16, pid: u16) -> Vec<hidapi::DeviceInfo> {
    let kind_tag = classify(vid, pid);

    // Primary filter: usage_page + usage (correct, but returns 0 when HidHide
    // intercepts enumeration on Windows even for whitelisted apps).
    let mut paths: Vec<hidapi::DeviceInfo> = api
        .device_list()
        .filter(|d| {
            d.vendor_id() == vid
                && d.product_id() == pid
                && d.usage_page() == USAGE_PAGE_GENERIC_DESKTOP
                && d.usage() == USAGE_GAMEPAD
        })
        .cloned()
        .collect();

    // Fallback: if usage fields came back as 0 (HidHide / Windows quirk),
    // use known interface numbers for each controller instead.
    if paths.is_empty() {
        if let Some(kind_tag) = &kind_tag {
            let iface = preferred_interface(kind_tag);
            paths = api
                .device_list()
                .filter(|d| {
                    d.vendor_id() == vid
                        && d.product_id() == pid
                        && d.interface_number() == iface
                })
                .cloned()
                .collect();
        }
    }

    // Last resort: accept any interface with the right VID/PID (e.g. BT
    // connections that only expose a single interface).
    if paths.is_empty() {
        paths = api
            .device_list()
            .filter(|d| d.vendor_id() == vid && d.product_id() == pid)
            .cloned()
            .collect();
    }
    paths
}

/// Open the Nth physical `(vid, pid)` gamepad and run any per-device init —
/// the WORKER side of the open path ("hid-open" thread). The device list read
/// and `open_path` hold the shared api lock only briefly; the Switch Pro init
/// handshake (~600 ms worst case over Bluetooth: USB-probe timeout +
/// subcommand acks + four SPI calibration reads) runs with the lock RELEASED
/// so nothing else queues behind it. Returns `None` on open failure or a
/// failed handshake — callers treat both as a rate-limited retry.
fn open_and_init(api: &Arc<Mutex<HidApi>>, vid: u16, pid: u16, idx: usize) -> Option<OpenedDevice> {
    let kind_tag = classify(vid, pid)?; // bail early for non-PS/Switch VID/PID
    let device = {
        let guard = api.lock().ok()?;
        // Real devices only: our own virtual (ROOT-enumerated `HIDCLASS`/
        // `HIDMAESTRO` path) must never be a gyro/HID target. Excluding it here
        // — combined with the real-only index the gilrs walk produces (poll
        // skips is_virt) and lookup_phys — keeps the (vid,pid,idx) handle
        // selection stable even when a same-VID/PID virtual shares the bus,
        // where the raw list order is not. The virtual/real CLASSIFIER
        // (`classify_own_virtual`) still reads the full list.
        let paths: Vec<hidapi::DeviceInfo> = gamepad_device_list_of(&guard, vid, pid)
            .into_iter()
            .filter(|d| !instance_path_is_virtual(&d.path().to_string_lossy()))
            .collect();

        #[cfg(debug_assertions)]
        eprintln!("[gyro] open_and_init vid={:04X} pid={:04X} idx={} iface={} path={:?}",
            vid, pid, idx,
            paths.get(idx).map_or(-1, |p| p.interface_number()),
            paths.get(idx).map(|p| p.path()));

        let info = paths.get(idx)?;
        match guard.open_path(info.path()) {
            Ok(d) => d,
            Err(e) => {
                #[cfg(debug_assertions)]
                eprintln!("[gyro] open_path failed: {e}");
                return None;
            }
        }
    };
    device.set_blocking_mode(false).ok()?;

    // On Windows, DualSense/DS4 LED output (report 0x02) is only processed by
    // the firmware when sent to interface 0 — but that interface is owned
    // exclusively by the Windows HID class driver and cannot be opened from
    // userspace. All LED/lightbar control is therefore unavailable on Windows
    // unless a WinRT (Windows.Gaming.Input) path is implemented in the future.
    // Trigger effects and rumble go to interface 3 (the accessible IMU interface)
    // and the firmware processes those fields on that interface.
    let mut packet_counter = 0u8;
    let mut calib = None;
    if matches!(kind_tag, KindTag::SwitchPro) {
        // Run the handshake to completion HERE (lock released). A failed init
        // is a failed open: the entry never ships half-initialized, and the
        // rate-limited retry gets a clean attempt — the old lazy re-init that
        // ran on every read() from the I/O thread is gone.
        if !init_switch_pro(&device, &mut packet_counter, &mut calib) {
            return None;
        }
    }
    Some(OpenedDevice { device, kind_tag, calib, packet_counter })
}

/// True if the Nth physical device with this VID/PID is one of FlexInput's
/// OWN emulated HIDMaestro controllers, judged by its device instance path.
///
/// This is the discriminator that finally works where the name/uuid markers
/// failed: gilrs's WGI backend reports a generic name ("HID-compliant game
/// controller") and a nil uuid for both a real and an emulated same-VID/PID
/// pad, but the underlying HID **instance path** differs — a real controller
/// enumerates as `HID\VID_054C&PID_0CE6&MI_..` (USB) or under `BTHENUM`,
/// while a HIDMaestro device is ROOT-enumerated and appears as
/// `HID\HIDCLASS\..` (its path has no `VID_`/`PID_` tokens). `idx` is the
/// same Nth-device index the gilrs walk derives per VID/PID, so the two
/// stay correlated. Returns false for non-PS devices and out-of-range idx.
///
/// Reads the list as-is (no refresh) — the classify worker refreshes the
/// shared api first, then calls this against the same locked instance.
pub(crate) fn classify_own_virtual(api: &HidApi, vid: u16, pid: u16, idx: usize) -> bool {
    match gamepad_device_list_of(api, vid, pid).get(idx) {
        Some(info) => instance_path_is_virtual(&info.path().to_string_lossy()),
        None => false,
    }
}

fn instance_path_is_virtual(path: &str) -> bool {
    let up = path.to_ascii_uppercase();
    // Explicit HIDMaestro SWD form (belt-and-suspenders; the HID child usually
    // shows as HIDCLASS rather than SWD\HIDMAESTRO).
    if up.contains("HIDMAESTRO") {
        return true;
    }
    // Root-enumerated HID child: HIDCLASS enumerator and no real USB VID token.
    up.contains("HIDCLASS") && !up.contains("VID_")
}

fn hid_write(device: &HidDevice, data: &[u8]) {
    // A failed write is expected and benign when the device is unplugged mid-flush
    // (the flush loop keeps running until the next enumerate drops the handle), so
    // it's silently swallowed — logging here spammed once per write on disconnect.
    let _ = device.write(data);
}

// ── Switch Pro initialisation ─────────────────────────────────────────────────

fn init_switch_pro(device: &HidDevice, counter: &mut u8, calib: &mut Option<SwitchProCalib>) -> bool {
    let mut buf = [0u8; 64];

    // USB handshake (silently ignored / fails on BT — that's fine).
    let _ = device.write(&pad64([0x80, 0x02]));
    if let Ok(n) = device.read_timeout(&mut buf, 100) {
        if n > 0 && buf[0] == 0x81 {
            // USB confirmed: disable USB inactivity timeout.
            let _ = device.write(&pad64([0x80, 0x04]));
            let _ = device.read_timeout(&mut buf, 100);
        }
    }

    // Subcommand 0x48 0x01 — enable vibration. Without this, output report
    // 0x10 (rumble-only) is silently ignored even with valid encoded packets.
    if device.write(&subcommand(*counter, 0x48, &[0x01])).is_err() {
        return false;
    }
    *counter = counter.wrapping_add(1);
    let ack_vibration = wait_for_ack(device, 0x21, &mut buf);

    // Subcommand 0x40 0x01 — enable IMU.
    if device.write(&subcommand(*counter, 0x40, &[0x01])).is_err() {
        return false;
    }
    *counter = counter.wrapping_add(1);
    let ack_imu = wait_for_ack(device, 0x21, &mut buf);

    // Read stick calibration from SPI flash before switching to full report mode —
    // SPI reads come back in a 0x21 ack with the requested data, and that's harder
    // to filter from the firehose of 0x30 reports once full mode is enabled.
    //
    // Layout (dekuNukem/Nintendo_Switch_Reverse_Engineering spi_flash_notes.md):
    //   0x603D, 9 bytes : factory L stick (max,center,min) — 12-bit packed
    //   0x6046, 9 bytes : factory R stick (center,min,max) — 12-bit packed
    //   0x8010, 11 bytes: user L calibration (first 2 bytes are "magic", 0xA1B2 = valid)
    //   0x801B, 11 bytes: user R calibration (same magic format)
    let factory_l = spi_read(device, 0x6_03D, 9, counter, &mut buf);
    let factory_r = spi_read(device, 0x6_046, 9, counter, &mut buf);
    let user_l    = spi_read(device, 0x8_010, 11, counter, &mut buf);
    let user_r    = spi_read(device, 0x8_01B, 11, counter, &mut buf);

    // User cal overrides factory when its magic bytes are 0xB2 0xA1 (LE 0xA1B2).
    let l_data = match &user_l {
        Some(d) if d.len() >= 11 && d[0] == 0xB2 && d[1] == 0xA1 => Some(&d[2..11]),
        _ => factory_l.as_deref(),
    };
    let r_data = match &user_r {
        Some(d) if d.len() >= 11 && d[0] == 0xB2 && d[1] == 0xA1 => Some(&d[2..11]),
        _ => factory_r.as_deref(),
    };

    if let (Some(l), Some(r)) = (l_data, r_data) {
        if l.len() >= 9 && r.len() >= 9 {
            let (l_x_max, l_y_max) = unpack_12bit_pair(l, 0);
            let (l_x_ctr, l_y_ctr) = unpack_12bit_pair(l, 3);
            let (l_x_min, l_y_min) = unpack_12bit_pair(l, 6);
            let (r_x_ctr, r_y_ctr) = unpack_12bit_pair(r, 0);
            let (r_x_min, r_y_min) = unpack_12bit_pair(r, 3);
            let (r_x_max, r_y_max) = unpack_12bit_pair(r, 6);
            // SPI factory blob stores offsets relative to center for L's max/min and
            // R's min/max. Convert to absolute raw values for the simple normalize.
            *calib = Some(SwitchProCalib {
                l_x: AxisCalib { min: l_x_ctr.saturating_sub(l_x_min), center: l_x_ctr, max: l_x_ctr.saturating_add(l_x_max) },
                l_y: AxisCalib { min: l_y_ctr.saturating_sub(l_y_min), center: l_y_ctr, max: l_y_ctr.saturating_add(l_y_max) },
                r_x: AxisCalib { min: r_x_ctr.saturating_sub(r_x_min), center: r_x_ctr, max: r_x_ctr.saturating_add(r_x_max) },
                r_y: AxisCalib { min: r_y_ctr.saturating_sub(r_y_min), center: r_y_ctr, max: r_y_ctr.saturating_add(r_y_max) },
            });
        }
    }

    // Subcommand 0x03 0x30 — full input report mode (sends 0x30 with IMU).
    if device.write(&subcommand(*counter, 0x03, &[0x30])).is_err() {
        return false;
    }
    *counter = counter.wrapping_add(1);
    let ack_full_mode = wait_for_ack(device, 0x21, &mut buf);

    // ── Diagnostics: confirm the pad actually came up (the BT-before-launch
    // freeze) ────────────────────────────────────────────────────────────────
    // Suspicion: over a Bluetooth link that hasn't settled (the pad was paired
    // BEFORE FlexInput launched), the handshake subcommands are written but the
    // pad never processes them, so it stays in simple/no-report mode — "frozen"
    // until a reconnect forces a fresh link + init. Sample the report stream
    // right after init: 0x30 = full mode w/ IMU (healthy); 0x3F = simple mode
    // (buttons/sticks only, IMU dead); EMPTY = no reports at all (fully frozen).
    let cal_ok = calib.is_some();
    let mut post_ids: Vec<u8> = Vec::new();
    for _ in 0..8 {
        if let Ok(n) = device.read_timeout(&mut buf, 50) {
            if n > 0 { post_ids.push(buf[0]); }
        }
    }
    let full_reports = post_ids.iter().any(|&id| id == 0x30);
    if !(ack_vibration && ack_imu && ack_full_mode && cal_ok && full_reports) {
        eprintln!(
            "[gyro] Switch Pro init DEGRADED: ack(vibration={ack_vibration}, \
             imu={ack_imu}, full_mode={ack_full_mode}) cal={cal_ok} \
             post_init_report_ids={post_ids:02X?} — expected acks + 0x30 reports. \
             Empty ids = no data (fully frozen); all 0x3F = stuck in simple mode. \
             Likely a cold BT link at launch racing the handshake; reconnecting \
             the pad forces a fresh init."
        );
    } else {
        eprintln!("[gyro] Switch Pro init OK (all acks, cal present, 0x30 reports flowing)");
    }

    true
}

/// Wait for a subcommand acknowledgement (`expected_id`, normally 0x21).
/// Returns `true` if the ack arrived, `false` on timeout — the caller uses that
/// to diagnose a handshake that a cold Bluetooth link swallowed.
fn wait_for_ack(device: &HidDevice, expected_id: u8, buf: &mut [u8; 64]) -> bool {
    for _ in 0..15 {
        if let Ok(n) = device.read_timeout(buf, 50) {
            if n > 0 && buf[0] == expected_id {
                return true;
            }
        }
    }
    false
}

/// Issue an SPI flash read subcommand (0x10) at `addr` for `len` bytes.
/// Returns the payload bytes from the matching ack, or None on timeout / failure.
/// The reply is a 0x21 ack whose subcommand byte (offset 14) is 0x10 and whose
/// payload bytes 15..19 echo the address+length, followed by the data at byte 20.
fn spi_read(device: &HidDevice, addr: u32, len: u8, counter: &mut u8, buf: &mut [u8; 64]) -> Option<Vec<u8>> {
    let args = [
        (addr & 0xFF) as u8,
        ((addr >> 8) & 0xFF) as u8,
        ((addr >> 16) & 0xFF) as u8,
        ((addr >> 24) & 0xFF) as u8,
        len, 0, 0, 0,
    ];
    if device.write(&subcommand(*counter, 0x10, &args)).is_err() {
        return None;
    }
    *counter = counter.wrapping_add(1);
    for _ in 0..30 {
        if let Ok(n) = device.read_timeout(buf, 50) {
            if n >= (20 + len as usize) && buf[0] == 0x21 && buf[14] == 0x10
                && buf[15] == args[0] && buf[16] == args[1]
                && buf[17] == args[2] && buf[18] == args[3]
            {
                return Some(buf[20..20 + len as usize].to_vec());
            }
        }
    }
    None
}

/// Unpack two consecutive 12-bit little-endian values from a 3-byte SPI calibration triple.
/// Layout: [b0, b1, b2] → x = b0 | ((b1 & 0x0F) << 8), y = (b1 >> 4) | (b2 << 4).
fn unpack_12bit_pair(data: &[u8], off: usize) -> (u16, u16) {
    let b0 = data[off]     as u16;
    let b1 = data[off + 1] as u16;
    let b2 = data[off + 2] as u16;
    let x = b0 | ((b1 & 0x0F) << 8);
    let y = (b1 >> 4) | (b2 << 4);
    (x, y)
}

/// Normalize a raw 12-bit stick reading into [-1, 1] using per-axis calibration.
/// Asymmetric range (negative side and positive side may have different spans)
/// is handled by dividing each half independently — matches Joy-Con / Pro
/// firmware's own normalization.
fn normalize_axis(raw: u16, c: AxisCalib) -> f32 {
    let raw = raw as i32;
    let center = c.center as i32;
    let v = if raw >= center {
        let span = (c.max as i32 - center).max(1);
        (raw - center) as f32 / span as f32
    } else {
        let span = (center - c.min as i32).max(1);
        (raw - center) as f32 / span as f32
    };
    v.clamp(-1.0, 1.0)
}

/// Build a 64-byte padded Switch Pro output report (report ID 0x01).
fn subcommand(counter: u8, id: u8, args: &[u8]) -> [u8; 64] {
    let mut cmd = [0u8; 64];
    cmd[0] = 0x01; // output report ID
    cmd[1] = counter & 0x0F;
    // Neutral rumble data at bytes 2–9.
    cmd[2] = 0x00; cmd[3] = 0x01; cmd[4] = 0x40; cmd[5] = 0x40;
    cmd[6] = 0x00; cmd[7] = 0x01; cmd[8] = 0x40; cmd[9] = 0x40;
    cmd[10] = id;
    for (i, &b) in args.iter().enumerate() {
        if 11 + i < 64 { cmd[11 + i] = b; }
    }
    cmd
}

/// Pad a short slice into a 64-byte array (for 0x80 USB handshake reports).
fn pad64(prefix: impl AsRef<[u8]>) -> [u8; 64] {
    let mut buf = [0u8; 64];
    for (i, &b) in prefix.as_ref().iter().enumerate() {
        if i < 64 { buf[i] = b; }
    }
    buf
}

// ── Report reading ────────────────────────────────────────────────────────────

/// Returns false if the device has errored out (caller should drop the entry).
fn drain_reports(entry: &mut HidEntry) -> bool {
    let mut buf = [0u8; 128];
    loop {
        match entry.device.read(&mut buf) {
            Ok(0) => break,
            Err(_) => return false,
            Ok(n) => {
                if let Some(mut r) = parse_report(&buf[..n], &mut entry.kind) {
                    entry.event_count = entry.event_count.saturating_add(1);
                    // Touchpad hold-last: when a finger lifts, a real pad keeps
                    // bit 7 set but the X/Y bytes go stale/garbage (often reading
                    // ~max → normalized ≈ +1, a visible jump). Freeze the inactive
                    // finger at its last reported position instead so the pin
                    // doesn't snap — it stays where the finger left and reads 0
                    // before any touch (TouchPoint default). Active fingers pass
                    // through untouched.
                    if r.has_touchpad {
                        hold_inactive_touch(&mut r.touch1, &entry.last.touch1);
                        hold_inactive_touch(&mut r.touch2, &entry.last.touch2);
                    }
                    // Filter at the device-poll boundary. Each parsed `r`
                    // is one true device sample (no engine upsampling),
                    // so the snap-back rule sees a clean device-rate
                    // signal. Sticks / buttons / touchpad are NOT
                    // filtered — only the six IMU axes are touched.
                    entry.last = if entry.spike_enabled {
                        apply_spike_filter(entry, r)
                    } else {
                        // Filter off: pass through and reset state so a
                        // future enable doesn't reuse stale anchor.
                        entry.spike_anchor = None;
                        entry.spike_pending = None;
                        r
                    };
                    entry.last_report_at = std::time::Instant::now();
                    if entry.stalled {
                        entry.stalled = false;
                        eprintln!("[gyro] HID reports RESUMED (was a transient stall, not the permanent freeze)");
                    }
                }
            }
        }
    }
    true
}

/// Map 0..100 % sensitivity to a multiplier on the IMU noise floor used to
/// qualify a packet as an outlier relative to its neighbors. Log-spaced so
/// the full slider range produces meaningfully different behaviour:
///   sensitivity = 0     → multiplier = 20  (only catches obvious excursions)
///   sensitivity = 25    → multiplier ≈ 8.4
///   sensitivity = 50    → multiplier ≈ 4
///   sensitivity = 75    → multiplier ≈ 2
///   sensitivity = 100   → multiplier = 1   (sub-noise spikes)
fn spike_sensitivity_to_multiplier(sensitivity_pct: f32) -> f32 {
    let s = (sensitivity_pct / 100.0).clamp(0.0, 1.0);
    let log_hi = 1.0_f32.ln();   // 0
    let log_lo = 20.0_f32.ln();
    (log_lo + (log_hi - log_lo) * s).exp()
}

/// Per-axis outlier check on a (anchor, pending, arrival) triple.
///
/// Conceptual model: the "expected" trajectory at the pending packet is the
/// linear interpolation between its neighbors (= the midpoint, since they
/// are equally-spaced in time). If pending is far from that midpoint AND
/// its neighbors are mutually consistent (small delta between anchor and
/// arrival), pending is an outlier.
///
/// This is more robust than the original "pending must snap back to anchor"
/// rule, which failed during real motion because the anchor itself drifts
/// between packets. Here we compare pending to the LOCAL TRAJECTORY rather
/// than a fixed point.
///
/// Returns `pending` value with each axis individually replaced by the
/// midpoint when an outlier is detected.
fn apply_spike_filter(entry: &mut HidEntry, arrival: HidReading) -> HidReading {
    // Warmup: first packet — seed anchor only, return it unchanged.
    if entry.spike_anchor.is_none() {
        entry.spike_anchor = Some(arrival);
        return arrival;
    }
    // Second packet — establish pending. Emit the anchor while we wait
    // for the third packet to judge `pending`. (One-packet output delay.)
    if entry.spike_pending.is_none() {
        let anchor = entry.spike_anchor.unwrap();
        entry.spike_pending = Some(arrival);
        return anchor;
    }
    // Steady state: judge `pending` against `anchor` and `arrival`.
    let anchor  = entry.spike_anchor.unwrap();
    let pending = entry.spike_pending.unwrap();

    let multiplier = spike_sensitivity_to_multiplier(entry.spike_sensitivity);
    // Empirical floor calibrated to typical IMU quiescent jitter on the
    // normalized scale (HidReading values are pre-divided by reference
    // ranges, so flat-on-table jitter is on the order of 1e-3).
    const NOISE_FLOOR: f32 = 0.003;
    let dev_thresh = NOISE_FLOOR * multiplier;

    let judge = |a: f32, p: f32, n: f32| -> f32 {
        // Expected position at `p`: midpoint of its neighbors.
        let midpoint = 0.5 * (a + n);
        let deviation = (p - midpoint).abs();
        // Neighbor consistency: how much the signal drifted between
        // anchor and arrival across two packet intervals. If pending is
        // a real fast-motion sample, this gap is comparable to its own
        // deviation. If pending is an outlier, neighbors are still on
        // the smooth trajectory and `outer_gap` is much smaller.
        let outer_gap = (n - a).abs();

        // Outlier iff pending is well off the midpoint AND its
        // deviation is large relative to the neighbor-to-neighbor gap.
        // The 2× factor is empirical: real motion typically has
        // pending-to-midpoint roughly equal to outer_gap/2, so anything
        // > outer_gap is excess deviation.
        let was_spike = deviation > dev_thresh && deviation > outer_gap;

        if was_spike { midpoint } else { p }
    };

    let mut emit = pending;
    emit.gyro_x  = judge(anchor.gyro_x,  pending.gyro_x,  arrival.gyro_x);
    emit.gyro_y  = judge(anchor.gyro_y,  pending.gyro_y,  arrival.gyro_y);
    emit.gyro_z  = judge(anchor.gyro_z,  pending.gyro_z,  arrival.gyro_z);
    emit.accel_x = judge(anchor.accel_x, pending.accel_x, arrival.accel_x);
    emit.accel_y = judge(anchor.accel_y, pending.accel_y, arrival.accel_y);
    emit.accel_z = judge(anchor.accel_z, pending.accel_z, arrival.accel_z);

    // Advance: the value we just emitted becomes the new anchor; arrival
    // becomes the new pending. (Per-axis emit means the anchor we carry
    // forward is each axis's accepted value, not pending wholesale.)
    let mut next_anchor = anchor;
    next_anchor.gyro_x  = emit.gyro_x;
    next_anchor.gyro_y  = emit.gyro_y;
    next_anchor.gyro_z  = emit.gyro_z;
    next_anchor.accel_x = emit.accel_x;
    next_anchor.accel_y = emit.accel_y;
    next_anchor.accel_z = emit.accel_z;
    entry.spike_anchor  = Some(next_anchor);
    entry.spike_pending = Some(arrival);
    emit
}

fn parse_report(buf: &[u8], kind: &mut DeviceKind) -> Option<HidReading> {
    if buf.is_empty() { return None; }
    match kind {
        DeviceKind::Ds4 => parse_ds4(buf),
        DeviceKind::DualSense { connection, .. } => parse_dualsense(buf, connection),
        DeviceKind::SwitchPro { calib, .. } => parse_switch_pro(buf, calib.as_ref()),
    }
}

fn parse_ds4(buf: &[u8]) -> Option<HidReading> {
    // Layout reference: Linux drivers/hid/hid-sony.c, struct dualshock4_input_report_common.
    //   payload offsets: lx,ly(0,1) rx,ry(2,3) buttons[3](4-6) l2,r2(7,8)
    //                    timestamp(9,10) battery(11) gyro[3](12-17) accel[3](18-23)
    //   buttons[0] (payload 4): dpad(3:0) square(4) cross(5) circle(6) triangle(7)
    //   buttons[1] (payload 5): l1(0) r1(1) l2(2) r2(3) share(4) options(5) l3(6) r3(7)
    //   buttons[2] (payload 6): bit 0 = PS, bit 1 = Touchpad click, bits 2-7 = counter.
    // USB: report 0x01, payload starts at byte 1 → gyro 13, accel 19, buttons 5/6/7.
    // BT:  report 0x11, BT prefix is 2 bytes, payload starts at byte 3 → gyro 15, accel 21, buttons 7/8/9.
    let (po, go, ao) = match buf[0] {
        0x01 if buf.len() >= 25 => (1usize, 13, 19),
        0x11 if buf.len() >= 77 => (3usize, 15, 21),
        _ => return None,
    };
    let btn0 = buf[po + 4];
    let btn1 = buf[po + 5];
    let btn2 = buf[po + 6];
    let dpad = btn0 & 0x0F;

    // Populate the full input state from the raw report so the gilrs backend can
    // OVERRIDE gilrs's WGI axis/button mapping (which mis-orders PS-family axes,
    // landing L2/R2 where the right stick should be). DualSense already does this;
    // without it, our emulated DS4 read-back falls through to the broken WGI path.
    let ds = DualSenseState {
        lx:  (buf[po]     as f32 - 128.0) / 128.0,
        ly: -(buf[po + 1] as f32 - 128.0) / 128.0, // HID Y down → +Y up
        rx:  (buf[po + 2] as f32 - 128.0) / 128.0,
        ry: -(buf[po + 3] as f32 - 128.0) / 128.0,
        l2: buf[po + 7] as f32 / 255.0,
        r2: buf[po + 8] as f32 / 255.0,
        btn_west:    btn0 & 0x10 != 0, // Square
        btn_south:   btn0 & 0x20 != 0, // Cross
        btn_east:    btn0 & 0x40 != 0, // Circle
        btn_north:   btn0 & 0x80 != 0, // Triangle
        btn_l1:      btn1 & 0x01 != 0,
        btn_r1:      btn1 & 0x02 != 0,
        btn_l2:      btn1 & 0x04 != 0,
        btn_r2:      btn1 & 0x08 != 0,
        btn_create:  btn1 & 0x10 != 0, // Share
        btn_options: btn1 & 0x20 != 0,
        btn_ls:      btn1 & 0x40 != 0,
        btn_rs:      btn1 & 0x80 != 0,
        btn_ps:      btn2 & 0x01 != 0,
        dpad_up:    matches!(dpad, 0 | 1 | 7),
        dpad_right: matches!(dpad, 1 | 2 | 3),
        dpad_down:  matches!(dpad, 3 | 4 | 5),
        dpad_left:  matches!(dpad, 5 | 6 | 7),
    };

    let mut r = build(buf, go, ao, DS4_GYRO_DPS_PER_LSB, DS4_ACCEL_G_PER_LSB);
    // DS4 touchpad: the two `dualshock4_touch_point` records use the SAME 4-byte
    // packing as the DualSense (`[id/active, x_lo, (y_lo<<4)|x_hi, y_hi]`, 1920×1080
    // sensor), so `parse_dualsense_touch` decodes them directly. Offsets are
    // RID-inclusive: USB report 0x01 → fingers @35/@39 (matches the profile's
    // `touch_fingers`); BT report 0x11 → +2 for the BT prefix (@37/@41). Guard the
    // length separately from the IMU read (a short report still yields IMU).
    let (t1, t2) = match buf[0] {
        0x01 => (35usize, 39usize),
        0x11 => (37usize, 41usize),
        _    => (usize::MAX, usize::MAX),
    };
    if t2 != usize::MAX && buf.len() >= t2 + 4 {
        r.has_touchpad = true;
        r.touch1 = parse_dualsense_touch(buf, t1);
        r.touch2 = parse_dualsense_touch(buf, t2);
    }
    r.touchpad_click = btn2 & 0x02 != 0;
    // Battery: report byte 12 (USB) / 14 (BT, +2 prefix) — `po + 11`. Bounds-
    // checked; informational only (never feeds a virtual report).
    r.battery = buf.get(po + 11).map(|&b| decode_battery(b));
    r.dualsense      = Some(ds);
    Some(r)
}

fn parse_dualsense(buf: &[u8], connection: &mut Option<Connection>) -> Option<HidReading> {
    // Layout reference: Linux drivers/hid/hid-playstation.c, struct dualsense_input_report.
    // Payload offsets (relative to payload base po):
    //   lx(0) ly(1) rx(2) ry(3) l2(4) r2(5) seq(6)
    //   buttons[0](7): dpad(3:0) square(4) cross(5) circle(6) triangle(7)
    //   buttons[1](8): l1(0) r1(1) l2_dig(2) r2_dig(3) create(4) options(5) l3(6) r3(7)
    //   buttons[2](9): ps(0) touchpad(1) mute(2)
    //   reserved[4](11-14) gyro[3](15-20) accel[3](21-26)
    //   timestamp(27-30) reserved2(31) touch[2](32-39)
    // USB: report 0x01, payload base po=1. BT: report 0x31, payload base po=2.
    let (conn, po, go, ao, t1, t2) = match buf[0] {
        0x01 if buf.len() >= 41 => (Connection::Usb, 1usize, 16, 22, 33, 37),
        0x31 if buf.len() >= 79 => (Connection::Bt,  2usize, 17, 23, 34, 38),
        _ => return None,
    };
    *connection = Some(conn);

    let btn0 = buf[po + 7];
    let btn1 = buf[po + 8];
    let btn2 = buf[po + 9];

    // D-Pad: low nibble of buttons[0], 0-7 clockwise from north, 8=neutral.
    let dpad = btn0 & 0x0F;

    let ds = DualSenseState {
        lx:  (buf[po]     as f32 - 128.0) / 128.0,
        ly: -(buf[po + 1] as f32 - 128.0) / 128.0, // HID Y increases downward; invert to +Y=up
        rx:  (buf[po + 2] as f32 - 128.0) / 128.0,
        ry: -(buf[po + 3] as f32 - 128.0) / 128.0, // same
        l2: buf[po + 4] as f32 / 255.0,
        r2: buf[po + 5] as f32 / 255.0,
        btn_west:    btn0 & 0x10 != 0, // Square
        btn_south:   btn0 & 0x20 != 0, // Cross
        btn_east:    btn0 & 0x40 != 0, // Circle
        btn_north:   btn0 & 0x80 != 0, // Triangle
        btn_l1:      btn1 & 0x01 != 0,
        btn_r1:      btn1 & 0x02 != 0,
        btn_l2:      btn1 & 0x04 != 0,
        btn_r2:      btn1 & 0x08 != 0,
        btn_create:  btn1 & 0x10 != 0,
        btn_options: btn1 & 0x20 != 0,
        btn_ls:      btn1 & 0x40 != 0,
        btn_rs:      btn1 & 0x80 != 0,
        btn_ps:      btn2 & 0x01 != 0,
        dpad_up:    matches!(dpad, 0 | 1 | 7),
        dpad_right: matches!(dpad, 1 | 2 | 3),
        dpad_down:  matches!(dpad, 3 | 4 | 5),
        dpad_left:  matches!(dpad, 5 | 6 | 7),
    };

    let mut r = build(buf, go, ao, DS4_GYRO_DPS_PER_LSB, DS4_ACCEL_G_PER_LSB);
    r.has_touchpad    = true;
    r.touch1          = parse_dualsense_touch(buf, t1);
    r.touch2          = parse_dualsense_touch(buf, t2);
    r.touchpad_click  = btn2 & 0x02 != 0;
    r.mic_button      = btn2 & 0x04 != 0;
    // Battery: report byte 53 (USB) / 54 (BT, +1 for the BT prefix). Bounds-
    // checked separately from the parse guard above (a short report still yields
    // sticks/buttons without a battery reading).
    let bat_off = if conn == Connection::Usb { 53 } else { 54 };
    r.battery = buf.get(bat_off).map(|&b| decode_battery(b));
    r.dualsense       = Some(ds);
    Some(r)
}

/// Decode a Sony battery status byte into a 0.0..1.0 charge fraction. Both DS4
/// and DualSense pack the capacity in the low nibble (0..10 per hid-playstation)
/// and a charging status in the high nibble. We map capacity → percent with the
/// standard `min(level*10 + 5, 100)` and report charging-complete as full. This
/// is informational only (FlexInput's representation); it never feeds a virtual
/// pad's report — virtual pads always emit 100%.
fn decode_battery(b: u8) -> f32 {
    let level = (b & 0x0F) as f32;
    let status = (b >> 4) & 0x0F;
    // status 0x2 = charging complete on DualSense → full. Otherwise treat the
    // low nibble as a 0..10 capacity.
    if status == 0x2 {
        return 1.0;
    }
    ((level * 10.0 + 5.0) / 100.0).clamp(0.0, 1.0)
}

/// Freeze an inactive touch finger at the previous frame's position. A real pad
/// stops updating the X/Y bytes once a finger lifts (they go stale or read near
/// max → a visible jump to ≈+1); holding the last position keeps the downstream
/// pin steady where the finger left it. Before any touch, `prev` is the default
/// (0,0) so the pin rests at 0. Active fingers are left exactly as parsed.
fn hold_inactive_touch(cur: &mut TouchPoint, prev: &TouchPoint) {
    if !cur.active {
        cur.x = prev.x;
        cur.y = prev.y;
    }
}

/// Parse one DualSense `dualsense_touch_point` (4 bytes). Coordinates are
/// 12-bit (X 0..1919, Y 0..1079) and we normalise to roughly [-1, 1] with
/// the centre of the touchpad mapping to 0.
fn parse_dualsense_touch(buf: &[u8], off: usize) -> TouchPoint {
    if off + 4 > buf.len() { return TouchPoint::default(); }
    let contact = buf[off];
    let active = (contact & 0x80) == 0;
    let x_lo = buf[off + 1] as u16;
    let mid  = buf[off + 2] as u16;
    let y_hi = buf[off + 3] as u16;
    let raw_x = ((mid & 0x0F) << 8) | x_lo;
    let raw_y = (y_hi << 4) | ((mid & 0xF0) >> 4);
    // DualSense touchpad: 1920 × 1080 sensor area.
    const HALF_W: f32 = 1920.0 / 2.0;
    const HALF_H: f32 = 1080.0 / 2.0;
    TouchPoint {
        active,
        x:  (raw_x as f32 - HALF_W) / HALF_W,
        y: -(raw_y as f32 - HALF_H) / HALF_H, // touchpad Y increases downward; invert to +Y=up
    }
}

fn parse_switch_pro(buf: &[u8], calib: Option<&SwitchProCalib>) -> Option<HidReading> {
    // Report 0x30: standard input report with full button state and 3 IMU samples.
    // Layout (per dekuNukem/Nintendo_Switch_Reverse_Engineering bluetooth_hid_notes.md):
    //   byte 3 = right buttons:  bit0=Y, bit1=X, bit2=B, bit3=A, bit6=R, bit7=ZR
    //   byte 4 = shared:         bit0=Minus, bit1=Plus, bit2=RStick, bit3=LStick,
    //                            bit4=Home, bit5=Capture
    //   byte 5 = left buttons:   bit0=Down, bit1=Up, bit2=Right, bit3=Left, bit6=L, bit7=ZL
    //   bytes 6..9   = left analog stick  (3 bytes packed 12-bit X / 12-bit Y)
    //   bytes 9..12  = right analog stick (same packing)
    //   bytes 13/25/37 = three IMU samples [ax, ay, az, gx, gy, gz] i16 LE each.
    if buf[0] != 0x30 || buf.len() < 49 { return None; }

    let right  = buf[3];
    let shared = buf[4];
    let left   = buf[5];

    // Raw 12-bit stick values, unpacked from the same packed-pair layout as SPI.
    let (lx_raw, ly_raw) = unpack_12bit_pair(buf, 6);
    let (rx_raw, ry_raw) = unpack_12bit_pair(buf, 9);

    // Fallback identity calibration if SPI read failed: centered at 2048, half-range ~1500.
    // The center matches an uncalibrated 12-bit stick at rest; range is the firmware default.
    static FALLBACK: SwitchProCalib = SwitchProCalib {
        l_x: AxisCalib { min: 548, center: 2048, max: 3548 },
        l_y: AxisCalib { min: 548, center: 2048, max: 3548 },
        r_x: AxisCalib { min: 548, center: 2048, max: 3548 },
        r_y: AxisCalib { min: 548, center: 2048, max: 3548 },
    };
    let c = calib.unwrap_or(&FALLBACK);
    let lx = normalize_axis(lx_raw, c.l_x);
    let ly = normalize_axis(ly_raw, c.l_y);
    let rx = normalize_axis(rx_raw, c.r_x);
    let ry = normalize_axis(ry_raw, c.r_y);

    let switch_buttons = SwitchProButtons {
        btn_y:       right  & 0x01 != 0,
        btn_x:       right  & 0x02 != 0,
        btn_b:       right  & 0x04 != 0,
        btn_a:       right  & 0x08 != 0,
        btn_r:       right  & 0x40 != 0,
        btn_zr:      right  & 0x80 != 0,
        btn_minus:   shared & 0x01 != 0,
        btn_plus:    shared & 0x02 != 0,
        btn_rstick:  shared & 0x04 != 0,
        btn_lstick:  shared & 0x08 != 0,
        btn_home:    shared & 0x10 != 0,
        btn_capture: shared & 0x20 != 0,
        dpad_down:   left   & 0x01 != 0,
        dpad_up:     left   & 0x02 != 0,
        dpad_right:  left   & 0x04 != 0,
        dpad_left:   left   & 0x08 != 0,
        btn_l:       left   & 0x40 != 0,
        btn_zl:      left   & 0x80 != 0,
        lstick_x: lx, lstick_y: ly, rstick_x: rx, rstick_y: ry,
    };

    let (mut ax, mut ay, mut az) = (0i32, 0i32, 0i32);
    let (mut gx, mut gy, mut gz) = (0i32, 0i32, 0i32);
    for s in 0..3usize {
        let o = 13 + s * 12;
        ax += ri16(buf, o)      as i32;
        ay += ri16(buf, o + 2)  as i32;
        az += ri16(buf, o + 4)  as i32;
        gx += ri16(buf, o + 6)  as i32;
        gy += ri16(buf, o + 8)  as i32;
        gz += ri16(buf, o + 10) as i32;
    }
    let gs = SWITCH_GYRO_DPS_PER_LSB / GYRO_REF_DPS;
    let as_ = SWITCH_ACCEL_G_PER_LSB / ACCEL_REF_G;
    // Switch Pro IS the canonical reference (see `HidReading`): its raw report
    // already lands in (forward, side, vertical), so accel passes straight
    // through and the Sony parser is the one that permutes to match this.
    Some(HidReading {
        gyro_x:  (gx / 3) as f32 * gs,
        gyro_y: -(gy / 3) as f32 * gs,
        gyro_z: -(gz / 3) as f32 * gs,
        accel_x: (ax / 3) as f32 * as_,
        accel_y: (ay / 3) as f32 * as_,
        accel_z: (az / 3) as f32 * as_,
        switch_buttons: Some(switch_buttons),
        ..HidReading::default()
    })
}

fn build(buf: &[u8], gyro_off: usize, accel_off: usize, gyro_dps_per_lsb: f32, accel_g_per_lsb: f32) -> HidReading {
    let gs  = gyro_dps_per_lsb  / GYRO_REF_DPS;
    let as_ = accel_g_per_lsb   / ACCEL_REF_G;
    // Sony (DS4 + DualSense — same IMU mounting, one parser) → the canonical
    // frame documented on `HidReading`. Raw gyro order is (pitch, yaw, roll) at
    // offsets [0,+2,+4]; raw accel order is (side, vertical, fwd-tilt).
    //
    // Gyro maps to (roll, pitch, yaw) with roll/yaw negated so its polarity
    // matches the Switch Pro (verified against a DualSense; DS4 shares this
    // exact path and Sony mounting, so it follows but was not measured here).
    //
    // Accel is permuted AND signed to the canonical (forward, side, vertical)
    // frame — this is the axis swap that was wrong before: the raw side axis
    // fed accel_x while the gyro frame has roll (forward) on x, so accel and
    // gyro disagreed on every Sony pad. Now:
    //   accel_x (forward, + nose-up)      = -raw fwd-tilt
    //   accel_y (side,    + right-grip)   = -raw side
    //   accel_z (vertical,+ flat)         =  raw vertical
    HidReading {
        gyro_x: -ri16(buf, gyro_off + 4)  as f32 * gs,   // raw[2] roll, negated to match Switch Pro
        gyro_y:  ri16(buf, gyro_off)      as f32 * gs,   // raw[0] pitch
        gyro_z: -ri16(buf, gyro_off + 2)  as f32 * gs,   // raw[1] yaw, negated: right=positive
        accel_x: -ri16(buf, accel_off + 4) as f32 * as_, // -raw[2] fwd-tilt → forward, + nose-up
        accel_y: -ri16(buf, accel_off)     as f32 * as_, // -raw[0] side     → side, + right-grip-down
        accel_z:  ri16(buf, accel_off + 2) as f32 * as_, //  raw[1] vertical → vertical, + flat
        ..HidReading::default()
    }
}

fn ri16(buf: &[u8], off: usize) -> i16 {
    i16::from_le_bytes([buf[off], buf[off + 1]])
}

// ── Output report builders (USB only — BT requires CRC + extra wrapping) ──────

/// DS4 USB output report 0x05 (32 bytes incl. report ID).
/// Sets rumble (heavy/light motors) and lightbar RGB.
fn build_ds4_usb_out(out: &OutputState) -> [u8; 32] {
    let mut r = [0u8; 32];
    r[0] = 0x05;                // Report ID
    r[1] = 0x07;                // valid: bit0 = rumble, bit1 = lightbar, bit2 = flash
    r[2] = 0x04;                // packet type — must be 0x04
    r[4] = out.rumble_weak;     // small motor (high-freq / right)
    r[5] = out.rumble_strong;   // large motor (low-freq  / left)
    r[6] = out.lightbar_r;
    r[7] = out.lightbar_g;
    r[8] = out.lightbar_b;
    // bytes 9, 10: flash on/off durations — leave at 0 (steady)
    r
}

/// Switch Pro rumble-only output report 0x10 (49 bytes — we send 64 to be safe).
/// `left` / `right` are the encoded 4-byte rumble packets per side.
fn build_switch_rumble_only(counter: u8, left: [u8; 4], right: [u8; 4]) -> [u8; 64] {
    let mut r = [0u8; 64];
    r[0] = 0x10;                  // Report ID
    r[1] = counter & 0x0F;        // Sequence (low nibble)
    r[2..6].copy_from_slice(&left);
    r[6..10].copy_from_slice(&right);
    r
}

/// Encode amplitude + frequency into a 4-byte Switch Pro HD rumble packet.
///
///   amp  — vibration intensity 0.0–1.0 (0 = silent, 1 = max safe ~1003 units)
///   freq — normalized 0.0–1.0 mapping to 41–1253 Hz
///          (0.0=41 Hz, ~0.6=320 Hz default, 1.0=1253 Hz)
///
/// The kernel uses one shared frequency table (joycon_rumble_frequencies[]) for
/// both the HF field (bytes 0–1, u16 `high`) and the LF field (byte 2, u8 `low`).
/// A single freq input drives both bands simultaneously, which is how the Switch
/// OS and kernel driver work by default.
///
// joycon_rumble_frequencies[] from drivers/hid/hid-nintendo.c.
// Each entry: (high: u16, low: u8) for the given Hz. 160 entries, 41 Hz (index 0)
// → 1253 Hz (index 159). Index 96 = 320 Hz (firmware default). Module-level so both
// the single-carrier and dual-carrier encoders share the SAME validated table.
#[rustfmt::skip]
const SWITCH_FREQ_TABLE: &[(u16, u8)] = &[
        (0x0000,0x01),(0x0000,0x02),(0x0000,0x03),(0x0000,0x04),(0x0000,0x05),
        (0x0000,0x06),(0x0000,0x07),(0x0000,0x08),(0x0000,0x09),(0x0000,0x0a),
        (0x0000,0x0b),(0x0000,0x0c),(0x0000,0x0d),(0x0000,0x0e),(0x0000,0x0f),
        (0x0000,0x10),(0x0000,0x11),(0x0000,0x12),(0x0000,0x13),(0x0000,0x14),
        (0x0000,0x15),(0x0000,0x16),(0x0000,0x17),(0x0000,0x18),(0x0000,0x19),
        (0x0000,0x1a),(0x0000,0x1b),(0x0000,0x1c),(0x0000,0x1d),(0x0000,0x1e),
        (0x0000,0x1f),(0x0000,0x20),(0x0400,0x21),(0x0800,0x22),(0x0c00,0x23),
        (0x1000,0x24),(0x1400,0x25),(0x1800,0x26),(0x1c00,0x27),(0x2000,0x28),
        (0x2400,0x29),(0x2800,0x2a),(0x2c00,0x2b),(0x3000,0x2c),(0x3400,0x2d),
        (0x3800,0x2e),(0x3c00,0x2f),(0x4000,0x30),(0x4400,0x31),(0x4800,0x32),
        (0x4c00,0x33),(0x5000,0x34),(0x5400,0x35),(0x5800,0x36),(0x5c00,0x37),
        (0x6000,0x38),(0x6400,0x39),(0x6800,0x3a),(0x6c00,0x3b),(0x7000,0x3c),
        (0x7400,0x3d),(0x7800,0x3e),(0x7c00,0x3f),(0x8000,0x40),(0x8400,0x41),
        (0x8800,0x42),(0x8c00,0x43),(0x9000,0x44),(0x9400,0x45),(0x9800,0x46),
        (0x9c00,0x47),(0xa000,0x48),(0xa400,0x49),(0xa800,0x4a),(0xac00,0x4b),
        (0xb000,0x4c),(0xb400,0x4d),(0xb800,0x4e),(0xbc00,0x4f),(0xc000,0x50),
        (0xc400,0x51),(0xc800,0x52),(0xcc00,0x53),(0xd000,0x54),(0xd400,0x55),
        (0xd800,0x56),(0xdc00,0x57),(0xe000,0x58),(0xe400,0x59),(0xe800,0x5a),
        (0xec00,0x5b),(0xf000,0x5c),(0xf400,0x5d),(0xf800,0x5e),(0xfc00,0x5f),
        // index 96 = 320 Hz
        (0x0001,0x60),(0x0401,0x61),(0x0801,0x62),(0x0c01,0x63),(0x1001,0x64),
        (0x1401,0x65),(0x1801,0x66),(0x1c01,0x67),(0x2001,0x68),(0x2401,0x69),
        (0x2801,0x6a),(0x2c01,0x6b),(0x3001,0x6c),(0x3401,0x6d),(0x3801,0x6e),
        (0x3c01,0x6f),(0x4001,0x70),(0x4401,0x71),(0x4801,0x72),(0x4c01,0x73),
        (0x5001,0x74),(0x5401,0x75),(0x5801,0x76),(0x5c01,0x77),(0x6001,0x78),
        (0x6401,0x79),(0x6801,0x7a),(0x6c01,0x7b),(0x7001,0x7c),(0x7401,0x7d),
        (0x7801,0x7e),(0x7c01,0x7f),
        // index 128 = 640 Hz, low = 0x00 for these (above LF range)
        (0x8001,0x00),(0x8401,0x00),(0x8801,0x00),(0x8c01,0x00),(0x9001,0x00),
        (0x9401,0x00),(0x9801,0x00),(0x9c01,0x00),(0xa001,0x00),(0xa401,0x00),
        (0xa801,0x00),(0xac01,0x00),(0xb001,0x00),(0xb401,0x00),(0xb801,0x00),
        (0xbc01,0x00),(0xc001,0x00),(0xc401,0x00),(0xc801,0x00),(0xcc01,0x00),
        (0xd001,0x00),(0xd401,0x00),(0xd801,0x00),(0xdc01,0x00),(0xe001,0x00),
        (0xe401,0x00),(0xe801,0x00),(0xec01,0x00),(0xf001,0x00),(0xf401,0x00),
        (0xf801,0x00),(0xfc01,0x00), // index 159 = 1253 Hz
];
// joycon_rumble_amplitudes[] safe range (0–1003 units) from drivers/hid/hid-nintendo.c.
// (high: u8, low: u16) — added into frequency bytes per joycon_encode_rumble().
// 101 entries: index 0 = silent (0 units), index 100 = max safe (1003 units).
#[rustfmt::skip]
const SWITCH_AMP_TABLE: &[(u8, u16)] = &[
        (0x00,0x0040),
        (0x02,0x8040),(0x04,0x0041),(0x06,0x8041),(0x08,0x0042),(0x0a,0x8042),
        (0x0c,0x0043),(0x0e,0x8043),(0x10,0x0044),(0x12,0x8044),(0x14,0x0045),
        (0x16,0x8045),(0x18,0x0046),(0x1a,0x8046),(0x1c,0x0047),(0x1e,0x8047),
        (0x20,0x0048),(0x22,0x8048),(0x24,0x0049),(0x26,0x8049),(0x28,0x004a),
        (0x2a,0x804a),(0x2c,0x004b),(0x2e,0x804b),(0x30,0x004c),(0x32,0x804c),
        (0x34,0x004d),(0x36,0x804d),(0x38,0x004e),(0x3a,0x804e),(0x3c,0x004f),
        (0x3e,0x804f),(0x40,0x0050),(0x42,0x8050),(0x44,0x0051),(0x46,0x8051),
        (0x48,0x0052),(0x4a,0x8052),(0x4c,0x0053),(0x4e,0x8053),(0x50,0x0054),
        (0x52,0x8054),(0x54,0x0055),(0x56,0x8055),(0x58,0x0056),(0x5a,0x8056),
        (0x5c,0x0057),(0x5e,0x8057),(0x60,0x0058),(0x62,0x8058),(0x64,0x0059),
        (0x66,0x8059),(0x68,0x005a),(0x6a,0x805a),(0x6c,0x005b),(0x6e,0x805b),
        (0x70,0x005c),(0x72,0x805c),(0x74,0x005d),(0x76,0x805d),(0x78,0x005e),
        (0x7a,0x805e),(0x7c,0x005f),(0x7e,0x805f),(0x80,0x0060),(0x82,0x8060),
        (0x84,0x0061),(0x86,0x8061),(0x88,0x0062),(0x8a,0x8062),(0x8c,0x0063),
        (0x8e,0x8063),(0x90,0x0064),(0x92,0x8064),(0x94,0x0065),(0x96,0x8065),
        (0x98,0x0066),(0x9a,0x8066),(0x9c,0x0067),(0x9e,0x8067),(0xa0,0x0068),
        (0xa2,0x8068),(0xa4,0x0069),(0xa6,0x8069),(0xa8,0x006a),(0xaa,0x806a),
        (0xac,0x006b),(0xae,0x806b),(0xb0,0x006c),(0xb2,0x806c),(0xb4,0x006d),
        (0xb6,0x806d),(0xb8,0x006e),(0xba,0x806e),(0xbc,0x006f),(0xbe,0x806f),
        (0xc0,0x0070),(0xc2,0x8070),(0xc4,0x0071),(0xc6,0x8071),(0xc8,0x0072),
];

/// Look up the proven Switch FREQ/AMP table fields for one (amp, freq) carrier.
/// Returns `(fh, fl, ah, al)`: `fh`(u16)/`fl`(u8) the frequency fields, `ah`(u8)/
/// `al`(u16) the amplitude fields, exactly as the Linux joycon_encode_rumble does
/// (this is the encoding that's actually FELT on real pads). The dual encoder uses
/// the HF carrier's (fh, ah) for bytes 0/1 and the LF carrier's (fl, al) for bytes
/// 2/3 — both via this same validated table.
fn switch_carrier_fields(amp: f32, freq: f32) -> (u16, u8, u8, u16) {
    // FREQ_TABLE valid indices 0–158. Safe range 32–127 (~82–626 Hz) — both HF and
    // LF fields non-zero. The fh field is NOT linear (wraps at 95→96), so it's an
    // opaque lookup key — never interpolate between entries.
    const FREQ_LO: usize = 32;  // ~82 Hz
    const FREQ_HI: usize = 127; // ~626 Hz
    let f_idx = ((FREQ_LO as f32 + freq.clamp(0.0, 1.0) * (FREQ_HI - FREQ_LO) as f32)
        .round() as usize)
        .clamp(FREQ_LO, FREQ_HI);
    let (fh, fl) = SWITCH_FREQ_TABLE[f_idx];
    // Perceptual amplitude: 0–1 → power-law (exp 1.8); skip index 0 for any non-zero
    // input so amp responds from the very start.
    let amp_c = amp.clamp(0.0, 1.0);
    let a_idx = if amp_c == 0.0 {
        0
    } else {
        let linear = amp_c.powf(1.8);
        (1 + (linear * (SWITCH_AMP_TABLE.len() - 2) as f32).round() as usize)
            .min(SWITCH_AMP_TABLE.len() - 1)
    };
    let (ah, al) = SWITCH_AMP_TABLE[a_idx];
    (fh, fl, ah, al)
}

/// Reference single-carrier encoder (Linux joycon_encode_rumble): drives both the
/// HF and LF amplitude fields from one (amp, freq). Kept for tests / legacy callers.
#[allow(dead_code)]
fn switch_rumble_encode(amp: f32, freq: f32) -> [u8; 4] {
    let (fh, fl, ah, al) = switch_carrier_fields(amp, freq);
    [
        ((fh >> 8) & 0xFF) as u8,
        ((fh & 0xFF) as u8).wrapping_add(ah),
        fl.wrapping_add(((al >> 8) & 0xFF) as u8),
        (al & 0xFF) as u8,
    ]
}

/// Map a band's 0..1 freq onto a FREQ_TABLE index window, returning the table's
/// normalized 0..1 freq for [`switch_carrier_fields`]. Keeps both carriers inside
/// the LRA's felt range (the actuators resonate ~150–200 Hz and roll off above
/// ~300 Hz): LF → idx 32..=64 (~82–180 Hz), HF → idx 64..=96 (~180–320 Hz).
fn band_freq_norm(freq01: f32, high_band: bool) -> f32 {
    let (lo, hi) = if high_band { (64.0, 96.0) } else { (32.0, 64.0) };
    let idx = lo + freq01.clamp(0.0, 1.0) * (hi - lo);
    ((idx - 32.0) / (127.0 - 32.0)).clamp(0.0, 1.0)
}

/// AM mod-rate window (Hz) the HF "carrier" maps onto when used as an amplitude
/// modulator for the LF rumble. A 40–150 Hz flutter on the LF amplitude reads as
/// "texture/sizzle" even though the felt carrier stays low.
const AM_RATE_MIN_HZ: f32 = 40.0;
const AM_RATE_MAX_HZ: f32 = 150.0;

/// Encode a Switch Pro HD rumble packet for one actuator.
///
/// The Switch packet's "HF carrier" doesn't produce a felt high-frequency tone on
/// these (clone-LRA) pads — verified live across several attempts. So instead of a
/// second carrier, we synthesize HF *texture* by AMPLITUDE-MODULATING the single
/// felt LF carrier: the LF rumble's amplitude pulses at a rate set by the HF
/// content, which the body perceives as roughness/sizzle.
///   * `lf_amp` / `lf_freq` — the base carrier (its frequency is what's felt).
///   * `mod_depth` — AM modulation DEPTH (0 = steady rumble, 1 = full pulse to ~0).
///   * `mod_rate` — AM modulation RATE, mapped onto [AM_RATE_MIN_HZ, AM_RATE_MAX_HZ].
///   * `mod_phase` — current modulator phase in RADIANS (advanced by the caller
///     from a monotonic clock so the flutter is smooth regardless of packet timing).
///
/// The emitted packet is a SINGLE carrier (LF) whose amplitude = lf_amp * AM
/// envelope. Bytes 0/1 hold the LF freq high/low + amp high; bytes 2/3 the LF
/// freq low field + amp low — exactly the proven [`switch_carrier_fields`] layout
/// (the encoding that's actually felt).
fn switch_rumble_encode_am(
    lf_amp: f32, lf_freq: f32, mod_depth: f32, mod_rate: f32, mod_phase: f32,
) -> [u8; 4] {
    let lf_a = lf_amp.clamp(0.0, 1.0);
    let depth = mod_depth.clamp(0.0, 1.0);

    // AM envelope: 1.0 at the wave peak, (1 - depth) at the trough. |sin| gives a
    // unipolar pulse so the rumble never inverts, only dips. mod_rate selects the
    // flutter frequency; the actual sin() uses the phase the caller advances.
    let env = if depth > 0.0 {
        let s = mod_phase.sin().abs();      // 0..1
        (1.0 - depth) + depth * s           // (1-depth)..1
    } else {
        1.0
    };
    let _ = mod_rate; // rate is applied by the caller when advancing mod_phase
    let amp = (lf_a * env).clamp(0.0, 1.0);

    // Single felt carrier at the LF band frequency, amplitude-modulated.
    let (fh, fl, ah, al) = switch_carrier_fields(amp, band_freq_norm(lf_freq, false));
    [
        ((fh >> 8) & 0xFF) as u8,
        ((fh & 0xFF) as u8).wrapping_add(ah),
        fl.wrapping_add(((al >> 8) & 0xFF) as u8),
        (al & 0xFF) as u8,
    ]
}

/// Map the HF "mod rate" 0..1 control to an angular increment (radians per second)
/// for the AM phase. Used by the Switch sink to advance `mod_phase` from wall time.
fn am_rate_rad_per_sec(mod_rate01: f32) -> f32 {
    let hz = AM_RATE_MIN_HZ + mod_rate01.clamp(0.0, 1.0) * (AM_RATE_MAX_HZ - AM_RATE_MIN_HZ);
    hz * std::f32::consts::TAU
}

/// Encode one DualSense adaptive trigger effect into 11 bytes:
/// byte[0] = mode byte, bytes[1..10] = effect params.
///
/// mode: 0=Off(0x05), 1=Feedback(0x21), 2=Weapon(0x25), 3=Vibration(0x26)
/// start/end: zone 0–9 along trigger travel (0=rest, 9=fully pressed)
/// strength: force 0–7
/// freq: oscillation 0–255 (Vibration mode only)
///
/// Bit-packing derived from MysteriousJ/Joystick-Input-Examples and confirmed
/// against Linux hid-playstation.c trigger effect structs.
fn encode_trigger_effect(mode: u8, start: u8, end: u8, strength: u8, freq: u8) -> [u8; 11] {
    let mut p = [0u8; 11];
    match mode {
        1 => {
            // Feedback (0x21): constant resistance from start zone onwards.
            p[0] = 0x21;
            let s = (start.min(9)) as u16;
            let f = (strength.min(7)) as u32;
            // active_zones: bits s..9 set (10-bit field, LSB-first across 2 bytes)
            let active: u16 = if s < 10 { ((1u16 << (10 - s)) - 1) << s } else { 0 };
            // force_zones: 3-bit force value repeated for each active zone (10 zones × 3 bits)
            let mut force = 0u32;
            for z in s..10 { force |= f << (z * 3); }
            p[1] = (active & 0xFF) as u8;
            p[2] = ((active >> 8) & 0xFF) as u8;
            p[3] = (force & 0xFF) as u8;
            p[4] = ((force >> 8) & 0xFF) as u8;
            p[5] = ((force >> 16) & 0xFF) as u8;
            p[6] = ((force >> 24) & 0xFF) as u8;
        }
        2 => {
            // Weapon (0x25): hard resistance between start and end, then releases.
            p[0] = 0x25;
            let s = (start.clamp(2, 7)) as u16;
            let e = (end.max(start.saturating_add(1)).clamp(3, 8)) as u16;
            let start_end: u16 = (1u16 << s) | (1u16 << e);
            p[1] = (start_end & 0xFF) as u8;
            p[2] = ((start_end >> 8) & 0xFF) as u8;
            p[3] = strength.min(7);
        }
        3 => {
            // Vibration (0x26): oscillating resistance in a zone range.
            p[0] = 0x26;
            let s = (start.min(9)) as u16;
            let f = (strength.min(7)) as u32;
            let active: u16 = if s < 10 { ((1u16 << (10 - s)) - 1) << s } else { 0 };
            let mut force = 0u32;
            for z in s..10 { force |= f << (z * 3); }
            p[1] = (active & 0xFF) as u8;
            p[2] = ((active >> 8) & 0xFF) as u8;
            p[3] = (force & 0xFF) as u8;
            p[4] = ((force >> 8) & 0xFF) as u8;
            p[5] = ((force >> 16) & 0xFF) as u8;
            p[6] = ((force >> 24) & 0xFF) as u8;
            p[9] = freq;
        }
        _ => {
            // Off (0x05) — deactivate any effect.
            p[0] = 0x05;
        }
    }
    p
}

/// Map player_led index (0–4) to the DualSense 5-bit LED bitmask.
fn player_led_mask(idx: u8) -> u8 {
    match idx {
        1 => 0x04,
        2 => 0x0A,
        3 => 0x15,
        4 => 0x1B,
        _ => 0x00, // off
    }
}

/// DualSense USB output report 0x02 (63 bytes incl. report ID).
/// Layout reference: Linux drivers/hid/hid-playstation.c, struct
/// `dualsense_output_report_common` (47 bytes) sits at buffer offset 1.
///
/// Valid-flag values mirror pydualsense / DS4Windows: enable everything except
/// One-shot lightbar init report: sends LIGHTBAR_SETUP=LIGHT_ON so the firmware
/// DualSense USB output report 0x02.
fn build_dualsense_usb_out(out: &OutputState) -> [u8; 63] {
    // Layout: Linux hid-playstation.c dualsense_output_report_common (47 bytes, __packed).
    // Report ID 0x02 at buf[0]; common struct starts at buf[1] (struct offset + 1).
    //
    // buf[ 0] = 0x02  report_id
    // buf[ 1] = valid_flag0  bit0=compatible_vibration, bit1=haptics_select
    // buf[ 2] = valid_flag1  bit0=mic_mute_led, bit1=power_save, bit2=lightbar,
    //                        bit3=RELEASE_LEDS (must be 0!), bit4=player_indicator
    // buf[ 3] = motor_right  (rumble weak)
    // buf[ 4] = motor_left   (rumble strong)
    // buf[ 5-8]  audio (leave 0)
    // buf[ 9] = mute_button_led
    // buf[10] = power_save_control
    // buf[11-37] = reserved2[27] — trigger effects placed here (non-kernel extension)
    // buf[38] = audio_control2
    // buf[39] = valid_flag2   bit1=lightbar_setup_control_enable, bit2=compat_vibration2
    // buf[40-41] = reserved3[2]
    // buf[42] = lightbar_setup  (0x02 = lightbar_on)
    // buf[43] = led_brightness  (0x02 = bright)
    // buf[44] = player_leds bitmask
    // buf[45] = lightbar_red
    // buf[46] = lightbar_green
    // buf[47] = lightbar_blue
    // buf[48-62] = padding
    // Offsets match DualSense-Windows DS5_Output.cpp (their buffer has no report ID,
    // so their 0x00 = our r[1], their 0x2C = our r[45], etc.)
    let mut r = [0u8; 63];
    r[0] = 0x02;
    fill_dualsense_common(&mut r[1..48], out);
    r
}

/// DualSense Bluetooth output report 0x31 (78 bytes incl. report ID).
/// Wraps the same 47-byte common struct as USB, prefixed with seq_tag + tag
/// and signed with a CRC32 over the report contents (excluding the 4 CRC bytes).
/// Without the CRC the firmware silently drops the packet.
fn build_dualsense_bt_out(out: &OutputState, seq: u8) -> [u8; 78] {
    let mut r = [0u8; 78];
    r[0] = 0x31;                       // report id
    r[1] = (seq & 0x0F) << 4;          // seq_tag: high nibble = sequence, low nibble = 0
    r[2] = 0x10;                       // DS_OUTPUT_TAG
    fill_dualsense_common(&mut r[3..50], out);
    // bytes 50..73 = reserved (zero)
    let crc = dualsense_bt_crc32(&r[..74]);
    r[74..78].copy_from_slice(&crc.to_le_bytes());
    r
}

/// Populate the 47-byte `dualsense_output_report_common` struct shared by USB
/// (offset +1) and BT (offset +3). Pure function so both transports stay in sync.
fn fill_dualsense_common(dst: &mut [u8], out: &OutputState) {
    debug_assert_eq!(dst.len(), 47);
    // +0 valid_flag0:
    //   bit0 COMPATIBLE_VIBRATION  — enables motor_left/motor_right (classic rumble)
    //   bit1 HAPTICS_SELECT        — routes audio ch3/ch4 to LRA actuators
    //   bit5 SPEAKER_VOLUME_ENABLE
    //   bit6 MIC_VOLUME_ENABLE
    //   bit7 AUDIO_CONTROL_ENABLE
    dst[0] = 0xFF;
    // +1 valid_flag1:
    //   bit0 MIC_MUTE_LED_CONTROL_ENABLE
    //   bit1 POWER_SAVE_CONTROL_ENABLE
    //   bit2 LIGHTBAR_CONTROL_ENABLE
    //   bit3 RELEASE_LEDS  — must be 0
    //   bit4 PLAYER_INDICATOR_CONTROL_ENABLE
    //   bit7 AUDIO_CONTROL2_ENABLE
    dst[1] = 0xF7;
    // +2 motor_right (weak / high-freq), +3 motor_left (strong / low-freq)
    dst[2] = out.rumble_weak;
    dst[3] = out.rumble_strong;
    // +8 mute_button_led
    dst[8] = out.mic_led.min(2);
    // +10..+36 reserved2[27] — trigger effect blobs are placed here.
    // Adaptive trigger effect encodings live at dst[10..21] (right) and dst[21..32] (left).
    let rt = encode_trigger_effect(
        out.trigger_r_mode, out.trigger_r_start,
        out.trigger_r_end,  out.trigger_r_strength, out.trigger_r_freq,
    );
    let lt = encode_trigger_effect(
        out.trigger_l_mode, out.trigger_l_start,
        out.trigger_l_end,  out.trigger_l_strength, out.trigger_l_freq,
    );
    dst[10..21].copy_from_slice(&rt);
    dst[21..32].copy_from_slice(&lt);
    // +38 valid_flag2:
    //   bit1 LIGHTBAR_SETUP_CONTROL_ENABLE
    //   bit2 COMPATIBLE_VIBRATION2 — newer firmware requires this alongside FLAG0 bit0
    dst[38] = 0x03;
    // +41 lightbar_setup (LIGHT_OUT), +42 led_brightness (0=firmware default),
    // +43 player_leds bitmask, +44..+46 RGB
    dst[41] = 0x02;
    dst[42] = 0x00;
    dst[43] = player_led_mask(out.player_led);
    dst[44] = out.lightbar_r;
    dst[45] = out.lightbar_g;
    dst[46] = out.lightbar_b;
}

/// CRC32 used by DualSense Bluetooth output reports.
/// Equivalent to `~crc32_le(crc32_le(0xFFFFFFFF, &0xA2, 1), data, len)`
/// from `drivers/hid/hid-playstation.c`. Polynomial 0xEDB88320 (reflected IEEE).
fn dualsense_bt_crc32(data: &[u8]) -> u32 {
    const SEED: u8 = 0xA2;
    let mut crc = crc32_le_update(0xFFFFFFFF, &[SEED]);
    crc = crc32_le_update(crc, data);
    !crc
}

fn crc32_le_update(mut crc: u32, data: &[u8]) -> u32 {
    const POLY: u32 = 0xEDB88320;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (POLY & 0u32.wrapping_sub(crc & 1));
        }
    }
    crc
}

// ── VID/PID classification ─────────────────────────────────────────────────────

enum KindTag { Ds4, DualSense, SwitchPro }

fn classify(vid: u16, pid: u16) -> Option<KindTag> {
    match vid {
        SONY_VID   if DS4_PIDS.contains(&pid)      => Some(KindTag::Ds4),
        SONY_VID   if DUALSENSE_PIDS.contains(&pid) => Some(KindTag::DualSense),
        SWITCH_VID if pid == SWITCH_PRO_PID      => Some(KindTag::SwitchPro),
        _ => None,
    }
}

// Windows HID interface numbers for the main gamepad interface when usage_page
// fields aren't available (e.g. HidHide intercepts enumeration).
// DualSense/DS4 USB: interface 3 carries input reports with IMU data.
// Switch Pro USB:    interface 0.
// BT connections expose a single interface (0) for all three controllers.
fn preferred_interface(kind: &KindTag) -> i32 {
    match kind {
        KindTag::Ds4 | KindTag::DualSense => 3,
        KindTag::SwitchPro => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a Sony HID buffer carrying raw accel `(r0 side, r1 vertical,
    /// r2 fwd-tilt)` and return the canonical accel `build` produces.
    fn sony_canonical_accel(r_side: i16, r_vert: i16, r_fwd: i16) -> (f32, f32, f32) {
        let mut buf = [0u8; 16];
        // accel_off = 6, three LE i16 at [0]=side, [+2]=vertical, [+4]=fwd-tilt.
        buf[6..8].copy_from_slice(&r_side.to_le_bytes());
        buf[8..10].copy_from_slice(&r_vert.to_le_bytes());
        buf[10..12].copy_from_slice(&r_fwd.to_le_bytes());
        let r = build(&buf, 0, 6, DS4_GYRO_DPS_PER_LSB, DS4_ACCEL_G_PER_LSB);
        (r.accel_x, r.accel_y, r.accel_z)
    }

    /// The Sony accel permutation must land in the canonical frame documented on
    /// `HidReading`, matching what the Switch Pro emits raw. Raw physical signs
    /// come from a DualSense measurement: raw side + = left grip down,
    /// raw vertical + = flat, raw fwd-tilt + = USB down.
    #[test]
    fn sony_accel_maps_to_canonical_frame() {
        // Flat, face up: only vertical → canonical +Z.
        let (x, y, z) = sony_canonical_accel(0, 8000, 0);
        assert!(x.abs() < 1e-3 && y.abs() < 1e-3 && z > 0.0, "flat → +Z, got ({x},{y},{z})");

        // Nose (USB) up: raw fwd-tilt negative → canonical +X (forward, nose-up).
        let (x, y, z) = sony_canonical_accel(0, 0, -8000);
        assert!(x > 0.0 && y.abs() < 1e-3 && z.abs() < 1e-3, "nose-up → +X, got ({x},{y},{z})");

        // Right grip down: raw side negative (side + is LEFT grip) → canonical
        // +Y (side, + right-grip-down).
        let (x, y, z) = sony_canonical_accel(-8000, 0, 0);
        assert!(y > 0.0 && x.abs() < 1e-3 && z.abs() < 1e-3, "right-grip-down → +Y, got ({x},{y},{z})");
    }

    #[test]
    fn am_full_amp_at_phase_peak() {
        // At the modulation peak (|sin| = 1, phase = π/2), ANY mod depth leaves the LF
        // amplitude at full — the envelope tops out at 1.0. So a full-depth packet at
        // the peak must equal the unmodulated (depth 0) packet.
        let peak = std::f32::consts::FRAC_PI_2;
        let unmod = switch_rumble_encode_am(1.0, 0.4, 0.0, 0.5, peak);
        let modd  = switch_rumble_encode_am(1.0, 0.4, 1.0, 0.5, peak);
        assert_eq!(unmod, modd, "AM peak must equal unmodulated: unmod={unmod:?} mod={modd:?}");
    }

    #[test]
    fn am_dips_amplitude_at_phase_trough() {
        // At the modulation trough (sin = 0, phase = 0) with full depth, the envelope
        // is (1 - depth) = 0 → the LF amplitude collapses to silence. So byte3 (LF amp
        // low) must drop well below the unmodulated peak.
        let trough = 0.0;
        let peak = std::f32::consts::FRAC_PI_2;
        let at_peak   = switch_rumble_encode_am(1.0, 0.4, 1.0, 0.5, peak);
        let at_trough = switch_rumble_encode_am(1.0, 0.4, 1.0, 0.5, trough);
        assert!(at_trough[3] < at_peak[3],
            "full-depth trough must dip amplitude vs peak: peak={at_peak:?} trough={at_trough:?}");
    }

    #[test]
    fn am_depth_zero_is_steady() {
        // With mod depth 0 the envelope is always 1.0, so the packet is independent of
        // phase — a steady rumble, regardless of where in the cycle we sample.
        let a = switch_rumble_encode_am(0.7, 0.5, 0.0, 0.5, 0.0);
        let b = switch_rumble_encode_am(0.7, 0.5, 0.0, 0.5, 1.234);
        let c = switch_rumble_encode_am(0.7, 0.5, 0.0, 0.5, 3.1);
        assert_eq!(a, b);
        assert_eq!(a, c);
    }

    #[test]
    fn am_rate_maps_into_flutter_window() {
        // The HF "mod rate" control maps 0..1 onto the AM_RATE_MIN/MAX Hz window.
        let tau = std::f32::consts::TAU;
        assert!((am_rate_rad_per_sec(0.0) - AM_RATE_MIN_HZ * tau).abs() < 1.0);
        assert!((am_rate_rad_per_sec(1.0) - AM_RATE_MAX_HZ * tau).abs() < 1.0);
        assert!(am_rate_rad_per_sec(1.0) > am_rate_rad_per_sec(0.0));
    }

    #[test]
    fn silent_lf_is_silent_regardless_of_modulation() {
        // No base rumble → modulating nothing is still nothing: the packet must match
        // the unmodulated silent packet at every phase (the AM envelope multiplies a
        // zero amplitude, which stays zero).
        let unmod_silent = switch_rumble_encode_am(0.0, 0.5, 0.0, 0.5, 0.0);
        for phase in [0.0, std::f32::consts::FRAC_PI_2, 1.234, 3.1] {
            let p = switch_rumble_encode_am(0.0, 0.5, 1.0, 0.5, phase);
            assert_eq!(p, unmod_silent, "silent LF must stay silent under AM @{phase}: {p:?}");
        }
    }

    // parse_ds4 must populate `dualsense` so the gilrs backend's raw-HID override
    // fires for DS4 the same way it does for DualSense. Without it, our emulated
    // DS4 read-back falls through to gilrs's WGI axis mapping, which mis-orders the
    // PS-family axes (L2/R2 land where the right stick should be) and scrambles the
    // shoulder/menu/stick-click buttons. Regression guard for that asymmetry.
    #[test]
    fn parse_ds4_populates_override_state() {
        // USB report 0x01, payload base 1. Build a 25-byte report:
        //   [1]=LX [2]=LY [3]=RX [4]=RY [5]=btn0 [6]=btn1 [7]=btn2 [8]=L2 [9]=R2
        let mut buf = [0u8; 25];
        buf[0] = 0x01;
        buf[1] = 255;   // LX full right → +1
        buf[2] = 128;   // LY center
        buf[3] = 128;   // RX center
        buf[4] = 0;     // RY raw 0 → +1 after inversion (HID up)
        buf[5] = 0x20 | 0x08; // btn0: Cross (bit5) + dpad nibble 8 (neutral)
        buf[6] = 0x04 | 0x20; // btn1: L2 digital (bit2) + Options (bit5)
        buf[7] = 0x01;  // btn2: PS
        buf[8] = 255;   // L2 analog full
        buf[9] = 0;     // R2 analog

        let r = parse_ds4(&buf).expect("valid DS4 report");
        let ds = r.dualsense.expect("DS4 must populate dualsense override state");
        assert!((ds.lx - 1.0).abs() < 0.02, "LX full right, got {}", ds.lx);
        assert!((ds.ly - 0.0).abs() < 0.02, "LY center, got {}", ds.ly);
        assert!((ds.ry - 1.0).abs() < 0.02, "RY raw 0 → +1 (inverted), got {}", ds.ry);
        assert!(ds.btn_south, "Cross/south set");
        assert!(ds.btn_l2, "L2 digital set");
        assert!(ds.btn_options, "Options set");
        assert!(ds.btn_ps, "PS set");
        assert!((ds.l2 - 1.0).abs() < 0.01, "L2 analog full");
        assert!(!ds.btn_north && !ds.btn_east && !ds.btn_west, "no other faces");
    }

    // DS4 physical touchpad must decode (it was previously deferred, so the
    // physical DS4's touch1/touch2 source pins emitted nothing). USB report 0x01
    // carries the two touch fingers at RID-inclusive bytes 35 and 39, packed
    // identically to the DualSense. A finger with bit 7 clear is active.
    #[test]
    fn parse_ds4_decodes_touchpad() {
        let mut buf = [0u8; 64]; // full-length DS4 USB report
        buf[0] = 0x01;
        buf[2] = 128; buf[3] = 128; buf[4] = 128; // sticks centred-ish (avoid panics)
        buf[5] = 0x08; // dpad neutral

        // Finger 0 @35: active (bit7 clear), centre of pad → raw (960, 540).
        // pack: [id, x_lo, (y_lo<<4)|x_hi, y_hi] with raw_x=960 (0x3C0), raw_y=540 (0x21C)
        let (rx, ry) = (960u16, 540u16);
        buf[35] = 0x00; // active, id 0
        buf[36] = (rx & 0xFF) as u8;
        buf[37] = (((ry & 0x0F) << 4) | ((rx >> 8) & 0x0F)) as u8;
        buf[38] = ((ry >> 4) & 0xFF) as u8;
        // Finger 1 @39: inactive (bit7 set).
        buf[39] = 0x80;

        let r = parse_ds4(&buf).expect("valid DS4 report");
        assert!(r.has_touchpad, "DS4 now reports a touchpad");
        assert!(r.touch1.active, "finger 0 active");
        // Centre maps to ≈0 on both axes (within one LSB of the normalisation).
        assert!(r.touch1.x.abs() < 0.01, "finger 0 x≈0 at centre, got {}", r.touch1.x);
        assert!(r.touch1.y.abs() < 0.01, "finger 0 y≈0 at centre, got {}", r.touch1.y);
        assert!(!r.touch2.active, "finger 1 inactive");
    }

    #[test]
    fn decode_battery_maps_capacity_and_full() {
        // Discharging: low nibble = capacity 0..10 → min(level*10+5,100).
        assert!((decode_battery(0x00) - 0.05).abs() < 0.001, "empty-ish");
        assert!((decode_battery(0x05) - 0.55).abs() < 0.001, "mid");
        assert!((decode_battery(0x0A) - 1.0).abs() < 0.001, "level 10 → 100%");
        // Charging-complete status (high nibble 0x2) → full regardless of level.
        assert_eq!(decode_battery(0x20), 1.0);
    }

    // DS4 report carries battery at byte 12 (USB). A full-length report with a
    // known battery byte decodes to the expected fraction.
    #[test]
    fn parse_ds4_decodes_battery() {
        let mut buf = [0u8; 64];
        buf[0] = 0x01;
        buf[5] = 0x08; // dpad neutral
        buf[12] = 0x05; // battery capacity 5 → 55%
        let r = parse_ds4(&buf).expect("valid DS4 report");
        assert!(r.battery.is_some(), "DS4 battery decoded");
        assert!((r.battery.unwrap() - 0.55).abs() < 0.01, "got {:?}", r.battery);
    }

    // A short DS4 report (no touch bytes) must NOT claim a touchpad — IMU/buttons
    // still parse, but touch stays absent rather than reading garbage.
    #[test]
    fn parse_ds4_short_report_has_no_touch() {
        let mut buf = [0u8; 25];
        buf[0] = 0x01;
        buf[5] = 0x08; // dpad neutral
        let r = parse_ds4(&buf).expect("valid short DS4 report");
        assert!(!r.has_touchpad, "short report → no touchpad claimed");
    }

    /// Reflected IEEE CRC32 of the empty input is 0 (the canonical identity).
    /// `dualsense_bt_crc32` mixes the 0xA2 seed byte first, so the result for
    /// empty data is ~crc32_le(0xFFFFFFFF, [0xA2]) — a fixed value we can pin.
    #[test]
    fn dualsense_bt_crc_is_deterministic() {
        // Two identical reports must produce identical CRCs.
        let out = OutputState::default();
        let a = build_dualsense_bt_out(&out, 0);
        let b = build_dualsense_bt_out(&out, 0);
        assert_eq!(a, b);
        // Different sequence numbers produce different CRCs (seq is covered).
        let c = build_dualsense_bt_out(&out, 1);
        assert_ne!(a[74..78], c[74..78], "seq counter must affect CRC");
    }

    /// USB and BT carry the same 47-byte common struct. After stripping
    /// transport wrappers (USB: 1-byte ID prefix; BT: 3-byte prefix + 4-byte
    /// CRC + 24-byte reserved suffix), the common payloads must match.
    #[test]
    fn dualsense_usb_and_bt_share_common_payload() {
        let mut out = OutputState::default();
        out.rumble_strong = 0xAB;
        out.rumble_weak   = 0xCD;
        out.lightbar_r    = 0x10;
        out.lightbar_g    = 0x20;
        out.lightbar_b    = 0x30;
        let usb = build_dualsense_usb_out(&out);
        let bt  = build_dualsense_bt_out(&out, 0);
        assert_eq!(&usb[1..48], &bt[3..50], "common struct must be identical");
    }

    #[test]
    fn dualsense_bt_report_has_correct_framing() {
        let out = OutputState::default();
        let bt  = build_dualsense_bt_out(&out, 0x5);
        assert_eq!(bt[0], 0x31);              // report id
        assert_eq!(bt[1], 0x50);              // seq high-nibble = 5, low = 0
        assert_eq!(bt[2], 0x10);              // DS_OUTPUT_TAG
        assert_eq!(bt.len(), 78);             // DS_OUTPUT_REPORT_BT_SIZE
    }

    #[test]
    fn dualsense_usb_report_has_correct_framing() {
        let out = OutputState::default();
        let usb = build_dualsense_usb_out(&out);
        assert_eq!(usb[0], 0x02);             // report id
        assert_eq!(usb.len(), 63);            // DS_OUTPUT_REPORT_USB_SIZE
    }

    #[test]
    fn dualsense_common_motor_bytes_match_layout() {
        // motor_right at struct offset +2, motor_left at +3 (XInput-style).
        let mut out = OutputState::default();
        out.rumble_weak   = 0xAA; // → motor_right
        out.rumble_strong = 0xBB; // → motor_left
        let usb = build_dualsense_usb_out(&out);
        assert_eq!(usb[3], 0xAA, "motor_right (rumble_weak) at USB byte 3");
        assert_eq!(usb[4], 0xBB, "motor_left  (rumble_strong) at USB byte 4");
    }

    /// The own-virtual discriminator: a root-enumerated HIDMaestro device's HID
    /// instance path (`HIDCLASS`, no `VID_` token) is virtual; a real USB or BT
    /// controller path (which carries `VID_`) is not. This is what replaced the
    /// failed name/uuid markers — gilrs reports a generic name and nil uuid for
    /// both a real and an emulated same-VID/PID pad, but their HID paths differ.
    #[test]
    fn instance_path_classifies_virtual_vs_real() {
        // Real DualSense (USB) — from FLEXINPUT_PAD_DIAG on the target machine.
        assert!(!instance_path_is_virtual(
            r"\\?\HID#VID_054C&PID_0CE6&MI_03#7&2fc679c4&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}"
        ));
        // Emulated DualSense / DS4 (root-enumerated HIDMaestro child).
        assert!(instance_path_is_virtual(
            r"\\?\HID#HIDCLASS#1&2d595ca7&45&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}"
        ));
        // Explicit HIDMaestro SWD form is also virtual.
        assert!(instance_path_is_virtual(r"\\?\HID#SWD\HIDMAESTRO\0001#..."));
        // A real Bluetooth controller path (carries VID_) must NOT be virtual.
        assert!(!instance_path_is_virtual(
            r"\\?\HID#{00001124-0000-1000-8000-00805f9b34fb}_VID&02054C_PID&0CE6#..."
        ));
    }

    /// Sanity-check the CRC32 routine against a known-good vector. The reflected
    /// IEEE CRC32 of "123456789" is the IEEE/PNG checksum 0xCBF43926.
    /// (We compute it directly via `crc32_le_update` to validate the bit-level
    /// arithmetic — the DualSense-specific seeding is then trivial to layer on.)
    #[test]
    fn crc32_le_matches_ieee_check_vector() {
        let crc = !crc32_le_update(0xFFFFFFFF, b"123456789");
        assert_eq!(crc, 0xCBF43926);
    }
}
