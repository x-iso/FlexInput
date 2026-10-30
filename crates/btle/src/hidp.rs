//! HIDP: the one-byte transaction header in front of everything on the two
//! HID channels (Bluetooth HID profile, "HID Transactions").
//!
//! The high nibble is the transaction, the low nibble its parameter — for the
//! report transactions, the report TYPE. Input reports arrive on the interrupt
//! channel as `DATA | INPUT` (`0xA1`); output reports go out on it as
//! `DATA | OUTPUT` (`0xA2`). Feature reports, and reads of any report, are
//! asked for on the CONTROL channel and answered there: `GET_REPORT` comes
//! back as a `DATA` of the same type or as a `HANDSHAKE` error, `SET_REPORT`
//! always as a `HANDSHAKE`.
//!
//! ❗ The device answers one control transaction at a time. A second request
//! sent before the first is answered may be dropped, so a host queues them.

/// Transaction types (high nibble).
pub const HANDSHAKE: u8 = 0x00;
pub const GET_REPORT: u8 = 0x40;
pub const SET_REPORT: u8 = 0x50;
pub const DATA: u8 = 0xA0;

/// Report types (low nibble of the report transactions).
pub const REPORT_INPUT: u8 = 0x01;
pub const REPORT_OUTPUT: u8 = 0x02;
pub const REPORT_FEATURE: u8 = 0x03;

/// `HANDSHAKE` result: success.
pub const HANDSHAKE_SUCCESSFUL: u8 = 0x00;

/// `GET_REPORT` for report `id` of `kind`. Without the size field: the device
/// answers with the whole report. Id 0 is a device that doesn't number its
/// reports, and then the id byte is left out.
pub fn get_report(kind: u8, id: u8) -> Vec<u8> {
    if id == 0 {
        vec![GET_REPORT | kind]
    } else {
        vec![GET_REPORT | kind, id]
    }
}

/// `SET_REPORT` carrying `report` (its id first, when the device numbers them).
pub fn set_report(kind: u8, report: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(1 + report.len());
    v.push(SET_REPORT | kind);
    v.extend_from_slice(report);
    v
}

/// An output report for the interrupt channel.
pub fn output(report: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(1 + report.len());
    v.push(DATA | REPORT_OUTPUT);
    v.extend_from_slice(report);
    v
}

/// What came back on the control channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlReply<'a> {
    /// A `HANDSHAKE`, with its result code (0 = success).
    Handshake(u8),
    /// A report: `DATA` of some type, the report after the header.
    Data { kind: u8, report: &'a [u8] },
}

pub fn parse_control(payload: &[u8]) -> Option<ControlReply<'_>> {
    let head = *payload.first()?;
    match head & 0xF0 {
        HANDSHAKE => Some(ControlReply::Handshake(head & 0x0F)),
        DATA => Some(ControlReply::Data { kind: head & 0x0F, report: &payload[1..] }),
        _ => None,
    }
}

/// An input report off the interrupt channel, the header stripped.
pub fn input(payload: &[u8]) -> Option<&[u8]> {
    match payload {
        [h, report @ ..] if *h == DATA | REPORT_INPUT && !report.is_empty() => Some(report),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_combine_transaction_and_type() {
        assert_eq!(get_report(REPORT_FEATURE, 0x05), vec![0x43, 0x05]);
        assert_eq!(get_report(REPORT_INPUT, 0), vec![0x41], "no id byte when reports aren't numbered");
        assert_eq!(set_report(REPORT_FEATURE, &[0x08, 0x01]), vec![0x53, 0x08, 0x01]);
        assert_eq!(output(&[0x31, 0x00]), vec![0xA2, 0x31, 0x00]);
    }

    #[test]
    fn control_replies_decode() {
        assert_eq!(parse_control(&[0x00]), Some(ControlReply::Handshake(0)));
        assert_eq!(parse_control(&[0x03]), Some(ControlReply::Handshake(3)));
        assert_eq!(
            parse_control(&[0xA3, 0x05, 0x10]),
            Some(ControlReply::Data { kind: REPORT_FEATURE, report: &[0x05, 0x10] })
        );
        assert_eq!(parse_control(&[0x15]), None);
    }

    #[test]
    fn only_input_data_is_an_input_report() {
        assert_eq!(input(&[0xA1, 0x31, 0x00]), Some(&[0x31, 0x00][..]));
        assert_eq!(input(&[0xA2, 0x31]), None);
        assert_eq!(input(&[0xA1]), None);
    }
}
