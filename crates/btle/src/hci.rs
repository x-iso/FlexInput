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
    /// Anything not decoded yet, kept whole so nothing is silently discarded.
    Other { code: u8, params: Vec<u8> },
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
    if buf.len() < 2 + len {
        return Err(crate::Error::Protocol(format!(
            "event {code:#04x} claims {len} params, got {}",
            buf.len() - 2
        )));
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
