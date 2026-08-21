//! HCI transport over a USB Bluetooth dongle bound to WinUSB.
//!
//! See `Cargo.toml` for why this crate exists. In short: the OS Bluetooth stack
//! cannot hold a Joy-Con 2 link and cannot bond with one, so FlexInput drives a
//! dedicated dongle itself.
//!
//! # USB transport layer
//!
//! The Bluetooth Core spec defines a fixed USB layout that every dongle
//! implements, so none of this is vendor-specific:
//!
//! | Direction | Endpoint | Carries |
//! |---|---|---|
//! | host → controller | control, `bmRequestType = 0x20` | HCI commands |
//! | controller → host | interrupt IN | HCI events |
//! | both | bulk | ACL data (connections) |
//!
//! Only commands and events are needed to prove the transport works; ACL comes
//! later, with connections.

use std::sync::Mutex;
use std::time::Duration;

pub mod acl;
pub mod keystore;
pub mod l2cap;
pub mod radio;
pub mod hci;
pub mod joycon;

pub use acl::{AclPacket, Notification};
pub use hci::{CommandComplete, Event, Opcode};

/// Errors from the dongle transport.
#[derive(Debug)]
pub enum Error {
    /// No USB device with the requested VID/PID, or it is not WinUSB-bound.
    NotFound { vid: u16, pid: u16 },
    /// The device IS present and WinUSB-bound, but could not be opened —
    /// something else holds it.
    ///
    /// ⭐ A separate case from [`Error::NotFound`] because the two need
    /// completely different responses from a user, and conflating them cost a
    /// real debugging session. `open_device_with_vid_pid` returns `None` for
    /// BOTH "not there" and "there but busy", so a claimed dongle was reported
    /// as "no USB device — is it bound to WinUSB via Zadig?" while Device
    /// Manager showed it present, healthy and bound. That message sends someone
    /// to re-run Zadig, which is precisely the wrong thing to do.
    Busy { vid: u16, pid: u16, source: rusb::Error },
    /// libusb refused an operation. On Windows the usual cause is the device
    /// still being owned by the Bluetooth driver rather than WinUSB.
    Usb(rusb::Error),
    /// The dongle does not expose the endpoints the Bluetooth spec requires.
    NoEndpoint(&'static str),
    /// A reply arrived but could not be decoded.
    Protocol(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotFound { vid, pid } => write!(
                f,
                "no USB device {vid:04x}:{pid:04x} (is it bound to WinUSB via Zadig?)"
            ),
            Error::Busy { vid, pid, source } => write!(
                f,
                "dongle {vid:04x}:{pid:04x} is present and WinUSB-bound but already \
                 in use ({source}) — another FlexInput instance, a jc2_*/bt_classic \
                 probe, or this app's other Bluetooth transport holds it"
            ),
            Error::Usb(e) => write!(f, "usb error: {e}"),
            Error::NoEndpoint(what) => write!(f, "dongle exposes no {what} endpoint"),
            Error::Protocol(m) => write!(f, "protocol error: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<rusb::Error> for Error {
    fn from(e: rusb::Error) -> Self {
        Error::Usb(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// What a link actually negotiated, as opposed to what was asked for.
#[derive(Debug, Clone, Copy)]
pub struct LinkParams {
    pub conn_handle: u16,
    /// Connection interval in 1.25 ms units.
    pub interval: u16,
    /// Supervision timeout in 10 ms units.
    pub supervision_timeout: u16,
}

impl LinkParams {
    pub fn interval_ms(&self) -> f32 {
        self.interval as f32 * 1.25
    }
    pub fn timeout_ms(&self) -> u32 {
        self.supervision_timeout as u32 * 10
    }
}

/// Passed as `psrm` to [`Dongle::page_and_pair`] to mean "do not page — the
/// link is already coming in".
pub const NO_PAGE: u8 = 0xFF;

/// A live BR/EDR link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassicLink {
    pub conn_handle: u16,
    pub address: [u8; 6],
    /// The bond. Persist it and the controller reconnects without pairing.
    pub link_key: Option<[u8; 16]>,
    pub encrypted: bool,
    /// True when the REMOTE opened this link, whoever tried first.
    ///
    /// ⭐ Not the same question as "did we page?". A page can lose a collision
    /// and be answered instead, and the answer decides who opens the L2CAP
    /// channels — get it wrong and both ends wait for the other.
    pub incoming: bool,
}

/// An open Bluetooth dongle, ready to exchange HCI traffic.
pub struct Dongle {
    handle: rusb::DeviceHandle<rusb::GlobalContext>,
    event_ep: u8,
    acl_in_ep: u8,
    acl_out_ep: u8,
    interface: u8,
    timeout: Duration,
    /// Events already decoded out of a transfer but not yet returned.
    ///
    /// ⭐ **A USB transfer can carry SEVERAL HCI events.** Treating one read as
    /// one event meant every event after the first in a transfer was silently
    /// swallowed — and worse, the first event's declared length was applied to
    /// a buffer holding several, so what came back was garbage. Captured logs
    /// showed complete `LE Advertising Report`s sitting inside the parameters
    /// of a misparsed event, along with 911 unknown event codes, 53 phantom
    /// `EncryptionChange`s the host never asked for, and disconnect events
    /// carrying handles that were never issued.
    ///
    /// The user-visible cost was discovery: a controller's advertisement is
    /// short and bursty, and any burst that landed behind another event in the
    /// same transfer was lost. One log showed 88 seconds of scanning with a
    /// single match, on a pad sitting at RSSI -30.
    pending: Mutex<std::collections::VecDeque<Event>>,
    /// Same queue, for ACL data — see [`Dongle::read_acl`].
    pending_acl: Mutex<std::collections::VecDeque<AclPacket>>,
    /// Local BD_ADDR, read once during [`Dongle::reset_and_init`].
    ///
    /// ❗ Cached because reading it LATER fails. Asking mid-session, with links
    /// live and the radio busy, returned "no Command Complete for
    /// Opcode(4105) within 2 s" every time — and the caller then paired a
    /// controller against a made-up host address, writing that into controller
    /// flash. Read once while the controller is quiet and the answer is
    /// immediate.
    bd_addr: std::sync::OnceLock<[u8; 6]>,
    /// Which device this is, for the open-dongle registry.
    vid: u16,
    pid: u16,
}

/// Dongles this process currently holds open.
///
/// ⭐ **So the UI can say "FlexInput is using it" instead of "in use".** An
/// adapter that cannot be opened is the normal state for two completely
/// different reasons — Windows owns the built-in radio, and one of our own
/// transports owns the dongle — and telling those apart is the difference
/// between "working as intended" and "something is wrong". libusb cannot
/// distinguish them; only we know what we opened.
///
/// Registered on a successful open and cleared on drop, so it tracks reality
/// rather than intent.
fn open_dongles() -> &'static std::sync::Mutex<Vec<(u16, u16)>> {
    static OPEN: std::sync::OnceLock<std::sync::Mutex<Vec<(u16, u16)>>> =
        std::sync::OnceLock::new();
    OPEN.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Whether this process holds `vid:pid` open right now.
pub fn is_ours(vid: u16, pid: u16) -> bool {
    open_dongles()
        .lock()
        .map(|v| v.contains(&(vid, pid)))
        .unwrap_or(false)
}

/// USB device class for a wireless controller.
///
/// ⭐ **A Bluetooth dongle announces itself by CLASS, not by vendor id.** The
/// USB spec assigns the HCI transport a fixed triple — class `0xE0` wireless
/// controller, subclass `0x01` RF controller, protocol `0x01` Bluetooth — and
/// every dongle that speaks standard HCI over USB reports it. That is what
/// makes this stack work with any adapter rather than the one it was written
/// against: nothing here is specific to a Realtek `0bda:a728` except the
/// default in some probe binaries.
const CLASS_WIRELESS: u8 = 0xE0;
const SUBCLASS_RF: u8 = 0x01;
const PROTOCOL_BLUETOOTH: u8 = 0x01;

/// A Bluetooth dongle visible to libusb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DongleInfo {
    pub vid: u16,
    pub pid: u16,
    /// USB bus and address, so two identical dongles can be told apart.
    pub bus: u8,
    pub address: u8,
    /// Whether it could be opened right now.
    pub available: bool,
    /// Whether THIS PROCESS is the one holding it — see [`is_ours`]. Only
    /// meaningful when `available` is false, and the whole reason "in use" can
    /// be reported as something other than a problem.
    pub ours: bool,
    /// USB manufacturer string, when it can be read.
    ///
    /// ❗ Reading a string descriptor requires OPENING the device, so an
    /// adapter in use by Windows or by another transport has none — which is
    /// most of the time, and exactly when a user most wants to know what it is.
    /// [`DongleInfo::describe`] falls back to the vendor id for that case.
    pub manufacturer: Option<String>,
    /// USB product string, same caveat.
    pub product: Option<String>,
}

/// USB vendor ids seen on Bluetooth adapters, for when the descriptor strings
/// cannot be read.
///
/// ⭐ Deliberately short and only the chipset vendors that actually turn up on
/// Bluetooth dongles. A full USB vendor database is a megabyte of data to name
/// a device the user is holding.
fn vendor_name(vid: u16) -> Option<&'static str> {
    Some(match vid {
        0x0A12 => "Cambridge Silicon Radio",
        0x0BDA => "Realtek",
        0x0CF3 | 0x13D3 => "Qualcomm Atheros / IMC",
        0x8087 => "Intel",
        0x0489 => "Foxconn / MediaTek",
        0x0E8D => "MediaTek",
        0x1131 => "Integrated System Solution",
        0x0B05 => "ASUS",
        0x050D => "Belkin",
        0x0930 => "Toshiba",
        0x04CA => "Lite-On",
        _ => return None,
    })
}

impl DongleInfo {
    /// `vid:pid` in the form the env overrides take.
    pub fn ids(&self) -> String {
        format!("{:04x}:{:04x}", self.vid, self.pid)
    }

    /// The best name available: the device's own strings if they could be read,
    /// otherwise the chipset vendor, otherwise the raw ids.
    ///
    /// ⭐ Never empty. A device list whose rows are blank for the adapters that
    /// are in use — which is the normal state — would be worse than one showing
    /// hex.
    pub fn describe(&self) -> String {
        match (&self.manufacturer, &self.product) {
            (Some(m), Some(p)) => format!("{m} {p}"),
            (None, Some(p)) => p.clone(),
            (Some(m), None) => format!("{m} adapter"),
            (None, None) => match vendor_name(self.vid) {
                Some(v) => format!("{v} adapter"),
                None => format!("Bluetooth adapter {}", self.ids()),
            },
        }
    }
}

/// Every Bluetooth dongle libusb can see.
///
/// ❗ Only adapters this app could USE appear — that is, WinUSB-bound ones. A
/// radio owned by the Windows Bluetooth stack is not a candidate and never can
/// be, so listing it would only invite the question of why the thing on screen
/// does not work.
///
/// An entry with `available = false` is therefore a usable dongle that
/// something already holds — this app's own shared radio, in the ordinary case.
///
/// The class triple is checked on the DEVICE descriptor and, failing that, on
/// every interface: composite dongles report `0x00` at device level and put the
/// real class on interface 0, and looking only at the device descriptor misses
/// them.
pub fn discover() -> Vec<DongleInfo> {
    let Ok(devices) = rusb::devices() else { return Vec::new() };
    let mut out = Vec::new();
    for device in devices.iter() {
        let Ok(desc) = device.device_descriptor() else { continue };
        let mut is_bt = desc.class_code() == CLASS_WIRELESS
            && desc.sub_class_code() == SUBCLASS_RF
            && desc.protocol_code() == PROTOCOL_BLUETOOTH;
        if !is_bt {
            if let Ok(config) = device.active_config_descriptor() {
                is_bt = config.interfaces().any(|i| {
                    i.descriptors().any(|d| {
                        d.class_code() == CLASS_WIRELESS
                            && d.sub_class_code() == SUBCLASS_RF
                            && d.protocol_code() == PROTOCOL_BLUETOOTH
                    })
                });
            }
        }
        if !is_bt {
            continue;
        }
        // ⛔ An adapter with no WinUSB driver is not listed at all.
        //
        // ⭐ libusb enumerates every USB device on Windows but can only OPEN
        // the ones bound to WinUSB, and the two failures are distinguishable:
        // a device owned by another driver refuses with `NotSupported`, while
        // one that is WinUSB-bound but busy refuses with `Access`/`Busy`.
        //
        // ❗ Only the second is worth showing. A machine's built-in radio, and
        // every other Bluetooth device Windows owns, can never be used by this
        // app — listing them invites the user to wonder why the thing they can
        // see does not work, and the honest answer is that it was never a
        // candidate. What IS worth showing is our own dongle while a transport
        // has it open, which is the normal working state.
        // ⭐ A WHITELIST, not a blacklist. Only two outcomes mean "this adapter
        // is one we could use": it opened, or it refused because something
        // already holds it. Every other refusal — no WinUSB driver, access
        // denied at the OS level, gone mid-enumeration — means it is not a
        // candidate and listing it only invites the question of why the thing
        // on screen does not work.
        //
        // ❗ Written as a whitelist because guessing which error code Windows
        // returns for "no driver" got it wrong: the list showed adapters the
        // app could never touch, and the Bluetooth button appeared on machines
        // with no usable dongle at all.
        let opened = match device.open() {
            Ok(h) => Some(h),
            Err(rusb::Error::Access) | Err(rusb::Error::Busy) => None,
            Err(_) => continue,
        };
        let (manufacturer, product) = match &opened {
            Some(h) => (
                h.read_manufacturer_string_ascii(&desc).ok().map(|s| s.trim().to_string()),
                h.read_product_string_ascii(&desc).ok().map(|s| s.trim().to_string()),
            ),
            None => (None, None),
        };
        let (vid, pid) = (desc.vendor_id(), desc.product_id());
        out.push(DongleInfo {
            vid,
            pid,
            bus: device.bus_number(),
            address: device.address(),
            available: opened.is_some(),
            ours: is_ours(vid, pid),
            manufacturer: manufacturer.filter(|s| !s.is_empty()),
            product: product.filter(|s| !s.is_empty()),
        });
    }
    out
}

/// Find a device by ids and open it, keeping the two failures apart.
///
/// ❗ `rusb::open_device_with_vid_pid` collapses "no such device" and "could not
/// open it" into a single `None`. Those are different problems with different
/// fixes — one means plug it in or run Zadig, the other means close whatever is
/// holding it — so this walks the bus itself and reports which happened.
fn open_by_ids(vid: u16, pid: u16) -> Result<rusb::DeviceHandle<rusb::GlobalContext>> {
    for device in rusb::devices()?.iter() {
        let Ok(desc) = device.device_descriptor() else { continue };
        if desc.vendor_id() != vid || desc.product_id() != pid {
            continue;
        }
        return device.open().map_err(|source| Error::Busy { vid, pid, source });
    }
    Err(Error::NotFound { vid, pid })
}

impl Dongle {
    /// Open the dongle at `vid:pid` and claim its HCI interface.
    ///
    /// Interface 0 is the HCI interface on every spec-compliant dongle;
    /// interface 1 carries isochronous SCO (voice), which is of no interest.
    pub fn open(vid: u16, pid: u16) -> Result<Self> {
        // `mut` is required by `set_auto_detach_kernel_driver` below, which is
        // compiled out on Windows — hence unused there specifically.
        #[cfg_attr(windows, allow(unused_mut))]
        let mut handle = open_by_ids(vid, pid)?;

        // Windows has no kernel driver to detach once WinUSB is bound, and this
        // is unsupported there — hence best-effort. It matters on Linux, where
        // btusb would otherwise still own the device.
        #[cfg(not(windows))]
        let _ = handle.set_auto_detach_kernel_driver(true);

        // ⭐ RESET THE DONGLE before claiming it.
        //
        // A BLE link lives in the dongle's firmware, not in this process. When
        // FlexInput exits without tearing its links down — which the logs show
        // is the normal case, because the worker thread never reaches its
        // teardown on app close — the controller stays CONNECTED to a dongle
        // owned by nobody. A connected peripheral does not advertise, so the
        // next run cannot see it at all.
        //
        // Measured: a run where the radio received 14-45 advertisements per ten
        // seconds from other devices saw the Joy-Con exactly twice in 98
        // seconds. The scanner was healthy; the controller was silent because
        // it still believed it was connected. That is the whole "takes forever
        // to find the damn Joy-Con".
        //
        // `HCI_Reset` alone is not enough — it resets the host interface, not
        // necessarily the radio's link state. A USB-level reset restarts the
        // firmware and drops everything, which is the only thing that does not
        // depend on the previous process having exited politely.
        //
        // Best-effort: some stacks invalidate the handle, so reopen if the
        // reset says the device went away, and carry on if it refuses outright.
        let handle = match handle.reset() {
            Ok(()) => handle,
            Err(rusb::Error::NotFound) => open_by_ids(vid, pid)?,
            Err(e) => {
                eprintln!("[btle] USB reset refused ({e}); continuing without it");
                handle
            }
        };
        // The firmware needs a moment after a reset before it will answer.
        std::thread::sleep(Duration::from_millis(150));

        let interface = 0u8;
        handle.claim_interface(interface)?;
        // Recorded only once the interface is actually CLAIMED — an open that
        // failed here would otherwise leave this process looking like the owner
        // of a dongle it does not hold.
        if let Ok(mut open) = open_dongles().lock() {
            open.push((vid, pid));
        }

        let eps = find_endpoints(handle.device(), interface)?;

        Ok(Self {
            handle,
            event_ep: eps.event,
            acl_in_ep: eps.acl_in,
            acl_out_ep: eps.acl_out,
            interface,
            timeout: Duration::from_secs(2),
            pending: Mutex::new(std::collections::VecDeque::new()),
            pending_acl: Mutex::new(std::collections::VecDeque::new()),
            bd_addr: std::sync::OnceLock::new(),
            vid,
            pid,
        })
    }

    /// The interrupt IN endpoint address events arrive on.
    pub fn event_endpoint(&self) -> u8 {
        self.event_ep
    }

    /// Send an HCI command.
    ///
    /// Commands go over the *control* endpoint, not a bulk one — a detail the
    /// Bluetooth spec fixes and which is easy to get wrong when coming from
    /// serial-transport (H4) examples, where commands carry a `0x01` type byte.
    /// Over USB the endpoint itself identifies the packet type, so there is no
    /// leading type byte here.
    pub fn send_command(&self, opcode: Opcode, params: &[u8]) -> Result<()> {
        let packet = hci::encode_command(opcode, params);
        // bmRequestType: host-to-device | class | interface.
        let request_type = rusb::request_type(
            rusb::Direction::Out,
            rusb::RequestType::Class,
            rusb::Recipient::Interface,
        );
        self.handle
            .write_control(request_type, 0x00, 0x0000, self.interface as u16, &packet, self.timeout)?;
        Ok(())
    }

    /// Read one HCI event, or `Ok(None)` if none arrived before the timeout.
    ///
    /// A timeout is not an error: dongles are quiet when idle, and callers
    /// waiting for a specific event need to distinguish "nothing yet" from
    /// "broken".
    pub fn read_event(&self) -> Result<Option<Event>> {
        self.read_event_timeout(self.timeout)
    }

    /// [`Dongle::read_event`] with an explicit timeout.
    ///
    /// A streaming loop has to poll the event endpoint *and* the ACL endpoint,
    /// and blocking two seconds on either would stall the other. Short
    /// alternating reads keep both responsive without threads.
    /// Put events back at the FRONT of the queue, in their original order.
    ///
    /// ⛔ **For handing back traffic that was read by somebody else.** When the
    /// shared router is reading, an event can be broadcast a moment before a
    /// transport takes its exclusive lease — and the helpers on this type read
    /// the dongle directly, so that event is simply gone as far as they are
    /// concerned. A `Link Key Request` lost that way is fatal: unanswered, the
    /// remote fails authentication with "key missing" and drops the link, and
    /// the host's own log shows the request arriving AFTER the failure it
    /// caused, on the wrong side of the fence.
    ///
    /// Front, not back: these were read before anything already queued here.
    pub fn push_events_front(&self, events: Vec<Event>) {
        let mut q = self.pending.lock().unwrap();
        for e in events.into_iter().rev() {
            q.push_front(e);
        }
    }

    /// The same, for ACL. See [`Dongle::push_events_front`].
    pub fn push_acl_front(&self, packets: Vec<AclPacket>) {
        let mut q = self.pending_acl.lock().unwrap();
        for p in packets.into_iter().rev() {
            q.push_front(p);
        }
    }

    pub fn read_event_timeout(&self, timeout: Duration) -> Result<Option<Event>> {
        if let Some(e) = self.pending.lock().unwrap().pop_front() {
            return Ok(Some(e));
        }

        let mut buf = [0u8; 512];
        let n = match self.handle.read_interrupt(self.event_ep, &mut buf, timeout) {
            Ok(n) => n,
            Err(rusb::Error::Timeout) => return Ok(None),
            Err(e) => return Err(Error::Usb(e)),
        };

        // Split the transfer into every complete event it holds.
        //
        // ⭐ One read is USUALLY one event — the USB transport terminates each
        // with a short packet — but not always: an event whose length is an
        // exact multiple of the endpoint's packet size has no short packet to
        // end it, so the next event is appended to the same transfer. Parsing
        // only the first one swallowed the rest, which is how advertising
        // reports went missing and how phantom events appeared for handles the
        // host never issued.
        //
        // ❗ A truncated TAIL is dropped, deliberately, and this is the second
        // attempt at this function. The first carried the tail forward to be
        // completed by the next read, which is more correct in principle and
        // was a disaster in practice: when a tail never completed, every event
        // behind it queued up and the transport went deaf. That surfaced as
        // commands timing out — first scan-parameters, then HCI_Reset itself —
        // and cost several rounds of testing.
        //
        // Losing an occasional partial event is cheap: advertisements repeat,
        // and anything that matters is retried. Stalling the command stream is
        // not recoverable, so the simpler behaviour is the safer one.
        let mut queue = self.pending.lock().unwrap();
        let mut i = 0usize;
        while i + 2 <= n {
            let want = 2 + buf[i + 1] as usize;
            if i + want > n {
                break;
            }
            if let Ok(e) = hci::parse_event(&buf[i..i + want]) {
                queue.push_back(e);
            }
            i += want;
        }
        Ok(queue.pop_front())
    }

    /// Throw away any events already queued or waiting on the wire.
    ///
    /// Used before a command whose reply must not be confused with the backlog
    /// of a previous run — most importantly `HCI_Reset`, which is the first
    /// thing sent to a dongle that may still be mid-conversation from a process
    /// that exited without closing its links.
    pub fn drain_events(&self) {
        self.pending.lock().unwrap().clear();
        // ❗ The ACL backlog goes too. Connection handles are REUSED, so data
        // queued for a link that no longer exists would otherwise be delivered
        // to whichever link inherits its handle — one controller's motion
        // decoded as the other's, which differ by a one-byte report offset and
        // so produce a plausible-looking but wrong accelerometer vector.
        self.pending_acl.lock().unwrap().clear();
        let mut buf = [0u8; 512];
        for _ in 0..64 {
            match self.handle.read_interrupt(self.event_ep, &mut buf, Duration::from_millis(5)) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        self.pending.lock().unwrap().clear();
    }

    /// Send a command and wait for the `Command Complete` that matches it.
    ///
    /// Unrelated events are skipped rather than treated as failures — a dongle
    /// emits plenty unprompted, and the earlier Joy-Con work was repeatedly
    /// misled by validators that locked onto the first thing they saw.
    pub fn command_sync(&self, opcode: Opcode, params: &[u8]) -> Result<CommandComplete> {
        self.send_command(opcode, params)?;
        // ❗ Bounded by TIME, not by a count of events.
        //
        // This used to give up after 16 events. That is fine on an idle dongle
        // and wrong the moment links are streaming: a controller emits
        // `Number Of Completed Packets` continuously for ACL flow control, and
        // with two halves at ~67 Hz that backlog exhausts a 16-event budget
        // before the Command Complete is ever reached. The command had usually
        // succeeded; we simply stopped listening too early and reported
        // "no Command Complete within timeout".
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match self.read_event_timeout(Duration::from_millis(100))? {
                Some(Event::CommandComplete(cc)) if cc.opcode == opcode => return Ok(cc),
                // Unrelated events are skipped rather than treated as failures —
                // a dongle emits plenty unprompted, and the earlier Joy-Con work
                // was repeatedly misled by validators that locked onto the first
                // thing they saw.
                _ => continue,
            }
        }
        Err(Error::Protocol(format!(
            "no Command Complete for {opcode:?} within 2 s"
        )))
    }
}

impl Dongle {
    /// Reset the controller and enable the events this stack depends on.
    ///
    /// The event-mask step is not optional housekeeping. After `HCI_Reset` the
    /// event mask reverts to the spec default `0x00001FFFFFFFFFFF`, which has
    /// **bit 61 (LE Meta) clear** — and every LE event, advertising reports
    /// included, arrives wrapped in LE Meta. Skip this and scanning appears to
    /// work, reports zero devices, and gives no error to explain why.
    pub fn reset_and_init(&self) -> Result<()> {
        // ⭐ Reset is retried, and it is the one command that must be.
        //
        // It is the FIRST thing sent to a dongle that may still be mid-flight
        // from a process which exited without closing its links — and this
        // project's own worker used to do exactly that. In that state the
        // controller has a backlog to clear and can take well over the standard
        // two seconds to answer, so a single attempt reports
        //
        //     no Command Complete for Opcode(3075) within 2 s
        //
        // and the whole transport gives up. That is much worse than it sounds:
        // the dongle thread then exits and publishes ABSENT, so the session has
        // no Joy-Con support at all until the application is restarted.
        //
        // Draining first matters as much as retrying: a stale reply from the
        // previous conversation is otherwise sitting in front of ours.
        let mut last = None;
        for attempt in 1..=3 {
            self.drain_events();
            match self.command_sync(hci::Opcode::RESET, &[]) {
                Ok(cc) if cc.succeeded() => {
                    last = None;
                    break;
                }
                Ok(cc) => {
                    last = Some(Error::Protocol(format!("HCI_Reset status {:?}", cc.status())));
                }
                Err(e) => {
                    eprintln!("[btle] HCI_Reset attempt {attempt}/3 failed: {e}");
                    last = Some(e);
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        if let Some(e) = last {
            return Err(e);
        }

        // 0x3FFFFFFFFFFFFFFF — everything the spec defines, LE Meta included,
        // without setting reserved bits above 61.
        let event_mask: [u8; 8] = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x3f];
        let cc = self.command_sync(hci::Opcode::SET_EVENT_MASK, &event_mask)?;
        if !cc.succeeded() {
            return Err(Error::Protocol(format!(
                "HCI_Set_Event_Mask status {:?}",
                cc.status()
            )));
        }

        // LE sub-events. Bit 1 is Advertising Report — the one that matters
        // here — but enabling the low byte covers connection complete and
        // connection update too, which the next stages need.
        let le_mask: [u8; 8] = [0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let cc = self.command_sync(hci::Opcode::LE_SET_EVENT_MASK, &le_mask)?;
        if !cc.succeeded() {
            return Err(Error::Protocol(format!(
                "HCI_LE_Set_Event_Mask status {:?}",
                cc.status()
            )));
        }

        // Read the local address now, while nothing else is using the radio,
        // and cache it — see the field. Not fatal if it fails: the address is
        // only needed for pairing, and a transport that otherwise works should
        // not be refused over it.
        match self.read_bd_addr_uncached() {
            Ok(addr) => {
                let _ = self.bd_addr.set(addr);
            }
            Err(e) => eprintln!("[btle] could not read local BD_ADDR at init: {e}"),
        }
        Ok(())
    }

    /// Cancel any connection attempt left pending.
    ///
    /// A failed `LE_Create_Connection` leaves the controller initiating, and
    /// while it is initiating it will refuse to scan with "Command Disallowed"
    /// — which previously manifested as the scanner finding nothing, forever,
    /// with no error anywhere. Harmless when nothing is pending: the command
    /// simply returns an error status, which is ignored.
    pub fn cancel_pending_connect(&self) {
        let _ = self.command_sync(hci::Opcode::LE_CREATE_CONNECTION_CANCEL, &[]);
    }

    /// Begin an active LE scan.
    ///
    /// ⭐ **PASSIVE, not active** — and this single byte was the connection
    /// problem all along.
    ///
    /// Joy-Con 2 controllers are identifiable only by the manufacturer data in
    /// their advertisements: no service UUIDs, no name. This used to scan
    /// ACTIVELY, on the reasoning that a scan request "reliably pulls the full
    /// payload". A capture says the opposite. In one 60-second run the pad was
    /// seen 223 times:
    ///
    /// | report type | count | payload |
    /// |---|---|---|
    /// | `0x04` SCAN_RSP | 220 | **0 bytes — empty** |
    /// | `0x00` ADV_IND  | 1   | full manufacturer data |
    ///
    /// This controller's scan response carries NOTHING. Everything that
    /// identifies it is in the advertisement — and active scanning made the
    /// dongle answer each advertisement with a scan request and hand us the
    /// empty reply instead of the payload. One usable report in 223, at RSSI
    /// −30, sixty seconds to connect to a controller sitting on the desk.
    ///
    /// Passive scanning sends no scan request, so every advertisement arrives
    /// intact. It is also less airtime and less battery for the controller.
    ///
    /// ❗ Do not "fix" this back to active without a capture showing scan
    /// responses that contain something.
    ///
    /// Interval and window are in 0.625 ms units. The defaults here scan ~30 ms
    /// out of every ~60 ms: aggressive enough to find a controller quickly
    /// without monopolising a radio that will soon also be holding a link.
    pub fn start_le_scan(&self) -> Result<()> {
        self.start_le_scan_duty(true)
    }


    /// [`Dongle::start_le_scan`] with an explicit duty cycle.
    ///
    /// ⭐ **`continuous` is what makes a Joy-Con findable.** These controllers
    /// put their manufacturer data — the ONLY thing that identifies them, since
    /// they advertise no service UUID and no name — in the SCAN RESPONSE, not
    /// in the advertisement. A scan response only arrives if the radio is
    /// listening when it comes back, so a part-time scan misses most of them.
    ///
    /// Measured on hardware at the old 30-of-60 ms duty: a controller sitting at
    /// RSSI −22 was seen **134 times in one run and identified once**. 131 of
    /// those reports were rejected as "no manufacturer data". First sighting at
    /// 12.4 s, first usable match at 38.9 s — twenty-six seconds of the pad
    /// advertising in plain view. That is the whole "it takes several attempts
    /// to connect" complaint, and it was never a timing or radio problem.
    ///
    /// Window equal to interval means the radio never stops listening. That is
    /// only appropriate with nothing connected; a live link needs the airtime,
    /// which is why the caller chooses.
    pub fn start_le_scan_duty(&self, continuous: bool) -> Result<()> {
        // Clear any half-finished initiator first, or scan-enable is refused.
        self.cancel_pending_connect();
        // ⭐ WINDOW == INTERVAL, unconditionally. The radio never stops
        // listening between windows, which is the maximum the Bluetooth spec
        // allows — a window may not exceed its interval.
        //
        // `continuous` is ignored. It used to select a half-duty scan whenever
        // a link was held, on the theory that a live connection needed the
        // airtime; combined with the loop's own 2-of-5-second windowing that
        // left the radio listening about a fifth of the time, and a Joy-Con
        // advertises in short bursts after a button wake. Four bursts in five
        // were simply never heard, which is the whole reason connecting a
        // second controller took several attempts.
        //
        // A shorter interval is also chosen than before: 30 ms means the
        // scanner revisits each advertising channel three times as often as a
        // 100 ms one would, so a burst confined to a single channel is far less
        // likely to be missed.
        let _ = continuous;
        let interval: u16 = 0x0030; // 48 × 0.625 ms = 30 ms
        let window: u16 = interval; // 100% duty — no listening gaps at all
        let mut params = Vec::with_capacity(7);
        params.push(0x00); // PASSIVE — see above; active hides the payload
        params.extend_from_slice(&interval.to_le_bytes());
        params.extend_from_slice(&window.to_le_bytes());
        params.push(0x00); // own address type: public
        params.push(0x00); // filter policy: accept all
        let cc = self.command_sync(hci::Opcode::LE_SET_SCAN_PARAMETERS, &params)?;
        if !cc.succeeded() {
            return Err(Error::Protocol(format!(
                "LE_Set_Scan_Parameters status {:?}",
                cc.status()
            )));
        }
        // filter_duplicates = 0: a controller's payload can change between
        // advertisements, and de-duplicating hides that.
        let cc = self.command_sync(hci::Opcode::LE_SET_SCAN_ENABLE, &[0x01, 0x00])?;
        if !cc.succeeded() {
            return Err(Error::Protocol(format!(
                "LE_Set_Scan_Enable status {:?}",
                cc.status()
            )));
        }
        Ok(())
    }

    /// Stop scanning. Best-effort: reported but not fatal.
    pub fn stop_le_scan(&self) -> Result<()> {
        self.command_sync(hci::Opcode::LE_SET_SCAN_ENABLE, &[0x00, 0x00])?;
        Ok(())
    }

    /// Connect to a peripheral, returning the connection handle.
    ///
    /// `address` is in natural (display) order; the wire wants it reversed.
    ///
    /// Note this command answers with `Command Status`, not `Command Complete`
    /// — the outcome arrives later as an `LE Connection Complete` sub-event, so
    /// `command_sync` is deliberately not used here.
    ///
    /// Tries for the console's 5 ms interval first, then falls back.
    ///
    /// 5 ms is BELOW the 7.5 ms Bluetooth minimum — the console reaches it with
    /// a vendor command — but many controllers honour it when asked directly,
    /// and it is worth attempting because the interval is the hard ceiling on
    /// report rate: one input report arrives per connection event, so 15 ms is
    /// 66 Hz and 5 ms is 200 Hz. With two halves sharing the radio the
    /// difference is the gap between usable and not.
    pub fn le_connect(&self, address: [u8; 6], address_type: u8) -> Result<u16> {
        match self.le_connect_interval(address, address_type, 4, 6) {
            Ok(h) => Ok(h),
            Err(e) => {
                log::debug!("btle: 5 ms interval refused ({e}); retrying at spec minimum");
                self.le_connect_interval(address, address_type, 6, 12)
            }
        }
    }

    /// Connect with an explicit interval range, in 1.25 ms units.
    pub fn le_connect_interval(
        &self,
        address: [u8; 6],
        address_type: u8,
        interval_min: u16,
        interval_max: u16,
    ) -> Result<u16> {
        self.le_connect_params(address, address_type, interval_min, interval_max)
            .map(|p| p.conn_handle)
    }

    /// [`Dongle::le_connect_interval`], returning what was actually negotiated.
    ///
    /// The granted interval is not necessarily the one requested, and it was
    /// only ever written to `log::info!` — invisible to any tool that does not
    /// install a logger. A link that silently came up at the wrong interval
    /// looked identical to one that came up correctly and then misbehaved.
    pub fn le_connect_params(
        &self,
        address: [u8; 6],
        address_type: u8,
        interval_min: u16,
        interval_max: u16,
    ) -> Result<LinkParams> {
        let mut wire_addr = address;
        wire_addr.reverse();

        let mut p = Vec::with_capacity(25);
        p.extend_from_slice(&0x0060u16.to_le_bytes()); // scan interval
        p.extend_from_slice(&0x0030u16.to_le_bytes()); // scan window
        p.push(0x00); // initiator filter policy: use the address below
        p.push(address_type);
        p.extend_from_slice(&wire_addr);
        p.push(0x00); // own address type: public
        p.extend_from_slice(&interval_min.to_le_bytes());
        p.extend_from_slice(&interval_max.to_le_bytes());
        p.extend_from_slice(&0x0000u16.to_le_bytes()); // peripheral latency
        p.extend_from_slice(&0x01F4u16.to_le_bytes()); // supervision timeout: 5 s
        p.extend_from_slice(&0x0000u16.to_le_bytes()); // min CE length
        p.extend_from_slice(&0x0000u16.to_le_bytes()); // max CE length

        // Drain stale events BEFORE issuing the command. A Connection Complete
        // left over from a previous link would otherwise be returned as this
        // one's result, handing the caller a handle that is already in use —
        // and two "links" reading the same stream look exactly like two working
        // controllers until you notice their frames are byte-identical.
        while let Ok(Some(_)) = self.read_event_timeout(Duration::from_millis(2)) {}

        self.send_command(hci::Opcode::LE_CREATE_CONNECTION, &p)?;

        // Short reads, not the default 2 s. A refused parameter set answers
        // with Command Status and then nothing, so 40 iterations of a 2 s
        // blocking read spent 80 SECONDS failing — long enough that the caller's
        // own scan deadline expired and the whole probe looked dead.
        for _ in 0..40 {
            match self.read_event_timeout(Duration::from_millis(250))? {
                // A non-zero Command Status means the controller rejected the
                // request outright; waiting for a Connection Complete that can
                // never arrive is pointless.
                Some(Event::CommandStatus { status, opcode })
                    if opcode == hci::Opcode::LE_CREATE_CONNECTION && status != 0 =>
                {
                    let _ = self.command_sync(hci::Opcode::LE_CREATE_CONNECTION_CANCEL, &[]);
                    return Err(Error::Protocol(format!(
                        "LE_Create_Connection rejected, status {status:#04x}"
                    )));
                }
                Some(Event::LeConnectionComplete {
                    status,
                    conn_handle,
                    interval,
                    supervision_timeout,
                }) => {
                    if status != 0 {
                        return Err(Error::Protocol(format!(
                            "LE Connection Complete status {status:#04x}"
                        )));
                    }
                    log::info!(
                        "btle: connected handle {conn_handle:#06x} interval {:.2}ms timeout {}ms",
                        interval as f32 * 1.25,
                        supervision_timeout as u32 * 10,
                    );
                    return Ok(LinkParams { conn_handle, interval, supervision_timeout });
                }
                Some(_) => continue,
                None => continue,
            }
        }
        // Leave no dangling initiator: without this the controller keeps trying
        // to connect and refuses the next `LE_Create_Connection` with
        // "Command Disallowed".
        let _ = self.command_sync(hci::Opcode::LE_CREATE_CONNECTION_CANCEL, &[]);
        Err(Error::Protocol("no LE Connection Complete".into()))
    }

    /// Tear down a connection.
    pub fn disconnect(&self, conn_handle: u16) -> Result<()> {
        let mut p = conn_handle.to_le_bytes().to_vec();
        p.push(0x13); // reason: remote user terminated connection
        self.send_command(hci::Opcode::DISCONNECT, &p)?;
        Ok(())
    }

    /// Send a raw L2CAP payload on a BR/EDR link's channel.
    ///
    /// ⭐ Public because a live link still has signalling to answer — a
    /// configuration or disconnection request from the remote arrives long
    /// after setup, on the steady-state path, where there is no lease and no
    /// helper. Uses the classic packet-boundary flag; see `encode_acl_pb`.
    pub fn send_att_raw(&self, conn_handle: u16, cid: u16, payload: &[u8]) -> Result<()> {
        let packet = acl::encode_acl_pb(conn_handle, cid, payload, acl::PB_FIRST_FLUSHABLE);
        self.handle
            .write_bulk(self.acl_out_ep, &packet, self.timeout)?;
        Ok(())
    }

    /// Send an ATT PDU over the connection's ACL channel.
    pub fn send_att(&self, conn_handle: u16, att_pdu: &[u8]) -> Result<()> {
        let packet = acl::encode_acl(conn_handle, acl::CID_ATT, att_pdu);
        self.handle
            .write_bulk(self.acl_out_ep, &packet, self.timeout)?;
        Ok(())
    }

    /// Drain every ACL packet currently waiting, up to `limit`.
    ///
    /// Reading one packet per caller iteration is what starved the second
    /// controller: with a 10 ms blocking read per packet the whole process
    /// could not exceed ~90 packets a second, shared between both links, and
    /// whichever half was serviced first took nearly all of it. Two halves at
    /// 200 Hz need 400 packets a second, so the drain has to be greedy and the
    /// timeout short.
    pub fn drain_acl(&self, limit: usize) -> Vec<AclPacket> {
        let mut out = Vec::new();
        while out.len() < limit {
            match self.read_acl(Duration::from_millis(1)) {
                Ok(Some(p)) => out.push(p),
                Ok(None) => break,
                Err(_) => break,
            }
        }
        out
    }

    /// Whether this controller supports BR/EDR (Bluetooth Classic) at all.
    ///
    /// ⭐ **Ask this before anything else classic.** An LE-only controller
    /// answers every BR/EDR command with `Unknown HCI Command`, which is
    /// indistinguishable from a bug in the command encoding — and the whole
    /// point of a first milestone is to tell "the radio cannot" from "the code
    /// is wrong". `Read Local Supported Features` settles it in one command.
    ///
    /// The flag is inverted in the spec: byte 4 bit 5 is **BR/EDR NOT
    /// Supported**, so classic is available when that bit is CLEAR.
    pub fn supports_bredr(&self) -> Result<bool> {
        let c = self.command_sync(Opcode::READ_LOCAL_FEATURES, &[])?;
        // params: [status][8 bytes of LMP features]
        if c.params.len() < 9 {
            return Err(Error::Protocol(format!(
                "Read Local Supported Features returned {} bytes, expected 9",
                c.params.len()
            )));
        }
        Ok(c.params[5] & 0x20 == 0)
    }

    /// Send an L2CAP payload on a BR/EDR link.
    fn send_l2cap(&self, conn_handle: u16, cid: u16, payload: &[u8]) -> Result<()> {
        let packet = acl::encode_acl_pb(conn_handle, cid, payload, acl::PB_FIRST_FLUSHABLE);
        self.handle
            .write_bulk(self.acl_out_ep, &packet, self.timeout)?;
        Ok(())
    }

    /// Bring up both HID channels, from WHICHEVER side offers first.
    ///
    /// ⛔ **Either end may open an L2CAP channel, and this controller uses both
    /// ways depending on how it woke up.** Waiting for one particular side is
    /// therefore wrong in both directions, and each way of being wrong has its
    /// own silent failure:
    ///
    /// * Host always initiates → a reconnecting device that is already waiting
    ///   for us never answers: `PSM 0x0011 timed out (granted=false)`.
    /// * Host always waits → a device that expects the host to open the
    ///   channels sits there and the link dies of an LMP response timeout:
    ///   `remote never opened its HID channels`, `reason 0x24`.
    ///
    /// Both were traced on the SAME controller minutes apart — it offered its
    /// own channels on one reconnection and waited to be asked on the next. So
    /// this does not choose. It gives the remote a short head start, asks for
    /// whatever it has not offered by then, and answers what arrives either
    /// way, which is what L2CAP's symmetry has always permitted and what a host
    /// has to implement to be reliable.
    ///
    /// Returns `(control, interrupt)`; input reports arrive on the interrupt
    /// channel.
    pub fn l2cap_hid(
        &self,
        conn_handle: u16,
        cid_base: u16,
        patience: Duration,
        on_event: &mut dyn FnMut(&str),
    ) -> Result<(l2cap::Channel, l2cap::Channel)> {
        use l2cap::*;

        /// How long the remote gets to start before we do.
        ///
        /// ❗ Long enough that a device which intends to initiate is not raced —
        /// both sides opening one PSM at once is legal but pointless churn —
        /// and short enough that a device which never will is not left waiting
        /// anywhere near the link supervision timeout.
        const GRACE: Duration = Duration::from_millis(700);

        struct Half {
            psm: u16,
            local_cid: u16,
            remote_cid: u16,
            ours_configured: bool,
            theirs_configured: bool,
            asked: bool,
        }
        impl Half {
            fn done(&self) -> bool {
                self.remote_cid != 0 && self.ours_configured && self.theirs_configured
            }
        }

        let mut halves = [
            Half {
                psm: PSM_HID_CONTROL,
                local_cid: cid_base,
                remote_cid: 0,
                ours_configured: false,
                theirs_configured: false,
                asked: false,
            },
            Half {
                psm: PSM_HID_INTERRUPT,
                local_cid: cid_base + 1,
                remote_cid: 0,
                ours_configured: false,
                theirs_configured: false,
                asked: false,
            },
        ];
        let mut ident: u8 = 1;
        let start = std::time::Instant::now();
        let deadline = start + patience;

        while std::time::Instant::now() < deadline {
            // Ask for whatever the remote has not offered by now.
            if start.elapsed() >= GRACE {
                for h in halves.iter_mut() {
                    if h.asked || h.remote_cid != 0 {
                        continue;
                    }
                    h.asked = true;
                    ident = ident.wrapping_add(1);
                    on_event(&format!("PSM {:#06x}: remote did not offer — asking", h.psm));
                    self.send_l2cap(
                        conn_handle,
                        CID_SIGNALLING,
                        &encode_signal(
                            SIG_CONNECTION_REQUEST,
                            ident,
                            &connection_request(h.psm, h.local_cid),
                        ),
                    )?;
                }
            }

            let Some(pkt) = self.read_acl(Duration::from_millis(100))? else {
                continue;
            };
            if pkt.conn_handle != conn_handle || pkt.cid != CID_SIGNALLING {
                continue;
            }
            let Some(sig) = parse_signal(&pkt.payload) else { continue };
            match sig.code {
                SIG_CONNECTION_REQUEST => {
                    let Some((psm, their_cid)) = parse_connection_request(&sig.data) else {
                        continue;
                    };
                    let Some(idx) = halves.iter().position(|h| h.psm == psm) else {
                        // ❗ Refused, not ignored: silence makes the remote
                        // retry until it gives up on the whole link.
                        let refuse = encode_signal(
                            SIG_CONNECTION_RESPONSE,
                            sig.identifier,
                            // 0x0002 = PSM not supported.
                            &connection_response(0, their_cid, 0x0002),
                        );
                        let _ = self.send_l2cap(conn_handle, CID_SIGNALLING, &refuse);
                        continue;
                    };
                    // ⭐ A remote offer WINS even if we had already asked. It is
                    // the side that will be sending the reports, and letting its
                    // channel be the live one avoids two half-open pairs.
                    let (local_cid, remote_cid) = {
                        let h = &mut halves[idx];
                        h.local_cid = cid_base + 4 + idx as u16;
                        h.remote_cid = their_cid;
                        h.ours_configured = false;
                        h.theirs_configured = false;
                        (h.local_cid, h.remote_cid)
                    };
                    on_event(&format!(
                        "PSM {psm:#06x}: remote opened it — granting as cid {local_cid:#06x}"
                    ));
                    self.send_l2cap(
                        conn_handle,
                        CID_SIGNALLING,
                        &encode_signal(
                            SIG_CONNECTION_RESPONSE,
                            sig.identifier,
                            &connection_response(local_cid, their_cid, 0),
                        ),
                    )?;
                    ident = ident.wrapping_add(1);
                    self.send_l2cap(
                        conn_handle,
                        CID_SIGNALLING,
                        &encode_signal(
                            SIG_CONFIGURE_REQUEST,
                            ident,
                            &configure_request(remote_cid, 672),
                        ),
                    )?;
                }
                SIG_CONNECTION_RESPONSE => {
                    let Some(r) = parse_connection_response(&sig.data) else { continue };
                    let Some(idx) = halves.iter().position(|h| h.local_cid == r.source_cid)
                    else {
                        continue;
                    };
                    match r.result {
                        0x0000 => {
                            halves[idx].remote_cid = r.dest_cid;
                            on_event(&format!(
                                "PSM {:#06x}: granted, remote cid {:#06x}",
                                halves[idx].psm, r.dest_cid
                            ));
                            ident = ident.wrapping_add(1);
                            self.send_l2cap(
                                conn_handle,
                                CID_SIGNALLING,
                                &encode_signal(
                                    SIG_CONFIGURE_REQUEST,
                                    ident,
                                    &configure_request(r.dest_cid, 672),
                                ),
                            )?;
                        }
                        // Still deciding — normal while it authenticates.
                        0x0001 => on_event(&format!("PSM {:#06x}: pending…", halves[idx].psm)),
                        other => {
                            return Err(Error::Protocol(format!(
                                "PSM {:#06x} refused: result {other:#06x}",
                                halves[idx].psm
                            )))
                        }
                    }
                }
                SIG_CONFIGURE_REQUEST => {
                    let Some((dest, opts)) = parse_configure_request(&sig.data) else {
                        continue;
                    };
                    // `dest` is OUR cid as the remote sees it. Answer even when
                    // it names a channel we do not know, or that one never
                    // opens; the reply is addressed with the remote's own id.
                    let reply_to = halves
                        .iter()
                        .find(|h| h.local_cid == dest)
                        .map(|h| h.remote_cid)
                        .filter(|c| *c != 0)
                        .unwrap_or(dest);
                    self.send_l2cap(
                        conn_handle,
                        CID_SIGNALLING,
                        &encode_signal(
                            SIG_CONFIGURE_RESPONSE,
                            sig.identifier,
                            &configure_response(reply_to, &opts),
                        ),
                    )?;
                    if let Some(h) = halves.iter_mut().find(|h| h.local_cid == dest) {
                        h.theirs_configured = true;
                        on_event(&format!("PSM {:#06x}: their side configured", h.psm));
                    }
                }
                SIG_CONFIGURE_RESPONSE => {
                    let Some((scid, result)) = parse_configure_response_full(&sig.data) else {
                        continue;
                    };
                    let Some(h) = halves.iter_mut().find(|h| h.local_cid == scid) else {
                        continue;
                    };
                    if result != 0 {
                        return Err(Error::Protocol(format!(
                            "PSM {:#06x} configuration refused: {result:#06x}",
                            h.psm
                        )));
                    }
                    h.ours_configured = true;
                    on_event(&format!("PSM {:#06x}: our side configured", h.psm));
                }
                SIG_DISCONNECTION_REQUEST => {
                    let reply =
                        encode_signal(SIG_DISCONNECTION_RESPONSE, sig.identifier, &sig.data);
                    let _ = self.send_l2cap(conn_handle, CID_SIGNALLING, &reply);
                    return Err(Error::Protocol("remote closed a channel".into()));
                }
                _ => {}
            }

            if halves.iter().all(Half::done) {
                return Ok((
                    l2cap::Channel {
                        psm: halves[0].psm,
                        local_cid: halves[0].local_cid,
                        remote_cid: halves[0].remote_cid,
                    },
                    l2cap::Channel {
                        psm: halves[1].psm,
                        local_cid: halves[1].local_cid,
                        remote_cid: halves[1].remote_cid,
                    },
                ));
            }
        }
        Err(Error::Protocol(format!(
            "HID channels timed out (control: granted={} ours={} theirs={}; \
             interrupt: granted={} ours={} theirs={})",
            halves[0].remote_cid != 0,
            halves[0].ours_configured,
            halves[0].theirs_configured,
            halves[1].remote_cid != 0,
            halves[1].ours_configured,
            halves[1].theirs_configured,
        )))
    }

    /// Wait for the REMOTE to open its HID channels, granting each one.
    ///
    /// ⭐ **On an incoming link the device is the initiator, and this is the
    /// only thing that works.** A reconnecting Bluetooth HID device sends its
    /// own `Connection Request` for PSM `0x11` then `0x13` and waits to be
    /// answered. A host that instead sends requests of its own gets silence
    /// from a device already waiting on it — both blocked, until the link dies
    /// of an LMP response timeout about ten seconds later.
    ///
    /// Traced on hardware: every incoming reconnection failed as
    /// `PSM 0x0011 timed out (granted=false)` followed by
    /// `DROPPED (reason 0x24)`, while the same controller PAGED from this host
    /// connected and streamed perfectly. Same controller, same keys, opposite
    /// direction of setup.
    ///
    /// Returns `(control, interrupt)` once the interrupt channel is granted and
    /// configured — that is the one input arrives on.
    pub fn l2cap_accept(
        &self,
        conn_handle: u16,
        patience: Duration,
        on_event: &mut dyn FnMut(&str),
    ) -> Result<(l2cap::Channel, l2cap::Channel)> {
        use l2cap::*;
        let mut control: Option<Channel> = None;
        let mut interrupt: Option<Channel> = None;
        let deadline = std::time::Instant::now() + patience;
        while std::time::Instant::now() < deadline {
            let Some(pkt) = self.read_acl(Duration::from_millis(100))? else {
                continue;
            };
            if pkt.conn_handle != conn_handle || pkt.cid != CID_SIGNALLING {
                continue;
            }
            let Some(sig) = parse_signal(&pkt.payload) else { continue };
            match sig.code {
                SIG_CONNECTION_REQUEST => {
                    let Some((psm, their_cid)) = parse_connection_request(&sig.data) else {
                        continue;
                    };
                    // Our ids are ours to choose; keep them distinct per PSM.
                    let our_cid = match psm {
                        PSM_HID_CONTROL => FIRST_DYNAMIC_CID,
                        PSM_HID_INTERRUPT => FIRST_DYNAMIC_CID + 1,
                        // Anything else is refused rather than ignored: silence
                        // makes the remote retry until it gives up on the link.
                        _ => {
                            let refuse = encode_signal(
                                SIG_CONNECTION_RESPONSE,
                                sig.identifier,
                                // 0x0002 = PSM not supported.
                                &connection_response(0, their_cid, 0x0002),
                            );
                            let _ = self.send_att_raw(conn_handle, CID_SIGNALLING, &refuse);
                            continue;
                        }
                    };
                    on_event(&format!("granting PSM {psm:#06x} as cid {our_cid:#06x}"));
                    let resp = encode_signal(
                        SIG_CONNECTION_RESPONSE,
                        sig.identifier,
                        &connection_response(our_cid, their_cid, 0),
                    );
                    self.send_att_raw(conn_handle, CID_SIGNALLING, &resp)?;
                    // Configure our side immediately; the remote configures its
                    // own and we answer that below.
                    let cfg = encode_signal(
                        SIG_CONFIGURE_REQUEST,
                        sig.identifier.wrapping_add(1),
                        &configure_request(their_cid, 672),
                    );
                    self.send_att_raw(conn_handle, CID_SIGNALLING, &cfg)?;
                    let ch = Channel { psm, local_cid: our_cid, remote_cid: their_cid };
                    if psm == PSM_HID_INTERRUPT {
                        interrupt = Some(ch);
                    } else {
                        control = Some(ch);
                    }
                }
                SIG_CONFIGURE_REQUEST => {
                    if let Some((dest, opts)) = parse_configure_request(&sig.data) {
                        // Echo the options back — a bare success makes some
                        // devices re-request forever.
                        let remote = [control, interrupt]
                            .iter()
                            .flatten()
                            .find(|c| c.local_cid == dest)
                            .map(|c| c.remote_cid)
                            .unwrap_or(dest);
                        let reply = encode_signal(
                            SIG_CONFIGURE_RESPONSE,
                            sig.identifier,
                            &configure_response(remote, &opts),
                        );
                        self.send_att_raw(conn_handle, CID_SIGNALLING, &reply)?;
                    }
                }
                SIG_DISCONNECTION_REQUEST => {
                    let reply =
                        encode_signal(SIG_DISCONNECTION_RESPONSE, sig.identifier, &sig.data);
                    let _ = self.send_att_raw(conn_handle, CID_SIGNALLING, &reply);
                    return Err(Error::Protocol("remote closed a channel".into()));
                }
                _ => {}
            }
            if let (Some(c), Some(i)) = (control, interrupt) {
                return Ok((c, i));
            }
        }
        Err(Error::Protocol(format!(
            "remote never opened its HID channels (control={}, interrupt={})",
            control.is_some(),
            interrupt.is_some()
        )))
    }

    /// Open one L2CAP connection-oriented channel and configure it both ways.
    ///
    /// ⭐ **Both ways is the whole point.** Requesting a channel and configuring
    /// our own side leaves the remote waiting for a Configuration Response it
    /// will never get — the channel reads as connected and delivers nothing,
    /// which is indistinguishable from a controller that simply is not sending.
    /// This drives the exchange to completion in both directions before it
    /// reports success.
    ///
    /// `local_cid` must be at least [`l2cap::FIRST_DYNAMIC_CID`] and unique on
    /// this link.
    ///
    /// ❗ Signalling for OTHER channels is handled while waiting, not ignored.
    /// The controller may configure the control channel while the interrupt one
    /// is still being set up, and a host that drops those packets deadlocks
    /// both.
    pub fn l2cap_connect(
        &self,
        conn_handle: u16,
        psm: u16,
        local_cid: u16,
        on_event: &mut dyn FnMut(&str),
    ) -> Result<l2cap::Channel> {
        use l2cap::*;
        let mut ident: u8 = 1;
        self.send_l2cap(
            conn_handle,
            CID_SIGNALLING,
            &encode_signal(SIG_CONNECTION_REQUEST, ident, &connection_request(psm, local_cid)),
        )?;

        let mut remote_cid = 0u16;
        let mut ours_configured = false;
        let mut theirs_configured = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            let Some(pkt) = self.read_acl(Duration::from_millis(250))? else {
                continue;
            };
            if pkt.conn_handle != conn_handle || pkt.cid != CID_SIGNALLING {
                continue;
            }
            let Some(sig) = parse_signal(&pkt.payload) else { continue };
            match sig.code {
                SIG_CONNECTION_RESPONSE => {
                    let Some(r) = parse_connection_response(&sig.data) else { continue };
                    if r.source_cid != local_cid {
                        continue; // a response for some other channel
                    }
                    match r.result {
                        0x0000 => {
                            remote_cid = r.dest_cid;
                            on_event(&format!(
                                "PSM {psm:#06x}: channel granted, remote cid {remote_cid:#06x}"
                            ));
                            ident = ident.wrapping_add(1);
                            self.send_l2cap(
                                conn_handle,
                                CID_SIGNALLING,
                                &encode_signal(
                                    SIG_CONFIGURE_REQUEST,
                                    ident,
                                    // 672 is the L2CAP default MTU; a HID report
                                    // is far smaller, and asking for more invites
                                    // a renegotiation for no benefit.
                                    &configure_request(remote_cid, 672),
                                ),
                            )?;
                        }
                        // 0x0001 is "pending" — the remote is still deciding,
                        // which is normal while it authenticates. Keep waiting.
                        0x0001 => on_event(&format!("PSM {psm:#06x}: pending…")),
                        other => {
                            return Err(Error::Protocol(format!(
                                "PSM {psm:#06x} refused: result {other:#06x}"
                            )))
                        }
                    }
                }
                SIG_CONFIGURE_RESPONSE => {
                    match parse_configure_response(&sig.data) {
                        Some(0) => {
                            ours_configured = true;
                            on_event(&format!("PSM {psm:#06x}: our side configured"));
                        }
                        Some(other) => {
                            return Err(Error::Protocol(format!(
                                "PSM {psm:#06x} configuration refused: {other:#06x}"
                            )))
                        }
                        None => {}
                    }
                }
                SIG_CONFIGURE_REQUEST => {
                    // The remote configuring US. Echo its options back with a
                    // success result — see `configure_response`.
                    if let Some((dest, opts)) = parse_configure_request(&sig.data) {
                        // `dest` is OUR cid from their point of view; answer
                        // even when it is another channel's, or that channel
                        // never opens.
                        let reply_to = if dest == local_cid { remote_cid } else { dest };
                        self.send_l2cap(
                            conn_handle,
                            CID_SIGNALLING,
                            &encode_signal(
                                SIG_CONFIGURE_RESPONSE,
                                sig.identifier,
                                &configure_response(reply_to, &opts),
                            ),
                        )?;
                        if dest == local_cid {
                            theirs_configured = true;
                            on_event(&format!("PSM {psm:#06x}: their side configured"));
                        }
                    }
                }
                SIG_DISCONNECTION_REQUEST => {
                    self.send_l2cap(
                        conn_handle,
                        CID_SIGNALLING,
                        &encode_signal(SIG_DISCONNECTION_RESPONSE, sig.identifier, &sig.data),
                    )?;
                    return Err(Error::Protocol(format!(
                        "PSM {psm:#06x}: remote closed the channel"
                    )));
                }
                _ => {}
            }
            if remote_cid != 0 && ours_configured && theirs_configured {
                return Ok(l2cap::Channel { psm, local_cid, remote_cid });
            }
        }
        Err(Error::Protocol(format!(
            "PSM {psm:#06x} timed out (granted={}, ours={ours_configured}, \
             theirs={theirs_configured})",
            remote_cid != 0
        )))
    }

    /// The outcome of paging and pairing a classic device.
    ///
    /// The link key is the part worth keeping: with it stored, the same
    /// controller reconnects to this dongle without pairing again, on any host
    /// that has the key.
    ///
    /// ⭐ The BOND lives between the controller and the DONGLE'S address, not
    /// this PC — so the same dongle carried to another machine is still the
    /// device the controller is looking for. What has to travel with it is this
    /// key, because both ends must hold it.
    ///
    /// ❗ `patience` is a real choice, not a safety margin. A user-driven
    /// pairing can afford half a minute for someone to walk over and press
    /// Sync. A background reconnect cannot: the caller's thread is also
    /// servicing controllers that are ALREADY connected, so every second spent
    /// waiting here is a second of their input on the floor.
    pub fn page_and_pair(
        &self,
        addr: [u8; 6],
        psrm: u8,
        clock_offset: u16,
        known_key: Option<[u8; 16]>,
        // How long to keep trying — see the note on `patience` above.
        patience: Duration,
        on_event: &mut dyn FnMut(&str),
    ) -> Result<ClassicLink> {
        // Secure Simple Pairing must be ON before paging: it is what makes the
        // controller use SSP rather than legacy PIN pairing, which modern
        // gamepads refuse outright. Best-effort — a controller that rejects it
        // will simply fail later, more informatively.
        if let Err(e) = self.command_sync(Opcode::WRITE_SIMPLE_PAIRING_MODE, &[0x01]) {
            on_event(&format!("simple pairing mode refused: {e}"));
        }

        // Create Connection: address, packet types, page scan repetition mode,
        // reserved, clock offset, allow role switch.
        //
        // ❗ 0xCC18 is the standard DM1/DH1/DM3/DH3/DM5/DH5 packet-type mask.
        // Offering too few packet types makes a link that negotiates but then
        // cannot carry a full-size HID report.
        // A page that fails because the remote was busy is ordinary; one that
        // fails because we never gave it long enough is not.
        // ⭐ Skipped when the remote is ALREADY calling us. `psrm == NO_PAGE`
        // means "an incoming connection has been accepted, just drive the
        // authentication" — paging a device that is mid-page at us is how two
        // radios end up talking past each other.
        if psrm != NO_PAGE {
            // ❗ Matched to the caller's patience, and only set when we are
            // actually paging. The two used to disagree — 8 s of paging behind
            // a 2 s wait — leaving the radio deaf for six seconds after every
            // attempt the host had already given up on.
            let _ = self.set_page_timeout(patience.as_secs_f32().clamp(1.0, 20.0));
            let mut params = Vec::with_capacity(13);
            params.extend_from_slice(&addr);
            params.extend_from_slice(&0xCC18u16.to_le_bytes());
            params.push(psrm);
            params.push(0x00);
            params.extend_from_slice(&clock_offset.to_le_bytes());
            params.push(0x01); // allow role switch
            self.send_command(Opcode::CREATE_CONNECTION, &params)?;
        }

        let mut link = ClassicLink {
            conn_handle: 0,
            address: addr,
            link_key: known_key,
            encrypted: false,
            incoming: psrm == NO_PAGE,
        };
        let mut connected = false;
        let mut paired = known_key.is_some();
        // Paging a controller that is awake is quick; one that has to be woken
        // by its Sync button can take most of this.
        let deadline = std::time::Instant::now() + patience;
        while std::time::Instant::now() < deadline {
            let Some(evt) = self.read_event_timeout(Duration::from_millis(250))? else {
                continue;
            };
            match evt {
                Event::ConnectionComplete { status, conn_handle, address } if address == addr => {
                    if status != 0 {
                        return Err(Error::Protocol(format!(
                            "page failed: status {status:#04x}"
                        )));
                    }
                    link.conn_handle = conn_handle;
                    connected = true;
                    on_event(&format!("ACL link up, handle {conn_handle:#06x}"));
                    // Nothing else demands authentication on a Just Works pair,
                    // so ask for it — otherwise the link sits unencrypted and
                    // the HID interrupt channel will be refused later.
                    if let Err(e) = self.send_command(
                        Opcode::AUTHENTICATION_REQUESTED,
                        &conn_handle.to_le_bytes(),
                    ) {
                        on_event(&format!("authentication request failed: {e}"));
                    }
                }
                // ⛔ **The collision that made reconnection impossible.**
                //
                // A bonded controller pages its host the moment it is switched
                // on; the host, finding it absent, pages back. Two radios that
                // are both paging are both deaf, and this arm did not exist —
                // so the controller's Connection Request fell through `_ => {}`
                // and was DISCARDED. Unanswered, the pad gave up and retried
                // from scratch, forever: the "blinks and searches again" loop,
                // with nothing in the host log to show a request had ever
                // arrived, because the code that ate it never mentioned it.
                //
                // Answering is also simply correct — it is calling us, and the
                // link it opens is the same link we wanted.
                Event::ConnectionRequest { address, .. } if address == addr && !connected => {
                    on_event("remote is paging us mid-page — answering instead");
                    let _ = self.cancel_page(addr);
                    self.accept_connection(addr)?;
                    link.incoming = true;
                }
                Event::LinkKeyRequest { address } if address == addr => {
                    match link.link_key {
                        Some(k) => {
                            on_event("remote asked for a stored link key — supplying it");
                            let mut p = Vec::with_capacity(22);
                            p.extend_from_slice(&addr);
                            p.extend_from_slice(&k);
                            self.command_sync(Opcode::LINK_KEY_REQUEST_REPLY, &p)?;
                        }
                        None => {
                            // Saying "no key" is what STARTS pairing. A host
                            // that stays silent here is disconnected.
                            on_event("no stored key — starting Secure Simple Pairing");
                            self.command_sync(
                                Opcode::LINK_KEY_REQUEST_NEGATIVE_REPLY,
                                &addr,
                            )?;
                        }
                    }
                }
                Event::IoCapabilityRequest { address } if address == addr => {
                    // NoInputNoOutput, no OOB data, MITM protection not
                    // required — "Just Works". A dongle has no screen and no
                    // keypad, and claiming otherwise makes the controller ask
                    // for a comparison nobody can answer.
                    let mut p = Vec::with_capacity(9);
                    p.extend_from_slice(&addr);
                    p.push(0x03); // NoInputNoOutput
                    p.push(0x00); // no OOB
                    p.push(0x00); // MITM not required
                    self.command_sync(Opcode::IO_CAPABILITY_REQUEST_REPLY, &p)?;
                    on_event("declared NoInputNoOutput (Just Works)");
                }
                Event::UserConfirmationRequest { address, .. } if address == addr => {
                    self.command_sync(Opcode::USER_CONFIRMATION_REQUEST_REPLY, &addr)?;
                    on_event("confirmed pairing");
                }
                Event::LinkKeyNotification { address, key, key_type } if address == addr => {
                    link.link_key = Some(key);
                    paired = true;
                    on_event(&format!("⭐ link key received (type {key_type:#04x})"));
                }
                Event::SimplePairingComplete { status, address } if address == addr => {
                    if status != 0 {
                        return Err(Error::Protocol(format!(
                            "pairing failed: status {status:#04x}"
                        )));
                    }
                    on_event("simple pairing complete");
                }
                Event::AuthenticationComplete { status, conn_handle }
                    if connected && conn_handle == link.conn_handle =>
                {
                    if status != 0 {
                        return Err(Error::Protocol(format!(
                            "authentication failed: status {status:#04x}"
                        )));
                    }
                    on_event("authenticated — enabling encryption");
                    let mut p = Vec::with_capacity(3);
                    p.extend_from_slice(&conn_handle.to_le_bytes());
                    p.push(0x01);
                    self.send_command(Opcode::SET_CONNECTION_ENCRYPTION, &p)?;
                }
                Event::EncryptionChange { status, conn_handle, enabled }
                    if connected && conn_handle == link.conn_handle =>
                {
                    if status == 0 && enabled != 0 {
                        link.encrypted = true;
                        on_event("⭐ link encrypted");
                        return Ok(link);
                    }
                    return Err(Error::Protocol(format!(
                        "encryption refused: status {status:#04x} enabled {enabled}"
                    )));
                }
                Event::DisconnectionComplete { conn_handle, reason }
                    if connected && conn_handle == link.conn_handle =>
                {
                    return Err(Error::Protocol(format!(
                        "remote dropped the link (reason {reason:#04x}) — \
                         paired={paired}, encrypted={}",
                        link.encrypted
                    )));
                }
                _ => {}
            }
        }
        if connected {
            let _ = self.disconnect(link.conn_handle);
        } else if psrm != NO_PAGE {
            // ⛔ Take the radio out of paging on the way out. Without this the
            // dongle goes on calling a controller that is calling US, and the
            // two page past each other for the rest of the page timeout.
            let _ = self.cancel_page(addr);
        }
        Err(Error::Protocol(format!(
            "timed out (connected={connected}, paired={paired})"
        )))
    }

    /// How long a page keeps trying before giving up, in seconds.
    ///
    /// Clamped to the 0.625 ms-slot range the command takes.
    pub fn set_page_timeout(&self, secs: f32) -> Result<()> {
        let slots = ((secs / 0.000_625) as u32).clamp(1, 0xFFFF) as u16;
        self.command_sync(Opcode::WRITE_PAGE_TIMEOUT, &slots.to_le_bytes())?;
        Ok(())
    }

    /// Run an inquiry, stopping as soon as `wanted` matches a device.
    ///
    /// ⭐ **Stopping early is the point, not an optimisation.** A device only
    /// answers a page while it is page-scanning, and a controller in pairing
    /// mode does not stay in that state indefinitely — it cycles, and it gives
    /// up. Running a fixed eight-second inquiry and only THEN paging spends the
    /// entire window during which the answer was easy, and the page that
    /// follows lands after the controller has moved on. That is what a
    /// `Page Timeout (0x04)` on a device that plainly just answered an inquiry
    /// actually means.
    ///
    /// Cancelling the moment the wanted device replies means the page goes out
    /// while it is still listening for one.
    pub fn inquiry_until(
        &self,
        secs: f32,
        wanted: &mut dyn FnMut(&hci::InquiryResult) -> bool,
    ) -> Result<Vec<hci::InquiryResult>> {
        const GIAC: [u8; 3] = [0x33, 0x8B, 0x9E];
        let units = ((secs / 1.28).round() as u8).clamp(1, 48);
        let mut params = Vec::with_capacity(5);
        params.extend_from_slice(&GIAC);
        params.push(units);
        params.push(0x00);
        self.send_command(Opcode::INQUIRY, &params)?;

        let mut found = Vec::new();
        let mut satisfied = false;
        let deadline =
            std::time::Instant::now() + Duration::from_secs_f32(units as f32 * 1.28 + 2.0);
        while !satisfied && std::time::Instant::now() < deadline {
            match self.read_event_timeout(Duration::from_millis(200))? {
                Some(Event::InquiryResults(r)) => {
                    satisfied = r.iter().any(|x| wanted(x));
                    found.extend(r);
                }
                Some(Event::InquiryComplete { .. }) => break,
                Some(Event::CommandStatus { status, opcode })
                    if opcode == Opcode::INQUIRY && status != 0 =>
                {
                    return Err(Error::Protocol(format!(
                        "inquiry refused with status {status:#04x}"
                    )));
                }
                _ => {}
            }
        }
        // ❗ Always cancelled, and the completion WAITED FOR. An inquiry still
        // running blocks paging outright, so returning while it winds down
        // guarantees the very Page Timeout this exists to avoid.
        let _ = self.command_sync(Opcode::INQUIRY_CANCEL, &[]);
        let settle = std::time::Instant::now() + Duration::from_millis(500);
        while std::time::Instant::now() < settle {
            match self.read_event_timeout(Duration::from_millis(100)) {
                Ok(Some(Event::InquiryComplete { .. })) => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        Ok(found)
    }

    /// Make this radio answer incoming pages, so a bonded controller can
    /// reconnect on its own.
    ///
    /// ⭐ `0x02` is page scan alone — discoverable to devices that already know
    /// this address, invisible to a general inquiry. That is the right setting
    /// for a dongle serving controllers: it must accept its own pads calling
    /// home without advertising itself to every phone in the room.
    ///
    /// ❗ The returned status is CHECKED. `command_sync` yields the Command
    /// Complete and this dropped it, so a controller that REFUSED to scan
    /// reported success — and the symptom is a pad that blinks at a radio which
    /// looks, from every log line, correctly set up.
    pub fn set_scan_enable(&self, mask: u8) -> Result<()> {
        let cc = self.command_sync(Opcode::WRITE_SCAN_ENABLE, &[mask])?;
        match cc.status() {
            Some(0) => Ok(()),
            other => Err(Error::Protocol(format!(
                "scan enable {mask:#04x} refused: status {:#04x}",
                other.unwrap_or(0xFF)
            ))),
        }
    }

    /// What the controller's scan state ACTUALLY is: bit 0 inquiry, bit 1 page.
    ///
    /// ⭐ Ground truth rather than "we sent the command". Worth having because
    /// every failure of incoming reconnection looks identical from the host
    /// side — nothing arrives — whether the radio is deaf, busy paging, or
    /// simply not being called.
    pub fn read_scan_enable(&self) -> Result<u8> {
        let cc = self.command_sync(Opcode::READ_SCAN_ENABLE, &[])?;
        match (cc.status(), cc.params.get(1)) {
            (Some(0), Some(&mask)) => Ok(mask),
            (st, _) => Err(Error::Protocol(format!(
                "read scan enable failed: status {:#04x}",
                st.unwrap_or(0xFF)
            ))),
        }
    }

    /// Abandon a page that is still outstanding.
    ///
    /// ⛔ **A radio that is paging cannot page-scan.** Giving up on a page in
    /// host code does nothing to the radio, which keeps paging until its own
    /// page timeout expires — so a host that pages a switched-off pad and waits
    /// only a fraction of that timeout stays deaf for the remainder, and deaf
    /// is exactly when the pad is calling. Both sides page, neither listens,
    /// and nothing connects.
    ///
    /// Errors are uninteresting: the usual one is "no such connection", because
    /// the page had already finished on its own.
    pub fn cancel_page(&self, addr: [u8; 6]) -> Result<()> {
        self.command_sync(Opcode::CREATE_CONNECTION_CANCEL, &addr)?;
        Ok(())
    }

    /// Accept an incoming connection from `addr`.
    ///
    /// ⛔ **Role `0x01` — REMAIN SLAVE. Do not ask for a role switch.**
    ///
    /// This asked for `0x00`, "become master", on the reasoning that a host
    /// ought to drive the link. The reasoning was wrong and the cost was the
    /// whole incoming path: a controller that pages us arrives as master, the
    /// switch is an LMP procedure it does not complete, and roughly ten seconds
    /// later the link dies of `LMP Response Timeout` (`0x24`).
    ///
    /// What that looks like from above is deeply misleading. Authentication
    /// succeeds, encryption succeeds — those are LMP procedures that DO
    /// complete — and then every L2CAP request goes out and is never answered,
    /// so it reads as a device that refuses to open its HID channels. The same
    /// controller, paged by us (where no switch is needed because we are
    /// already master), sets both channels up in well under a second.
    ///
    /// Being slave costs nothing here: L2CAP and HID are symmetric, and the
    /// working outgoing links prove the channels behave identically either way.
    pub fn accept_connection(&self, addr: [u8; 6]) -> Result<()> {
        let mut p = Vec::with_capacity(7);
        p.extend_from_slice(&addr);
        p.push(0x01);
        // Answers with Command Status; the link shows up as Connection Complete.
        self.send_command(Opcode::ACCEPT_CONNECTION_REQUEST, &p)
    }

    /// Ask a discovered device for its friendly name.
    ///
    /// ⭐ Worth doing before connecting to anything. A Class of Device says
    /// "some gamepad"; the name says WHICH, and paging the wrong device in a
    /// room full of Bluetooth is how a bond gets disturbed by accident.
    ///
    /// `psrm` and `clock_offset` come from the inquiry result for this device —
    /// the controller needs them to know when the remote is listening. The
    /// clock offset's top bit is a validity flag the inquiry does not set, so
    /// it is passed through as received.
    pub fn remote_name(&self, addr: [u8; 6], psrm: u8, clock_offset: u16) -> Result<String> {
        let mut params = Vec::with_capacity(10);
        params.extend_from_slice(&addr);
        params.push(psrm);
        params.push(0x00); // reserved
        params.extend_from_slice(&clock_offset.to_le_bytes());
        // Like Inquiry, this answers with Command Status and completes later.
        self.send_command(Opcode::REMOTE_NAME_REQUEST, &params)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(6);
        while std::time::Instant::now() < deadline {
            match self.read_event_timeout(Duration::from_millis(200))? {
                Some(Event::RemoteNameComplete { status, address, name })
                    if address == addr =>
                {
                    return if status == 0 {
                        Ok(name)
                    } else {
                        Err(Error::Protocol(format!("name request failed: {status:#04x}")))
                    };
                }
                _ => {}
            }
        }
        Err(Error::Protocol("no name response".into()))
    }

    /// Ask the controller for RSSI (and optionally EIR) in inquiry results.
    ///
    /// Best-effort: a controller that refuses simply keeps the older format,
    /// which costs signal strength and nothing else.
    pub fn set_inquiry_mode(&self, mode: u8) -> Result<()> {
        self.command_sync(Opcode::WRITE_INQUIRY_MODE, &[mode])?;
        Ok(())
    }

    /// Run a BR/EDR inquiry and collect what answers.
    ///
    /// ⭐ **This is the classic equivalent of an LE scan, and it works the
    /// other way round.** An LE device advertises continuously and the host
    /// listens; a classic device is silent until the host asks the whole room
    /// at once, on a hopping sequence, and waits for replies. That is why it
    /// takes seconds rather than being instant, and why a controller has to be
    /// in pairing mode (discoverable) to answer at all.
    ///
    /// `secs` is rounded to the 1.28 s units the command takes, and clamped to
    /// the 1–61 s the spec allows.
    ///
    /// ❗ Duplicates are expected and are NOT filtered here: a device answers
    /// once per inquiry cycle, so the same address arrives repeatedly with
    /// varying RSSI. The caller decides what to do with that — for choosing a
    /// device to pair, several samples of RSSI is useful information rather
    /// than noise.
    pub fn inquiry(&self, secs: f32) -> Result<Vec<hci::InquiryResult>> {
        // General/Unlimited Inquiry Access Code, 0x9E8B33, little-endian.
        const GIAC: [u8; 3] = [0x33, 0x8B, 0x9E];
        let units = ((secs / 1.28).round() as u8).clamp(1, 48);
        let mut params = Vec::with_capacity(5);
        params.extend_from_slice(&GIAC);
        params.push(units);
        params.push(0x00); // unlimited responses
        // ❗ Inquiry answers with Command STATUS, not Command Complete: it is a
        // long-running operation whose result arrives later as events. Waiting
        // for a Command Complete here would time out on a working radio.
        self.send_command(Opcode::INQUIRY, &params)?;

        let mut found = Vec::new();
        let deadline = std::time::Instant::now()
            + Duration::from_secs_f32(units as f32 * 1.28 + 2.0);
        while std::time::Instant::now() < deadline {
            match self.read_event_timeout(Duration::from_millis(200))? {
                Some(Event::InquiryResults(mut r)) => found.append(&mut r),
                Some(Event::InquiryComplete { .. }) => break,
                Some(Event::CommandStatus { status, opcode })
                    if opcode == Opcode::INQUIRY && status != 0 =>
                {
                    return Err(Error::Protocol(format!(
                        "inquiry refused with status {status:#04x}"
                    )));
                }
                _ => {}
            }
        }
        // Leaves the radio idle even if the loop ended on the deadline rather
        // than on Inquiry Complete; an inquiry still running blocks paging.
        let _ = self.command_sync(Opcode::INQUIRY_CANCEL, &[]);
        Ok(found)
    }

    /// Read one inbound ACL packet, or `Ok(None)` on timeout.
    ///
    /// ⭐ **One bulk transfer can carry SEVERAL ACL packets, and this used to
    /// keep only the first.** Everything past `4 + total_len` went on the
    /// floor — silently, with no error and no counter.
    ///
    /// It is the same mistake already found and fixed on the event endpoint,
    /// left standing here because the data path "worked". It does work, right
    /// up until the host is slow to poll: the dongle then buffers, packets
    /// coalesce into one transfer, and a share of every report vanishes. A game
    /// running is exactly that condition, and two controllers streaming at once
    /// fills the buffer twice as fast.
    ///
    /// ❗ Lost reports are not cosmetic here. Gaps in the motion stream become
    /// gaps in the device timestamp, which the decoder must then spread or
    /// reject, and they starve one half while the other keeps up — so a grip
    /// disagrees with itself.
    ///
    /// A truncated tail is dropped rather than carried forward, for the reason
    /// on [`Dongle::read_event_timeout`]: carrying it stalled the whole
    /// transport whenever a tail never completed.
    pub fn read_acl(&self, timeout: Duration) -> Result<Option<AclPacket>> {
        if let Some(p) = self.pending_acl.lock().unwrap().pop_front() {
            return Ok(Some(p));
        }
        let mut buf = [0u8; 1024];
        let n = match self.handle.read_bulk(self.acl_in_ep, &mut buf, timeout) {
            Ok(n) => n,
            Err(rusb::Error::Timeout) => return Ok(None),
            Err(e) => return Err(Error::Usb(e)),
        };
        let mut queue = self.pending_acl.lock().unwrap();
        queue.extend(acl::split_acl(&buf[..n]));
        Ok(queue.pop_front())
    }

    /// Send an ATT request and wait for the matching response on this link.
    ///
    /// Input-report notifications arrive continuously at the connection
    /// interval, so a naive "read one packet" would almost always return a
    /// report rather than the response. Everything that is not the awaited
    /// opcode — or an error response for it — is therefore skipped.
    fn att_request(
        &self,
        conn_handle: u16,
        request: &[u8],
        response_opcode: u8,
        timeout: Duration,
    ) -> Result<Option<Vec<u8>>> {
        self.send_att(conn_handle, request)?;
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            let Some(pkt) = self.read_acl(Duration::from_millis(20))? else {
                continue;
            };
            if pkt.conn_handle != conn_handle || pkt.cid != acl::CID_ATT {
                continue;
            }
            match pkt.payload.first() {
                Some(&op) if op == response_opcode => return Ok(Some(pkt.payload)),
                // An error response ends the exchange. Returning it rather than
                // swallowing it lets the caller tell "walk finished" apart from
                // "the controller refused", which look identical from a timeout.
                Some(&acl::ATT_ERROR_RESPONSE) if pkt.payload.get(1) == request.first() => {
                    return Ok(Some(pkt.payload))
                }
                _ => continue,
            }
        }
        Ok(None)
    }

    /// Walk the peer's whole attribute table: every handle and its type.
    ///
    /// Worth doing despite the captured handles in [`crate::joycon`]: a capture
    /// only shows the handles the capturing stack chose to touch. A
    /// characteristic Windows never subscribed to leaves no trace in one, so
    /// "not in the capture" and "not on the device" are indistinguishable
    /// without an actual walk.
    pub fn discover_attributes(&self, conn_handle: u16) -> Result<Vec<acl::AttrInfo>> {
        let mut out: Vec<acl::AttrInfo> = Vec::new();
        let mut start: u16 = 0x0001;
        // Bounded so a peer that keeps answering cannot spin forever; an
        // attribute table this stack cares about is a few dozen entries.
        for _ in 0..64 {
            let req = acl::find_information_request(start, 0xFFFF);
            let rsp = match self.att_request(
                conn_handle,
                &req,
                acl::ATT_FIND_INFORMATION_RESPONSE,
                Duration::from_secs(2),
            )? {
                Some(r) => r,
                // A timeout on the FIRST request is a failure worth naming; a
                // timeout later just ends a walk that already produced results.
                None if out.is_empty() => {
                    return Err(Error::Protocol(
                        "no reply to Find Information Request within 2 s".into(),
                    ))
                }
                None => break,
            };
            if acl::is_attribute_not_found(&rsp) {
                break;
            }
            if let Some(code) = acl::att_error_code(&rsp) {
                return Err(Error::Protocol(format!(
                    "Find Information refused: {code:#04x} {}",
                    acl::att_error_name(code)
                )));
            }
            let Some(entries) = acl::parse_find_information_response(&rsp) else {
                return Err(Error::Protocol(format!(
                    "undecodable Find Information Response: {:02x?}",
                    &rsp[..rsp.len().min(16)]
                )));
            };
            if entries.is_empty() {
                break;
            }
            let last = entries[entries.len() - 1].handle;
            out.extend(entries);
            if last == 0xFFFF {
                break;
            }
            start = last + 1;
        }
        Ok(out)
    }

    /// Walk every characteristic declaration, recovering properties and value
    /// handles — which is what says whether a characteristic can notify at all.
    pub fn discover_characteristics(&self, conn_handle: u16) -> Result<Vec<acl::CharDecl>> {
        let mut out: Vec<acl::CharDecl> = Vec::new();
        let mut start: u16 = 0x0001;
        for _ in 0..64 {
            let req = acl::read_by_type_request(start, 0xFFFF, acl::GATT_CHARACTERISTIC);
            let rsp = match self.att_request(
                conn_handle,
                &req,
                acl::ATT_READ_BY_TYPE_RESPONSE,
                Duration::from_secs(2),
            )? {
                Some(r) => r,
                None if out.is_empty() => {
                    return Err(Error::Protocol(
                        "no reply to Read By Type Request within 2 s".into(),
                    ))
                }
                None => break,
            };
            if acl::is_attribute_not_found(&rsp) {
                break;
            }
            if let Some(code) = acl::att_error_code(&rsp) {
                return Err(Error::Protocol(format!(
                    "Read By Type refused: {code:#04x} {}",
                    acl::att_error_name(code)
                )));
            }
            let Some(pairs) = acl::parse_read_by_type_response(&rsp) else {
                return Err(Error::Protocol(format!(
                    "undecodable Read By Type Response: {:02x?}",
                    &rsp[..rsp.len().min(16)]
                )));
            };
            if pairs.is_empty() {
                break;
            }
            let last = pairs[pairs.len() - 1].0;
            for (h, v) in pairs {
                if let Some(c) = acl::parse_characteristic(h, &v) {
                    out.push(c);
                }
            }
            if last == 0xFFFF {
                break;
            }
            start = last + 1;
        }
        Ok(out)
    }

    /// The dongle's own BD_ADDR, in natural (display) order.
    ///
    /// The wire carries it least-significant byte first, like every multi-byte
    /// HCI field, so it is reversed here to match how addresses are written
    /// down and how [`Dongle::le_connect`] takes them.
    /// Local BD_ADDR, from the cache filled during `reset_and_init`.
    ///
    /// Falls back to asking the controller, which usually fails once links are
    /// live — see [`Dongle::bd_addr`].
    pub fn read_bd_addr(&self) -> Result<[u8; 6]> {
        if let Some(addr) = self.bd_addr.get() {
            return Ok(*addr);
        }
        self.read_bd_addr_uncached()
    }

    fn read_bd_addr_uncached(&self) -> Result<[u8; 6]> {
        let cc = self.command_sync(hci::Opcode::READ_BD_ADDR, &[])?;
        // params = [status][BD_ADDR 6, little-endian]
        if cc.params.len() < 7 || cc.params[0] != 0 {
            return Err(Error::Protocol(format!(
                "HCI_Read_BD_ADDR failed: {:02x?}",
                cc.params
            )));
        }
        let mut addr = [0u8; 6];
        addr.copy_from_slice(&cc.params[1..7]);
        addr.reverse();
        Ok(addr)
    }

    /// Read one attribute's value.
    pub fn read_attribute(&self, conn_handle: u16, handle: u16) -> Result<Option<Vec<u8>>> {
        Ok(self.read_attribute_detail(conn_handle, handle)?.ok().flatten())
    }

    /// [`Dongle::read_attribute`], keeping the ATT error code when refused.
    ///
    /// ❗ **"Refused" and "silent" are completely different facts and were being
    /// collapsed into one.** `att_request` returns an Error Response as a
    /// payload; `parse_read_response` then sees a non-`0x0B` opcode, returns
    /// `None`, and the caller printed "no reply".
    ///
    /// That mattered: a read of `0x000e` — a characteristic streaming
    /// notifications at 67 Hz, unambiguously alive — reported "no reply", and
    /// was nearly recorded as evidence that the neighbouring `0x000a` was a
    /// dead buffer. The error code is the whole content of the answer.
    ///
    /// `Ok(Err(code))` = refused with that ATT error. `Ok(Ok(None))` = genuine
    /// silence, no response at all.
    pub fn read_attribute_detail(
        &self,
        conn_handle: u16,
        handle: u16,
    ) -> Result<std::result::Result<Option<Vec<u8>>, u8>> {
        let rsp = self.att_request(
            conn_handle,
            &acl::read_request(handle),
            acl::ATT_READ_RESPONSE,
            Duration::from_millis(800),
        )?;
        let Some(payload) = rsp else { return Ok(Ok(None)) };
        if let Some(code) = acl::att_error_code(&payload) {
            return Ok(Err(code));
        }
        Ok(Ok(acl::parse_read_response(&payload)))
    }

    /// Recover characteristic properties by reading each declaration directly.
    ///
    /// The fallback for a peer that answers [`Dongle::discover_attributes`] but
    /// never answers `Read By Type` — which is exactly what the Joy-Con 2 does,
    /// and it is not a transient failure: both halves, every run, no reply
    /// within two seconds while Find Information returns 48 entries happily.
    ///
    /// Without this, properties are simply unknown, and "unknown" has meant
    /// guessing which write opcode a characteristic accepts. That guess has now
    /// been wrong in both directions.
    ///
    /// `attrs` is the table from [`Dongle::discover_attributes`]; every handle
    /// in it typed `0x2803` is a characteristic declaration whose *value* is
    /// the properties, value handle and UUID.
    pub fn read_characteristics(
        &self,
        conn_handle: u16,
        attrs: &[acl::AttrInfo],
    ) -> Vec<acl::CharDecl> {
        let mut out = Vec::new();
        for a in attrs
            .iter()
            .filter(|a| a.uuid == acl::AttUuid::Short(acl::GATT_CHARACTERISTIC))
        {
            // A single unreadable declaration is skipped rather than aborting
            // the walk: the rest of the table is still worth having, and a
            // partial map beats none.
            if let Ok(Some(v)) = self.read_attribute(conn_handle, a.handle) {
                if let Some(c) = acl::parse_characteristic(a.handle, &v) {
                    out.push(c);
                }
            }
        }
        out
    }

    /// Write to an attribute using whichever opcode it actually accepts.
    ///
    /// `opcode` comes from [`acl::CharDecl::write_opcode`]. For an acknowledged
    /// write this waits for the Write Response and reports an ATT error by
    /// name; for an unacknowledged one there is nothing to wait for, so it
    /// returns as soon as the packet is out.
    pub fn write_attribute(
        &self,
        conn_handle: u16,
        handle: u16,
        value: &[u8],
        opcode: u8,
    ) -> Result<()> {
        if opcode == acl::ATT_WRITE_COMMAND {
            return self.send_att(conn_handle, &acl::write_command(handle, value));
        }
        let rsp = self.att_request(
            conn_handle,
            &acl::write_request(handle, value),
            acl::ATT_WRITE_RESPONSE,
            Duration::from_millis(800),
        )?;
        match rsp {
            Some(r) if r.first() == Some(&acl::ATT_WRITE_RESPONSE) => Ok(()),
            Some(r) => {
                let code = acl::att_error_code(&r).unwrap_or(0);
                Err(Error::Protocol(format!(
                    "write to {handle:#06x} refused: {code:#04x} {}",
                    acl::att_error_name(code)
                )))
            }
            None => Err(Error::Protocol(format!(
                "no Write Response from {handle:#06x} within 800 ms"
            ))),
        }
    }

    /// Start link encryption from an out-of-band LTK, bypassing SMP entirely.
    ///
    /// This is the capability the whole crate exists for. Windows will only
    /// encrypt as the outcome of its own SMP pairing, which a Joy-Con 2 fails
    /// with `Confirm Value Failed`; here the key from the controller's
    /// pseudo-OOB GATT exchange is handed straight to the controller.
    ///
    /// `rand` and `ediv` are zero for a key that did not come from legacy SMP.
    pub fn le_enable_encryption(
        &self,
        conn_handle: u16,
        rand: u64,
        ediv: u16,
        ltk: &[u8; 16],
    ) -> Result<()> {
        let mut p = Vec::with_capacity(28);
        p.extend_from_slice(&conn_handle.to_le_bytes());
        p.extend_from_slice(&rand.to_le_bytes());
        p.extend_from_slice(&ediv.to_le_bytes());
        // The LTK goes out least-significant byte first, like every other
        // multi-byte HCI field.
        let mut key = *ltk;
        key.reverse();
        p.extend_from_slice(&key);
        self.send_command(hci::Opcode::LE_ENABLE_ENCRYPTION, &p)?;
        Ok(())
    }
}

impl Drop for Dongle {
    fn drop(&mut self) {
        // Deregister BEFORE releasing: a UI frame that lands in between would
        // otherwise see a dongle that is neither ours nor openable, and report
        // it as taken by something else.
        if let Ok(mut open) = open_dongles().lock() {
            if let Some(i) = open.iter().position(|d| *d == (self.vid, self.pid)) {
                open.swap_remove(i);
            }
        }
        let _ = self.handle.release_interface(self.interface);
    }
}

struct Endpoints {
    event: u8,
    acl_in: u8,
    acl_out: u8,
}

/// Locate the endpoints the Bluetooth USB transport defines.
///
/// Read from the descriptors rather than assuming the conventional `0x81` /
/// `0x82` / `0x02`, because dongles do vary and a wrong address fails as a
/// silent timeout — indistinguishable from a dongle that is not answering.
fn find_endpoints(device: rusb::Device<rusb::GlobalContext>, interface: u8) -> Result<Endpoints> {
    let config = device.active_config_descriptor()?;
    let (mut event, mut acl_in, mut acl_out) = (None, None, None);
    for iface in config.interfaces() {
        if iface.number() != interface {
            continue;
        }
        for desc in iface.descriptors() {
            for ep in desc.endpoint_descriptors() {
                match (ep.transfer_type(), ep.direction()) {
                    (rusb::TransferType::Interrupt, rusb::Direction::In) => {
                        event.get_or_insert(ep.address());
                    }
                    (rusb::TransferType::Bulk, rusb::Direction::In) => {
                        acl_in.get_or_insert(ep.address());
                    }
                    (rusb::TransferType::Bulk, rusb::Direction::Out) => {
                        acl_out.get_or_insert(ep.address());
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(Endpoints {
        event: event.ok_or(Error::NoEndpoint("interrupt IN"))?,
        acl_in: acl_in.ok_or(Error::NoEndpoint("bulk IN"))?,
        acl_out: acl_out.ok_or(Error::NoEndpoint("bulk OUT"))?,
    })
}
