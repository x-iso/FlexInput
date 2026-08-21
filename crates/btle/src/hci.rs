//! HCI packet encoding and decoding.
//!
//! Pure functions over byte slices, so the wire format is unit-testable without
//! a dongle plugged in. Every layout here is from the Bluetooth Core spec, Vol 4
//! Part E.

/// An HCI opcode: a 6-bit Opcode Group Field and a 10-bit Opcode Command Field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opcode(pub u16);

impl Opcode {
    /// Build an opcode from its group and command fields.
    pub const fn new(ogf: u16, ocf: u16) -> Self {
        Opcode((ogf << 10) | (ocf & 0x03FF))
    }

    /// `HCI_Reset` — OGF 0x03 (Controller & Baseband), OCF 0x0003.
    ///
    /// The natural first command: every controller implements it, it takes no
    /// parameters, and it answers with a single status byte. If this one
    /// round-trips, the USB transport is proven end to end.
    pub const RESET: Opcode = Opcode::new(0x03, 0x0003);
    /// `HCI_Set_Event_Mask` — OGF 0x03, OCF 0x0001.
    ///
    /// Must be sent after a reset. The spec's default event mask is
    /// `0x00001FFFFFFFFFFF`, in which **bit 61 (LE Meta) is CLEAR** — so a
    /// freshly reset controller scans happily and reports nothing at all. The
    /// symptom is a scan that finds zero devices, which looks like broken
    /// hardware rather than a masked event.
    pub const SET_EVENT_MASK: Opcode = Opcode::new(0x03, 0x0001);
    /// `HCI_LE_Set_Event_Mask` — OGF 0x08, OCF 0x0001.
    pub const LE_SET_EVENT_MASK: Opcode = Opcode::new(0x08, 0x0001);
    /// `HCI_Read_Local_Version_Information` — OGF 0x04, OCF 0x0001.
    pub const READ_LOCAL_VERSION: Opcode = Opcode::new(0x04, 0x0001);
    /// `HCI_LE_Set_Scan_Parameters` — OGF 0x08 (LE Controller), OCF 0x000B.
    pub const LE_SET_SCAN_PARAMETERS: Opcode = Opcode::new(0x08, 0x000B);
    /// `HCI_LE_Set_Scan_Enable` — OGF 0x08, OCF 0x000C.
    pub const LE_SET_SCAN_ENABLE: Opcode = Opcode::new(0x08, 0x000C);
    /// `HCI_LE_Create_Connection` — OGF 0x08, OCF 0x000D.
    ///
    /// Answers with `Command Status`, not `Command Complete`: the result
    /// arrives later as an `LE Connection Complete` sub-event.
    pub const LE_CREATE_CONNECTION: Opcode = Opcode::new(0x08, 0x000D);
    /// `HCI_LE_Create_Connection_Cancel` — OGF 0x08, OCF 0x000E.
    pub const LE_CREATE_CONNECTION_CANCEL: Opcode = Opcode::new(0x08, 0x000E);
    /// `HCI_Disconnect` — OGF 0x01 (Link Control), OCF 0x0006.
    pub const DISCONNECT: Opcode = Opcode::new(0x01, 0x0006);
    /// `HCI_Read_BD_ADDR` — OGF 0x04 (Informational Parameters), OCF 0x0009.
    ///
    /// The dongle's own address. Needed because the Joy-Con 2 pairing handshake
    /// tells the controller which host to bond to, and over a dedicated dongle
    /// that is the DONGLE's address, not the machine's onboard radio — reading
    /// the wrong one bonds the controller to a radio that will never talk to it.
    pub const READ_BD_ADDR: Opcode = Opcode::new(0x04, 0x0009);

    // ── BR/EDR (Bluetooth Classic) ────────────────────────────────────────
    //
    // ⭐ A SECOND radio protocol, not an extension of the LE one. Everything
    // above talks to devices that advertise and are connected by address; a
    // classic device is found by INQUIRY, connected by PAGING, and then speaks
    // L2CAP channels negotiated over SDP rather than a fixed ATT channel.
    // Sharing a transport and an event loop is most of what the two have in
    // common.
    //
    /// Local controller features — bit 5 of byte 4 is `BR/EDR Not Supported`.
    /// This is the first thing to ask: an LE-only dongle answers every classic
    /// command with "unknown command", which is indistinguishable from a bug
    /// in the command encoding unless the capability was checked first.
    pub const READ_LOCAL_FEATURES: Opcode = Opcode::new(0x04, 0x0003);
    /// Begin an inquiry. Parameters are a 3-byte LAP, a duration in 1.28 s
    /// units, and a response limit (0 = unlimited).
    pub const INQUIRY: Opcode = Opcode::new(0x01, 0x0001);
    /// Stop an inquiry early.
    pub const INQUIRY_CANCEL: Opcode = Opcode::new(0x01, 0x0002);
    /// Ask a discovered device for its friendly name, by address.
    pub const REMOTE_NAME_REQUEST: Opcode = Opcode::new(0x01, 0x0019);
    /// Choose what an inquiry result carries: `0x00` the original form,
    /// `0x01` adds RSSI, `0x02` adds RSSI and the extended (EIR) payload.
    ///
    /// ❗ Controllers default to `0x00`, which has no RSSI field at all — so a
    /// probe that never sets this reports "n/a" for every device and looks like
    /// a decoding failure rather than a mode that was never asked for.
    pub const WRITE_INQUIRY_MODE: Opcode = Opcode::new(0x03, 0x0045);

    // ── Paging and Secure Simple Pairing ──────────────────────────────────
    //
    // ⭐ These arrive as a GROUP because a classic connection cannot be
    // established without most of them. Paging a modern controller opens an
    // ACL link and the remote immediately demands authentication; a host that
    // pages and then says nothing is disconnected within a couple of seconds.
    // "Connect" and "pair" are one operation here in a way they are not on LE.
    //
    /// Page a device: open an ACL link to an address found by inquiry.
    pub const CREATE_CONNECTION: Opcode = Opcode::new(0x01, 0x0005);
    pub const CREATE_CONNECTION_CANCEL: Opcode = Opcode::new(0x01, 0x0008);
    /// Turn on Secure Simple Pairing. Required before any SSP event will fire;
    /// without it the controller falls back to legacy PIN pairing, which modern
    /// gamepads refuse.
    pub const WRITE_SIMPLE_PAIRING_MODE: Opcode = Opcode::new(0x03, 0x0056);
    /// Answer `IO Capability Request` — what this host can display and input.
    pub const IO_CAPABILITY_REQUEST_REPLY: Opcode = Opcode::new(0x01, 0x002B);
    /// Accept the numeric comparison. With NoInputNoOutput on both sides this
    /// is "Just Works" and there is nothing for a person to compare, but the
    /// controller still expects the reply.
    pub const USER_CONFIRMATION_REQUEST_REPLY: Opcode = Opcode::new(0x01, 0x002C);
    /// Hand back a stored link key so a known device reconnects without
    /// pairing again.
    pub const LINK_KEY_REQUEST_REPLY: Opcode = Opcode::new(0x01, 0x000B);
    /// "I have no key for this device" — which is what starts SSP.
    pub const LINK_KEY_REQUEST_NEGATIVE_REPLY: Opcode = Opcode::new(0x01, 0x000C);
    /// Ask the remote to authenticate, which is what kicks off pairing when the
    /// controller does not demand it first.
    pub const AUTHENTICATION_REQUESTED: Opcode = Opcode::new(0x01, 0x0011);
    /// Turn on encryption once authenticated. HID interrupt traffic needs it.
    pub const SET_CONNECTION_ENCRYPTION: Opcode = Opcode::new(0x01, 0x0013);
    /// ⭐ Whether this radio ANSWERS when someone calls it: bit 0 inquiry scan,
    /// bit 1 page scan.
    ///
    /// ❗ Both are OFF after `HCI_Reset`, and that is the whole reason a bonded
    /// controller could not reconnect. A classic pairing is symmetric — a
    /// controller that is switched on PAGES its host rather than waiting to be
    /// found — so a host that only ever pages outward and never listens is deaf
    /// to exactly the device it is trying to reach. Both sides calling, neither
    /// picking up.
    pub const READ_SCAN_ENABLE: Opcode = Opcode::new(0x03, 0x0019);
    pub const WRITE_SCAN_ENABLE: Opcode = Opcode::new(0x03, 0x001A);
    /// How long a page attempt keeps trying, in 0.625 ms slots.
    ///
    /// ❗ Worth setting rather than inheriting. The default varies by
    /// controller, and a short one turns "the controller was mid-something"
    /// into a hard `Page Timeout (0x04)` that reads like the device is not
    /// there at all.
    pub const WRITE_PAGE_TIMEOUT: Opcode = Opcode::new(0x03, 0x0018);
    /// Accept an incoming connection. `role`: `0x00` become master, `0x01` stay
    /// slave.
    pub const ACCEPT_CONNECTION_REQUEST: Opcode = Opcode::new(0x01, 0x0009);
    /// `HCI_LE_Enable_Encryption` — OGF 0x08, OCF 0x0019.
    ///
    /// The reason this whole crate exists: it starts link encryption from an
    /// out-of-band LTK with no SMP involved, which is exactly what the console
    /// does after the Joy-Con's pseudo-OOB GATT exchange, and exactly what
    /// WinRT offers no way to do.
    pub const LE_ENABLE_ENCRYPTION: Opcode = Opcode::new(0x08, 0x0019);
}

/// Encode an HCI command packet: opcode (little-endian), length, parameters.
///
/// No leading packet-type byte: that belongs to the serial (H4) transport. Over
/// USB the endpoint identifies the type, and a stray `0x01` here shifts every
/// field by one — which the controller answers with silence rather than an
/// error, making it a genuinely nasty mistake to debug.
pub fn encode_command(opcode: Opcode, params: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(3 + params.len());
    out.extend_from_slice(&opcode.0.to_le_bytes());
    out.push(params.len() as u8);
    out.extend_from_slice(params);
    out
}

/// A `Command Complete` event (event code `0x0E`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandComplete {
    /// Command slots the controller can accept now; used for flow control.
    pub num_hci_command_packets: u8,
    /// The command this completes.
    pub opcode: Opcode,
    /// Return parameters. Byte 0 is the status for most commands: 0 = success.
    pub params: Vec<u8>,
}

impl CommandComplete {
    /// Status byte, when the command has one.
    pub fn status(&self) -> Option<u8> {
        self.params.first().copied()
    }

    /// Whether the command succeeded (status 0x00).
    pub fn succeeded(&self) -> bool {
        self.status() == Some(0x00)
    }
}

/// A decoded HCI event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    CommandComplete(CommandComplete),
    /// `Command Status` (`0x0F`) — a command accepted but not yet finished.
    CommandStatus { status: u8, opcode: Opcode },
    /// One device seen during an LE scan.
    LeAdvertisingReport(AdvReport),
    /// The result of `LE Create Connection`.
    LeConnectionComplete {
        status: u8,
        conn_handle: u16,
        /// Connection interval in 1.25 ms units.
        interval: u16,
        /// Supervision timeout in 10 ms units.
        supervision_timeout: u16,
    },
    /// The link went away. `reason` uses the standard HCI error codes — `0x13`
    /// remote terminated, `0x16` local host, `0x08` supervision timeout.
    DisconnectionComplete { conn_handle: u16, reason: u8 },
    /// Link encryption turned on or off.
    EncryptionChange { status: u8, conn_handle: u16, enabled: u8 },
    /// Devices seen during a BR/EDR inquiry. A single event carries a count
    /// and that many devices, so this is a list rather than one result.
    InquiryResults(Vec<InquiryResult>),
    /// The inquiry ran to completion (or was cancelled).
    InquiryComplete { status: u8 },
    /// A remote device answered with its friendly name.
    RemoteNameComplete { status: u8, address: [u8; 6], name: String },
    /// A BR/EDR ACL link was established (or failed to be).
    ConnectionComplete { status: u8, conn_handle: u16, address: [u8; 6] },
    /// A remote device is calling US — the reconnect path for an already-bonded
    /// controller.
    ConnectionRequest { address: [u8; 6], class_of_device: [u8; 3], link_type: u8 },
    /// The controller wants a stored link key for this device.
    LinkKeyRequest { address: [u8; 6] },
    /// Pairing produced a link key — this is the thing worth persisting.
    LinkKeyNotification { address: [u8; 6], key: [u8; 16], key_type: u8 },
    /// The remote is asking what this host can display and input.
    IoCapabilityRequest { address: [u8; 6] },
    /// "Do these numbers match?" — under Just Works there is nothing to show,
    /// but the reply is still required.
    UserConfirmationRequest { address: [u8; 6], numeric: u32 },
    /// Secure Simple Pairing finished.
    SimplePairingComplete { status: u8, address: [u8; 6] },
    /// Authentication finished on an established link.
    AuthenticationComplete { status: u8, conn_handle: u16 },
    /// Anything not decoded yet, kept whole so nothing is silently discarded.
    Other { code: u8, params: Vec<u8> },
}

/// One device found by a BR/EDR inquiry.
///
/// The three inquiry-result events (`0x02` plain, `0x22` with RSSI, `0x2F`
/// extended) differ only in what they append, so they decode into one type —
/// the fields a later one adds are `None` when the controller sent the shorter
/// form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InquiryResult {
    pub address: [u8; 6],
    /// Class of Device: 3 bytes saying broadly what the device IS. The low
    /// bits of byte 0 plus byte 1 give the major/minor class — a gamepad
    /// reports major class Peripheral (0x05) with the gamepad minor bits set,
    /// which is how a controller is told from a headset without pairing it.
    pub class_of_device: [u8; 3],
    /// Page scan repetition mode, needed later to page (connect to) it.
    pub page_scan_repetition_mode: u8,
    /// Clock offset, also needed to page it.
    pub clock_offset: u16,
    pub rssi: Option<i8>,
}

impl InquiryResult {
    /// Major device class, from the Class of Device bits.
    pub fn major_class(&self) -> u8 {
        (self.class_of_device[1] >> 0) & 0x1F
    }

    /// Whether the Class of Device marks this as a peripheral with the
    /// gamepad/joystick bits set.
    ///
    /// Advisory only — it is a hint the device broadcasts about itself, not a
    /// guarantee, and some controllers report themselves oddly. Good enough to
    /// sort a device list; not good enough to gate a connection on.
    pub fn looks_like_a_gamepad(&self) -> bool {
        // Major class 0x05 = Peripheral. Minor bits 0x01 = joystick,
        // 0x02 = gamepad, 0x03 = remote control.
        self.major_class() == 0x05 && matches!((self.class_of_device[0] >> 2) & 0x0F, 1 | 2)
    }
}

/// A single advertising report from an `LE Advertising Report` sub-event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvReport {
    /// `0x00` connectable undirected, `0x04` scan response, etc.
    pub event_type: u8,
    /// `0x00` public, `0x01` random.
    pub address_type: u8,
    /// BD_ADDR in natural (display) order — the wire carries it reversed.
    pub address: [u8; 6],
    /// Raw AD structures, still in `[len][type][data…]` form.
    pub data: Vec<u8>,
    pub rssi: i8,
}

impl AdvReport {
    /// Extract the manufacturer-specific data (AD type `0xFF`), if present.
    ///
    /// This is the only way to recognise a Joy-Con 2: it advertises no service
    /// UUIDs and no name, just Nintendo's company id and a product id. Returned
    /// **including** the leading 2-byte company id, unlike btleplug, which
    /// strips it into a map key — a difference that silently shifts every
    /// offset by two if assumed the other way.
    pub fn manufacturer_data(&self) -> Option<&[u8]> {
        let mut i = 0usize;
        while i < self.data.len() {
            let len = self.data[i] as usize;
            if len == 0 || i + 1 + len > self.data.len() {
                break;
            }
            let ad_type = self.data[i + 1];
            if ad_type == 0xFF {
                return Some(&self.data[i + 2..i + 1 + len]);
            }
            i += 1 + len;
        }
        None
    }
}

pub const EVT_COMMAND_COMPLETE: u8 = 0x0E;
pub const EVT_COMMAND_STATUS: u8 = 0x0F;
pub const EVT_DISCONNECTION_COMPLETE: u8 = 0x05;
pub const EVT_ENCRYPTION_CHANGE: u8 = 0x08;
pub const EVT_LE_META: u8 = 0x3E;
pub const SUBEVT_LE_CONNECTION_COMPLETE: u8 = 0x01;
pub const SUBEVT_LE_ADVERTISING_REPORT: u8 = 0x02;

/// Decode one HCI event packet: `[code][param_len][params…]`.
/// `Inquiry Complete`.
const EVT_INQUIRY_COMPLETE: u8 = 0x01;
/// `Remote Name Request Complete`.
const EVT_REMOTE_NAME_COMPLETE: u8 = 0x07;
/// `Connection Complete`.
const EVT_CONNECTION_COMPLETE: u8 = 0x03;
/// `Connection Request` — an incoming link.
const EVT_CONNECTION_REQUEST: u8 = 0x04;
/// `Authentication Complete`.
const EVT_AUTHENTICATION_COMPLETE: u8 = 0x06;
/// `Link Key Request`.
const EVT_LINK_KEY_REQUEST: u8 = 0x17;
/// `Link Key Notification`.
const EVT_LINK_KEY_NOTIFICATION: u8 = 0x18;
/// `IO Capability Request`.
const EVT_IO_CAPABILITY_REQUEST: u8 = 0x31;
/// `User Confirmation Request`.
const EVT_USER_CONFIRMATION_REQUEST: u8 = 0x33;
/// `Simple Pairing Complete`.
const EVT_SIMPLE_PAIRING_COMPLETE: u8 = 0x36;

/// Read a little-endian BD_ADDR out of an event parameter block.
fn addr_at(params: &[u8], off: usize) -> [u8; 6] {
    let mut a = [0u8; 6];
    a.copy_from_slice(&params[off..off + 6]);
    a
}
/// `Inquiry Result` — the original form, no RSSI.
const EVT_INQUIRY_RESULT: u8 = 0x02;
/// `Inquiry Result with RSSI`.
const EVT_INQUIRY_RESULT_RSSI: u8 = 0x22;
/// `Extended Inquiry Result` — one device per event, with EIR data appended.
const EVT_EXTENDED_INQUIRY_RESULT: u8 = 0x2F;

/// Decode the repeated body of an inquiry-result event.
///
/// ❗ The three forms differ in stride and in whether RSSI is present, and — for
/// the two older ones — carry SEVERAL devices in one event, with the fields
/// stored COLUMN-WISE: every address, then every repetition mode, then every
/// class, and so on. Reading them as a row of structs per device is the obvious
/// mistake and yields addresses spliced together from different devices.
fn parse_inquiry_results(code: u8, params: &[u8]) -> Vec<InquiryResult> {
    let mut out = Vec::new();
    if params.is_empty() {
        return out;
    }
    let n = params[0] as usize;
    let p = &params[1..];
    // (stride per device, whether RSSI is present). The extended form is always
    // a single device and its own layout.
    let (with_rssi, per) = match code {
        EVT_INQUIRY_RESULT => (false, 14),
        EVT_INQUIRY_RESULT_RSSI => (true, 14),
        _ => (true, 14),
    };
    if n == 0 || p.len() < n * per {
        return out;
    }
    // Column offsets, in units of "n devices".
    let addr = 0;
    let psrm = addr + n * 6;
    let reserved = psrm + n;
    // The plain form has two reserved bytes per device, the RSSI form one.
    let cls = reserved + if code == EVT_INQUIRY_RESULT { n * 2 } else { n };
    let clk = cls + n * 3;
    let rssi = clk + n * 2;
    for i in 0..n {
        if clk + i * 2 + 2 > p.len() {
            break;
        }
        let mut a = [0u8; 6];
        a.copy_from_slice(&p[addr + i * 6..addr + i * 6 + 6]);
        out.push(InquiryResult {
            address: a,
            page_scan_repetition_mode: p[psrm + i],
            class_of_device: [p[cls + i * 3], p[cls + i * 3 + 1], p[cls + i * 3 + 2]],
            clock_offset: u16::from_le_bytes([p[clk + i * 2], p[clk + i * 2 + 1]]),
            rssi: if with_rssi && rssi + i < p.len() {
                Some(p[rssi + i] as i8)
            } else {
                None
            },
        });
    }
    out
}

pub fn parse_event(buf: &[u8]) -> crate::Result<Event> {
    if buf.len() < 2 {
        return Err(crate::Error::Protocol(format!(
            "event packet too short: {} bytes",
            buf.len()
        )));
    }
    let code = buf[0];
    let len = buf[1] as usize;
    // Trust the length field over the transfer size: a short read means a
    // truncated packet, and decoding it would invent values.
    //
    // ❗ But only for events we actually DECODE, and this is a BACKSTOP rather
    // than the fix. The real cause of
    //
    //     event 0xfc claims 19 params, got 9
    //     event 0x3e claims 41 params, got 36
    //
    // was the transport handing over one USB transfer at a time for events that
    // span several; `Dongle::read_event_timeout` now reassembles them. The
    // first of those was wrongly put down to Realtek "emitting short vendor
    // events", which explained one symptom and predicted nothing — the second
    // arrived on `0x3E`, an event this stack very much does decode, and would
    // have aborted init regardless.
    //
    // What survives here is narrow: if reassembly still comes up short, an
    // event nobody decodes should not take the link down with it. Truncation
    // stays fatal for the five codes below, where inventing a connection handle
    // or a disconnect reason from bytes that never arrived is far worse than an
    // error.
    const DECODED: [u8; 5] = [
        EVT_COMMAND_COMPLETE,
        EVT_COMMAND_STATUS,
        EVT_DISCONNECTION_COMPLETE,
        EVT_ENCRYPTION_CHANGE,
        EVT_LE_META,
    ];
    if buf.len() < 2 + len {
        if DECODED.contains(&code) {
            return Err(crate::Error::Protocol(format!(
                "event {code:#04x} claims {len} params, got {}",
                buf.len() - 2
            )));
        }
        // Undecoded and short: hand back what arrived rather than failing.
        return Ok(Event::Other {
            code,
            params: buf[2..].to_vec(),
        });
    }
    let params = &buf[2..2 + len];

    match code {
        EVT_COMMAND_COMPLETE if params.len() >= 3 => Ok(Event::CommandComplete(CommandComplete {
            num_hci_command_packets: params[0],
            opcode: Opcode(u16::from_le_bytes([params[1], params[2]])),
            params: params[3..].to_vec(),
        })),
        EVT_COMMAND_STATUS if params.len() >= 4 => Ok(Event::CommandStatus {
            status: params[0],
            opcode: Opcode(u16::from_le_bytes([params[2], params[3]])),
        }),
        EVT_INQUIRY_COMPLETE if !params.is_empty() => {
            Ok(Event::InquiryComplete { status: params[0] })
        }
        EVT_REMOTE_NAME_COMPLETE if params.len() >= 7 => {
            let mut address = [0u8; 6];
            address.copy_from_slice(&params[1..7]);
            // The name field is a fixed 248 bytes, NUL-padded — not a
            // length-prefixed string. Trimming at the first NUL is the whole
            // decode; taking all 248 gives a name with 200 nulls glued to it.
            let raw = &params[7..];
            let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
            Ok(Event::RemoteNameComplete {
                status: params[0],
                address,
                name: String::from_utf8_lossy(&raw[..end]).into_owned(),
            })
        }
        EVT_INQUIRY_RESULT | EVT_INQUIRY_RESULT_RSSI | EVT_EXTENDED_INQUIRY_RESULT => {
            // ❗ ALL of them. One inquiry event carries a COUNT and that many
            // devices; returning the first and discarding the rest is the
            // classic-radio version of the multi-packet bug already fixed twice
            // on this transport, and it would silently hide whichever
            // controller happened not to be first.
            Ok(Event::InquiryResults(parse_inquiry_results(code, params)))
        }
        EVT_CONNECTION_COMPLETE if params.len() >= 9 => Ok(Event::ConnectionComplete {
            status: params[0],
            conn_handle: u16::from_le_bytes([params[1], params[2]]) & 0x0FFF,
            address: addr_at(params, 3),
        }),
        EVT_CONNECTION_REQUEST if params.len() >= 10 => Ok(Event::ConnectionRequest {
            address: addr_at(params, 0),
            class_of_device: [params[6], params[7], params[8]],
            link_type: params[9],
        }),
        EVT_AUTHENTICATION_COMPLETE if params.len() >= 3 => Ok(Event::AuthenticationComplete {
            status: params[0],
            conn_handle: u16::from_le_bytes([params[1], params[2]]) & 0x0FFF,
        }),
        EVT_LINK_KEY_REQUEST if params.len() >= 6 => Ok(Event::LinkKeyRequest {
            address: addr_at(params, 0),
        }),
        EVT_LINK_KEY_NOTIFICATION if params.len() >= 23 => {
            let mut key = [0u8; 16];
            key.copy_from_slice(&params[6..22]);
            Ok(Event::LinkKeyNotification {
                address: addr_at(params, 0),
                key,
                key_type: params[22],
            })
        }
        EVT_IO_CAPABILITY_REQUEST if params.len() >= 6 => Ok(Event::IoCapabilityRequest {
            address: addr_at(params, 0),
        }),
        EVT_USER_CONFIRMATION_REQUEST if params.len() >= 10 => {
            Ok(Event::UserConfirmationRequest {
                address: addr_at(params, 0),
                numeric: u32::from_le_bytes([params[6], params[7], params[8], params[9]]),
            })
        }
        EVT_SIMPLE_PAIRING_COMPLETE if params.len() >= 7 => Ok(Event::SimplePairingComplete {
            status: params[0],
            address: addr_at(params, 1),
        }),
        EVT_DISCONNECTION_COMPLETE if params.len() >= 4 => Ok(Event::DisconnectionComplete {
            conn_handle: u16::from_le_bytes([params[1], params[2]]) & 0x0FFF,
            reason: params[3],
        }),
        EVT_ENCRYPTION_CHANGE if params.len() >= 4 => Ok(Event::EncryptionChange {
            status: params[0],
            conn_handle: u16::from_le_bytes([params[1], params[2]]) & 0x0FFF,
            enabled: params[3],
        }),
        EVT_LE_META
            if params.first() == Some(&SUBEVT_LE_CONNECTION_COMPLETE) && params.len() >= 19 =>
        {
            Ok(Event::LeConnectionComplete {
                status: params[1],
                conn_handle: u16::from_le_bytes([params[2], params[3]]) & 0x0FFF,
                interval: u16::from_le_bytes([params[12], params[13]]),
                supervision_timeout: u16::from_le_bytes([params[16], params[17]]),
            })
        }
        EVT_LE_META if params.first() == Some(&SUBEVT_LE_ADVERTISING_REPORT) => {
            match parse_adv_report(&params[1..]) {
                Some(r) => Ok(Event::LeAdvertisingReport(r)),
                None => Ok(Event::Other {
                    code,
                    params: params.to_vec(),
                }),
            }
        }
        _ => Ok(Event::Other {
            code,
            params: params.to_vec(),
        }),
    }
}

/// Decode the first report in an `LE Advertising Report` sub-event body.
///
/// Layout: `[num_reports][event_type][addr_type][addr 6][data_len][data…][rssi]`.
/// Only the first report is decoded — controllers advertise one at a time, and
/// mis-walking a multi-report packet would fabricate addresses.
fn parse_adv_report(body: &[u8]) -> Option<AdvReport> {
    if body.len() < 11 {
        return None;
    }
    let event_type = body[1];
    let address_type = body[2];
    let mut address = [0u8; 6];
    address.copy_from_slice(&body[3..9]);
    // The wire carries BD_ADDR least-significant byte first; everything else in
    // FlexInput passes it around in display order.
    address.reverse();
    let data_len = body[9] as usize;
    let data_end = 10 + data_len;
    if body.len() < data_end + 1 {
        return None;
    }
    Some(AdvReport {
        event_type,
        address_type,
        address,
        data: body[10..data_end].to_vec(),
        rssi: body[data_end] as i8,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ⭐ Several events in one buffer must all decode.
    ///
    /// This is the framing that `Dongle::read_event_timeout` performs, asserted
    /// on the parser it depends on. One USB transfer can carry several HCI
    /// events, and treating a read as a single event swallowed every one after
    /// the first — which showed up as a flaky radio rather than as a parsing
    /// bug: advertisements disappeared, and phantom events appeared for handles
    /// the host never issued.
    ///
    /// The stream below is exactly the shape seen in a capture: a short vendor
    /// event immediately followed by a complete LE Advertising Report.
    #[test]
    fn a_transfer_holding_several_events_frames_them_all() {
        let mut stream: Vec<u8> = Vec::new();
        // Vendor event, 3 params.
        stream.extend_from_slice(&[0xFC, 0x03, 0xAA, 0xBB, 0xCC]);
        // LE Meta / Advertising Report: 1 report, type 0, public address,
        // 6-byte address, no data, rssi.
        let adv: [u8; 12] = [
            SUBEVT_LE_ADVERTISING_REPORT, 0x01, 0x00, 0x00,
            0x78, 0x1b, 0xfd, 0x05, 0x48, 0xc8, 0x00, 0xE2,
        ];
        stream.push(EVT_LE_META);
        stream.push(adv.len() as u8);
        stream.extend_from_slice(&adv);

        // Walk it the way the transport does.
        let mut events = Vec::new();
        let mut i = 0;
        while stream.len() >= i + 2 {
            let want = 2 + stream[i + 1] as usize;
            assert!(stream.len() >= i + want, "framing walked off the end");
            events.push(parse_event(&stream[i..i + want]).expect("each event decodes"));
            i += want;
        }
        assert_eq!(i, stream.len(), "the whole buffer must be consumed");
        assert_eq!(events.len(), 2, "both events must be recovered");
        assert!(matches!(events[0], Event::Other { code: 0xFC, .. }));
        assert!(
            matches!(events[1], Event::LeAdvertisingReport(_)),
            "the SECOND event is the advertising report — the one that used to \
             be swallowed, and the one discovery depends on",
        );
    }

    /// ⭐ A short VENDOR event must not fail the connection.
    ///
    /// This is the reported `event 0xfc claims 19 params, got 9`, which aborted
    /// controller init outright. Realtek dongles emit vendor events constantly
    /// and this stack decodes none of them, so refusing to connect over one
    /// arriving short is failing on data that was going to be discarded either
    /// way — and it made the dongle look flaky when the link was fine.
    #[test]
    fn a_truncated_vendor_event_is_kept_not_rejected() {
        // Claims 19 params, carries 9 — exactly the observed packet shape.
        let mut buf = vec![0xFC, 19];
        buf.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9]);
        match parse_event(&buf) {
            Ok(Event::Other { code, params }) => {
                assert_eq!(code, 0xFC);
                assert_eq!(params.len(), 9, "the bytes that DID arrive are kept");
            }
            other => panic!("expected Event::Other, got {other:?}"),
        }
    }

    /// ⛔ But truncation still fails for events we decode.
    ///
    /// Reading a connection handle or a disconnect reason out of bytes that
    /// never arrived would invent link state, which is far worse than an error.
    #[test]
    fn a_truncated_decoded_event_is_still_an_error() {
        for code in [
            EVT_COMMAND_COMPLETE,
            EVT_COMMAND_STATUS,
            EVT_DISCONNECTION_COMPLETE,
            EVT_ENCRYPTION_CHANGE,
            EVT_LE_META,
        ] {
            let buf = vec![code, 19, 1, 2, 3];
            assert!(
                parse_event(&buf).is_err(),
                "{code:#04x} truncated must not decode",
            );
        }
    }

    #[test]
    fn reset_opcode_is_0x0c03() {
        // OGF 0x03 << 10 | OCF 0x0003. Spelled out because getting the shift
        // wrong produces a valid-looking opcode for a different command.
        assert_eq!(Opcode::RESET.0, 0x0C03);
        assert_eq!(Opcode::LE_SET_SCAN_ENABLE.0, 0x200C);
        assert_eq!(Opcode::LE_SET_SCAN_PARAMETERS.0, 0x200B);
    }

    #[test]
    fn reset_encodes_to_three_bytes_with_no_type_prefix() {
        assert_eq!(encode_command(Opcode::RESET, &[]), vec![0x03, 0x0C, 0x00]);
    }

    #[test]
    fn command_encodes_params_after_the_length() {
        assert_eq!(
            encode_command(Opcode::LE_SET_SCAN_ENABLE, &[0x01, 0x00]),
            vec![0x0C, 0x20, 0x02, 0x01, 0x00],
        );
    }

    #[test]
    fn parses_a_reset_command_complete() {
        // What a controller actually answers HCI_Reset with.
        let evt = parse_event(&[0x0E, 0x04, 0x01, 0x03, 0x0C, 0x00]).unwrap();
        let Event::CommandComplete(cc) = evt else {
            panic!("expected Command Complete, got {evt:?}");
        };
        assert_eq!(cc.opcode, Opcode::RESET);
        assert_eq!(cc.num_hci_command_packets, 1);
        assert!(cc.succeeded());
    }

    #[test]
    fn a_nonzero_status_is_not_success() {
        let evt = parse_event(&[0x0E, 0x04, 0x01, 0x03, 0x0C, 0x12]).unwrap();
        let Event::CommandComplete(cc) = evt else { panic!() };
        assert_eq!(cc.status(), Some(0x12));
        assert!(!cc.succeeded());
    }

    #[test]
    fn unknown_events_are_preserved_not_dropped() {
        let evt = parse_event(&[0x3E, 0x02, 0xAA, 0xBB]).unwrap();
        assert_eq!(
            evt,
            Event::Other { code: 0x3E, params: vec![0xAA, 0xBB] }
        );
    }

    #[test]
    fn parses_an_advertising_report_with_nintendo_manufacturer_data() {
        // LE Meta / Advertising Report, one report, address c8:48:05:fd:1b:78
        // on the wire in reverse, carrying AD type 0xFF with Nintendo's company
        // id 0x0553 and the Joy-Con 2 (R) product id 0x2066.
        let body = [
            0x02, // subevent: advertising report
            0x01, // num reports
            0x00, // event type: connectable undirected
            0x00, // address type: public
            0x78, 0x1b, 0xfd, 0x05, 0x48, 0xc8, // address, LSB first
            0x0b, // data length: one 11-byte AD structure
            // [len=0x0A][type=0xFF][company 0553][3 unknown][vid 057E][pid 2066]
            0x0a, 0xff, 0x53, 0x05, 0x00, 0x00, 0x00, 0x7e, 0x05, 0x66, 0x20,
            0xc4, // rssi = -60
        ];
        let mut packet = vec![EVT_LE_META, body.len() as u8];
        packet.extend_from_slice(&body);

        let Event::LeAdvertisingReport(r) = parse_event(&packet).unwrap() else {
            panic!("expected an advertising report");
        };
        // Display order, not wire order — reversing is easy to forget and gives
        // a plausible-looking wrong address.
        assert_eq!(r.address, [0xc8, 0x48, 0x05, 0xfd, 0x1b, 0x78]);
        assert_eq!(r.rssi, -60);

        let md = r.manufacturer_data().expect("manufacturer data present");
        // Company id is INCLUDED here, so every field sits two bytes later than
        // in btleplug, which strips it into a map key. Getting this wrong is
        // silent: it yields a plausible u16 from the neighbouring fields.
        assert_eq!(u16::from_le_bytes([md[0], md[1]]), 0x0553, "Nintendo company id");
        assert_eq!(u16::from_le_bytes([md[5], md[6]]), 0x057E, "Nintendo VID");
        assert_eq!(u16::from_le_bytes([md[7], md[8]]), 0x2066, "Joy-Con 2 (R) PID");
    }

    #[test]
    fn an_advert_without_manufacturer_data_returns_none() {
        let r = AdvReport {
            event_type: 0,
            address_type: 0,
            address: [0; 6],
            // AD type 0x09 (complete local name), not 0xFF.
            data: vec![0x05, 0x09, b't', b'e', b's', b't'],
            rssi: -40,
        };
        assert!(r.manufacturer_data().is_none());
    }

    #[test]
    fn a_truncated_packet_is_rejected_rather_than_decoded() {
        // Claims 4 params, carries 2. Decoding it would fabricate an opcode.
        assert!(parse_event(&[0x0E, 0x04, 0x01, 0x03]).is_err());
        assert!(parse_event(&[0x0E]).is_err());
    }
}

#[cfg(test)]
mod inquiry_tests {
    use super::*;

    /// ⛔ Inquiry results are stored COLUMN-WISE, and one event carries several.
    ///
    /// ⭐ This is the layout mistake that would be invisible in a room with one
    /// device and wrong in a room with two. The event is not an array of
    /// structs: it is a count, then EVERY address, then every repetition mode,
    /// then every reserved byte, then every class, then every clock offset.
    /// Reading it as rows splices one device's address onto another's class,
    /// and both decode to plausible-looking values.
    #[test]
    fn two_devices_in_one_event_decode_column_wise() {
        // Two devices, Inquiry Result with RSSI (0x22): 6 addr, 1 psrm,
        // 1 reserved, 3 cod, 2 clock, 1 rssi — all column-wise.
        let mut p = vec![2u8];
        p.extend_from_slice(&[1, 1, 1, 1, 1, 1]); // addr A
        p.extend_from_slice(&[2, 2, 2, 2, 2, 2]); // addr B
        p.extend_from_slice(&[0x01, 0x02]);       // psrm
        p.extend_from_slice(&[0x00, 0x00]);       // reserved
        p.extend_from_slice(&[0x08, 0x05, 0x00]); // cod A: peripheral/gamepad
        p.extend_from_slice(&[0x04, 0x04, 0x24]); // cod B: audio/video
        p.extend_from_slice(&[0x11, 0x00, 0x22, 0x00]); // clock offsets
        p.extend_from_slice(&[0xD0, 0xC0]);       // rssi -48, -64

        let got = parse_inquiry_results(0x22, &p);
        assert_eq!(got.len(), 2, "only {} of 2 devices decoded", got.len());
        assert_eq!(got[0].address, [1; 6]);
        assert_eq!(got[1].address, [2; 6]);
        assert_eq!(got[0].class_of_device, [0x08, 0x05, 0x00]);
        assert_eq!(got[1].class_of_device, [0x04, 0x04, 0x24]);
        assert_eq!(got[0].rssi, Some(-48));
        assert_eq!(got[1].rssi, Some(-64));
        assert_eq!(got[0].clock_offset, 0x0011);
    }

    /// The Class of Device tells a gamepad from a headset without pairing it.
    #[test]
    fn a_gamepad_is_recognised_from_its_class_of_device() {
        let pad = InquiryResult {
            address: [0; 6],
            class_of_device: [0x08, 0x05, 0x00], // peripheral, gamepad
            page_scan_repetition_mode: 0,
            clock_offset: 0,
            rssi: None,
        };
        assert!(pad.looks_like_a_gamepad(), "cod {:02x?}", pad.class_of_device);
        assert_eq!(pad.major_class(), 0x05);

        let headset = InquiryResult { class_of_device: [0x04, 0x04, 0x24], ..pad.clone() };
        assert!(!headset.looks_like_a_gamepad());
    }

    /// A truncated event must decode nothing rather than inventing a device
    /// from whatever bytes arrived.
    #[test]
    fn a_truncated_inquiry_event_yields_nothing() {
        let p = vec![2u8, 1, 1, 1]; // claims two devices, carries four bytes
        assert!(parse_inquiry_results(0x22, &p).is_empty());
    }

    /// ⛔ A remote name is a fixed 248-byte NUL-PADDED field, not a
    /// length-prefixed string.
    ///
    /// Taking the field whole yields a name with 200-odd NUL bytes glued to it,
    /// which compares unequal to itself in every UI and logs as a wall of
    /// nothing. Trimming at the first NUL is the entire decode.
    #[test]
    fn a_remote_name_is_trimmed_at_its_first_nul() {
        let mut params = vec![0x00];                  // status
        params.extend_from_slice(&[0xAA; 6]);         // address
        params.extend_from_slice(b"Pro Controller");
        params.resize(1 + 6 + 248, 0);                // NUL padding to 248
        let mut evt = vec![EVT_REMOTE_NAME_COMPLETE, params.len() as u8];
        evt.extend_from_slice(&params);

        match parse_event(&evt).expect("remote name event must decode") {
            Event::RemoteNameComplete { status, address, name } => {
                assert_eq!(status, 0);
                assert_eq!(address, [0xAA; 6]);
                assert_eq!(name, "Pro Controller", "name was {name:?}");
            }
            other => panic!("decoded as {other:?}"),
        }
    }

    /// ⛔ The link key is the BOND — decode it off by one byte and pairing
    /// silently produces a key that will never authenticate again.
    ///
    /// Layout is address(6), key(16), key type(1). The key is NOT reversed:
    /// the address is little-endian because BD_ADDRs are, the key is a byte
    /// string and is not.
    #[test]
    fn a_link_key_notification_decodes_address_key_and_type() {
        let mut params = Vec::new();
        params.extend_from_slice(&[0x11; 6]);
        params.extend_from_slice(&(0u8..16).collect::<Vec<u8>>());
        params.push(0x05); // authenticated combination key, P-256
        let mut evt = vec![EVT_LINK_KEY_NOTIFICATION, params.len() as u8];
        evt.extend_from_slice(&params);

        match parse_event(&evt).expect("must decode") {
            Event::LinkKeyNotification { address, key, key_type } => {
                assert_eq!(address, [0x11; 6]);
                assert_eq!(key[0], 0);
                assert_eq!(key[15], 15, "key decoded off the end: {key:02x?}");
                assert_eq!(key_type, 0x05);
            }
            other => panic!("decoded as {other:?}"),
        }
    }

    /// A BR/EDR `Connection Complete` is a different event from the LE one and
    /// carries the address, which the LE version does not.
    #[test]
    fn a_classic_connection_complete_carries_handle_and_address() {
        let mut params = vec![0x00, 0x0C, 0x00];       // status, handle 0x000C
        params.extend_from_slice(&[0xAB; 6]);          // address
        params.push(0x01);                             // link type ACL
        params.push(0x00);                             // encryption off
        let mut evt = vec![EVT_CONNECTION_COMPLETE, params.len() as u8];
        evt.extend_from_slice(&params);

        match parse_event(&evt).expect("must decode") {
            Event::ConnectionComplete { status, conn_handle, address } => {
                assert_eq!(status, 0);
                assert_eq!(conn_handle, 0x000C);
                assert_eq!(address, [0xAB; 6]);
            }
            other => panic!("decoded as {other:?}"),
        }
    }

    /// The classic opcodes must be the spec values; a wrong OGF is answered
    /// with "unknown command", which reads exactly like an unsupported radio.
    #[test]
    fn the_classic_opcodes_are_the_spec_values() {
        assert_eq!(Opcode::INQUIRY.0, 0x0401);
        assert_eq!(Opcode::INQUIRY_CANCEL.0, 0x0402);
        assert_eq!(Opcode::REMOTE_NAME_REQUEST.0, 0x0419);
        assert_eq!(Opcode::READ_LOCAL_FEATURES.0, 0x1003);
        assert_eq!(Opcode::CREATE_CONNECTION.0, 0x0405);
        assert_eq!(Opcode::CREATE_CONNECTION_CANCEL.0, 0x0408);
        assert_eq!(Opcode::READ_SCAN_ENABLE.0, 0x0C19);
        assert_eq!(Opcode::LINK_KEY_REQUEST_REPLY.0, 0x040B);
        assert_eq!(Opcode::LINK_KEY_REQUEST_NEGATIVE_REPLY.0, 0x040C);
        assert_eq!(Opcode::AUTHENTICATION_REQUESTED.0, 0x0411);
        assert_eq!(Opcode::SET_CONNECTION_ENCRYPTION.0, 0x0413);
        assert_eq!(Opcode::IO_CAPABILITY_REQUEST_REPLY.0, 0x042B);
        assert_eq!(Opcode::USER_CONFIRMATION_REQUEST_REPLY.0, 0x042C);
        assert_eq!(Opcode::WRITE_SIMPLE_PAIRING_MODE.0, 0x0C56);
    }
}