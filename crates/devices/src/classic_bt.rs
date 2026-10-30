//! Bluetooth Classic gamepads on FlexInput's own dongle.
//!
//! ⭐ **Why this exists at all.** Windows runs one Bluetooth radio stack, so a
//! controller paired through Windows shares its adapter with every headset,
//! mouse and phone in range. A dongle bound to WinUSB and driven by our own HCI
//! stack is a radio that belongs to one controller — which is where the latency
//! and the polling rate come from.
//!
//! The path is the one proven by `bt_classic`: page the controller, hand back
//! the stored link key, open the two HID L2CAP channels, and read input reports
//! off the interrupt channel.
//!
//! ⭐ **Which pads, and who reads them.** What a pad answers over SDP decides
//! (see [`Family`]): a Switch Pro is read here on every system; on Windows an
//! Xbox controller, DualShock 4 or DualSense is read here too.
//! Every other HID pad is handed to the system with its own report
//! descriptor — on Linux through UHID, which is what BlueZ does; on Windows
//! as a USB device over USB/IP — so its driver, SDL and FlexInput's own pad
//! backends read it as any pad.
//!
//! ❗ **Pairing is NOT done here, deliberately.** Pairing replaces whatever host
//! the controller was bonded to — on a Switch Pro, its console — and a
//! background thread that quietly re-bonds any gamepad it can see would be a
//! genuinely bad thing to ship. Pairing is an explicit act performed with the
//! `bt_classic` tool; this connects only to addresses that already have a
//! stored link key.
//!
//! ⭐ **Runs alongside the Joy-Con 2 transport on the same dongle.** A
//! dual-mode adapter carries Bluetooth Classic and LE at once — that is what
//! dual mode is — and both transports share one radio through
//! [`flexinput_btle::radio`]. There is no switch and no trade: the reason this
//! ever looked like a choice was that each transport called `Dongle::open`
//! separately, and WinUSB grants an interface to one claimant.
//!
//! ❗ Reads come from a subscription. Anything that sends and waits for a reply
//! — paging, pairing, opening an L2CAP channel — runs under
//! `Radio::with_dongle`, which holds the shared reader off for the length of
//! that conversation.
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flexinput_btle::{hidp, keystore, l2cap};
use flexinput_core::Signal;

use crate::gyro::{
    hold_inactive_touch, output_pin_byte, parse_sony_bt, parse_switch_pro_report_calibrated,
    push_sony_reading, push_switch_pro_buttons, sony_bt_output, switch_pro_calib_from_spi,
    switch_pro_enable_vibration, switch_pro_spi_reply, switch_pro_spi_request,
    switch_rumble_report, xbox_bt_rumble, DualSenseState, HidReading, OutputState, SwitchProCalib,
    SWITCH_PRO_CALIB_READS,
};
use crate::spike::SpikeFilter;
use crate::{layouts, ControllerKind, DeviceBackend, DevicePin, PhysicalDevice};

/// Whether to print the per-link report-cadence line.
///
/// On in debug; in release it takes `FLEXINPUT_BTC_CADENCE=on`, so asking a user
/// "is your controller's rate its own choice, or are we dropping reports?" costs
/// them a restart rather than a special build.
fn cadence_logging() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        cfg!(debug_assertions)
            || std::env::var("FLEXINPUT_BTC_CADENCE").is_ok_and(|v| v.eq_ignore_ascii_case("on"))
    })
}

/// How long a link may go silent before it is treated as gone.
const STALE: Duration = Duration::from_secs(3);

/// Gap between connection attempts, so a controller that is simply switched off
/// does not spin the radio.
const RETRY_GAP: Duration = Duration::from_secs(5);

/// What a paired controller is, and so who reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    /// Read here, in its own protocol, with the Switch Pro decode `gyro`
    /// shares with the HID path. Also what a pairing made before device
    /// records existed is taken for until it answers SDP: every one of those
    /// was a Switch Pro, the only pad this transport used to read.
    SwitchPro,
    /// A DualShock 4 or DualSense, read here with the HID path's Sony decode.
    /// Windows: the only other way to it is Windows' own Bluetooth stack,
    /// which doesn't own this radio.
    Sony { dualsense: bool, edge: bool },
    /// Windows: an Xbox controller (or a pad in its Xbox mode), read here
    /// with SDL's Bluetooth decode ([`XboxState`]).
    ///
    /// ⛔ Not handed over as a USB device like other pads: FlexInput reads
    /// every Microsoft pad through XInput, which a plain USB HID device isn't
    /// on, and those empty reads overrode the pad's real input — it reported,
    /// and nothing moved. SDL is no way round it either: its Xbox driver
    /// picks the Bluetooth report format by the bus, and a USB device gets the
    /// wired protocol.
    Xbox,
    /// Handed to the system with its own report descriptor ([`Bridge`]): on
    /// Linux through UHID, as BlueZ hands a Bluetooth pad over; on Windows as
    /// a USB device over USB/IP. Any HID pad: the system's driver for it reads
    /// it, and FlexInput reads it like any pad — so this backend never lists
    /// it itself.
    Passthrough,
    /// Nothing on this system reads it over the dongle yet.
    Unsupported,
}

/// A pad's family from what it answered over SDP.
fn family_of(info: Option<&keystore::DeviceInfo>) -> Family {
    let Some(d) = info else { return Family::SwitchPro };
    if (d.vid, d.pid) == (0x057E, 0x2009) {
        return Family::SwitchPro;
    }
    // ❗ Windows reads the Sony pads here, not through a passthrough: SDL
    // would take a USB device for a USB pad, and a Sony pad's USB reports are
    // not its Bluetooth ones. Linux hands the kernel a Bluetooth device, whose
    // driver knows the difference.
    if !cfg!(target_os = "linux") {
        if d.vid == 0x045E {
            return Family::Xbox;
        }
        match (d.vid, ControllerKind::detect("", Some(d.vid), Some(d.pid))) {
            (0x054C, ControllerKind::DualShock4) => return Family::Sony { dualsense: false, edge: false },
            (0x054C, ControllerKind::DualSense) => {
                return Family::Sony {
                    dualsense: true,
                    edge: crate::identification::is_dualsense_edge(Some(d.vid), Some(d.pid)),
                }
            }
            _ => {}
        }
    }
    if d.descriptor.is_empty() {
        Family::Unsupported
    } else {
        Family::Passthrough
    }
}

/// Why a paired controller can't be read over the dongle right now, for the
/// paired list: `None` for one that can, or that hasn't said what it is yet.
pub fn unusable_reason(info: Option<&keystore::DeviceInfo>) -> Option<&'static str> {
    not_readable(family_of(info))
}

/// Why a pad of `family` can't be read here now, if it can't.
fn not_readable(family: Family) -> Option<&'static str> {
    match family {
        Family::Unsupported => Some(unsupported_reason()),
        Family::Passthrough if !bridge_available() => Some(NEEDS_USBIP),
        _ => None,
    }
}

/// Whether a pad streams reports whether or not anything changes, so silence
/// means its link is gone. Xbox and most generic pads report only on change:
/// for them it's the link going down that says so (`DisconnectionComplete`).
fn streams(family: Family) -> bool {
    matches!(family, Family::SwitchPro | Family::Sony { .. })
}

/// An Xbox controller's Bluetooth reports, as SDL's HIDAPI driver reads them
/// (`SDL_hidapi_xboxone.c`, `HIDAPI_DriverXboxOneBluetooth_*`, every
/// firmware's layout): `0x01` the state, `0x02` the guide button on early
/// firmware, `0x04` the battery. Kept between reports, since the guide
/// button and battery come in reports of their own.
///
/// The state rides in the struct a PlayStation pad's does: the pins it lands
/// on are positional and shared (`gyro::push_sony_reading`).
#[derive(Default, Clone, Copy)]
struct XboxState {
    pad: DualSenseState,
    /// Early firmware sends the guide button in a report of its own; once one
    /// has, the state report's guide bit isn't it.
    guide_report: bool,
    battery: Option<f32>,
}

/// XInput's digital-trigger threshold, about 30 of 255.
const TRIGGER_DIGITAL: f32 = 0.117;

impl XboxState {
    /// Take one report: the pad's reading after it, or `None` for a report
    /// this doesn't read.
    fn take(&mut self, r: &[u8]) -> Option<HidReading> {
        match *r.first()? {
            0x01 if r.len() >= 16 => {
                let word = |i: usize| u16::from_le_bytes([r[i], r[i + 1]]);
                let stick = |i: usize| ((word(i) as f32 - 32768.0) / 32768.0).clamp(-1.0, 1.0);
                let trigger = |i: usize| (word(i) & 0x03FF) as f32 / 1023.0;
                let p = &mut self.pad;
                p.lx = stick(1);
                p.ly = -stick(3); // down-positive in the report
                p.rx = stick(5);
                p.ry = -stick(7);
                p.l2 = trigger(9);
                p.r2 = trigger(11);
                p.btn_l2 = p.l2 > TRIGGER_DIGITAL;
                p.btn_r2 = p.r2 > TRIGGER_DIGITAL;
                // The hat: 1 = up, clockwise to 8; 0 = centred.
                let hat = r[13];
                p.dpad_up = matches!(hat, 1 | 2 | 8);
                p.dpad_right = matches!(hat, 2..=4);
                p.dpad_down = matches!(hat, 4..=6);
                p.dpad_left = matches!(hat, 6..=8);
                let (b14, b15) = (r[14], r[15]);
                if r.len() == 16 {
                    // The original Xbox One S.
                    p.btn_south = b14 & 0x01 != 0;
                    p.btn_east = b14 & 0x02 != 0;
                    p.btn_west = b14 & 0x04 != 0;
                    p.btn_north = b14 & 0x08 != 0;
                    p.btn_l1 = b14 & 0x10 != 0;
                    p.btn_r1 = b14 & 0x20 != 0;
                    p.btn_create = b14 & 0x40 != 0;
                    p.btn_options = b14 & 0x80 != 0;
                    p.btn_ls = b15 & 0x01 != 0;
                    p.btn_rs = b15 & 0x02 != 0;
                } else {
                    p.btn_south = b14 & 0x01 != 0;
                    p.btn_east = b14 & 0x02 != 0;
                    p.btn_west = b14 & 0x08 != 0;
                    p.btn_north = b14 & 0x10 != 0;
                    p.btn_l1 = b14 & 0x40 != 0;
                    p.btn_r1 = b14 & 0x80 != 0;
                    if !self.guide_report {
                        p.btn_ps = b15 & 0x10 != 0;
                    }
                    p.btn_options = b15 & 0x08 != 0;
                    p.btn_ls = b15 & 0x20 != 0;
                    p.btn_rs = b15 & 0x40 != 0;
                    // View: byte 15 on current firmware, byte 16 on 4.8.
                    p.btn_create = b15 & 0x04 != 0 || r.get(16).is_some_and(|b| b & 0x01 != 0);
                }
            }
            0x02 if r.len() >= 2 => {
                self.guide_report = true;
                self.pad.btn_ps = r[1] & 0x01 != 0;
            }
            // Four charge levels, in the low two bits.
            0x04 if r.len() >= 2 => self.battery = Some([0.1, 0.4, 0.7, 1.0][(r[1] & 0x03) as usize]),
            _ => return None,
        }
        Some(HidReading { dualsense: Some(self.pad), battery: self.battery, ..Default::default() })
    }
}

/// Never send an Xbox controller rumble faster than this: SDL holds 50 ms
/// between its Bluetooth rumble reports.
const XBOX_RUMBLE_GAP: Duration = Duration::from_millis(50);

/// Why an [`Family::Unsupported`] pad isn't read, for the log and the panel.
fn unsupported_reason() -> &'static str {
    "it offers no HID service to read"
}

/// Why a passthrough pad waits on Windows.
const NEEDS_USBIP: &str = "it needs usbip-win2, which the Bluetooth window installs";

/// Whether the system can take a passthrough pad: on Windows, usbip-win2's
/// host controller is installed. Linux asks for UHID access when it first
/// needs it, as the virtual pads do.
fn bridge_available() -> bool {
    #[cfg(windows)]
    return flexinput_usbip::vhci::is_available();
    #[cfg(not(windows))]
    true
}

/// A pad handed to the system: UHID on Linux, USB/IP on Windows. Both take
/// the same requests (`flexinput_usbip::HostRequest`).
#[cfg(target_os = "linux")]
type Bridge = flexinput_virtual::Passthrough;
#[cfg(windows)]
type Bridge = flexinput_usbip::Passthrough;

/// One connected classic controller.
#[derive(Clone)]
struct PadState {
    family: Family,
    address: [u8; 6],
    reading: HidReading,
    last: Instant,
    /// The controller's friendly name from the key store, if it was recorded.
    name: Option<String>,
    /// Reports since the last drain, for the per-device rate display.
    ///
    /// ❗ Counted here rather than inferred from the signal stream: `poll`
    /// republishes the last reading every tick whether or not a new report
    /// arrived, so counting polls would show the UI's frame rate and counting
    /// changes would show zero for a controller sitting still. Neither is the
    /// polling rate.
    events: u32,
    /// Outlier spike suppression on the six IMU axes, run on every report as
    /// it arrives — one report is one true sample, as on the HID path. Its
    /// settings come from the UI through `set_spike_filter`.
    spike: SpikeFilter,
}

/// What a pairing run is doing, for the UI to show.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PairPhase {
    #[default]
    Idle,
    /// Looking, until a controller pairs or the search is cancelled. The note
    /// says how the last attempt went, when one failed.
    Searching(Option<String>),
    Pairing(String),
    Done(String),
    Failed(String),
}

/// What the transport is doing, so the UI never has to say nothing.
///
/// ⭐ Every one of these was previously invisible. "The button does nothing"
/// is the report that follows from a background thread that fails silently, and
/// the fix is not a better button — it is saying which of these states it is in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Disabled,
    /// Enabled, but no Bluetooth adapter could be opened.
    NoRadio(String),
    /// Radio held, nothing paired yet.
    Idle,
    /// Radio held. `connected` links are up; `streaming` of those are actually
    /// delivering input.
    ///
    /// ⭐ Two numbers, because they came apart on hardware and one number hid
    /// it: the panel said "1 of 1 connected" while no controller appeared
    /// anywhere, because a link being UP and a link DELIVERING REPORTS are
    /// different things and only the second produces a device.
    Running { paired: usize, connected: usize, streaming: usize },
}

#[derive(Default)]
struct Shared {
    status: Mutex<Status>,
    /// Set by the UI to ask for a pairing run; cleared when it starts.
    ///
    /// ⭐ A REQUEST, not an action. The radio belongs to this backend's thread,
    /// and a UI that opened the dongle itself would be a second owner — the
    /// exact conflict that already cost a debugging session. The button sets a
    /// flag; the thread that owns the hardware does the work and reports back.
    pair_requested: AtomicBool,
    /// Set by the UI to stop a search: its Cancel button, or the window
    /// closing. Cleared by the next request.
    pair_cancel: AtomicBool,
    pair_phase: Mutex<PairPhase>,
    pads: Mutex<HashMap<[u8; 6], PadState>>,
    /// Feedback staged by `send`, per controller, for the worker to write.
    ///
    /// ⭐ Staged, not sent. `send` runs on the I/O thread and the radio
    /// belongs to the worker — the same rule as pairing: the owner of the
    /// hardware does the talking.
    outputs: Mutex<HashMap<[u8; 6], OutputState>>,
    shutdown: AtomicBool,
    /// Set once the radio could not be opened, so the reason is logged once
    /// rather than every retry.
    yielded: AtomicBool,
    /// Same, for "enabled but nothing is paired".
    said_empty: AtomicBool,
    /// Said once when this transport stands aside for another — see the page
    /// gate. Repeating it every pass would be a firehose.
    yielded_radio: AtomicBool,
    /// Set by the worker once it has disconnected everything and returned.
    stopped: AtomicBool,
}

pub struct ClassicBtBackend {
    shared: Arc<Shared>,
}

/// Handle the UI uses to drive pairing without touching the radio.
#[derive(Clone, Default)]
pub struct PairControl {
    shared: Arc<Shared>,
}

impl PairControl {
    /// Ask the backend to look for a controller in pairing mode and bond it.
    /// It keeps looking until one pairs or [`cancel`](Self::cancel).
    pub fn request(&self) {
        self.shared.pair_cancel.store(false, Ordering::Relaxed);
        self.shared.pair_requested.store(true, Ordering::Relaxed);
    }

    /// Stop a search. Harmless when none is running.
    pub fn cancel(&self) {
        self.shared.pair_requested.store(false, Ordering::Relaxed);
        self.shared.pair_cancel.store(true, Ordering::Relaxed);
    }

    /// Whether a search, or the pairing it found, is under way.
    pub fn busy(&self) -> bool {
        matches!(self.phase(), PairPhase::Searching(_) | PairPhase::Pairing(_))
    }

    /// What the current (or last) run is doing.
    pub fn phase(&self) -> PairPhase {
        self.shared.pair_phase.lock().map(|p| p.clone()).unwrap_or_default()
    }

    /// What the transport itself is doing.
    pub fn status(&self) -> Status {
        self.shared.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Whether a usable radio was found. A pair button is pointless without
    /// one, and saying so beats a button that silently does nothing.
    pub fn transport_enabled(&self) -> bool {
        !matches!(self.status(), Status::Disabled | Status::NoRadio(_))
    }
}

/// The process-wide control handle.
///
/// One backend is constructed per process, so a global handle is honest about
/// what is actually there — and it saves threading an Arc through the whole UI
/// for one button.
pub fn pair_control() -> PairControl {
    static CTL: std::sync::OnceLock<PairControl> = std::sync::OnceLock::new();
    CTL.get_or_init(PairControl::default).clone()
}

impl ClassicBtBackend {
    pub fn new() -> Self {
        // Shares the same `Shared` the UI's control handle holds, so a request
        // made before the thread starts is still seen.
        let shared = Arc::clone(&pair_control().shared);
        let s = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("bt-classic".into())
            .spawn(move || run(s))
            .ok();
        Self { shared }
    }
}

impl Drop for ClassicBtBackend {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Relaxed);
        // ⭐ Wait for the worker to tear its links down.
        //
        // ❗ A link lives in the DONGLE, not in this process. Exiting without
        // disconnecting leaves the controller believing it is still connected —
        // after which it neither pages, nor advertises, nor answers an inquiry,
        // so the next run cannot find it by any means and the user has to
        // power-cycle the pad. Bounded, because a tidy exit is not worth
        // hanging the application over.
        let deadline = Instant::now() + Duration::from_millis(700);
        while Instant::now() < deadline && !self.shared.stopped.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// `vid:pid` of the dongle to use.
///
/// ⭐ **Nothing here is specific to one adapter.** A Bluetooth dongle announces
/// itself by USB CLASS — wireless controller / RF / Bluetooth — so the first
/// one that is actually openable is picked automatically and any WinUSB-bound
/// adapter works (on Linux, any adapter handed to FlexInput). The Realtek ids are only a last-resort fallback for the case
/// where discovery finds nothing at all, which keeps the error message naming a
/// concrete device instead of `0000:0000`.
///
/// The environment variable still wins, and is what you use to pin a SECOND
/// adapter to this transport while the Joy-Con hub keeps the first.
fn dongle_ids() -> Option<(u16, u16)> {
    let Ok(raw) = std::env::var("FLEXINPUT_BT_CLASSIC_DONGLE") else {
        // ⛔ No hardcoded fallback. Guessing a vendor id reports "your dongle
        // is missing" about somebody else's dongle — see
        // `flexinput_btle::preferred_dongle`.
        return flexinput_btle::preferred_dongle();
    };
    let mut it = raw.split(':');
    match (
        it.next().and_then(|v| u16::from_str_radix(v.trim_start_matches("0x"), 16).ok()),
        it.next().and_then(|v| u16::from_str_radix(v.trim_start_matches("0x"), 16).ok()),
    ) {
        (Some(v), Some(p)) => Some((v, p)),
        _ => {
            eprintln!(
                "[bt-classic] FLEXINPUT_BT_CLASSIC_DONGLE={raw:?} is not vid:pid                  — ignoring it and using whichever adapter is present"
            );
            flexinput_btle::preferred_dongle()
        }
    }
}

/// The most controllers held at once.
///
/// Not a protocol limit — a classic radio manages seven active links — but a
/// practical one: every connected pad shares this thread and the dongle's
/// bandwidth, and four is already more than a couch holds.
const MAX_LINKS: usize = 4;

/// How long a background page may hold the radio.
///
/// ⛔ **Every second here is a second the shared radio is deaf to everything
/// else**, Joy-Cons included — a page takes an exclusive lease. At three
/// seconds every five, a switched-off controller was making the radio
/// unavailable sixty per cent of the time, which is not a background task, it
/// is a denial of service with a retry timer.
const CONNECT_PATIENCE: Duration = Duration::from_secs(2);

/// How long to spend on a link the REMOTE opened.
///
/// ⛔ **Not the same budget, because it is not the same cost.** The two seconds
/// above are a limit on how long a PAGE may hold the radio away from everything
/// else, and a page at a switched-off controller earns nothing by lasting
/// longer. A link the controller opened is the opposite: the conversation is
/// already happening, the radio is doing the one thing it is here to do, and
/// cutting it off achieves nothing but losing it.
///
/// Sharing the page budget cost exactly that — a reconnection that had
/// authenticated and was mid-handshake, abandoned at two seconds with
/// `timed out (connected=false, paired=true)` while the controller was still
/// answering. Connection Complete on an incoming link routinely takes longer
/// than a page does, because authentication happens along the way.

const ADOPT_PATIENCE: Duration = Duration::from_secs(10);

/// How often to page a paired controller that has not called in, while at
/// least one other controller is connected.
///
/// ⭐ **Rare, because a live link is worth more than a fast reconnect.** Every
/// page takes an exclusive lease, and spending it on a pad that is switched off
/// steals it from pads that are switched on.
const PAGE_FALLBACK: Duration = Duration::from_secs(30);

/// How often to page when NOTHING is connected.
///
/// ⛔ **Paging and listening are MUTUALLY EXCLUSIVE, and listening wins.**
///
/// A radio that is paging is not page-scanning, so every second spent calling a
/// controller is a second in which that controller cannot call us — and calling
/// us is how a bonded pad reconnects. The two mechanisms are not complementary;
/// they compete for the same radio.
///
/// This was briefly set to 3 s on the theory that a page is a coin flip worth
/// flipping often. Measured live, it was catastrophic: 23 attempts in 46
/// seconds at 2 s of patience each is a radio that is paging ONE HUNDRED PER
/// CENT of the time. The pad called and called into a host that was, by
/// construction, never listening. The probe which did receive incoming pages
/// was the one that never paged at all.
///
/// So the gap is long, the patience is short, and the duty cycle is stated
/// rather than left to emerge: roughly one second of paging per twenty, and
/// nineteen spent listening — which is what the mechanism actually needs.
///
/// ❗ Timed from the END of an attempt. Timing it from the start subtracts the
/// patience from the gap, which is how three seconds became zero.
const PAGE_EAGER: Duration = Duration::from_secs(20);

/// How often to repeat the "waiting for a controller" line.
///
/// ❗ Said once, it reads as "gave up". Said every pass, it is a firehose.
const NOTE_REPEAT: Duration = Duration::from_secs(120);

/// Reading the controller's own stick calibration, without blocking the link.
///
/// ⭐ **Why this is not a blocking exchange in `bring_up`.** This transport
/// took a long time to make reliable, and the controller is streaming ~200
/// reports a second by the time anyone could ask it anything. Stopping to wait
/// on a reply means either not draining that stream — which fills the radio
/// queue and stalls the link — or writing a second, parallel drain that has to
/// agree with the first. So the requests are fired into the normal flow and the
/// answers are picked out of it as they pass.
///
/// ❗ Retried, because it is fire-and-forget over a lossy link and a dropped
/// request is otherwise a controller that silently keeps the square response.
#[derive(Default)]
struct SpiProbe {
    /// One slot per entry in `SWITCH_PRO_CALIB_READS`.
    blobs: [Option<Vec<u8>>; 4],
    attempts: u8,
    next_try: Option<Instant>,
}

/// How many times to ask before settling for the fallback numbers.
const SPI_ATTEMPTS: u8 = 4;
/// Gap between rounds of requests.
const SPI_RETRY: Duration = Duration::from_millis(600);

/// One live controller.
struct Link {
    addr: [u8; 6],
    conn: u16,
    control: l2cap::Channel,
    interrupt: l2cap::Channel,
    last: Instant,
    name: Option<String>,
    /// Whether any input report has arrived on this link yet.
    reported: bool,
    /// Inter-report timing, so "170 Hz" can be told apart from "200 Hz with a
    /// sixth of the reports lost".
    ///
    /// ⭐ The same measurement the LE transport uses, from the same type. The
    /// question there was whether a rate was the device's choice or our loss,
    /// and it is the same question here — asked with different numbers, the
    /// answers could not be compared.
    cadence: flexinput_btle::cadence::Cadence,
    /// The controller's own stick calibration, once it has answered.
    ///
    /// ⛔ `None` means the sticks are being normalised against a NOMINAL
    /// 12-bit range rather than this unit's real travel, which reads as a
    /// square circularity plot — see `switch_pro_fallback_calib`.
    calib: Option<SwitchProCalib>,
    spi: SpiProbe,
    /// The Switch output packet counter, shared by every report we send —
    /// subcommands and rumble alike.
    counter: u8,
    /// Whether the controller acknowledged "enable vibration" (`0x48`).
    ///
    /// ⛔ Until it does, every rumble report is silently ignored. The HID
    /// path sends it at init; this transport never did, which is half of why
    /// rumble over the dongle did nothing at all.
    vib_on: bool,
    vib_tries: u8,
    vib_next: Option<Instant>,
    /// What output state (Switch Pro rumble, a Sony pad's whole output
    /// report) the controller last received, and when.
    rumble_sent: Option<OutputState>,
    rumble_at: Option<Instant>,
    family: Family,
    /// What it answered over SDP, when it has: on Linux, what its passthrough
    /// is created from.
    info: Option<keystore::DeviceInfo>,
    /// Control-channel transactions: one in flight, the rest waiting — see
    /// `flexinput_btle::hidp`.
    ctl_queue: VecDeque<Ctl>,
    ctl_in_flight: Option<(Ctl, Instant)>,
    /// Sony: whether full reports (`0x31` / `0x11`) are arriving yet, and the
    /// asks for them.
    full: bool,
    full_tries: u8,
    full_next: Option<Instant>,
    /// Sony: the DualSense's output sequence and one-shot lightbar setup, as
    /// the HID path keeps them.
    sony_seq: u8,
    lightbar_set_up: bool,
    /// Xbox: the pad's state between its reports.
    xbox: XboxState,
    /// The device the system reads this pad through, for a passthrough pad.
    #[cfg(any(target_os = "linux", windows))]
    passthrough: Option<Bridge>,
}

impl Link {
    fn next_counter(&mut self) -> u8 {
        let c = self.counter;
        self.counter = self.counter.wrapping_add(1);
        c
    }
}

/// One control-channel transaction, and who wants its answer.
struct Ctl {
    frame: Vec<u8>,
    answer: Answer,
}

#[derive(Clone, Copy)]
enum Answer {
    /// Ours; nothing to pass on.
    Ignore,
    /// A report read or write the host made through the passthrough, by the
    /// id it came with.
    Get(u32),
    Set(u32),
}

/// How long a control transaction may go unanswered before it is failed and
/// the next one sent. Well inside the five seconds the kernel gives a UHID
/// report request.
const CTL_PATIENCE: Duration = Duration::from_millis(1500);

/// Asks for a Sony pad's full reports, and the gap between them.
const FULL_ATTEMPTS: u8 = 5;
const FULL_RETRY: Duration = Duration::from_secs(1);

/// Send the next waiting control transaction when none is in flight, failing
/// one the pad has sat on too long first.
fn pump_control(radio: &flexinput_btle::radio::Radio, link: &mut Link) {
    if let Some((_, at)) = &link.ctl_in_flight {
        if at.elapsed() < CTL_PATIENCE {
            return;
        }
        if let Some((ctl, _)) = link.ctl_in_flight.take() {
            answer_host(link, ctl.answer, None);
        }
    }
    if let Some(ctl) = link.ctl_queue.pop_front() {
        let _ = radio.with_dongle(|d| d.send_att_raw(link.conn, link.control.remote_cid, &ctl.frame));
        link.ctl_in_flight = Some((ctl, Instant::now()));
    }
}

/// A packet on the control channel: the answer to the transaction in flight.
fn control_reply(link: &mut Link, payload: &[u8]) {
    let Some(reply) = hidp::parse_control(payload) else { return };
    let Some((ctl, _)) = link.ctl_in_flight.take() else { return };
    let result = match reply {
        hidp::ControlReply::Data { report, .. } => Some(report.to_vec()),
        hidp::ControlReply::Handshake(code) => (code == hidp::HANDSHAKE_SUCCESSFUL).then(Vec::new),
    };
    answer_host(link, ctl.answer, result);
}

/// Pass a control transaction's outcome to whoever asked; `None` failed.
fn answer_host(link: &Link, answer: Answer, result: Option<Vec<u8>>) {
    #[cfg(any(target_os = "linux", windows))]
    if let Some(p) = &link.passthrough {
        match answer {
            // A read answered by a bare success handshake brought no report.
            Answer::Get(id) => p.reply_get(id, result.as_deref().filter(|r| !r.is_empty())),
            Answer::Set(id) => p.reply_set(id, result.is_some()),
            Answer::Ignore => {}
        }
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    let _ = (link, answer, result);
}

/// Put what the host asked of a passthrough pad to the pad. Output reports
/// go straight out; report reads and writes queue on the control channel and
/// are answered as the pad answers.
#[cfg(any(target_os = "linux", windows))]
fn serve_host(radio: &flexinput_btle::radio::Radio, link: &mut Link) {
    use flexinput_usbip::{HostRequest, ReportKind};
    let kind = |k: ReportKind| match k {
        ReportKind::Input => hidp::REPORT_INPUT,
        ReportKind::Output => hidp::REPORT_OUTPUT,
        ReportKind::Feature => hidp::REPORT_FEATURE,
    };
    let Some(p) = &link.passthrough else { return };
    while let Some(req) = p.next_request() {
        match req {
            HostRequest::Output(report) => {
                let frame = hidp::output(&report);
                let _ = radio.with_dongle(|d| d.send_att_raw(link.conn, link.interrupt.remote_cid, &frame));
            }
            HostRequest::GetReport { id, kind: k, number } => link.ctl_queue.push_back(Ctl {
                frame: hidp::get_report(kind(k), number),
                answer: Answer::Get(id),
            }),
            HostRequest::SetReport { id, kind: k, data } => link.ctl_queue.push_back(Ctl {
                frame: hidp::set_report(kind(k), &data),
                answer: Answer::Set(id),
            }),
        }
    }
}

/// Make a fresh link ready for what it carries. `Err` says why it can't be
/// used; the caller closes it.
fn start_link(link: &mut Link, adapter: Option<[u8; 6]>) -> Result<(), String> {
    match link.family {
        Family::SwitchPro | Family::Sony { .. } | Family::Xbox => Ok(()),
        Family::Unsupported => Err(unsupported_reason().into()),
        Family::Passthrough => {
            if let Some(why) = not_readable(link.family) {
                return Err(why.into());
            }
            #[cfg(windows)]
            {
                let _ = adapter;
                let d = link.info.as_ref().ok_or("no device record")?;
                let name = link.name.clone().unwrap_or_else(|| "Bluetooth controller".into());
                // The pad's own address: its USB instance, and so its HidHide
                // entry, stays the same across connections.
                let serial: String = link.addr.iter().map(|b| format!("{b:02X}")).collect();
                let p = flexinput_usbip::Passthrough::create(&flexinput_usbip::PassthroughSpec {
                    vid: d.vid,
                    pid: d.pid,
                    version: d.version,
                    product: &name,
                    serial: &serial,
                    descriptor: &d.descriptor,
                })?;
                eprintln!(
                    "[bt-classic] {} handed to the system as \"{name}\" ({:04x}:{:04x})",
                    keystore::format_addr(link.addr),
                    d.vid,
                    d.pid
                );
                link.passthrough = Some(p);
                Ok(())
            }
            #[cfg(target_os = "linux")]
            {
                let d = link.info.as_ref().ok_or("no device record")?;
                let name = link.name.clone().unwrap_or_else(|| "Bluetooth controller".into());
                let phys = adapter.map(keystore::format_addr).unwrap_or_default();
                let uniq = keystore::format_addr(link.addr);
                let p = flexinput_virtual::Passthrough::create(&flexinput_virtual::PassthroughSpec {
                    name: &name,
                    phys: &phys,
                    uniq: &uniq,
                    bus: flexinput_virtual::BUS_BLUETOOTH,
                    vid: d.vid,
                    pid: d.pid,
                    version: d.version,
                    descriptor: &d.descriptor,
                })?;
                eprintln!(
                    "[bt-classic] {uniq} handed to the system as \"{name}\" ({:04x}:{:04x})",
                    d.vid, d.pid
                );
                link.passthrough = Some(p);
                Ok(())
            }
            #[cfg(not(any(target_os = "linux", windows)))]
            {
                let _ = adapter;
                Err(unsupported_reason().into())
            }
        }
    }
}

/// A report decoded: the pad is alive and sending, its state goes out, and
/// its cadence is measured.
fn took_report(shared: &Shared, link: &mut Link, reading: HidReading) {
    if !link.reported {
        link.reported = true;
        eprintln!(
            "[bt-classic] {} is sending input",
            keystore::format_addr(link.addr)
        );
    }
    link.last = Instant::now();
    publish(shared, link, reading);
    // ⛔ Debug builds, or when explicitly asked for. A line every five
    // seconds per pad is a diagnostic, not something a shipped build
    // should print at a user forever.
    if let Some(c) = link.cadence.tick(Instant::now(), Duration::from_secs(5)) {
        if cadence_logging() {
            eprintln!(
                "[bt-classic] {} cadence: {:.1} Hz over {} reports, mean {:.2} ms                          (min {:.2}, max {:.2}), jitter {:.2} ms/step",
                keystore::format_addr(link.addr),
                c.hz,
                c.samples,
                c.mean_ms,
                c.min_ms,
                c.max_ms,
                c.jitter_ms,
            );
        }
    }
}

/// Put one decoded report on the pad's shared state.
///
/// ⛔ **ONE lock, held once.** This used to call `pads.lock()` a second time
/// inside the value expression of an `insert` whose own guard was still
/// alive — and `std::sync::Mutex` is not reentrant, so the transport thread
/// deadlocked against itself on the FIRST input report to arrive. Everything
/// up to that point looked perfect: paired, encrypted, both HID channels
/// configured, "is sending input" printed once, and then the whole backend
/// stopped dead. `enumerate` blocked on the same mutex from the UI thread, so
/// no device ever appeared either.
///
/// The `entry` API does the read and the write under the single guard, which
/// is both correct and shorter than what it replaces.
fn publish(shared: &Shared, link: &Link, mut reading: HidReading) {
    let mut pads = shared.pads.lock().unwrap();
    let slot = pads.entry(link.addr).or_insert_with(|| PadState {
        family: link.family,
        address: link.addr,
        reading,
        last: link.last,
        name: link.name.clone(),
        events: 0,
        spike: SpikeFilter::new(),
    });
    // A lifted finger keeps its last position, as on the HID path.
    if reading.has_touchpad {
        hold_inactive_touch(&mut reading.touch1, &slot.reading.touch1);
        hold_inactive_touch(&mut reading.touch2, &slot.reading.touch2);
    }
    let imu = slot.spike.push([
        reading.gyro_x, reading.gyro_y, reading.gyro_z,
        reading.accel_x, reading.accel_y, reading.accel_z,
    ]);
    [reading.gyro_x, reading.gyro_y, reading.gyro_z] = [imu[0], imu[1], imu[2]];
    [reading.accel_x, reading.accel_y, reading.accel_z] = [imu[3], imu[4], imu[5]];
    slot.family = link.family;
    slot.reading = reading;
    slot.last = link.last;
    slot.name = link.name.clone();
    slot.events = slot.events.saturating_add(1);
}

/// How many times to ask for vibration before giving up on it.
const VIB_ATTEMPTS: u8 = 5;
/// Gap between those asks.
const VIB_RETRY: Duration = Duration::from_millis(500);
/// Re-send an unchanged rumble state this often — the same heartbeat the HID
/// path keeps.
const RUMBLE_HEARTBEAT: Duration = Duration::from_secs(1);
/// Never send rumble faster than this. A modulated rumble changes every tick,
/// and the I/O thread ticks far faster than a controller can use; unthrottled
/// it would spend the link's airtime on output instead of input.
const RUMBLE_MIN_GAP: Duration = Duration::from_millis(12);

/// Answer the remote's L2CAP signalling on a live link.
///
/// ⭐ Only configuration and disconnection, which are the two a HID device
/// actually sends once its channels are up. Both must be answered: an
/// unanswered configuration request is re-sent until the remote gives up, and
/// an unanswered disconnection request leaves a half-closed channel behind.
fn answer_signalling(
    radio: &flexinput_btle::radio::Radio,
    link: &Link,
    pkt: &flexinput_btle::AclPacket,
) {
    let send = |reply: &[u8]| {
        let _ = radio.with_dongle(|d| d.send_att_raw(link.conn, l2cap::CID_SIGNALLING, reply));
    };
    // ⛔ Every command in the packet, not just the first — see
    // `l2cap::parse_signals`.
    for sig in l2cap::parse_signals(&pkt.payload) {
    match sig.code {
        l2cap::SIG_CONFIGURE_REQUEST => {
            if let Some((dest, opts)) = l2cap::parse_configure_request(&sig.data) {
                let remote = if dest == link.interrupt.local_cid {
                    link.interrupt.remote_cid
                } else {
                    link.control.remote_cid
                };
                send(&l2cap::encode_signal(
                    l2cap::SIG_CONFIGURE_RESPONSE,
                    sig.identifier,
                    &l2cap::configure_response(remote, &opts),
                ));
            }
        }
        l2cap::SIG_DISCONNECTION_REQUEST => {
            let reply = l2cap::encode_signal(
                l2cap::SIG_DISCONNECTION_RESPONSE,
                sig.identifier,
                &sig.data,
            );
            send(&reply);
        }
        // A channel requested once the HID pair is already up — an SDP query,
        // typically. Refused out loud, because silence leaves the controller
        // waiting on it.
        l2cap::SIG_CONNECTION_REQUEST => {
            if let Some((psm, their_cid)) = l2cap::parse_connection_request(&sig.data) {
                if trace() {
                    eprintln!(
                        "[bt-classic] {} asked for PSM {psm:#06x} on a live link — refusing",
                        keystore::format_addr(link.addr)
                    );
                }
                send(&l2cap::encode_signal(
                    l2cap::SIG_CONNECTION_RESPONSE,
                    sig.identifier,
                    &l2cap::connection_response(0, their_cid, 0x0002),
                ));
            }
        }
        // ⭐ Information and Echo Requests, and anything unimplemented — see
        // `l2cap::housekeeping_reply`. These were ignored here too.
        _ => {
            if let Some(reply) = l2cap::housekeeping_reply(&sig) {
                send(&reply);
            }
        }
    }
    }
}

/// Say so when a stored key belongs to a DIFFERENT dongle.
///
/// ⛔ **The failure this exists for is completely silent.** The controller
/// remembers the address of the radio it paired with and pages that address;
/// a key carried over from another adapter leaves the pad blinking at a host
/// which is listening correctly, with page scan verified on, for a call that is
/// addressed to somebody else. No error is raised anywhere, because nothing
/// went wrong — the two are simply not talking to each other.
///
/// Entries written before the adapter was recorded have `None` and are left
/// alone: unknown is not the same as wrong, and guessing would cry wolf on
/// every pairing that predates this.
fn warn_about_foreign_keys(
    ours: Option<[u8; 6]>,
    known: &std::collections::BTreeMap<String, keystore::Pairing>,
) {
    let Some(ours) = ours else { return };
    if trace() {
        eprintln!("[bt-classic] this adapter is {}", keystore::format_addr(ours));
    }
    for (addr, p) in known {
        match p.adapter {
            Some(a) if a != ours => eprintln!(
                "[bt-classic] ⚠ the key for {addr} was made with adapter {} —                  this dongle is {}. The controller is paging the other one and                  will never call us. Pair it again with this dongle.",
                keystore::format_addr(a),
                keystore::format_addr(ours),
            ),
            _ => {}
        }
    }
}

fn trace() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("FLEXINPUT_BT_TRACE").is_ok())
}

/// Milliseconds since the transport started, for trace lines. A setup that
/// fails is a question of ORDER and TIMING — what came 50 ms before the drop
/// — and untimed lines answer neither.
fn ms() -> u128 {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_millis()
}

fn set_status(shared: &Arc<Shared>, s: Status) {
    if let Ok(mut g) = shared.status.lock() {
        if *g != s {
            *g = s;
        }
    }
}

fn run(shared: Arc<Shared>) {
    run_inner(&shared);
    shared.stopped.store(true, Ordering::Relaxed);
}

fn run_inner(shared: &Arc<Shared>) {
    let mut links: Vec<Link> = Vec::new();
    let mut next_try = Instant::now();
    let mut last_note: Option<Instant> = None;
    let mut radio: Option<Arc<flexinput_btle::radio::Radio>> = None;
    let mut sub: Option<flexinput_btle::radio::Subscriber> = None;
    // The adapter's own address: the `phys` of a passthrough pad.
    let mut adapter: Option<[u8; 6]> = None;

    // ❗ Cached. `keystore::load` reads a file, and this loop runs as fast as
    // the radio delivers packets — a disk read per input report is absurd, and
    // it is time not spent draining the queue that carries them.
    let mut known = keystore::load();
    let mut devices = keystore::load_devices();
    let mut known_at = Instant::now();
    // Pads nothing here reads (see `Family::Unsupported`): neither paged nor
    // answered, and said once.
    let unusable = |devices: &std::collections::BTreeMap<String, keystore::DeviceInfo>, a: [u8; 6]| {
        not_readable(family_of(devices.get(&keystore::format_addr(a)))).is_some()
    };
    let mut said_unusable: HashSet<[u8; 6]> = HashSet::new();
    let mut search: Option<Search> = None;
    while !shared.shutdown.load(Ordering::Relaxed) {
        if known_at.elapsed() > Duration::from_secs(2) {
            known = keystore::load();
            devices = keystore::load_devices();
            known_at = Instant::now();
        }
        // ⛔ Read BEFORE the empty-store early return.
        //
        // ❗ This is what made "Pair new controller" do nothing at all: with no
        // controllers paired the loop hit `continue` here and never reached the
        // pairing code below — so pairing from scratch, the one case the button
        // exists for, was the exact case it could not serve. An empty store is
        // a reason to keep the radio closed, not a reason to ignore the user.
        let want_pair = shared.pair_requested.load(Ordering::Relaxed) || search.is_some();
        if known.is_empty() && !want_pair {
            set_status(&shared, Status::Idle);
            // ⭐ Said ONCE, with the path. "Enabled but nothing happens" is the
            // state a user is most likely to hit — the pairing tool writes its
            // key file beside its own binary, which need not be where the app
            // looks — and an empty store is indistinguishable from a broken
            // transport unless it says which file it read.
            if !shared.said_empty.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "[bt-classic] no controllers paired — key store is {}\n\
                     [bt-classic] pair one with: cargo run -p flexinput-btle \
                     --bin bt_classic -- --pair <address>",
                    keystore::path().display(),
                );
            }
            radio = None;
            sub = None;
            links.clear();
            std::thread::sleep(RETRY_GAP);
            continue;
        }
        shared.said_empty.store(false, Ordering::Relaxed);
        // ⭐ An adapter that went away — unplugged, or given back to the
        // system — is let go of, links and all, so the next pass opens
        // whatever FlexInput has now instead of holding a dead one.
        if radio.as_ref().is_some_and(|r| r.is_gone()) {
            links.clear();
            sub = None;
            radio = None;
            adapter = None;
            if let Ok(mut pads) = shared.pads.lock() {
                pads.clear();
            }
        }
        if radio.is_none() {
            match dongle_ids().and_then(|(v, p)| flexinput_btle::radio::shared(v, p)) {
                Some(r) => {
                    if trace() {
                        eprintln!("[bt-classic] radio obtained; enabling page scan");
                    }
                    // ⭐ ANSWER incoming pages. A bonded controller that is
                    // switched on calls its host rather than waiting to be
                    // called, and `HCI_Reset` leaves scanning off — so without
                    // this the pad blinks, pages a deaf radio, and gives up.
                    // Page scan only: our own controllers can find us, a
                    // general inquiry cannot.
                    // ⛔ Listen OFTEN, not at the controller's default. After a
                    // reset it listens 11.25 ms every 1.28 s, and a switched-on
                    // pad pages for only a few seconds — so whether it got
                    // through was luck. See `Dongle::set_fast_connectable`.
                    if let Err(e) = r.with_dongle(|d| d.set_fast_connectable()) {
                        eprintln!(
                            "[bt-classic] fast page scan refused ({e}) — paired                              controllers may take several attempts to reconnect"
                        );
                    }
                    match r.with_dongle(|d| {
                        d.set_scan_enable(0x02).and_then(|()| d.read_scan_enable())
                    }) {
                        // ⭐ Read back, not assumed. "Nothing arrives" looks the
                        // same whether the radio is deaf or simply not being
                        // called, and only one of those is our bug.
                        Ok(mask) if mask & 0x02 != 0 => {
                            if trace() {
                                eprintln!("[bt-classic] page scan on (scan_enable {mask:#04x})");
                            }
                        }
                        Ok(mask) => eprintln!(
                            "[bt-classic] page scan did NOT take: scan_enable {mask:#04x}"
                        ),
                        Err(e) => eprintln!("[bt-classic] could not enable page scan: {e}"),
                    }
                    adapter = r.with_dongle(|d| d.read_bd_addr()).ok();
                    warn_about_foreign_keys(adapter, &known);
                    // Without adverts: this transport reads none, and a
                    // Joy-Con scan's flood of them evicted the Connection
                    // Requests it does need. See `radio::subscribe_without_adverts`.
                    sub = Some(flexinput_btle::radio::subscribe_without_adverts(&r));
                    shared.yielded.store(false, Ordering::Relaxed);
                    radio = Some(r);
                }
                None => {
                    set_status(&shared, Status::NoRadio(flexinput_btle::NO_ADAPTER.into()));
                    if !shared.yielded.swap(true, Ordering::Relaxed) {
                        eprintln!("[bt-classic] {}", flexinput_btle::Error::NoAdapter);
                    }
                }
            }
            if radio.is_none() {
                std::thread::sleep(RETRY_GAP);
                continue;
            }
        }
        let (Some(r), Some(sub)) = (radio.as_ref(), sub.as_ref()) else { continue };

        // ── Drop links that have gone quiet ──────────────────────────────
        //
        // ⛔ **Disconnected, not just forgotten.** A link that stops
        // delivering can still be UP in the dongle — and a controller that is
        // still connected at the radio level never calls again. Forgetting it
        // here stranded the pad exactly as a failed setup did: searching,
        // unreachable, until power-cycled.
        //
        // ❗ Only a pad that streams (see `streams`): an Xbox or generic HID
        // pad sends a report when something changes and nothing while it lies
        // still, so silence there is not a dead link. Its link going down is
        // (`DisconnectionComplete`).
        links.retain(|l| {
            let alive = !streams(l.family) || l.last.elapsed() < STALE;
            if !alive {
                eprintln!("[bt-classic] {} disconnected", keystore::format_addr(l.addr));
                shared.pads.lock().unwrap().remove(&l.addr);
                let _ = r.with_dongle(|d| d.disconnect(l.conn));
            }
            alive
        });

        // ── Bring up anything paired but not yet connected ───────────────
        //
        // ⭐ One attempt per pass, not a loop over every missing address: each
        // attempt costs `CONNECT_PATIENCE` of this thread, and trying four
        // switched-off controllers back to back would stall the ones that are
        // on for twelve seconds.
        // ⛔ **Never black out a radio somebody else is using.**
        //
        // A page holds an exclusive lease, and an exclusive lease stops the
        // router — so every OTHER transport's link goes unserviced for its
        // whole duration. Paging a switched-off controller every 20 s for 2 s
        // at a time froze connected Joy-Cons for two seconds in every twenty,
        // reported as random dropouts and chased as radio instability. Neither
        // link was faulty; this transport was starving the other one on a
        // timer.
        //
        // ❗ Deliberately generous. Half a second of silence means the other
        // transport really has stopped, not that we caught it between packets:
        // a streaming controller sends every few milliseconds, so this cannot
        // be tripped by ordinary jitter, and being wrong costs only a delayed
        // reconnection of a pad that is switched off anyway.
        let someone_streaming = r.quiet_for() < Duration::from_millis(500);
        if someone_streaming && links.is_empty() {
            if !shared.yielded_radio.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "[bt-classic] another transport is streaming — not paging                      while it is, so its link is not interrupted."
                );
            }
            next_try = Instant::now() + Duration::from_secs(2);
        } else {
            shared.yielded_radio.store(false, Ordering::Relaxed);
        }
        if !someone_streaming && Instant::now() >= next_try && links.len() < MAX_LINKS {
            // With a link up the lease belongs to the controller that is
            // actually playing; with none, to listening for one.
            let gap = if links.is_empty() { PAGE_EAGER } else { PAGE_FALLBACK };
            // ⛔ Set from the END of the attempt, below — never here. Timing
            // the gap from the start subtracts the page's own patience from it,
            // and a 3 s gap behind a 2 s page leaves one second of listening in
            // every three. Measured, it left none at all.
            next_try = Instant::now() + gap;
            let missing = known
                .iter()
                .filter_map(|(t, p)| keystore::parse_addr(t).map(|a| (a, p.key)))
                .find(|(a, _)| !links.iter().any(|l| l.addr == *a) && !unusable(&devices, *a));
            if let Some((addr, key)) = missing {
                // ❗ A CID pair per SLOT. Channel ids are scoped to their ACL
                // link so reuse would in fact be legal, but distinct ids make a
                // stray packet impossible to misattribute while several links
                // are being set up — and cost nothing.
                let base = 0x0040 + (links.len() as u16) * 2;
                if trace() {
                    eprintln!("[bt-classic] paging {}", keystore::format_addr(addr));
                }
                let text = keystore::format_addr(addr);
                let outcome = connect(sub, addr, key, base, devices.get(&text).cloned());
                next_try = Instant::now() + gap;
                match outcome {
                    Ok(mut link) => {
                        link.name = known.get(&text).and_then(|p| p.name.clone());
                        match start_link(&mut link, adapter) {
                            Ok(()) => {
                                eprintln!("[bt-classic] {text} connected");
                                last_note = None;
                                links.push(link);
                            }
                            Err(e) => {
                                if said_unusable.insert(addr) {
                                    eprintln!("[bt-classic] {text} connected but can't be used: {e}");
                                }
                                let _ = r.with_dongle(|d| d.disconnect(link.conn));
                            }
                        }
                    }
                    Err(ref e) if trace() => {
                        eprintln!("[bt-classic] page {} failed: {e}",
                            keystore::format_addr(addr));
                        let due = last_note
                            .map(|t: Instant| t.elapsed() > NOTE_REPEAT)
                            .unwrap_or(true);
                        if due {
                            last_note = Some(Instant::now());
                        }
                    }
                    Err(_) => {
                        // ⭐ Not an error, and no longer reported as one.
                        //
                        // ❗ A page failing means the controller is not
                        // listening right now, which for a switched-off pad is
                        // every single time. The old message said "did not
                        // answer" once and then went quiet forever, which reads
                        // exactly like the transport gave up — while it was in
                        // fact still waiting, and would still have accepted the
                        // controller the moment it was switched on.
                        //
                        // What is worth saying is what is actually true: we are
                        // listening. Repeated on a slow timer so it stays
                        // visible without becoming noise.
                        let due = last_note
                            .map(|t: Instant| t.elapsed() > NOTE_REPEAT)
                            .unwrap_or(true);
                        if due {
                            last_note = Some(Instant::now());
                            eprintln!(
                                "[bt-classic] waiting for {} — switch it on and it \
                                 will connect itself.",
                                keystore::format_addr(addr),
                            );
                        }
                    }
                }
            }
        }

        let streaming = shared
            .pads
            .lock()
            // A pad that reports only on change and has reported once is
            // streaming, quiet or not — see the staleness rule above.
            .map(|p| p.values().filter(|s| !streams(s.family) || s.last.elapsed() < STALE).count())
            .unwrap_or(0);
        set_status(
            shared,
            Status::Running { paired: known.len(), connected: links.len(), streaming },
        );

        // ── A pairing search, while the UI wants one ─────────────────────
        //
        // ❗ Done HERE rather than anywhere else because this thread owns the
        // radio. It runs until a controller pairs or the person stops it — the
        // window's Cancel, or closing the window — never on its own.
        if shared.pair_requested.swap(false, Ordering::Relaxed) && search.is_none() {
            search = Some(Search::default());
            set_phase(shared, PairPhase::Searching(None));
        }
        if shared.pair_cancel.swap(false, Ordering::Relaxed) && search.take().is_some() {
            r.with_dongle(|d| d.cancel_inquiry());
            set_phase(shared, PairPhase::Idle);
        }
        let mut finished: Option<PairPhase> = None;
        if let Some(s) = search.as_mut() {
            // ⛔ No background page meanwhile: an inquiry blocks paging outright.
            next_try = next_try.max(Instant::now() + Duration::from_secs(1));
            if !s.running && s.next_round.is_none_or(|t| Instant::now() >= t) {
                s.heard.clear();
                s.round_over = false;
                match r.with_dongle(|d| {
                    d.set_inquiry_mode(0x01).and_then(|()| d.start_inquiry(SEARCH_ROUND))
                }) {
                    Ok(_) => s.running = true,
                    Err(e) => {
                        s.next_round = Some(Instant::now() + Duration::from_secs(1));
                        if trace() {
                            eprintln!("[bt-classic] inquiry not started: {e}");
                        }
                    }
                }
            }
            if let Some(found) = s.pick(&known) {
                let began = Instant::now();
                let outcome = pair_one(r, shared, &found);
                // The pairing's lease stopped the round.
                s.running = false;
                match outcome {
                    Paired::Done(label) => finished = Some(PairPhase::Done(label)),
                    Paired::Stop(msg) => finished = Some(PairPhase::Failed(msg)),
                    Paired::Retry(msg) => set_phase(shared, PairPhase::Searching(Some(msg))),
                }
                // ⛔ **Do not count our own pairing against the pads.**
                //
                // A pairing holds the radio for up to half a minute, and
                // `STALE` is three seconds — so pairing a second controller
                // silently killed every controller already connected, which is
                // exactly what a live trace showed: link up at 1.7 s, pairing
                // requested, "disconnected" a few seconds later, with the pad
                // still sitting there perfectly happy.
                //
                // The pad was not quiet; WE were not listening. Pushing `last`
                // forward by the time we spent elsewhere makes staleness mean
                // what it is supposed to mean.
                let held = began.elapsed();
                for l in &mut links {
                    l.last += held;
                }
                if let Ok(mut pads) = shared.pads.lock() {
                    for p in pads.values_mut() {
                        p.last += held;
                    }
                }
            }
        }
        if let Some(phase) = finished {
            search = None;
            set_phase(shared, phase);
            continue;
        }

        // ── Answer anything calling us ───────────────────────────────────
        //
        // Checked before the ACL read: a Connection Request that is not
        // answered promptly is abandoned by the remote, and the pad then
        // retries from scratch — which is exactly the "blinks and searches
        // again" loop.
        while let Some(evt) = sub.recv_event(Duration::from_millis(1)) {
            // Temporary trace: FLEXINPUT_BT_TRACE=1 prints every event the
            // subscription delivers, so "the controller never called" can be
            // told apart from "the call was delivered and dropped".
            if trace() {
                eprintln!("[bt-classic] {:>7} evt {evt:?}", ms());
            }
            let address = match evt {
                flexinput_btle::Event::ConnectionRequest { address, .. } => address,
                // A pairing search's answers — see `Search`.
                flexinput_btle::Event::InquiryResults(found) => {
                    if let Some(s) = search.as_mut() {
                        s.hear(found);
                    }
                    continue;
                }
                flexinput_btle::Event::InquiryComplete { .. } => {
                    if let Some(s) = search.as_mut() {
                        s.running = false;
                        s.round_over = true;
                    }
                    continue;
                }
                flexinput_btle::Event::CommandStatus { status, opcode }
                    if opcode == flexinput_btle::Opcode::INQUIRY && status != 0 =>
                {
                    if let Some(s) = search.as_mut() {
                        s.running = false;
                        s.next_round = Some(Instant::now() + Duration::from_secs(1));
                    }
                    continue;
                }
                // ⭐ A link that went down is dropped NOW, not after `STALE`.
                // Until it is, a controller calling straight back is taken for
                // one that is already connected and ignored below.
                flexinput_btle::Event::DisconnectionComplete { conn_handle, reason } => {
                    if let Some(i) = links.iter().position(|l| l.conn == conn_handle) {
                        let gone = links.remove(i);
                        eprintln!(
                            "[bt-classic] {} disconnected (reason {reason:#04x})",
                            keystore::format_addr(gone.addr)
                        );
                        shared.pads.lock().unwrap().remove(&gone.addr);
                    }
                    continue;
                }
                // ⛔ **A link nobody is tracking is closed.** An incoming link
                // whose setup gave up before `Connection Complete` arrived
                // comes up anyway, moments later, with no one waiting for it —
                // and a controller connected to a link we ignore can never
                // call again. Closing it lets it retry cleanly.
                flexinput_btle::Event::ConnectionComplete { status: 0, conn_handle, address }
                    if !links.iter().any(|l| l.conn == conn_handle) =>
                {
                    eprintln!(
                        "[bt-classic] {} came up untracked — closing it so it can call again",
                        keystore::format_addr(address)
                    );
                    let _ = r.with_dongle(|d| d.disconnect(conn_handle));
                    continue;
                }
                _ => continue,
            };
            let text = keystore::format_addr(address);
            let Some(key) = known.get(&text).map(|p| p.key) else {
                // ❗ Not ours. Left unanswered rather than rejected: a refusal
                // is a definite "go away" to a device that may be trying to
                // reach a completely different host, and silence costs it only
                // one retry.
                continue;
            };
            // Ours, but nothing here reads it: left unanswered the same way.
            if unusable(&devices, address) {
                continue;
            }
            // ⛔ **A controller that is calling us is NOT connected to us.**
            // It cannot page while it holds a link, so any link we still have
            // for it is dead and only not yet timed out. Ignoring the call on
            // the strength of that dead link threw away the reconnection.
            if let Some(i) = links.iter().position(|l| l.addr == address) {
                let old = links.remove(i);
                shared.pads.lock().unwrap().remove(&old.addr);
                let _ = r.with_dongle(|d| d.disconnect(old.conn));
            }
            eprintln!("[bt-classic] {text} is calling — accepting");
            let base = 0x0040 + (links.len() as u16) * 2;
            // ❗ Accept and authenticate under ONE lease.
            //
            // Accepting in its own lease and then taking a second one to wait
            // for the reply is a race the shared reader wins about as often as
            // not: `Connection Complete` can arrive in the gap, get broadcast,
            // and never be seen by the code waiting for it — which shows up as
            // a reconnect that times out for no visible reason.
            match adopt(sub, address, key, base, devices.get(&text).cloned()) {
                Ok(mut link) => {
                    link.name = known.get(&text).and_then(|p| p.name.clone());
                    match start_link(&mut link, adapter) {
                        Ok(()) => {
                            eprintln!("[bt-classic] {text} connected (incoming)");
                            last_note = None;
                            links.push(link);
                        }
                        Err(e) => {
                            if said_unusable.insert(address) {
                                eprintln!("[bt-classic] {text} connected but can't be used: {e}");
                            }
                            let _ = r.with_dongle(|d| d.disconnect(link.conn));
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[bt-classic] {text} incoming link failed: {e}");
                    // ⛔ **Listen, do not page.** A controller whose incoming
                    // link just failed goes straight back to calling us, and a
                    // radio that is paging cannot hear it — the page times out
                    // (`0x04`) while the pad's own call goes unanswered. Traced:
                    // exactly that, immediately after a failed setup.
                    next_try = Instant::now() + PAGE_EAGER;
                }
            }
        }

        // ── Ask for stick calibration on any link still without it ───────
        //
        // ⭐ Fired into the stream rather than waited on; the replies are
        // collected below as they pass. A link that answers nothing after
        // `SPI_ATTEMPTS` rounds keeps the fallback numbers and streams
        // perfectly well — it simply keeps the square response, which is what
        // every Classic link did before this existed.
        for link in links.iter_mut() {
            if link.family != Family::SwitchPro
                || link.calib.is_some()
                || !link.reported
                || link.spi.attempts >= SPI_ATTEMPTS
            {
                continue;
            }
            if link.spi.next_try.is_some_and(|t| Instant::now() < t) {
                continue;
            }
            link.spi.attempts += 1;
            link.spi.next_try = Some(Instant::now() + SPI_RETRY);
            for (i, (addr, len)) in SWITCH_PRO_CALIB_READS.iter().enumerate() {
                if link.spi.blobs[i].is_some() {
                    continue;
                }
                // `0xa2` is the HID DATA header on the OUTPUT pipe, the
                // counterpart of the `0xa1` every input report carries.
                let mut frame = Vec::with_capacity(65);
                frame.push(0xA2);
                let counter = link.next_counter();
                frame.extend_from_slice(&switch_pro_spi_request(*addr, *len, counter));
                let _ = r.with_dongle(|d| {
                    d.send_att_raw(link.conn, link.interrupt.remote_cid, &frame)
                });
            }
        }

        // ── Vibration on, then rumble ────────────────────────────────────
        //
        // Fire-and-forget like the SPI reads: the ack is picked out of the
        // input stream below. Rumble goes out on change, on a one-second
        // heartbeat, or continuously while amplitude modulation is running —
        // the same policy as the HID path, from the same encoder.
        let staged = shared.outputs.lock().map(|o| o.clone()).unwrap_or_default();
        for link in links.iter_mut() {
            if link.family != Family::SwitchPro || !link.reported {
                continue;
            }
            if !link.vib_on
                && link.vib_tries < VIB_ATTEMPTS
                && link.vib_next.is_none_or(|t| Instant::now() >= t)
            {
                link.vib_tries += 1;
                link.vib_next = Some(Instant::now() + VIB_RETRY);
                let mut frame = Vec::with_capacity(65);
                frame.push(0xA2);
                let counter = link.next_counter();
                frame.extend_from_slice(&switch_pro_enable_vibration(counter));
                let _ = r.with_dongle(|d| {
                    d.send_att_raw(link.conn, link.interrupt.remote_cid, &frame)
                });
            }
            let Some(out) = staged.get(&link.addr).copied() else { continue };
            let since = link.rumble_at.map(|t| t.elapsed());
            if since.is_some_and(|d| d < RUMBLE_MIN_GAP) {
                continue;
            }
            let changed = link.rumble_sent != Some(out);
            let heartbeat = since.is_none_or(|d| d >= RUMBLE_HEARTBEAT);
            if !changed && !heartbeat {
                continue;
            }
            let counter = link.next_counter();
            let (report, am) = switch_rumble_report(&out, counter);
            let mut frame = Vec::with_capacity(65);
            frame.push(0xA2);
            frame.extend_from_slice(&report);
            let _ = r.with_dongle(|d| d.send_att_raw(link.conn, link.interrupt.remote_cid, &frame));
            // A modulated rumble differs every packet: never "unchanged".
            link.rumble_sent = if am { None } else { Some(out) };
            link.rumble_at = Some(Instant::now());
        }

        // ── Sony: full reports, then outputs ─────────────────────────────
        //
        // ⭐ A DualShock 4 or DualSense on Bluetooth sends a short report with
        // no motion or touch until the host reads its calibration (feature
        // `0x05`), which is what switches it to the full one. Asked until the
        // full reports arrive. Outputs follow the Switch Pro's policy: on
        // change, on a heartbeat, never faster than the gap — a whole output
        // report each time, as the HID path sends it.
        for link in links.iter_mut() {
            let Family::Sony { dualsense, .. } = link.family else { continue };
            if !link.full
                && link.full_tries < FULL_ATTEMPTS
                && link.full_next.is_none_or(|t| Instant::now() >= t)
            {
                link.full_tries += 1;
                link.full_next = Some(Instant::now() + FULL_RETRY);
                link.ctl_queue.push_back(Ctl {
                    frame: hidp::get_report(hidp::REPORT_FEATURE, 0x05),
                    answer: Answer::Ignore,
                });
            }
            let Some(out) = staged.get(&link.addr).copied() else { continue };
            let since = link.rumble_at.map(|t| t.elapsed());
            if !link.full || since.is_some_and(|d| d < RUMBLE_MIN_GAP) {
                continue;
            }
            if link.rumble_sent == Some(out) && since.is_some_and(|d| d < RUMBLE_HEARTBEAT) {
                continue;
            }
            let report = sony_bt_output(&out, dualsense, &mut link.sony_seq, &mut link.lightbar_set_up);
            let frame = hidp::output(&report);
            let _ = r.with_dongle(|d| d.send_att_raw(link.conn, link.interrupt.remote_cid, &frame));
            link.rumble_sent = Some(out);
            link.rumble_at = Some(Instant::now());
        }

        // ── Xbox: rumble ─────────────────────────────────────────────────
        for link in links.iter_mut().filter(|l| l.family == Family::Xbox) {
            let Some(out) = staged.get(&link.addr).copied() else { continue };
            let since = link.rumble_at.map(|t| t.elapsed());
            if since.is_some_and(|d| d < XBOX_RUMBLE_GAP) {
                continue;
            }
            if link.rumble_sent == Some(out) && since.is_some_and(|d| d < RUMBLE_HEARTBEAT) {
                continue;
            }
            let frame = hidp::output(&xbox_bt_rumble(&out));
            let _ = r.with_dongle(|d| d.send_att_raw(link.conn, link.interrupt.remote_cid, &frame));
            link.rumble_sent = Some(out);
            link.rumble_at = Some(Instant::now());
        }

        // ── The host's requests (Linux passthrough), then the control queue
        for link in links.iter_mut() {
            #[cfg(any(target_os = "linux", windows))]
            serve_host(r, link);
            pump_control(r, link);
        }

        // ── Service every link from ONE read ─────────────────────────────
        //
        // The whole point of the restructure: reports for all connected
        // controllers arrive interleaved on the same transport, so there is one
        // reader and it routes by connection handle. A blocking read per pad
        // meant the second controller was never serviced at all.
        // ⭐ A BATCH per pass, not one packet. A Pro Controller sends about 220
        // reports a second; taking one per loop iteration cannot keep up with
        // that, so the shared radio's queue fills and the oldest input is
        // dropped — the controller looks connected and stutters or stalls.
        for _ in 0..64 {
            let Some(pkt) = sub.recv_acl(Duration::from_millis(2)) else {
                break;
            };
            let Some(link) = links.iter_mut().find(|l| l.conn == pkt.conn_handle) else {
                continue;
            };
            // ❗ L2CAP signalling must be ANSWERED, not skipped.
            //
            // Everything that was not an input report used to be discarded
            // here, including the remote's own configuration and disconnection
            // requests. A controller that asks something and is met with
            // silence tears the channel down — which presents as a link that is
            // up and delivers nothing, exactly what the panel reported while no
            // device appeared.
            if pkt.cid == l2cap::CID_SIGNALLING {
                answer_signalling(r, link, &pkt);
                continue;
            }
            // The answer to a report read or write — see `pump_control`.
            if pkt.cid == link.control.local_cid {
                control_reply(link, &pkt.payload);
                continue;
            }
            if pkt.cid != link.interrupt.local_cid {
                continue;
            }
        // The leading byte is the HID transaction header (`0xa1` = DATA on the
        // input pipe), not part of the report. Handing it to the parser shifts
        // every field by one and yields plausible nonsense.
            if pkt.payload.len() < 2 || pkt.payload[0] != 0xA1 {
                continue;
            }
            if link.family != Family::SwitchPro {
                let report = &pkt.payload[1..];
                match link.family {
                    Family::Sony { dualsense, .. } => {
                        // Any report proves the link alive, the short one the
                        // pad sends before it is asked for the full one too.
                        link.last = Instant::now();
                        let Some(g) = parse_sony_bt(report, dualsense) else { continue };
                        if !link.full {
                            link.full = true;
                            if trace() {
                                eprintln!(
                                    "[bt-classic] {} sending full reports",
                                    keystore::format_addr(link.addr)
                                );
                            }
                        }
                        took_report(shared, link, g);
                    }
                    Family::Xbox => {
                        link.last = Instant::now();
                        let Some(g) = link.xbox.take(report) else { continue };
                        took_report(shared, link, g);
                    }
                    Family::Passthrough => {
                        #[cfg(any(target_os = "linux", windows))]
                        if let Some(p) = &link.passthrough {
                            p.input(report);
                        }
                        took_report(shared, link, HidReading::default());
                    }
                    Family::SwitchPro | Family::Unsupported => {}
                }
                continue;
            }
            // Vibration acknowledged — subcommand id echoed at byte 14 of the
            // `0x21` reply (byte 15 of the payload, behind the DATA header).
            if pkt.payload[1] == 0x21 && !link.vib_on && pkt.payload.get(15) == Some(&0x48) {
                link.vib_on = true;
                if trace() {
                    eprintln!(
                        "[bt-classic] {} vibration enabled",
                        keystore::format_addr(link.addr)
                    );
                }
            }
            // ⭐ A subcommand acknowledgement, not an input report. These carry
            // the stick calibration and arrive interleaved with the input
            // stream, which is why they are matched by echoed address rather
            // than by being "the next reply".
            if pkt.payload[1] == 0x21 && link.calib.is_none() {
                let report = &pkt.payload[1..];
                for (i, (addr, len)) in SWITCH_PRO_CALIB_READS.iter().enumerate() {
                    if link.spi.blobs[i].is_some() {
                        continue;
                    }
                    if let Some(data) = switch_pro_spi_reply(report, *addr, *len) {
                        link.spi.blobs[i] = Some(data.to_vec());
                    }
                }
                // ⛔ **Wait for all four, not just the factory pair.**
                //
                // Building the moment the two FACTORY blobs land throws away
                // the USER calibration, which arrives in its own reply a moment
                // later — and once `calib` is Some this whole block is skipped,
                // so it is discarded permanently. A controller recalibrated on
                // a Switch stores its corrected centre and travel there, so the
                // symptom is one stick right and the other reaching further one
                // way than the other: exactly the axis whose real centre is not
                // where the factory said it was.
                //
                // Giving up is still allowed — but only once the retries are
                // spent, and then it is a deliberate fallback rather than a
                // race that the factory blobs happen to win every time.
                let all = link.spi.blobs.iter().all(|b| b.is_some());
                let out_of_tries = link.spi.attempts >= SPI_ATTEMPTS
                    && link.spi.blobs[0].is_some()
                    && link.spi.blobs[1].is_some();
                if all || out_of_tries {
                    link.calib = switch_pro_calib_from_spi(
                        link.spi.blobs[0].as_deref(),
                        link.spi.blobs[1].as_deref(),
                        link.spi.blobs[2].as_deref(),
                        link.spi.blobs[3].as_deref(),
                    );
                    if let Some(c) = &link.calib {
                        // ⭐ The NUMBERS, not just "it worked". An unbalanced
                        // stick is a wrong centre or a wrong endpoint, and
                        // those are two different bugs that a success line
                        // cannot tell apart.
                        let user = |i: usize| {
                            link.spi.blobs[i]
                                .as_deref()
                                .is_some_and(|d| d.len() >= 2 && d[0] == 0xB2 && d[1] == 0xA1)
                        };
                        eprintln!(
                            "[bt-classic] {} stick calibration: L x {}..{}..{} y {}..{}..{} | \
                             R x {}..{}..{} y {}..{}..{} (user cal L {} R {})",
                            keystore::format_addr(link.addr),
                            c.l_x.min, c.l_x.center, c.l_x.max,
                            c.l_y.min, c.l_y.center, c.l_y.max,
                            c.r_x.min, c.r_x.center, c.r_x.max,
                            c.r_y.min, c.r_y.center, c.r_y.max,
                            user(2), user(3),
                        );
                    }
                }
                continue;
            }
            let Some(reading) = parse_switch_pro_report_calibrated(
                &pkt.payload[1..],
                link.calib.as_ref(),
            ) else {
                continue;
            };
            took_report(shared, link, reading);
        }
    }

    // ⭐ Hand every controller back before the thread dies.
    //
    // ❗ The link lives in the DONGLE. Exiting without disconnecting leaves the
    // pad believing it is still connected, after which it neither pages nor
    // advertises nor answers an inquiry — invisible in every direction, with
    // power-cycling the controller as the user's only recourse. This is the
    // difference between "quit and relaunch" working and not.
    if let (Some(r), false) = (radio.as_ref(), links.is_empty()) {
        eprintln!("[bt-classic] disconnecting {} controller(s)", links.len());
        r.with_dongle(|d| {
            for l in &links {
                let _ = d.disconnect(l.conn);
            }
            // Let the disconnections actually go out; dropping the handle the
            // instant after queueing them can lose them.
            let deadline = Instant::now() + Duration::from_millis(400);
            while Instant::now() < deadline {
                match d.read_event_timeout(Duration::from_millis(50)) {
                    Ok(Some(_)) => {}
                    _ => break,
                }
            }
        });
    }
}

fn set_phase(shared: &Shared, p: PairPhase) {
    if let Ok(mut g) = shared.pair_phase.lock() {
        *g = p;
    }
}

/// How long one inquiry round of a pairing search runs. Another starts as
/// each ends, until a controller pairs or the search is cancelled.
const SEARCH_ROUND: f32 = 6.4;

/// A pairing search: inquiry rounds until a controller in pairing mode
/// answers, then pairing it.
///
/// ⭐ Picks by Class of Device. Bonding a controller REPLACES the host it
/// belongs to, so choosing the wrong device here means walking to a console
/// and re-pairing it — the reason this is one deliberate button and not
/// something that happens on its own.
///
/// ⭐ **Rounds on the shared radio, not one long lease.** A search runs until
/// a person stops it, and an exclusive lease that long would stop every other
/// link on the radio for as long, Joy-Cons included. The rounds are started
/// and their answers read off the subscription like any other event; only
/// pairing what they found takes the radio.
#[derive(Default)]
struct Search {
    /// A round is running.
    running: bool,
    /// When to start the next round, after the radio refused one.
    next_round: Option<Instant>,
    /// Gamepads heard this round.
    heard: HashMap<[u8; 6], flexinput_btle::hci::InquiryResult>,
    /// The round has ended: already-paired pads heard in it may be picked.
    round_over: bool,
}

impl Search {
    fn hear(&mut self, results: Vec<flexinput_btle::hci::InquiryResult>) {
        for r in results {
            // ⭐ Every answer, not just the ones that qualify. "No controller
            // found" has two very different causes — nothing on the air, or
            // something on the air that was filtered out — and they need
            // opposite fixes.
            if trace() {
                eprintln!(
                    "[bt-classic]   heard {} class {:02x?} rssi {:?} gamepad={}",
                    keystore::format_addr(r.address),
                    r.class_of_device,
                    r.rssi,
                    r.looks_like_a_gamepad(),
                );
            }
            if r.looks_like_a_gamepad() {
                self.heard.insert(r.address, r);
            }
        }
    }

    /// The controller to pair now, if any.
    ///
    /// ⭐ An unpaired one the INSTANT it answers.
    ///
    /// ❗ A device answers a page only while it is page-scanning, and a
    /// controller in pairing mode cycles through that state rather than
    /// sitting in it. Running the inquiry to its full length and paging
    /// afterwards spends exactly the window in which the answer was easy —
    /// which is why a controller that had plainly just replied to the inquiry
    /// then failed to be paged with `Page Timeout`. Taking it at once puts the
    /// page out while it is still listening for one.
    ///
    /// ⛔ **Already-paired controllers are CANDIDATES, not exclusions.**
    ///
    /// They used to be filtered out entirely, on the reasoning that re-bonding
    /// a working pad by accident would be bad. The effect was that the single
    /// case most in need of this button — a stored key that no longer
    /// matches, after a dongle swap, a re-pair on a console, or a key file
    /// copied between machines — was the one case it refused to handle. The
    /// user held Sync, the controller answered the inquiry, and pairing
    /// reported "No new controller found" about a controller sitting right in
    /// front of them.
    ///
    /// The original worry is met by ORDERING rather than by exclusion: a
    /// paired pad waits for the end of the round, so an unpaired one heard in
    /// it still wins, and within each group the strongest signal wins — the
    /// pad the user just picked up is the near one.
    fn pick(
        &mut self,
        known: &std::collections::BTreeMap<String, keystore::Pairing>,
    ) -> Option<flexinput_btle::hci::InquiryResult> {
        let paired = |r: &flexinput_btle::hci::InquiryResult| {
            known.contains_key(&keystore::format_addr(r.address))
        };
        let best = self
            .heard
            .values()
            .filter(|r| self.round_over || !paired(r))
            .min_by_key(|r| (paired(r), -(r.rssi.unwrap_or(-127) as i32)))
            .cloned();
        if best.is_some() {
            self.heard.clear();
        }
        best
    }
}

/// How pairing one controller went.
enum Paired {
    Done(String),
    /// An outcome the search ends on: the controller paired but can't be
    /// used, or its key couldn't be kept.
    Stop(String),
    /// It didn't pair; the search carries on.
    Retry(String),
}

/// Pair a controller the search found: stop the round, page and bond it, ask
/// what it is, keep the key.
fn pair_one(
    radio: &flexinput_btle::radio::Radio,
    shared: &Shared,
    r: &flexinput_btle::hci::InquiryResult,
) -> Paired {
    let addr = r.address;
    let text = keystore::format_addr(addr);
    set_phase(shared, PairPhase::Pairing(text.clone()));
    // ⭐ ONE lease across stopping the round AND the page. Releasing between
    // them would let the shared reader consume the very Connection Complete
    // the page is waiting for — and an inquiry that has to be cancelled
    // cleanly before a page can go out is a single conversation, not two.
    let lease = radio.exclusive();
    let dongle = lease.dongle();
    dongle.cancel_inquiry();
    // Recorded WITH the bond: a key only works for the adapter that made it.
    let adapter = dongle.read_bd_addr().ok();

    let mut quiet = |_: &str| {};
    // ⭐ Retried, because a Page Timeout is an ordinary event rather than a
    // verdict. The remote has to be page-scanning at the moment the page lands,
    // and a controller alternating between scanning and paging its old host
    // will miss some attempts however well timed. Three tries turns a coin flip
    // into a near certainty; reporting the first failure to the user does not.
    let mut outcome = Err(String::new());
    for attempt in 1..=3 {
        match dongle.page_and_pair(
            addr,
            r.page_scan_repetition_mode,
            r.clock_offset,
            None,
            Duration::from_secs(12),
            &mut quiet,
        ) {
            Ok(link) => {
                outcome = Ok(link);
                break;
            }
            Err(e) => {
                let msg = e.to_string();
                // Only a timeout is worth retrying. A refusal is a decision the
                // remote made, and asking again just annoys it.
                let retryable = msg.contains("0x04") || msg.contains("timed out");
                outcome = Err(msg);
                // Nor after the person stopped the search.
                if !retryable || attempt == 3 || shared.pair_cancel.load(Ordering::Relaxed) {
                    break;
                }
                set_phase(shared, PairPhase::Pairing(format!("{text} (attempt {})", attempt + 1)));
                std::thread::sleep(Duration::from_millis(600));
            }
        }
    }
    match outcome {
        Ok(link) => {
            let name = dongle
                .remote_name(addr, r.page_scan_repetition_mode, r.clock_offset)
                .ok()
                .filter(|n| !n.is_empty());
            let label = name.clone().unwrap_or_else(|| text.clone());
            // ⭐ What it is, asked while the link is up: its ids say how it is
            // read, and its descriptor is what the system reads it by on
            // Linux. A pad that doesn't answer is asked again when it next
            // connects (`bring_up`).
            let info = dongle
                .sdp_identify(link.conn_handle, SDP_CID_PAIRING, &mut quiet)
                .ok()
                .map(device_info);
            let family = family_of(info.as_ref());
            if family == Family::Unsupported {
                let _ = dongle.disconnect(link.conn_handle);
                return Paired::Stop(format!(
                    "{label} paired, but {} — pair it with this computer's own Bluetooth again to use it there.",
                    unsupported_reason()
                ));
            }
            if let Some(i) = &info {
                if let Err(e) = keystore::put_device(addr, i) {
                    eprintln!("[bt-classic] {text}: device record not saved: {e}");
                }
            }
            let result = match link.link_key {
                Some(k) => match keystore::put(addr, k, name.as_deref(), adapter) {
                    // Paired and kept; one that waits on usbip-win2 says so.
                    Ok(_) => Paired::Done(match not_readable(family) {
                        Some(why) => format!("{label}, but {why}"),
                        None => label,
                    }),
                    // The bond already replaced the controller's old host, so a
                    // key that did not save is a real loss, not a retry.
                    Err(e) => Paired::Stop(format!("Paired, but the key could not be saved: {e}")),
                },
                None => Paired::Retry("Paired but no link key arrived".into()),
            };
            let _ = dongle.disconnect(link.conn_handle);
            result
        }
        Err(e) => Paired::Retry(format!("{text} did not pair after 3 tries: {e}")),
    }
}

/// Finish a link the REMOTE started: authenticate, encrypt, open HID.
///
/// The same path as [`connect`] minus the paging — see `NO_PAGE`.
fn adopt(
    sub: &flexinput_btle::radio::Subscriber,
    addr: [u8; 6],
    key: [u8; 16],
    cid_base: u16,
    info: Option<keystore::DeviceInfo>,
) -> Result<Link, String> {
    bring_up(sub, addr, key, cid_base, flexinput_btle::NO_PAGE, 0x0000, info)
}

/// Page a known device and bring its HID interrupt channel up.
fn connect(
    sub: &flexinput_btle::radio::Subscriber,
    addr: [u8; 6],
    key: [u8; 16],
    cid_base: u16,
    info: Option<keystore::DeviceInfo>,
) -> Result<Link, String> {
    // ⭐ Page scan repetition mode R2 and an UNKNOWN clock offset.
    //
    // A bonded controller that is simply switched on is not discoverable, so
    // there is no fresh inquiry result to take these from. R2 with a zero
    // offset is the conservative pair: the radio listens across the full
    // page-scan window instead of assuming it knows when the remote wakes.
    bring_up(sub, addr, key, cid_base, 0x02, 0x0000, info)
}

/// Where a pairing run opens its SDP channel. Its link is new and carries
/// nothing else yet.
const SDP_CID_PAIRING: u16 = 0x0070;

/// A device record from an SDP answer; zeros and an empty descriptor for
/// what it didn't say.
fn device_info(r: flexinput_btle::sdp::DeviceRecord) -> keystore::DeviceInfo {
    keystore::DeviceInfo {
        vid: r.vid.unwrap_or(0),
        pid: r.pid.unwrap_or(0),
        version: r.version.unwrap_or(0),
        descriptor: r.descriptor.unwrap_or_default(),
    }
}

#[allow(clippy::too_many_arguments)]
fn bring_up(
    sub: &flexinput_btle::radio::Subscriber,
    addr: [u8; 6],
    key: [u8; 16],
    cid_base: u16,
    psrm: u8,
    clock: u16,
    mut info: Option<keystore::DeviceInfo>,
) -> Result<Link, String> {
    // Silent normally; under trace, the whole conversation. A setup that
    // fails at one step is unreadable without the steps that came before it.
    // See `ADOPT_PATIENCE`: a page is rationed, an incoming link is not.
    let patience = if psrm == flexinput_btle::NO_PAGE {
        ADOPT_PATIENCE
    } else {
        CONNECT_PATIENCE
    };
    let mut quiet = |m: &str| {
        if trace() {
            eprintln!("[bt-classic] {:>7}   {m}", ms());
        }
    };
    // ⭐ ONE lease for accept/page, pairing and both L2CAP channels. Each step
    // waits for a reply, and a lease released in between would hand those
    // replies to the shared reader instead.
    // ⛔ Reclaiming, not plain. The events of this very conversation may
    // already be on the bus — see `exclusive_reclaiming`.
    let lease = sub.exclusive_reclaiming();
    let dongle = lease.dongle();
    // An incoming link has to be accepted from INSIDE the lease, or its
    // `Connection Complete` can be broadcast before anyone is waiting for it.
    if psrm == flexinput_btle::NO_PAGE {
        dongle.accept_connection(addr).map_err(|e| e.to_string())?;
    }
    let link = dongle
        .page_and_pair(addr, psrm, clock, Some(key), patience, &mut quiet)
        .map_err(|e| e.to_string())?;
    // ⛔ **A key we were just given is SAVED, not dropped on the floor.**
    //
    // `page_and_pair` re-pairs from scratch whenever the remote asks it to —
    // which a controller sitting in Sync mode always does — and it hands the
    // new key back on the link. Only the pairing button ever stored one, so
    // every other path completed a real bond and then discarded the half of it
    // the host is responsible for keeping.
    //
    // The result is a controller that pairs perfectly and can never reconnect
    // again: its flash holds the new key, our file still holds the old one, and
    // every reconnection afterwards fails authentication with `0x05` — key
    // missing — from a host that is certain it has the key. Traced end to end;
    // it is what broke this bond in the first place.
    if let Some(fresh) = link.link_key.filter(|k| *k != key) {
        let adapter = dongle.read_bd_addr().ok();
        match keystore::put(addr, fresh, None, adapter) {
            Ok(_) => eprintln!(
                "[bt-classic] {} issued a new link key — saved",
                keystore::format_addr(addr)
            ),
            // Loud: the bond on the controller has ALREADY been replaced, so a
            // key that failed to save means re-pairing, not retrying.
            Err(e) => eprintln!(
                "[bt-classic] ⚠ {} re-paired but its new key could NOT be saved: {e}",
                keystore::format_addr(addr)
            ),
        }
    }
    // Control before interrupt — the HID profile expects that order and some
    // devices refuse the reverse.
    // ⭐ ONE path, whichever side called. `l2cap_hid` gives the remote a head
    // start and asks for anything it does not offer, so this no longer has to
    // guess which end of the link intends to open the channels — a guess that
    // was wrong in one direction or the other on every reconnection.
    let (control, interrupt) =
        match dongle.l2cap_hid(link.conn_handle, cid_base, Duration::from_secs(6), &mut quiet) {
            Ok(channels) => channels,
            Err(e) => {
                // ⛔ **Take the link down, or the controller is stranded.**
                //
                // By this point the link is up, authenticated and encrypted —
                // only the HID channels failed. Returning the error used to
                // leave that link standing: the controller was still connected
                // to us at the radio level, so it could not call again, and
                // this side no longer tracked it, so nothing would ever use or
                // close it. The pad sat "searching for its host" until it
                // switched itself off, and only restarting it gave a second
                // attempt. A clean disconnect lets it call straight back.
                let closed = dongle.disconnect_and_wait(link.conn_handle, Duration::from_millis(800));
                quiet(&format!(
                    "HID setup failed — link {}",
                    if closed { "closed so the controller can call again" } else { "close not confirmed" }
                ));
                return Err(e.to_string());
            }
        };
    // ⭐ What it is, asked once and kept: a pairing made before device
    // records existed, or one that didn't answer while pairing. Asked after
    // the HID channels so the pad's own setup isn't kept waiting; the few
    // reports that arrive meanwhile are dropped. One that won't say is read
    // as before, as a Switch Pro.
    if info.is_none() {
        // Clear of the HID pair's ids (`cid_base`, `cid_base + 1`) for every slot.
        match dongle.sdp_identify(link.conn_handle, cid_base + 0x20, &mut quiet) {
            Ok(rec) => {
                let d = device_info(rec);
                if let Err(e) = keystore::put_device(addr, &d) {
                    eprintln!("[bt-classic] {}: device record not saved: {e}", keystore::format_addr(addr));
                }
                info = Some(d);
            }
            Err(e) => quiet(&format!("SDP failed ({e}) — reading it as a Switch Pro")),
        }
    }
    Ok(Link {
        family: family_of(info.as_ref()),
        info,
        ctl_queue: VecDeque::new(),
        ctl_in_flight: None,
        full: false,
        full_tries: 0,
        full_next: None,
        sony_seq: 0,
        lightbar_set_up: false,
        xbox: XboxState::default(),
        #[cfg(any(target_os = "linux", windows))]
        passthrough: None,
        addr,
        conn: link.conn_handle,
        control,
        interrupt,
        last: Instant::now(),
        name: None,
        reported: false,
        cadence: flexinput_btle::cadence::Cadence::new(),
        calib: None,
        spi: SpiProbe::default(),
        counter: 0,
        vib_on: false,
        vib_tries: 0,
        vib_next: None,
        rumble_sent: None,
        rumble_at: None,
    })
}

/// What a pad this backend lists is, by family: `None` for one it doesn't
/// list (a passthrough pad is listed by whichever backend reads it from the
/// system).
fn kind_of(family: Family) -> Option<ControllerKind> {
    match family {
        Family::SwitchPro => Some(ControllerKind::SwitchPro),
        Family::Sony { dualsense: true, .. } => Some(ControllerKind::DualSense),
        Family::Sony { dualsense: false, .. } => Some(ControllerKind::DualShock4),
        Family::Xbox => Some(ControllerKind::XInput),
        Family::Passthrough | Family::Unsupported => None,
    }
}

/// `btc:<kind slug>:<address hex>`, the shape the UI reads a pad's kind from.
fn device_id(kind: ControllerKind, addr: [u8; 6]) -> String {
    format!("btc:{}:{}", kind.id_slug(), addr.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// The address back out of a [`device_id`]; `None` for anybody else's id.
fn parse_device_id(id: &str) -> Option<[u8; 6]> {
    let (_, hex) = id.strip_prefix("btc:")?.split_once(':')?;
    if hex.len() != 12 {
        return None;
    }
    let mut addr = [0u8; 6];
    for (i, b) in addr.iter_mut().enumerate() {
        *b = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(addr)
}

impl DeviceBackend for ClassicBtBackend {
    fn name(&self) -> &'static str { "BT Classic" }

    fn enumerate(&mut self) -> Vec<PhysicalDevice> {
        let pads = self.shared.pads.lock().unwrap();
        pads.values()
            .filter(|p| !streams(p.family) || p.last.elapsed() < STALE)
            .filter_map(|p| {
                let kind = kind_of(p.family)?;
                // The Edge's back and Fn buttons come with its ids.
                let (vid, pid) = match p.family {
                    Family::Sony { edge: true, .. } => (Some(0x054C), Some(0x0DF2)),
                    _ => (None, None),
                };
                Some(PhysicalDevice {
                    id: device_id(kind, p.address),
                    // ⭐ The controller's OWN name, captured at pair time. A
                    // device list that reads "Pro Controller" beats one that
                    // reads a hard-coded guess, and with several pads it is the
                    // only way to tell them apart.
                    display_name: p
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("{} (dongle)", kind.display_name())),
                    kind,
                    outputs: layouts::outputs_for_pad(kind, vid, pid),
                    inputs: layouts::inputs_for(kind),
                    // No Windows HID node exists for it — the OS never bound a
                    // driver, which is the whole point — so there is nothing
                    // for HidHide to mask and nothing to look up.
                    instance_path: None,
                    vid: None,
                    pid: None,
                })
            })
            .collect()
    }

    fn send(&mut self, device_id: &str, pin_id: &str, signal: Signal) {
        let Some(addr) = parse_device_id(device_id) else { return };
        let f = match signal {
            Signal::Float(f) => f.clamp(0.0, 1.0),
            Signal::Bool(b) => if b { 1.0 } else { 0.0 },
            _ => return,
        };
        // Each pin in its own range, as the HID path scales it: rumble is a
        // linear byte, a Sony pad's trigger effects and LEDs are not.
        let byte = output_pin_byte(pin_id, f);
        if let Ok(mut outs) = self.shared.outputs.lock() {
            outs.entry(addr).or_default().stage(pin_id, byte);
        }
    }

    /// Applied to the pad's filter on the transport thread's side of the
    /// pads lock. A pad that hasn't sent a report yet has no filter to set;
    /// the settings arrive again next tick.
    fn set_spike_filter(&mut self, device_id: &str, enabled: bool, sensitivity_pct: f32, window: u8) {
        let Some(addr) = parse_device_id(device_id) else { return };
        let Ok(mut pads) = self.shared.pads.lock() else { return };
        if let Some(p) = pads.get_mut(&addr) {
            p.spike.bind_capture(device_id);
            p.spike.set_config(enabled, sensitivity_pct, window);
        }
    }

    fn take_event_counts(&mut self) -> Vec<(String, u32)> {
        let mut pads = self.shared.pads.lock().unwrap();
        pads.values_mut()
            .filter_map(|p| {
                let kind = kind_of(p.family)?;
                let n = std::mem::take(&mut p.events);
                Some((device_id(kind, p.address), n))
            })
            .collect()
    }

    fn poll(&mut self) -> Vec<(String, String, Signal)> {
        let pads = self.shared.pads.lock().unwrap();
        let mut out = Vec::with_capacity(pads.len() * 32);
        for p in pads.values().filter(|p| !streams(p.family) || p.last.elapsed() < STALE) {
            let Some(kind) = kind_of(p.family) else { continue };
            let dev = device_id(kind, p.address);
            let g = &p.reading;
            // Motion: the pads that have it.
            if matches!(p.family, Family::SwitchPro | Family::Sony { .. }) {
                for (pin, v) in [
                    ("gyro_x", g.gyro_x),
                    ("gyro_y", g.gyro_y),
                    ("gyro_z", g.gyro_z),
                    ("accel_x", g.accel_x),
                    ("accel_y", g.accel_y),
                    ("accel_z", g.accel_z),
                ] {
                    out.push((dev.clone(), pin.into(), Signal::Float(v)));
                }
            }
            match p.family {
                Family::Sony { dualsense, edge } => push_sony_reading(&mut out, &dev, g, dualsense, edge),
                // Same positional pins; see `XboxState`.
                Family::Xbox => push_sony_reading(&mut out, &dev, g, false, false),
                _ => {
                    if let Some(sb) = g.switch_buttons {
                        push_switch_pro_buttons(&mut out, &dev, &sb);
                    }
                    if let Some(b) = g.battery {
                        out.push((dev.clone(), "battery".into(), Signal::Float(b)));
                    }
                }
            }
        }
        out
    }
}

/// Pins a Switch Pro on the dongle publishes, for tests and for the device list.
pub fn outputs() -> Vec<DevicePin> {
    layouts::outputs_for(ControllerKind::SwitchPro)
}

#[cfg(test)]
mod tests {
    use super::*;
    // These check the REPORT layout, which calibration does not touch, so the
    // uncalibrated entry point is the right one here — and it now exists only
    // for tests, so no transport can reach for it by accident again.
    use crate::gyro::parse_switch_pro_report;

    /// One IMU frame from the real capture, repeated three times — the report
    /// carries three and the parser averages them, so identical frames give
    /// exactly the single-frame value.
    const IMU_FRAME: [u8; 12] = [
        0x90, 0xfd, 0x09, 0x00, 0x9f, 0x10, // accel  -624,   9, 4255
        0xfd, 0xff, 0x02, 0x00, 0xfc, 0xff, // gyro     -3,   2,   -4
    ];

    /// A real `0x30` report, captured off the interrupt channel of a Pro
    /// Controller connected through the dongle.
    fn real_report() -> Vec<u8> {
        let mut r = vec![
            0x30, // report id
            0xa2, // timer
            0x60, // battery / connection info
            0x00, 0x00, 0x00, // buttons: right, shared, left — none held
            0xec, 0xe7, 0x7e, // left stick, 12-bit packed
            0x3a, 0xb8, 0x79, // right stick
            0x0c, // vibrator ack
        ];
        for _ in 0..3 {
            r.extend_from_slice(&IMU_FRAME);
        }
        assert_eq!(r.len(), 49, "a 0x30 report is 49 bytes");
        r
    }

    /// ⭐ The captured report must decode to a controller sitting still on a
    /// desk — which is what it was.
    ///
    /// This is the strongest assertion available without hardware: the
    /// accelerometer measures a constant-magnitude vector, so checking that the
    /// decode yields ONE GRAVITY validates the field offsets, the byte order
    /// and the scale factor together. Any one of them wrong and the magnitude
    /// is wrong.
    #[test]
    fn the_captured_report_decodes_to_one_gravity() {
        let r = real_report();
        let g = parse_switch_pro_report(&r).expect("a real 0x30 report must parse");
        let mag = (g.accel_x * g.accel_x + g.accel_y * g.accel_y + g.accel_z * g.accel_z).sqrt();
        // Pins are normalised so 1.0 = 8 g, making one gravity 0.125.
        assert!(
            (mag - 0.125).abs() < 0.02,
            "accel magnitude {mag} is not one gravity — offsets, byte order or              scale is wrong (raw was -624, 9, 4255)",
        );
        // Resting on a desk: gravity almost entirely on the vertical axis.
        assert!(g.accel_z > 0.11, "gravity not on the vertical axis: {}", g.accel_z);
        // And a still controller reads a near-zero rate.
        let spin = g.gyro_x.abs() + g.gyro_y.abs() + g.gyro_z.abs();
        assert!(spin < 0.01, "a resting controller reported {spin} of rotation");
    }

    /// ⛔ The HID transaction header is NOT part of the report.
    ///
    /// `0xa1` prefixes every input report on the interrupt channel. Handing it
    /// to the parser shifts every field by one byte, and the result is not an
    /// error — it is a report that decodes to plausible nonsense, which is far
    /// harder to notice than a refusal.
    #[test]
    fn the_transaction_header_must_be_stripped_first() {
        let r = real_report();
        let mut with_header = vec![0xA1u8];
        with_header.extend_from_slice(&r);
        assert!(
            parse_switch_pro_report(&with_header).is_none(),
            "a report parsed WITH its transaction header still attached",
        );
        assert!(parse_switch_pro_report(&with_header[1..]).is_some());
    }

    /// Buttons decode by physical position, not by Nintendo's labels.
    #[test]
    fn buttons_land_on_positional_pins() {
        let mut r = real_report();
        r[3] = 0x08; // right byte, bit 3 = A
        let g = parse_switch_pro_report(&r).expect("parses");
        let sb = g.switch_buttons.expect("switch buttons present");
        assert!(sb.btn_a, "A not decoded");
        let mut out = Vec::new();
        push_switch_pro_buttons(&mut out, "test", &sb);
        let east = out.iter().find(|(_, p, _)| p == "btn_east").expect("btn_east pin");
        assert!(matches!(east.2, Signal::Bool(true)), "A must publish as EAST");
        let south = out.iter().find(|(_, p, _)| p == "btn_south").expect("btn_south pin");
        assert!(matches!(south.2, Signal::Bool(false)), "A must not publish as south");
    }

    #[test]
    fn a_device_id_parses_back_to_its_address() {
        let a = [0xda, 0x2d, 0x16, 0x0f, 0x01, 0x69];
        assert_eq!(parse_device_id(&device_id(ControllerKind::SwitchPro, a)), Some(a));
        assert_eq!(parse_device_id(&device_id(ControllerKind::DualSense, a)), Some(a));
        assert_eq!(parse_device_id("gilrs:switch_pro:0"), None);
        assert_eq!(parse_device_id("btc:switch_pro:da2d"), None);
    }

    #[test]
    fn a_device_id_is_stable_and_address_tagged() {
        let a = [0xda, 0x2d, 0x16, 0x0f, 0x01, 0x69];
        // ⛔ Unchanged for the Switch Pro: saved patches name it this way.
        assert_eq!(device_id(ControllerKind::SwitchPro, a), "btc:switch_pro:da2d160f0169");
        assert_eq!(device_id(ControllerKind::DualShock4, a), "btc:ds4:da2d160f0169");
        // Two controllers must not collide.
        assert_ne!(device_id(ControllerKind::SwitchPro, a), device_id(ControllerKind::SwitchPro, [0; 6]));
    }

    fn info(vid: u16, pid: u16, descriptor: &[u8]) -> keystore::DeviceInfo {
        keystore::DeviceInfo { vid, pid, version: 0x0100, descriptor: descriptor.to_vec() }
    }

    /// ⛔ A pairing with no device record is a Switch Pro: every one made
    /// before records existed was, and they must keep working.
    #[test]
    fn families_follow_the_device_record() {
        assert_eq!(family_of(None), Family::SwitchPro);
        assert_eq!(family_of(Some(&info(0x057E, 0x2009, &[0x05]))), Family::SwitchPro);
        let ds = family_of(Some(&info(0x054C, 0x0CE6, &[0x05])));
        let edge = family_of(Some(&info(0x054C, 0x0DF2, &[0x05])));
        let ds4 = family_of(Some(&info(0x054C, 0x09CC, &[0x05])));
        let xbox = family_of(Some(&info(0x045E, 0x02E0, &[0x05])));
        let other = family_of(Some(&info(0x2DC8, 0x6001, &[0x05])));
        let no_hid = family_of(Some(&info(0x2DC8, 0x6001, &[])));
        if cfg!(target_os = "linux") {
            for f in [ds, edge, ds4, xbox] {
                assert_eq!(f, Family::Passthrough);
            }
        } else {
            assert_eq!(xbox, Family::Xbox);
            assert_eq!(ds, Family::Sony { dualsense: true, edge: false });
            assert_eq!(edge, Family::Sony { dualsense: true, edge: true });
            assert_eq!(ds4, Family::Sony { dualsense: false, edge: false });
        }
        // Everything else with a HID service goes to the system, everywhere.
        assert_eq!(other, Family::Passthrough);
        assert_eq!(no_hid, Family::Unsupported);
        assert_eq!(not_readable(Family::Unsupported), Some(unsupported_reason()));
        assert_eq!(not_readable(Family::SwitchPro), None);
    }

    /// A 17-byte state report (current firmware) as SDL decodes it: sticks
    /// centred at 0x8000 with Y down-positive, 10-bit triggers, the hat 1–8
    /// from up, the face buttons with gaps where the original's were.
    #[test]
    fn an_xbox_state_report_decodes_as_sdl_reads_it() {
        let mut r = [0u8; 17];
        r[0] = 0x01;
        r[1..3].copy_from_slice(&0xFFFFu16.to_le_bytes()); // left X full right
        r[3..5].copy_from_slice(&0x0000u16.to_le_bytes()); // left Y full UP (report is down-positive)
        r[5..7].copy_from_slice(&0x8000u16.to_le_bytes());
        r[7..9].copy_from_slice(&0x8000u16.to_le_bytes());
        r[9..11].copy_from_slice(&1023u16.to_le_bytes()); // left trigger full
        r[13] = 3; // hat right
        r[14] = 0x01 | 0x08; // A, X
        r[15] = 0x10 | 0x08; // guide, menu
        let mut x = XboxState::default();
        let ds = x.take(&r).unwrap().dualsense.unwrap();
        assert!(ds.lx > 0.99 && ds.ly > 0.99, "{} {}", ds.lx, ds.ly);
        assert!(ds.rx.abs() < 1e-3 && ds.ry.abs() < 1e-3);
        assert!((ds.l2 - 1.0).abs() < 1e-6 && ds.btn_l2 && !ds.btn_r2);
        assert!(ds.dpad_right && !ds.dpad_up && !ds.dpad_down);
        assert!(ds.btn_south && ds.btn_west && !ds.btn_east && !ds.btn_north);
        assert!(ds.btn_ps && ds.btn_options && !ds.btn_create);
        // A guide report of its own takes the guide button over from then on.
        assert!(x.take(&[0x02, 0x00]).unwrap().dualsense.unwrap().btn_ps == false);
        assert!(!x.take(&r).unwrap().dualsense.unwrap().btn_ps, "the state report's bit is no longer the guide");
        // The battery, in four steps.
        assert_eq!(x.take(&[0x04, 0x02]).unwrap().battery, Some(0.7));
        // The original Xbox One S's 16-byte layout.
        let mut old = [0u8; 16];
        old[0] = 0x01;
        old[14] = 0x04 | 0x40; // X, View
        old[15] = 0x02; // right stick
        let ds = XboxState::default().take(&old).unwrap().dualsense.unwrap();
        assert!(ds.btn_west && ds.btn_create && ds.btn_rs && !ds.btn_south);
    }

    /// Only what this backend reads itself is listed.
    #[test]
    fn passthrough_pads_are_not_listed_here() {
        assert_eq!(kind_of(Family::Passthrough), None);
        assert_eq!(kind_of(Family::Unsupported), None);
        assert_eq!(kind_of(Family::Sony { dualsense: false, edge: false }), Some(ControllerKind::DualShock4));
        assert_eq!(kind_of(Family::Xbox), Some(ControllerKind::XInput));
        // Only the pads that stream are timed out by silence.
        assert!(streams(Family::SwitchPro) && !streams(Family::Xbox) && !streams(Family::Passthrough));
    }

    /// ⛔ The dongle selector must not silently fall back on a typo.
    #[test]
    fn the_dongle_selector_parses_vid_pid() {
        std::env::set_var("FLEXINPUT_BT_CLASSIC_DONGLE", "0bda:a728");
        assert_eq!(dongle_ids(), Some((0x0BDA, 0xA728)));
        std::env::set_var("FLEXINPUT_BT_CLASSIC_DONGLE", "0x1234:0x5678");
        assert_eq!(dongle_ids(), Some((0x1234, 0x5678)));
        // ⛔ With no override it is DISCOVERED, never assumed. Asserting a
        // particular Realtek here is what let the hardcoded fallback survive —
        // the test passed on the one machine where the wrong answer was right.
        std::env::remove_var("FLEXINPUT_BT_CLASSIC_DONGLE");
        assert_eq!(dongle_ids(), flexinput_btle::preferred_dongle());
    }
}
