//! ACL data, L2CAP and the small slice of ATT this stack needs.
//!
//! Deliberately **not** a general GATT client. The Joy-Con's handles are
//! already known from an HCI capture taken while the Windows stack drove the
//! controller, so discovery can be skipped entirely: writing to a known handle
//! is the same operation whether or not we walked the attribute table to find
//! it. See [`crate::joycon`] for the handles themselves.
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

// ATT opcodes. Only what is actually sent or received here.
pub const ATT_WRITE_REQUEST: u8 = 0x12;
pub const ATT_WRITE_RESPONSE: u8 = 0x13;
pub const ATT_WRITE_COMMAND: u8 = 0x52;
pub const ATT_HANDLE_VALUE_NOTIFICATION: u8 = 0x1B;
pub const ATT_ERROR_RESPONSE: u8 = 0x01;
pub const ATT_EXCHANGE_MTU_REQUEST: u8 = 0x02;
pub const ATT_EXCHANGE_MTU_RESPONSE: u8 = 0x03;

/// Client Characteristic Configuration value that enables notifications.
pub const CCCD_NOTIFY: [u8; 2] = [0x01, 0x00];

/// Wrap an ATT PDU in L2CAP and HCI ACL framing, ready for the bulk endpoint.
///
/// No fragmentation: every packet this stack sends is far below the ACL
/// payload size a controller must accept, and a Joy-Con command is 49 bytes.
pub fn encode_acl(conn_handle: u16, cid: u16, payload: &[u8]) -> Vec<u8> {
    let header = (conn_handle & 0x0FFF) | (PB_FIRST << 12);
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
