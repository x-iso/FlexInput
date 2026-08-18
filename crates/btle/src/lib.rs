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
    pub fn read_bd_addr(&self) -> Result<[u8; 6]> {
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
