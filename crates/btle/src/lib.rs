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

use std::time::Duration;

pub mod acl;
pub mod hci;
pub mod joycon;

pub use acl::{AclPacket, Notification};
pub use hci::{CommandComplete, Event, Opcode};

/// Errors from the dongle transport.
#[derive(Debug)]
pub enum Error {
    /// No USB device with the requested VID/PID, or it is not WinUSB-bound.
    NotFound { vid: u16, pid: u16 },
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

/// An open Bluetooth dongle, ready to exchange HCI traffic.
pub struct Dongle {
    handle: rusb::DeviceHandle<rusb::GlobalContext>,
    event_ep: u8,
    acl_in_ep: u8,
    acl_out_ep: u8,
    interface: u8,
    timeout: Duration,
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
        let mut handle = rusb::open_device_with_vid_pid(vid, pid)
            .ok_or(Error::NotFound { vid, pid })?;

        // Windows has no kernel driver to detach once WinUSB is bound, and this
        // is unsupported there — hence best-effort. It matters on Linux, where
        // btusb would otherwise still own the device.
        #[cfg(not(windows))]
        let _ = handle.set_auto_detach_kernel_driver(true);

        let interface = 0u8;
        handle.claim_interface(interface)?;

        let eps = find_endpoints(handle.device(), interface)?;

        Ok(Self {
            handle,
            event_ep: eps.event,
            acl_in_ep: eps.acl_in,
            acl_out_ep: eps.acl_out,
            interface,
            timeout: Duration::from_secs(2),
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
    pub fn read_event_timeout(&self, timeout: Duration) -> Result<Option<Event>> {
        let mut buf = [0u8; 260];
        match self.handle.read_interrupt(self.event_ep, &mut buf, timeout) {
            Ok(n) => hci::parse_event(&buf[..n]).map(Some),
            Err(rusb::Error::Timeout) => Ok(None),
            Err(e) => Err(Error::Usb(e)),
        }
    }

    /// Send a command and wait for the `Command Complete` that matches it.
    ///
    /// Unrelated events are skipped rather than treated as failures — a dongle
    /// emits plenty unprompted, and the earlier Joy-Con work was repeatedly
    /// misled by validators that locked onto the first thing they saw.
    pub fn command_sync(&self, opcode: Opcode, params: &[u8]) -> Result<CommandComplete> {
        self.send_command(opcode, params)?;
        for _ in 0..16 {
            match self.read_event()? {
                Some(Event::CommandComplete(cc)) if cc.opcode == opcode => return Ok(cc),
                Some(_) => continue,
                None => break,
            }
        }
        Err(Error::Protocol(format!(
            "no Command Complete for {opcode:?} within timeout"
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
        let cc = self.command_sync(hci::Opcode::RESET, &[])?;
        if !cc.succeeded() {
            return Err(Error::Protocol(format!("HCI_Reset status {:?}", cc.status())));
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
    /// Active (not passive) because Joy-Con 2 controllers are only identifiable
    /// by the manufacturer data in their advertisements — they publish no
    /// service UUIDs and no name — and a scan request is what reliably pulls
    /// the full payload.
    ///
    /// Interval and window are in 0.625 ms units. The defaults here scan ~30 ms
    /// out of every ~60 ms: aggressive enough to find a controller quickly
    /// without monopolising a radio that will soon also be holding a link.
    pub fn start_le_scan(&self) -> Result<()> {
        // Clear any half-finished initiator first, or scan-enable is refused.
        self.cancel_pending_connect();
        let interval: u16 = 0x0060; // 60 × 0.625 ms = 60 ms
        let window: u16 = 0x0030; //   48 × 0.625 ms = 30 ms
        let mut params = Vec::with_capacity(7);
        params.push(0x01); // active scanning
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
                    return Ok(conn_handle);
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

    /// Read one inbound ACL packet, or `Ok(None)` on timeout.
    pub fn read_acl(&self, timeout: Duration) -> Result<Option<AclPacket>> {
        let mut buf = [0u8; 1024];
        match self.handle.read_bulk(self.acl_in_ep, &mut buf, timeout) {
            Ok(n) => Ok(acl::parse_acl(&buf[..n])),
            Err(rusb::Error::Timeout) => Ok(None),
            Err(e) => Err(Error::Usb(e)),
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
