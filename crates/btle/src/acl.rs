//! ACL data, L2CAP and the small slice of ATT this stack needs.
//!
//! Mostly **not** a general GATT client. The Joy-Con's handles are already
//! known from an HCI capture taken while the Windows stack drove the
//! controller, so normal operation skips discovery entirely: writing to a known
//! handle is the same operation whether or not we walked the attribute table to
//! find it. See [`crate::joycon`] for the handles themselves.
//!
//! The exception is [`find_information_request`] and [`read_by_type_request`],
//! added for one specific reason: the *captured* handles are what the Windows
//! stack happened to use, which is not the same as the complete list of what
//! the controller offers. A characteristic Windows never subscribed to is
//! invisible to a capture-derived handle table, and the gyro search has spent
//! itself entirely inside the one stream that capture did show.
//!
//! Layering, outermost first:
//!
//! ```text
//! HCI ACL : [handle:12 | pb:2 | bc:2][total_len u16][payload]
//! L2CAP   : [len u16][cid u16][payload]
//! ATT     : [opcode u8][params]
//! ```

/// L2CAP channel id for the Attribute Protocol. Fixed by the spec.
pub const CID_ATT: u16 = 0x0004;

/// Packet Boundary flag for the first fragment of an L2CAP message.
///
/// **`0b00`, not `0b10`.** Core spec Vol 4 Part E §5.4.2 defines `0b10` as
/// "first *automatically-flushable* packet", which exists for BR/EDR; an LE-U
/// link has no automatic flush, so the host sends `0b00` — "first
/// non-automatically-flushable" — for the start of every L2CAP PDU, and `0b01`
/// for continuations.
///
/// Getting this wrong is silent in the worst way: the controller accepts the
/// USB bulk write, `write_bulk` reports success, and the packet is simply never
/// put on air. The observed symptom was a healthy connection that held for 53
/// seconds with **zero** ATT traffic in either direction — no MTU response, no
/// ATT error, nothing — followed by the peripheral giving up and terminating
/// the link itself.
const PB_FIRST: u16 = 0b00;
/// Packet-boundary flag for a CONTINUATION of an L2CAP PDU already begun.
///
/// ❗ A continuing fragment carries NO L2CAP header — it is raw payload picked
/// up mid-PDU. Decoding one as though it started a fresh packet reads two
/// arbitrary payload bytes as a length and two more as a channel id, and if
/// those happen to pass the sanity checks the result is a well-formed-looking
/// ATT PDU assembled from the middle of an IMU sample. Roughly one continuation
/// in 256 would then be accepted as a notification, with a garbage attribute
/// handle and a garbage value.
///
/// That is the worst possible failure for this decode: a motion report parsed
/// from misaligned bytes yields an accelerometer vector of about the right
/// magnitude pointing the wrong way, and the orientation tracker LATCHES the
/// last plausible gravity it saw. One accepted fragment can therefore leave
/// pitch inverted and stuck that way.
const PB_CONTINUING: u16 = 0b01;

// ATT opcodes. Only what is actually sent or received here.
pub const ATT_WRITE_REQUEST: u8 = 0x12;
pub const ATT_WRITE_RESPONSE: u8 = 0x13;
pub const ATT_WRITE_COMMAND: u8 = 0x52;
pub const ATT_HANDLE_VALUE_NOTIFICATION: u8 = 0x1B;
pub const ATT_ERROR_RESPONSE: u8 = 0x01;
pub const ATT_EXCHANGE_MTU_REQUEST: u8 = 0x02;
pub const ATT_EXCHANGE_MTU_RESPONSE: u8 = 0x03;
pub const ATT_FIND_INFORMATION_REQUEST: u8 = 0x04;
pub const ATT_FIND_INFORMATION_RESPONSE: u8 = 0x05;
pub const ATT_READ_BY_TYPE_REQUEST: u8 = 0x08;
pub const ATT_READ_BY_TYPE_RESPONSE: u8 = 0x09;
pub const ATT_READ_REQUEST: u8 = 0x0A;
pub const ATT_READ_RESPONSE: u8 = 0x0B;

/// ATT error code meaning "no more attributes in the requested range".
///
/// This is the normal, expected way an attribute-table walk ends — not a
/// failure. Treating it as one would turn a completed discovery into an error.
pub const ATT_ERR_ATTRIBUTE_NOT_FOUND: u8 = 0x0A;

/// Attribute type of a GATT characteristic declaration.
pub const GATT_CHARACTERISTIC: u16 = 0x2803;
/// Attribute type of a Client Characteristic Configuration descriptor.
pub const GATT_CCCD: u16 = 0x2902;

/// Characteristic property bits, from the declaration value.
pub const PROP_READ: u8 = 0x02;
/// Write **without** response — the attribute accepts opcode `0x52` only.
///
/// The distinction between this and [`PROP_WRITE`] is not cosmetic and cost
/// this project several rounds of wrong guesses. A characteristic that offers
/// only this bit answers an acknowledged Write Request (`0x12`) with
/// `Write Not Permitted`, and one that offers only [`PROP_WRITE`] silently
/// discards an unacknowledged Write Command (`0x52`). Both failures look
/// identical from the outside: the command has no effect and nothing is
/// reported. Reading the properties is the only way to know which to send.
pub const PROP_WRITE_NO_RESPONSE: u8 = 0x04;
/// Write **with** response — the attribute accepts opcode `0x12`.
pub const PROP_WRITE: u8 = 0x08;
pub const PROP_NOTIFY: u8 = 0x10;
pub const PROP_INDICATE: u8 = 0x20;

/// Client Characteristic Configuration value that enables notifications.
pub const CCCD_NOTIFY: [u8; 2] = [0x01, 0x00];

/// Wrap an ATT PDU in L2CAP and HCI ACL framing, ready for the bulk endpoint.
///
/// No fragmentation: every packet this stack sends is far below the ACL
/// payload size a controller must accept, and a Joy-Con command is 49 bytes.
pub fn encode_acl(conn_handle: u16, cid: u16, payload: &[u8]) -> Vec<u8> {
    encode_acl_pb(conn_handle, cid, payload, PB_FIRST)
}

/// First packet of an L2CAP PDU on a BR/EDR link.
///
/// ⭐ **A different flag from the LE one, and the difference is not cosmetic.**
/// On LE, `0b00` means "first, non-automatically-flushable" and is what every
/// packet uses. On BR/EDR the same value means the packet is NOT flushable,
/// which is wrong for HID: an interrupt report that cannot be flushed will be
/// retransmitted indefinitely when the air is busy, stalling the channel behind
/// it rather than being dropped in favour of the next, fresher report.
///
/// `0b10` — first, automatically flushable — is the classic form.
pub const PB_FIRST_FLUSHABLE: u16 = 0b10;

/// [`encode_acl`] with the packet-boundary flag chosen explicitly.
pub fn encode_acl_pb(conn_handle: u16, cid: u16, payload: &[u8], pb: u16) -> Vec<u8> {
    let header = (conn_handle & 0x0FFF) | (pb << 12);
    let l2cap_len = payload.len() as u16;
    let total_len = (payload.len() + 4) as u16;

    let mut out = Vec::with_capacity(9 + payload.len());
    out.extend_from_slice(&header.to_le_bytes());
    out.extend_from_slice(&total_len.to_le_bytes());
    out.extend_from_slice(&l2cap_len.to_le_bytes());
    out.extend_from_slice(&cid.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// One decoded inbound ACL packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclPacket {
    pub conn_handle: u16,
    pub cid: u16,
    pub payload: Vec<u8>,
}

/// Decode an inbound HCI ACL packet.
///
/// Returns `None` for anything malformed rather than guessing. A truncated ACL
/// packet decoded optimistically yields a plausible-looking ATT PDU built from
/// the wrong bytes, which is far worse than dropping it.
pub fn parse_acl(buf: &[u8]) -> Option<AclPacket> {
    if buf.len() < 8 {
        return None;
    }
    let header = u16::from_le_bytes([buf[0], buf[1]]);
    let conn_handle = header & 0x0FFF;
    // ⭐ The boundary flag is READ, not masked away and forgotten.
    //
    // Reassembling fragments properly would be better still, but dropping them
    // is both correct and safe: the report is lost either way, and a lost
    // report is invisible next to one decoded from the wrong bytes.
    if (header >> 12) & 0b11 == PB_CONTINUING {
        return None;
    }
    let total_len = u16::from_le_bytes([buf[2], buf[3]]) as usize;
    if buf.len() < 4 + total_len || total_len < 4 {
        return None;
    }
    let l2cap_len = u16::from_le_bytes([buf[4], buf[5]]) as usize;
    let cid = u16::from_le_bytes([buf[6], buf[7]]);
    let body = &buf[8..4 + total_len];
    if body.len() < l2cap_len {
        return None;
    }
    Some(AclPacket {
        conn_handle,
        cid,
        payload: body[..l2cap_len].to_vec(),
    })
}

/// Split one USB bulk transfer into every complete ACL packet it holds.
///
/// ⭐ Separated from the read so it can be tested without a dongle. The bug it
/// exists to prevent — keeping only the first packet of a transfer — is silent,
/// produces no error, and only shows up under load, which is the hardest
/// possible combination to catch by running the thing.
///
/// A truncated tail is dropped rather than held for the next read. See
/// `Dongle::read_event_timeout` for why carrying it forward is worse.
pub fn split_acl(buf: &[u8]) -> Vec<AclPacket> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 4 <= buf.len() {
        let want = 4 + u16::from_le_bytes([buf[i + 2], buf[i + 3]]) as usize;
        if i + want > buf.len() {
            break;
        }
        if let Some(p) = parse_acl(&buf[i..i + want]) {
            out.push(p);
        }
        i += want;
    }
    out
}

/// An ATT notification: a characteristic value pushed by the controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub handle: u16,
    pub value: Vec<u8>,
}

/// Decode a `Handle Value Notification`, or `None` if this PDU is not one.
pub fn parse_notification(att: &[u8]) -> Option<Notification> {
    if att.len() < 3 || att[0] != ATT_HANDLE_VALUE_NOTIFICATION {
        return None;
    }
    Some(Notification {
        handle: u16::from_le_bytes([att[1], att[2]]),
        value: att[3..].to_vec(),
    })
}

/// Build an ATT `Write Command` — no response, used for controller commands.
pub fn write_command(handle: u16, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(3 + value.len());
    out.push(ATT_WRITE_COMMAND);
    out.extend_from_slice(&handle.to_le_bytes());
    out.extend_from_slice(value);
    out
}

/// Build an ATT `Write Request` — acknowledged, used for CCCD writes so that a
/// failed subscribe is visible instead of silently producing no notifications.
pub fn write_request(handle: u16, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(3 + value.len());
    out.push(ATT_WRITE_REQUEST);
    out.extend_from_slice(&handle.to_le_bytes());
    out.extend_from_slice(value);
    out
}

/// Build an ATT `Exchange MTU Request`.
///
/// Worth sending: the default ATT MTU is 23 bytes, so a 63-byte input report
/// would otherwise arrive fragmented across several notifications.
pub fn exchange_mtu_request(mtu: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(3);
    out.push(ATT_EXCHANGE_MTU_REQUEST);
    out.extend_from_slice(&mtu.to_le_bytes());
    out
}

/// An attribute UUID, which ATT sends as either 16 or 128 bits.
///
/// Kept as a two-variant enum rather than widening everything to 128 bits: the
/// Joy-Con mixes both — standard descriptors like the CCCD are 16-bit, vendor
/// characteristics are 128-bit — and a short UUID printed as its full 128-bit
/// Bluetooth-base expansion is much harder to recognise at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttUuid {
    Short(u16),
    Long([u8; 16]),
}

impl std::fmt::Display for AttUuid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Short(v) => write!(f, "{v:#06x}"),
            // ATT sends 128-bit UUIDs little-endian; print them in the
            // conventional big-endian text form so they can be compared
            // directly against the UUIDs written down in `protocol.rs`.
            Self::Long(b) => {
                let r: Vec<u8> = b.iter().rev().copied().collect();
                write!(
                    f,
                    "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                    r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7],
                    r[8], r[9], r[10], r[11], r[12], r[13], r[14], r[15],
                )
            }
        }
    }
}

/// One entry from a `Find Information Response`: a handle and its type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttrInfo {
    pub handle: u16,
    pub uuid: AttUuid,
}

/// Build an ATT `Find Information Request` — walks handles and their types.
pub fn find_information_request(start: u16, end: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(5);
    out.push(ATT_FIND_INFORMATION_REQUEST);
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&end.to_le_bytes());
    out
}

/// Decode a `Find Information Response`, or `None` if this PDU is not one.
///
/// Format byte `0x01` means 16-bit UUIDs follow, `0x02` means 128-bit; a
/// response never mixes the two, which is why the entry size is fixed per PDU.
pub fn parse_find_information_response(att: &[u8]) -> Option<Vec<AttrInfo>> {
    if att.len() < 2 || att[0] != ATT_FIND_INFORMATION_RESPONSE {
        return None;
    }
    let uuid_len = match att[1] {
        0x01 => 2usize,
        0x02 => 16,
        _ => return None,
    };
    let entry = 2 + uuid_len;
    let mut out = Vec::new();
    for chunk in att[2..].chunks(entry) {
        if chunk.len() < entry {
            break;
        }
        let handle = u16::from_le_bytes([chunk[0], chunk[1]]);
        let uuid = if uuid_len == 2 {
            AttUuid::Short(u16::from_le_bytes([chunk[2], chunk[3]]))
        } else {
            AttUuid::Long(chunk[2..18].try_into().ok()?)
        };
        out.push(AttrInfo { handle, uuid });
    }
    Some(out)
}

/// Build an ATT `Read By Type Request` for a 16-bit attribute type.
pub fn read_by_type_request(start: u16, end: u16, uuid: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(7);
    out.push(ATT_READ_BY_TYPE_REQUEST);
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&end.to_le_bytes());
    out.extend_from_slice(&uuid.to_le_bytes());
    out
}

/// Decode a `Read By Type Response` into `(handle, value)` pairs.
pub fn parse_read_by_type_response(att: &[u8]) -> Option<Vec<(u16, Vec<u8>)>> {
    if att.len() < 2 || att[0] != ATT_READ_BY_TYPE_RESPONSE {
        return None;
    }
    let entry = att[1] as usize;
    if entry < 3 {
        return None;
    }
    let mut out = Vec::new();
    for chunk in att[2..].chunks(entry) {
        if chunk.len() < entry {
            break;
        }
        out.push((
            u16::from_le_bytes([chunk[0], chunk[1]]),
            chunk[2..].to_vec(),
        ));
    }
    Some(out)
}

/// Build an ATT `Read Request` for one attribute's value.
///
/// The slow, one-at-a-time cousin of [`read_by_type_request`], and the reason
/// it exists here: this controller answers Find Information and plain Read, but
/// never answers Read By Type at all. Reading each characteristic declaration
/// individually recovers the same properties one round trip at a time.
pub fn read_request(handle: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(3);
    out.push(ATT_READ_REQUEST);
    out.extend_from_slice(&handle.to_le_bytes());
    out
}

/// Decode a `Read Response` into the attribute value, or `None` if not one.
pub fn parse_read_response(att: &[u8]) -> Option<Vec<u8>> {
    (!att.is_empty() && att[0] == ATT_READ_RESPONSE).then(|| att[1..].to_vec())
}

/// A decoded GATT characteristic declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharDecl {
    /// Handle of the declaration itself.
    pub decl_handle: u16,
    /// Property bits — see [`PROP_NOTIFY`].
    pub properties: u8,
    /// Handle the value lives at, which is what notifications carry.
    pub value_handle: u16,
    pub uuid: AttUuid,
}

impl CharDecl {
    pub fn notifiable(&self) -> bool {
        self.properties & (PROP_NOTIFY | PROP_INDICATE) != 0
    }

    /// The ATT opcode this characteristic will actually accept for a write.
    ///
    /// Prefers the acknowledged form when the attribute offers it, because an
    /// acknowledged write reports refusal instead of vanishing. Falls back to
    /// Write Command when that is all the attribute supports — which is the
    /// case for the Joy-Con 2 command channel, and sending `0x12` there earns
    /// a `Write Not Permitted` for every command in the init sequence.
    ///
    /// `None` means the attribute is not writable at all, so a write to it is
    /// a bug rather than a choice of opcode.
    pub fn write_opcode(&self) -> Option<u8> {
        if self.properties & PROP_WRITE != 0 {
            Some(ATT_WRITE_REQUEST)
        } else if self.properties & PROP_WRITE_NO_RESPONSE != 0 {
            Some(ATT_WRITE_COMMAND)
        } else {
            None
        }
    }

    /// Spell the property bits, so a log line explains a refusal by itself.
    pub fn properties_text(&self) -> String {
        let mut s = Vec::new();
        for (bit, name) in [
            (PROP_READ, "READ"),
            (PROP_WRITE_NO_RESPONSE, "WRITE_NO_RSP"),
            (PROP_WRITE, "WRITE"),
            (PROP_NOTIFY, "NOTIFY"),
            (PROP_INDICATE, "INDICATE"),
        ] {
            if self.properties & bit != 0 {
                s.push(name);
            }
        }
        if s.is_empty() {
            "none".into()
        } else {
            s.join("|")
        }
    }
}

/// Decode one characteristic declaration value: `[props][value handle][uuid]`.
pub fn parse_characteristic(decl_handle: u16, value: &[u8]) -> Option<CharDecl> {
    if value.len() < 5 {
        return None;
    }
    let uuid = match value.len() - 3 {
        2 => AttUuid::Short(u16::from_le_bytes([value[3], value[4]])),
        16 => AttUuid::Long(value[3..19].try_into().ok()?),
        _ => return None,
    };
    Some(CharDecl {
        decl_handle,
        properties: value[0],
        value_handle: u16::from_le_bytes([value[1], value[2]]),
        uuid,
    })
}

/// Is this ATT PDU an error response saying the walk has reached the end?
pub fn is_attribute_not_found(att: &[u8]) -> bool {
    att.len() >= 5 && att[0] == ATT_ERROR_RESPONSE && att[4] == ATT_ERR_ATTRIBUTE_NOT_FOUND
}

/// The error code from an ATT `Error Response`, if this PDU is one.
pub fn att_error_code(att: &[u8]) -> Option<u8> {
    (att.len() >= 5 && att[0] == ATT_ERROR_RESPONSE).then(|| att[4])
}

/// Spell an ATT error code, so a refused request says what it needs.
///
/// The difference between these matters enormously and is invisible from a bare
/// number: `Insufficient Authentication` means discovery needs an encrypted
/// link, `Request Not Supported` means the peer has no GATT server to walk at
/// all, and `Attribute Not Found` means the walk simply finished. Reporting
/// "discovery returned nothing" for all three is how a diagnosable failure
/// turns into a dead end.
pub fn att_error_name(code: u8) -> &'static str {
    match code {
        0x01 => "Invalid Handle",
        0x02 => "Read Not Permitted",
        0x03 => "Write Not Permitted",
        0x04 => "Invalid PDU",
        0x05 => "Insufficient Authentication (the link must be ENCRYPTED first)",
        0x06 => "Request Not Supported",
        0x07 => "Invalid Offset",
        0x08 => "Insufficient Authorization",
        0x09 => "Prepare Queue Full",
        0x0A => "Attribute Not Found (end of the walk)",
        0x0B => "Attribute Not Long",
        0x0C => "Insufficient Encryption Key Size",
        0x0D => "Invalid Attribute Value Length",
        0x0E => "Unlikely Error",
        0x0F => "Insufficient Encryption (the link must be ENCRYPTED first)",
        0x10 => "Unsupported Group Type",
        0x11 => "Insufficient Resources",
        _ => "unknown ATT error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acl_framing_round_trips() {
        let att = write_command(0x0016, &[0xaa, 0xbb]);
        let acl = encode_acl(0x0040, CID_ATT, &att);

        // handle 0x0040 with PB=0b00 -> 0x0040. Pinned deliberately: 0x2040
        // here (PB=0b10) is a BR/EDR flag that an LE controller drops silently.
        assert_eq!(&acl[0..2], &[0x40, 0x00]);
        // total length = L2CAP header (4) + payload
        assert_eq!(u16::from_le_bytes([acl[2], acl[3]]) as usize, att.len() + 4);

        let parsed = parse_acl(&acl).expect("round trip");
        assert_eq!(parsed.conn_handle, 0x0040);
        assert_eq!(parsed.cid, CID_ATT);
        assert_eq!(parsed.payload, att);
    }

    #[test]
    fn write_command_uses_the_unacknowledged_opcode() {
        // 0x52, not 0x12: the Joy-Con command channel is write-without-response,
        // and using Write Request instead stalls waiting for a reply.
        let pdu = write_command(0x0016, &[1, 2, 3]);
        assert_eq!(pdu[0], ATT_WRITE_COMMAND);
        assert_eq!(u16::from_le_bytes([pdu[1], pdu[2]]), 0x0016);
        assert_eq!(&pdu[3..], &[1, 2, 3]);
    }

    #[test]
    fn parses_a_notification_on_the_input_handle() {
        // What the Windows capture showed arriving every 15 ms: a notification
        // on handle 0x000e carrying the 63-byte input report.
        let mut att = vec![ATT_HANDLE_VALUE_NOTIFICATION, 0x0e, 0x00];
        att.extend_from_slice(&[0x01, 0x18, 0x00]);
        let n = parse_notification(&att).expect("notification");
        assert_eq!(n.handle, 0x000e);
        assert_eq!(n.value, vec![0x01, 0x18, 0x00]);
    }

    #[test]
    fn a_write_response_is_not_mistaken_for_a_notification() {
        assert!(parse_notification(&[ATT_WRITE_RESPONSE]).is_none());
        assert!(parse_notification(&[ATT_ERROR_RESPONSE, 0x12, 0x0e, 0x00, 0x05]).is_none());
    }

    #[test]
    fn find_information_response_decodes_short_uuids() {
        // Two 16-bit entries: a CCCD and the vendor report-rate descriptor slot.
        let att = [
            ATT_FIND_INFORMATION_RESPONSE,
            0x01,
            0x0f, 0x00, 0x02, 0x29,
            0x10, 0x00, 0x01, 0x29,
        ];
        let got = parse_find_information_response(&att).expect("decodes");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], AttrInfo { handle: 0x000f, uuid: AttUuid::Short(GATT_CCCD) });
        assert_eq!(got[1].handle, 0x0010);
    }

    #[test]
    fn find_information_response_decodes_a_vendor_128_bit_uuid() {
        // CHR_INPUT_COMMON, ab7de9be-89fe-49ad-828f-118f09df7fd2, sent
        // little-endian on the wire. The reversal is the whole point of the
        // test: printing it byte-order-swapped would make a correct discovery
        // look like it found an unrelated characteristic.
        let mut att = vec![ATT_FIND_INFORMATION_RESPONSE, 0x02, 0x0a, 0x00];
        let be: [u8; 16] = [
            0xab, 0x7d, 0xe9, 0xbe, 0x89, 0xfe, 0x49, 0xad,
            0x82, 0x8f, 0x11, 0x8f, 0x09, 0xdf, 0x7f, 0xd2,
        ];
        att.extend(be.iter().rev().copied());
        let got = parse_find_information_response(&att).expect("decodes");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].handle, 0x000a);
        assert_eq!(
            got[0].uuid.to_string(),
            "ab7de9be-89fe-49ad-828f-118f09df7fd2"
        );
    }

    #[test]
    fn a_characteristic_declaration_yields_its_value_handle_and_properties() {
        // props = READ|NOTIFY, value at 0x000e, 128-bit UUID.
        let mut value = vec![0x02 | PROP_NOTIFY, 0x0e, 0x00];
        value.extend_from_slice(&[0u8; 16]);
        let c = parse_characteristic(0x000d, &value).expect("decodes");
        assert_eq!(c.value_handle, 0x000e);
        assert!(c.notifiable());

        // A write-without-response characteristic must NOT be reported as
        // notifiable, or discovery would try to subscribe to the command
        // channel and the resulting ATT error would look like a bug elsewhere.
        let mut w = vec![0x04, 0x16, 0x00];
        w.extend_from_slice(&[0u8; 16]);
        assert!(!parse_characteristic(0x0015, &w).expect("decodes").notifiable());
    }

    #[test]
    fn the_write_opcode_follows_the_declared_properties() {
        let decl = |props: u8| {
            let mut v = vec![props, 0x14, 0x00];
            v.extend_from_slice(&[0u8; 16]);
            parse_characteristic(0x0013, &v).expect("decodes")
        };

        // The Joy-Con 2 command channel at 0x0014: WRITE_NO_RESPONSE only.
        // Hardware answered a Write Request here with ATT 0x03 Write Not
        // Permitted, nine times over, once per init command.
        let cmd = decl(PROP_WRITE_NO_RESPONSE);
        assert_eq!(cmd.write_opcode(), Some(ATT_WRITE_COMMAND));
        assert!(!cmd.notifiable());

        // A CCCD-style acknowledged write.
        assert_eq!(decl(PROP_READ | PROP_WRITE).write_opcode(), Some(ATT_WRITE_REQUEST));
        // Offering both, the acknowledged form wins: a refusal is visible.
        assert_eq!(
            decl(PROP_WRITE | PROP_WRITE_NO_RESPONSE).write_opcode(),
            Some(ATT_WRITE_REQUEST)
        );
        // Read-only and notify-only attributes are not write targets at all.
        assert_eq!(decl(PROP_READ | PROP_NOTIFY).write_opcode(), None);
    }

    #[test]
    fn a_read_response_yields_the_declaration_bytes() {
        // What a Read Request on a 0x2803 handle returns: the same value a
        // Read By Type Response would have carried, one attribute at a time.
        let mut rsp = vec![ATT_READ_RESPONSE, PROP_WRITE_NO_RESPONSE, 0x14, 0x00];
        rsp.extend_from_slice(&[0u8; 16]);
        let value = parse_read_response(&rsp).expect("decodes");
        let c = parse_characteristic(0x0013, &value).expect("decodes");
        assert_eq!(c.value_handle, 0x0014);
        assert_eq!(c.write_opcode(), Some(ATT_WRITE_COMMAND));

        assert!(parse_read_response(&[ATT_ERROR_RESPONSE, 0x0a, 0x14, 0x00, 0x02]).is_none());
    }

    #[test]
    fn the_end_of_a_walk_is_recognised_as_completion_not_failure() {
        // Error response to a Find Information Request: "attribute not found".
        assert!(is_attribute_not_found(&[
            ATT_ERROR_RESPONSE, ATT_FIND_INFORMATION_REQUEST, 0x20, 0x00,
            ATT_ERR_ATTRIBUTE_NOT_FOUND,
        ]));
        // A genuine failure — insufficient authentication — is not completion.
        assert!(!is_attribute_not_found(&[
            ATT_ERROR_RESPONSE, ATT_FIND_INFORMATION_REQUEST, 0x20, 0x00, 0x05,
        ]));
    }

    #[test]
    fn truncated_acl_is_rejected_rather_than_decoded() {
        let acl = encode_acl(0x0040, CID_ATT, &[0x52, 0x16, 0x00, 0xff]);
        for cut in 1..acl.len() {
            assert!(
                parse_acl(&acl[..cut]).is_none(),
                "a {cut}-byte prefix must not decode"
            );
        }
        assert!(parse_acl(&acl).is_some());
    }
}

#[cfg(test)]
mod transfer_tests {
    use super::*;

    /// One ACL packet, as the controller sends a motion report.
    fn packet(handle: u16, pb: u16, att: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&((handle & 0x0FFF) | (pb << 12)).to_le_bytes());
        v.extend_from_slice(&((att.len() + 4) as u16).to_le_bytes());
        v.extend_from_slice(&(att.len() as u16).to_le_bytes());
        v.extend_from_slice(&CID_ATT.to_le_bytes());
        v.extend_from_slice(att);
        v
    }

    /// ⛔ A transfer carrying several packets must yield all of them.
    ///
    /// USB bulk does not guarantee one packet per transfer, and the dongle
    /// coalesces whenever the host is slow to poll — which is precisely when a
    /// game is running. Keeping only the first silently dropped a share of
    /// every controller's reports, with no error and no counter, and got worse
    /// exactly when it mattered most.
    #[test]
    fn every_packet_in_a_transfer_is_recovered() {
        let mut buf = Vec::new();
        buf.extend(packet(0x0040, 0b00, &[0x1b, 0x0e, 0x00, 1, 2, 3]));
        buf.extend(packet(0x0041, 0b00, &[0x1b, 0x0e, 0x00, 4, 5, 6]));
        buf.extend(packet(0x0040, 0b00, &[0x1b, 0x0e, 0x00, 7, 8, 9]));

        let got = split_acl(&buf);
        assert_eq!(got.len(), 3, "only {} of 3 packets recovered", got.len());
        assert_eq!(got[0].conn_handle, 0x0040);
        assert_eq!(got[1].conn_handle, 0x0041, "the second link's report was lost");
        assert_eq!(got[2].payload, vec![0x1b, 0x0e, 0x00, 7, 8, 9]);
    }

    /// A truncated tail is dropped, and must not take the good packets with it.
    #[test]
    fn a_truncated_tail_costs_only_itself() {
        let mut buf = packet(0x0040, 0b00, &[0x1b, 0x0e, 0x00, 1, 2, 3]);
        let partial = packet(0x0040, 0b00, &[0x1b, 0x0e, 0x00, 4, 5, 6]);
        buf.extend_from_slice(&partial[..partial.len() - 3]);
        let got = split_acl(&buf);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].payload, vec![0x1b, 0x0e, 0x00, 1, 2, 3]);
    }

    /// ⛔ A CONTINUATION fragment must be refused, not decoded as a new PDU.
    ///
    /// It carries no L2CAP header, so decoding it reads two payload bytes as a
    /// length and two more as a channel id. When those happen to pass the
    /// sanity checks — and payload bytes eventually will — the result is an ATT
    /// PDU assembled from the middle of an IMU sample: an accelerometer vector
    /// of roughly the right magnitude pointing the wrong way, which the
    /// orientation tracker then LATCHES as its gravity reference.
    #[test]
    fn a_continuation_fragment_is_refused() {
        // Bytes chosen so the fragment WOULD parse if the flag were ignored.
        let frag = packet(0x0040, 0b01, &[0x1b, 0x0e, 0x00, 1, 2, 3]);
        assert!(
            parse_acl(&frag).is_none(),
            "a continuation fragment was decoded as a fresh L2CAP packet",
        );
        // The identical bytes with a FIRST flag are still accepted, so the
        // check is on the flag and not on something incidental.
        let first = packet(0x0040, 0b00, &[0x1b, 0x0e, 0x00, 1, 2, 3]);
        assert!(parse_acl(&first).is_some());
    }
}
