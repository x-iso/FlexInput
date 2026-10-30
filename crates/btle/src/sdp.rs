//! SDP: asking a Classic device what it is.
//!
//! A Bluetooth HID device publishes two service records that matter here:
//!
//! - **HID** (`0x1124`): its report descriptor, attribute `0x0206`
//!   (`HIDDescriptorList`). This is the descriptor a host hands to its HID
//!   stack, and what makes a pad readable without code written for it.
//! - **Device ID** (`0x1200`): vendor, product and version (`0x0201`–`0x0203`),
//!   the same ids the pad carries over USB. This is what tells a DualSense from
//!   a DualShock 4 — both call themselves "Wireless Controller".
//!
//! ⭐ Asked once, after pairing, and kept beside the link key: the answers do
//! not change, and BlueZ keeps them the same way.
//!
//! ❗ **SDP is big-endian**, unlike every other layer this crate speaks. Its
//! PDU header, its lengths and its integers all go most significant byte first.
//!
//! ❗ **Small answers on purpose.** The ACL reader drops continuation
//! fragments (see `acl::parse_acl`), so a response must arrive in ONE baseband
//! packet. Asking for at most [`CHUNK`] bytes of attributes per response keeps
//! each under a DH3 packet; the continuation state stitches the chunks back
//! together.

use std::time::{Duration, Instant};

use crate::{l2cap, Dongle, Error, Result};

/// The SDP PSM.
pub const PSM_SDP: u16 = 0x0001;

/// Service class UUIDs.
pub const UUID_HID: u16 = 0x1124;
pub const UUID_PNP_INFO: u16 = 0x1200;

/// `HIDDescriptorList`, in the HID record.
const ATTR_HID_DESCRIPTOR_LIST: u16 = 0x0206;
/// Device ID record attributes.
const ATTR_VENDOR_ID: u16 = 0x0201;
const ATTR_PRODUCT_ID: u16 = 0x0202;
const ATTR_VERSION: u16 = 0x0203;

/// PDU ids.
const PDU_ERROR_RESPONSE: u8 = 0x01;
const PDU_SERVICE_SEARCH_ATTRIBUTE_REQUEST: u8 = 0x06;
const PDU_SERVICE_SEARCH_ATTRIBUTE_RESPONSE: u8 = 0x07;

/// The most attribute bytes asked for per response — see the module notes.
const CHUNK: u16 = 0x0080;

/// What a device said about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceRecord {
    pub vid: Option<u16>,
    pub pid: Option<u16>,
    pub version: Option<u16>,
    /// The HID report descriptor, when the device has a HID record.
    pub descriptor: Option<Vec<u8>>,
}

/// One SDP data element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Element {
    Nil,
    /// Unsigned and signed integers alike, as their big-endian bytes.
    Int(Vec<u8>),
    Uuid(Vec<u8>),
    /// Text strings, URLs and booleans: the bytes, uninterpreted. A report
    /// descriptor travels as a "text string".
    Bytes(Vec<u8>),
    /// Sequences and alternatives.
    Seq(Vec<Element>),
}

impl Element {
    /// The value of a 1-, 2- or 4-byte integer.
    pub fn uint(&self) -> Option<u32> {
        match self {
            Element::Int(b) if !b.is_empty() && b.len() <= 4 => {
                Some(b.iter().fold(0u32, |acc, x| (acc << 8) | *x as u32))
            }
            _ => None,
        }
    }
}

/// Parse one data element off the front of `buf`: the element and the bytes
/// it used. `None` for anything truncated or of an unassigned type.
pub fn parse_element(buf: &[u8]) -> Option<(Element, usize)> {
    let head = *buf.first()?;
    let (kind, size) = (head >> 3, head & 0x07);
    // Fixed sizes for types 1–3 and 5; a length prefix for 4, 6, 7 and 8.
    let (len, at) = match size {
        0..=4 if kind != 0 => (1usize << size, 1usize),
        0 => (0, 1),
        5 => (*buf.get(1)? as usize, 2),
        6 => (u16::from_be_bytes([*buf.get(1)?, *buf.get(2)?]) as usize, 3),
        7 => (u32::from_be_bytes([*buf.get(1)?, *buf.get(2)?, *buf.get(3)?, *buf.get(4)?]) as usize, 5),
        _ => return None,
    };
    let body = buf.get(at..at.checked_add(len)?)?;
    let el = match kind {
        0 => Element::Nil,
        1 | 2 => Element::Int(body.to_vec()),
        3 => Element::Uuid(body.to_vec()),
        4 | 5 | 8 => Element::Bytes(body.to_vec()),
        6 | 7 => {
            let mut items = Vec::new();
            let mut rest = body;
            while !rest.is_empty() {
                let (item, used) = parse_element(rest)?;
                items.push(item);
                rest = &rest[used..];
            }
            Element::Seq(items)
        }
        _ => return None,
    };
    Some((el, at + len))
}

/// `ServiceSearchAttributeRequest` for one service class, every attribute.
pub fn search_attribute_request(transaction: u16, uuid: u16, continuation: &[u8]) -> Vec<u8> {
    let mut params = Vec::with_capacity(16 + continuation.len());
    // ServiceSearchPattern: a sequence holding one 16-bit UUID.
    params.extend_from_slice(&[0x35, 0x03, 0x19]);
    params.extend_from_slice(&uuid.to_be_bytes());
    params.extend_from_slice(&CHUNK.to_be_bytes());
    // AttributeIDList: one 32-bit range, 0x0000–0xFFFF.
    params.extend_from_slice(&[0x35, 0x05, 0x0A, 0x00, 0x00, 0xFF, 0xFF]);
    params.push(continuation.len() as u8);
    params.extend_from_slice(continuation);
    let mut pdu = Vec::with_capacity(5 + params.len());
    pdu.push(PDU_SERVICE_SEARCH_ATTRIBUTE_REQUEST);
    pdu.extend_from_slice(&transaction.to_be_bytes());
    pdu.extend_from_slice(&(params.len() as u16).to_be_bytes());
    pdu.extend_from_slice(&params);
    pdu
}

/// One `ServiceSearchAttributeResponse`: its slice of the attribute lists and
/// the continuation state to ask for the next (empty when it was the last).
#[derive(Debug, PartialEq, Eq)]
pub struct ResponsePart {
    pub transaction: u16,
    pub lists: Vec<u8>,
    pub continuation: Vec<u8>,
}

/// Decode a response PDU. `Err` carries the device's error code for an
/// `ErrorResponse`, and 0 for anything malformed.
pub fn parse_response(pdu: &[u8]) -> std::result::Result<ResponsePart, u16> {
    if pdu.len() < 5 {
        return Err(0);
    }
    let transaction = u16::from_be_bytes([pdu[1], pdu[2]]);
    let params = pdu.get(5..5 + u16::from_be_bytes([pdu[3], pdu[4]]) as usize).ok_or(0u16)?;
    match pdu[0] {
        PDU_ERROR_RESPONSE => Err(params.get(0..2).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]))),
        PDU_SERVICE_SEARCH_ATTRIBUTE_RESPONSE => {
            let n = u16::from_be_bytes([*params.first().ok_or(0u16)?, *params.get(1).ok_or(0u16)?]) as usize;
            let lists = params.get(2..2 + n).ok_or(0u16)?.to_vec();
            let cont_len = *params.get(2 + n).ok_or(0u16)? as usize;
            let continuation = params.get(3 + n..3 + n + cont_len).ok_or(0u16)?.to_vec();
            Ok(ResponsePart { transaction, lists, continuation })
        }
        _ => Err(0),
    }
}

/// The attributes of every record in a complete attribute-lists blob, as
/// `(id, value)` pairs per record.
pub fn records(lists: &[u8]) -> Vec<Vec<(u16, Element)>> {
    let Some((Element::Seq(recs), _)) = parse_element(lists) else { return Vec::new() };
    recs.into_iter()
        .filter_map(|r| match r {
            Element::Seq(items) => Some(items),
            _ => None,
        })
        .map(|items| {
            let mut out = Vec::new();
            let mut it = items.into_iter();
            while let (Some(id), Some(value)) = (it.next(), it.next()) {
                if let Some(id) = id.uint() {
                    out.push((id as u16, value));
                }
            }
            out
        })
        .collect()
}

/// The report descriptor out of a HID record's `HIDDescriptorList`: the first
/// entry of class `0x22` (report).
pub fn hid_descriptor(record: &[(u16, Element)]) -> Option<Vec<u8>> {
    let (_, Element::Seq(list)) = record.iter().find(|(id, _)| *id == ATTR_HID_DESCRIPTOR_LIST)? else {
        return None;
    };
    list.iter().find_map(|entry| match entry {
        Element::Seq(pair) => match pair.as_slice() {
            [class, Element::Bytes(d), ..] if class.uint() == Some(0x22) => Some(d.clone()),
            _ => None,
        },
        _ => None,
    })
}

/// Vendor, product and version out of a Device ID record.
pub fn pnp_ids(record: &[(u16, Element)]) -> (Option<u16>, Option<u16>, Option<u16>) {
    let get = |want: u16| {
        record.iter().find(|(id, _)| *id == want).and_then(|(_, v)| v.uint()).map(|v| v as u16)
    };
    (get(ATTR_VENDOR_ID), get(ATTR_PRODUCT_ID), get(ATTR_VERSION))
}

impl Dongle {
    /// Ask the device on `conn_handle` for its HID and Device ID records.
    ///
    /// Opens the SDP channel as `local_cid`, asks, and closes it again. Runs
    /// under the caller's lease: it reads ACL packets itself, and anything on
    /// other channels meanwhile — input reports included — is dropped.
    ///
    /// A device with no Device ID record still answers with its descriptor;
    /// one with neither is an error, since nothing was learned.
    pub fn sdp_identify(
        &self,
        conn_handle: u16,
        local_cid: u16,
        on_event: &mut dyn FnMut(&str),
    ) -> Result<DeviceRecord> {
        let ch = self.l2cap_connect(conn_handle, PSM_SDP, local_cid, on_event)?;
        let mut rec = DeviceRecord::default();
        let mut transaction = 1u16;
        let result = (|| -> Result<()> {
            for uuid in [UUID_HID, UUID_PNP_INFO] {
                let lists = self.sdp_search(conn_handle, ch, &mut transaction, uuid)?;
                for r in records(&lists) {
                    if uuid == UUID_HID {
                        rec.descriptor = rec.descriptor.take().or_else(|| hid_descriptor(&r));
                    } else {
                        let (v, p, ver) = pnp_ids(&r);
                        rec.vid = rec.vid.or(v);
                        rec.pid = rec.pid.or(p);
                        rec.version = rec.version.or(ver);
                    }
                }
            }
            Ok(())
        })();
        // Closed either way; the reply is left to the link's own signalling
        // handler, which ignores it.
        let _ = self.send_l2cap(
            conn_handle,
            l2cap::CID_SIGNALLING,
            &l2cap::encode_signal(
                l2cap::SIG_DISCONNECTION_REQUEST,
                0x7F,
                &l2cap::disconnection_request(ch.remote_cid, ch.local_cid),
            ),
        );
        result?;
        if rec.descriptor.is_none() && rec.vid.is_none() {
            return Err(Error::Protocol("SDP: the device has no HID or Device ID record".into()));
        }
        on_event(&format!(
            "SDP: {:04x}:{:04x} v{:04x}, descriptor {} bytes",
            rec.vid.unwrap_or(0),
            rec.pid.unwrap_or(0),
            rec.version.unwrap_or(0),
            rec.descriptor.as_ref().map_or(0, Vec::len)
        ));
        Ok(rec)
    }

    /// One service search, every continuation followed: the complete
    /// attribute-lists blob.
    fn sdp_search(
        &self,
        conn_handle: u16,
        ch: l2cap::Channel,
        transaction: &mut u16,
        uuid: u16,
    ) -> Result<Vec<u8>> {
        let mut lists = Vec::new();
        let mut continuation = Vec::new();
        // A device that keeps handing back continuations is broken; a record
        // is never this big.
        for _ in 0..64 {
            *transaction = transaction.wrapping_add(1);
            let pdu = search_attribute_request(*transaction, uuid, &continuation);
            self.send_l2cap(conn_handle, ch.remote_cid, &pdu)?;
            let deadline = Instant::now() + Duration::from_secs(3);
            let part = loop {
                if Instant::now() >= deadline {
                    return Err(Error::Protocol(format!("SDP: no answer for {uuid:#06x}")));
                }
                let Some(pkt) = self.read_acl(Duration::from_millis(100))? else { continue };
                if pkt.conn_handle != conn_handle || pkt.cid != ch.local_cid {
                    continue;
                }
                match parse_response(&pkt.payload) {
                    Ok(p) if p.transaction == *transaction => break p,
                    Ok(_) => continue,
                    // 0x0002: no such service — an empty answer, not a failure.
                    Err(0x0002) => return Ok(Vec::new()),
                    Err(code) => {
                        return Err(Error::Protocol(format!("SDP error {code:#06x} for {uuid:#06x}")))
                    }
                }
            };
            lists.extend_from_slice(&part.lists);
            if part.continuation.is_empty() {
                return Ok(lists);
            }
            continuation = part.continuation;
        }
        Err(Error::Protocol(format!("SDP: {uuid:#06x} never finished")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A HID record as a DualShock 4 answers it, trimmed: the service class
    /// list, then the descriptor list carrying a 4-byte descriptor.
    fn hid_record_blob() -> Vec<u8> {
        let mut rec = vec![
            0x09, 0x00, 0x01, // attr 0x0001 ServiceClassIDList
            0x35, 0x03, 0x19, 0x11, 0x24, // { UUID 0x1124 }
            0x09, 0x02, 0x06, // attr 0x0206 HIDDescriptorList
            0x35, 0x08, // {
            0x35, 0x06, // {
            0x08, 0x22, // uint8 0x22
            0x25, 0x02, 0x05, 0x01, // text(2) 05 01
            // } }
        ];
        // The outer per-record sequence, then the list of records.
        let mut one = vec![0x35, rec.len() as u8];
        one.append(&mut rec);
        let mut all = vec![0x35, one.len() as u8];
        all.extend(one);
        all
    }

    #[test]
    fn the_request_is_big_endian_and_asks_for_every_attribute() {
        let pdu = search_attribute_request(0x0102, UUID_HID, &[]);
        assert_eq!(pdu[0], 0x06);
        assert_eq!(&pdu[1..3], &[0x01, 0x02], "transaction id, big-endian");
        let plen = u16::from_be_bytes([pdu[3], pdu[4]]) as usize;
        assert_eq!(pdu.len(), 5 + plen);
        assert_eq!(&pdu[5..10], &[0x35, 0x03, 0x19, 0x11, 0x24]);
        assert_eq!(&pdu[10..12], &CHUNK.to_be_bytes());
        assert_eq!(&pdu[12..19], &[0x35, 0x05, 0x0A, 0x00, 0x00, 0xFF, 0xFF]);
        assert_eq!(pdu[19], 0, "no continuation state");
    }

    #[test]
    fn a_continuation_is_echoed_back() {
        let pdu = search_attribute_request(1, UUID_PNP_INFO, &[0xAA, 0xBB]);
        assert_eq!(&pdu[pdu.len() - 3..], &[0x02, 0xAA, 0xBB]);
    }

    #[test]
    fn a_response_splits_into_lists_and_continuation() {
        // transaction 7, params: count 3, lists 01 02 03, continuation 1 byte 09.
        let pdu = [0x07, 0x00, 0x07, 0x00, 0x07, 0x00, 0x03, 0x01, 0x02, 0x03, 0x01, 0x09];
        let part = parse_response(&pdu).unwrap();
        assert_eq!(part.transaction, 7);
        assert_eq!(part.lists, vec![1, 2, 3]);
        assert_eq!(part.continuation, vec![9]);
    }

    #[test]
    fn an_error_response_carries_its_code() {
        let pdu = [0x01, 0x00, 0x01, 0x00, 0x02, 0x00, 0x02];
        assert_eq!(parse_response(&pdu), Err(0x0002));
    }

    #[test]
    fn the_descriptor_comes_out_of_the_hid_record() {
        let recs = records(&hid_record_blob());
        assert_eq!(recs.len(), 1);
        assert_eq!(hid_descriptor(&recs[0]), Some(vec![0x05, 0x01]));
    }

    #[test]
    fn device_ids_come_out_of_the_pnp_record() {
        let rec = vec![
            (0x0201, Element::Int(vec![0x05, 0x4C])),
            (0x0202, Element::Int(vec![0x0C, 0xE6])),
            (0x0203, Element::Int(vec![0x01, 0x00])),
        ];
        assert_eq!(pnp_ids(&rec), (Some(0x054C), Some(0x0CE6), Some(0x0100)));
    }

    /// A descriptor longer than 255 bytes travels with a 2-byte length.
    #[test]
    fn long_strings_use_the_wide_length() {
        let mut blob = vec![0x26, 0x01, 0x2C];
        blob.extend(std::iter::repeat_n(0xAB, 300));
        let (el, used) = parse_element(&blob).unwrap();
        assert_eq!(used, 303);
        assert!(matches!(el, Element::Bytes(b) if b.len() == 300));
    }

    #[test]
    fn truncated_elements_are_refused() {
        assert!(parse_element(&[0x25, 0x05, 0x01]).is_none());
        assert!(parse_element(&[0x0A, 0x00]).is_none());
    }
}
