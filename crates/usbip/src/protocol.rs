//! USB/IP wire protocol: constants and codecs.
//!
//! Grounded in the peer we speak to, usbip-win2, read at 0.9.7.5 and unchanged
//! on the wire through 0.9.8.1: the 48-byte command header and 16-byte
//! isochronous packet descriptor are `include/usbip/proto.h`, the OP_*
//! handshake structs `include/usbip/proto_op.h`, the version and status codes
//! `include/usbip/consts.h`. Everything on the wire is big-endian except the
//! eight setup bytes, which keep wire order.
//!
//! Reply rules from the driver's receive path (`drivers/ude/wsk_receive.cpp`):
//! - RET_SUBMIT for a non-isochronous transfer carries `number_of_packets = -1`.
//! - A server reply's devid / direction / ep are zero (the driver reads the
//!   direction from the seqnum's low bit and never reads the other two).
//! - RET_SUBMIT payload: for IN, the transfer data (compacted for isochronous:
//!   no padding between packets, but the descriptors keep the submit's
//!   offsets) then the isochronous descriptors; for OUT isochronous, the
//!   descriptors alone; for OUT non-isochronous, nothing.
//! - RET_UNLINK status is `-ECONNRESET` when the victim was still queued, 0
//!   when its RET_SUBMIT had already been sent.

pub const VERSION: u16 = 0x0111;
pub const HEADER_SIZE: usize = 48;
pub const ISO_DESCRIPTOR_SIZE: usize = 16;
pub const BUS_ID_SIZE: usize = 32;
const DEV_PATH_MAX: usize = 256;
/// `usbip_usb_device`, shared by the import reply and each devlist row.
pub const USB_DEVICE_SIZE: usize = DEV_PATH_MAX + BUS_ID_SIZE + 4 + 4 + 4 + 2 + 2 + 2 + 6; // 312

pub const CMD_SUBMIT: u32 = 1;
pub const CMD_UNLINK: u32 = 2;
pub const RET_SUBMIT: u32 = 3;
pub const RET_UNLINK: u32 = 4;

/// `number_of_packets` of a transfer that isn't isochronous.
pub const NON_ISOCH: i32 = -1;
pub const MAX_ISO_PACKETS: i32 = 1024;

pub const OP_REQ_IMPORT: u16 = 0x8003;
pub const OP_REP_IMPORT: u16 = 0x0003;
pub const OP_REQ_DEVLIST: u16 = 0x8005;
pub const OP_REP_DEVLIST: u16 = 0x0005;

pub const ST_OK: u32 = 0;
pub const ST_DEV_BUSY: u32 = 2;
pub const ST_NODEV: u32 = 4;

/// `usb_device_speed` (`include/usbip/ch9.h`).
pub const SPEED_FULL: u32 = 2;
pub const SPEED_HIGH: u32 = 3;

// Linux errno values the driver maps to Windows statuses. Sent negated.
/// Stall: an unsupported or refused request.
pub const EPIPE: i32 = 32;
/// A successful unlink.
pub const ECONNRESET: i32 = 104;

/// One parsed command header, both the submit and the unlink shape.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Command {
    pub command: u32,
    pub seqnum: u32,
    pub devid: u32,
    /// 0 out, 1 in.
    pub direction: u32,
    /// The endpoint NUMBER, without the direction bit.
    pub ep: u32,
    // CMD_SUBMIT
    pub transfer_flags: u32,
    pub transfer_buffer_length: i32,
    pub start_frame: i32,
    pub number_of_packets: i32,
    pub interval: i32,
    pub setup: [u8; 8],
    // CMD_UNLINK
    pub unlink_seqnum: u32,
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_be_bytes());
}

impl Command {
    pub fn parse(h: &[u8; HEADER_SIZE]) -> Self {
        let mut c = Command {
            command: be32(h, 0),
            seqnum: be32(h, 4),
            devid: be32(h, 8),
            direction: be32(h, 12),
            ep: be32(h, 16),
            ..Default::default()
        };
        match c.command {
            CMD_SUBMIT => {
                c.transfer_flags = be32(h, 20);
                c.transfer_buffer_length = be32(h, 24) as i32;
                c.start_frame = be32(h, 28) as i32;
                c.number_of_packets = be32(h, 32) as i32;
                c.interval = be32(h, 36) as i32;
                c.setup.copy_from_slice(&h[40..48]);
            }
            CMD_UNLINK => c.unlink_seqnum = be32(h, 20),
            _ => {}
        }
        c
    }

    /// The header on the wire: what a client sends. The server never sends a
    /// command; tests use this to drive it.
    pub fn to_bytes(&self) -> [u8; HEADER_SIZE] {
        let mut h = [0u8; HEADER_SIZE];
        put32(&mut h, 0, self.command);
        put32(&mut h, 4, self.seqnum);
        put32(&mut h, 8, self.devid);
        put32(&mut h, 12, self.direction);
        put32(&mut h, 16, self.ep);
        match self.command {
            CMD_SUBMIT => {
                put32(&mut h, 20, self.transfer_flags);
                put32(&mut h, 24, self.transfer_buffer_length as u32);
                put32(&mut h, 28, self.start_frame as u32);
                put32(&mut h, 32, self.number_of_packets as u32);
                put32(&mut h, 36, self.interval as u32);
                h[40..48].copy_from_slice(&self.setup);
            }
            CMD_UNLINK => put32(&mut h, 20, self.unlink_seqnum),
            _ => {}
        }
        h
    }

    pub fn is_in(&self) -> bool {
        self.direction == 1
    }

    pub fn is_isoch(&self) -> bool {
        self.command == CMD_SUBMIT && self.number_of_packets != NON_ISOCH
    }
}

/// A RET_SUBMIT header.
pub fn write_ret_submit(
    h: &mut [u8],
    seqnum: u32,
    status: i32,
    actual_length: i32,
    start_frame: i32,
    number_of_packets: i32,
    error_count: i32,
) {
    h[..HEADER_SIZE].fill(0);
    put32(h, 0, RET_SUBMIT);
    put32(h, 4, seqnum);
    put32(h, 20, status as u32);
    put32(h, 24, actual_length as u32);
    put32(h, 28, start_frame as u32);
    put32(h, 32, number_of_packets as u32);
    put32(h, 36, error_count as u32);
}

/// A RET_UNLINK header.
pub fn write_ret_unlink(h: &mut [u8], seqnum: u32, status: i32) {
    h[..HEADER_SIZE].fill(0);
    put32(h, 0, RET_UNLINK);
    put32(h, 4, seqnum);
    put32(h, 20, status as u32);
}

/// One packet of an isochronous transfer, as the submit laid it out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IsoPacket {
    pub offset: u32,
    pub length: u32,
}

pub fn read_iso_descriptor(d: &[u8]) -> IsoPacket {
    IsoPacket { offset: be32(d, 0), length: be32(d, 4) }
}

pub fn write_iso_descriptor(d: &mut [u8], offset: u32, length: u32, actual_length: u32, status: u32) {
    put32(d, 0, offset);
    put32(d, 4, length);
    put32(d, 8, actual_length);
    put32(d, 12, status);
}

/// `op_common`: version, code, status. 8 bytes.
pub fn write_op_common(b: &mut [u8], code: u16, status: u32) {
    b[0..2].copy_from_slice(&VERSION.to_be_bytes());
    b[2..4].copy_from_slice(&code.to_be_bytes());
    put32(b, 4, status);
}

/// The fields of a `usbip_usb_device` block.
pub struct UsbDeviceBlock<'a> {
    pub path: &'a str,
    pub busid: &'a str,
    pub busnum: u32,
    pub devnum: u32,
    pub speed: u32,
    pub vid: u16,
    pub pid: u16,
    pub bcd_device: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    pub configuration_value: u8,
    pub num_configurations: u8,
    pub num_interfaces: u8,
}

/// Write a 312-byte `usbip_usb_device`. Strings are UTF-8, NUL-padded.
pub fn write_usb_device(b: &mut [u8], d: &UsbDeviceBlock) {
    b[..USB_DEVICE_SIZE].fill(0);
    write_fixed_str(&mut b[..DEV_PATH_MAX], d.path);
    write_fixed_str(&mut b[DEV_PATH_MAX..DEV_PATH_MAX + BUS_ID_SIZE], d.busid);
    let t = &mut b[DEV_PATH_MAX + BUS_ID_SIZE..];
    put32(t, 0, d.busnum);
    put32(t, 4, d.devnum);
    put32(t, 8, d.speed);
    t[12..14].copy_from_slice(&d.vid.to_be_bytes());
    t[14..16].copy_from_slice(&d.pid.to_be_bytes());
    t[16..18].copy_from_slice(&d.bcd_device.to_be_bytes());
    t[18] = d.device_class;
    t[19] = d.device_subclass;
    t[20] = d.device_protocol;
    t[21] = d.configuration_value;
    t[22] = d.num_configurations;
    t[23] = d.num_interfaces;
}

/// Copy `s` into a zeroed fixed field, leaving room for the terminator.
fn write_fixed_str(dst: &mut [u8], s: &str) {
    let n = s.len().min(dst.len() - 1);
    dst[..n].copy_from_slice(&s.as_bytes()[..n]);
}

/// A NUL-terminated UTF-8 field.
pub fn read_fixed_str(src: &[u8]) -> String {
    let end = src.iter().position(|&b| b == 0).unwrap_or(src.len());
    String::from_utf8_lossy(&src[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_header_round_trips() {
        let c = Command {
            command: CMD_SUBMIT,
            seqnum: 0x1234_5679,
            devid: 0x0001_0001,
            direction: 1,
            ep: 4,
            transfer_flags: 0x200,
            transfer_buffer_length: 64,
            start_frame: 0,
            number_of_packets: NON_ISOCH,
            interval: 6,
            setup: [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x12, 0x00],
            unlink_seqnum: 0,
        };
        let parsed = Command::parse(&c.to_bytes());
        assert_eq!(parsed, c);
        assert!(parsed.is_in() && !parsed.is_isoch());
    }

    #[test]
    fn header_fields_are_big_endian_and_setup_keeps_wire_order() {
        let c = Command {
            command: CMD_SUBMIT,
            seqnum: 7,
            number_of_packets: 3,
            setup: [1, 2, 3, 4, 5, 6, 7, 8],
            ..Default::default()
        };
        let b = c.to_bytes();
        assert_eq!(&b[0..4], &[0, 0, 0, 1]);
        assert_eq!(&b[4..8], &[0, 0, 0, 7]);
        assert_eq!(&b[32..36], &[0, 0, 0, 3]);
        assert_eq!(&b[40..48], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(Command::parse(&b).is_isoch());
    }

    #[test]
    fn unlink_header_carries_the_victim() {
        let c = Command { command: CMD_UNLINK, seqnum: 9, unlink_seqnum: 5, ..Default::default() };
        assert_eq!(Command::parse(&c.to_bytes()).unlink_seqnum, 5);
    }

    #[test]
    fn ret_submit_zeroes_devid_direction_and_ep() {
        let mut h = [0xFFu8; HEADER_SIZE];
        write_ret_submit(&mut h, 11, -EPIPE, 0, 0, NON_ISOCH, 0);
        assert_eq!(be32(&h, 0), RET_SUBMIT);
        assert_eq!(be32(&h, 4), 11);
        assert_eq!(&h[8..20], &[0u8; 12]);
        assert_eq!(be32(&h, 20) as i32, -EPIPE);
        assert_eq!(be32(&h, 32) as i32, NON_ISOCH);
    }

    #[test]
    fn usb_device_block_layout() {
        let mut b = [0xAAu8; USB_DEVICE_SIZE];
        write_usb_device(&mut b, &UsbDeviceBlock {
            path: "/flexinput/1-1",
            busid: "1-1",
            busnum: 1,
            devnum: 1,
            speed: SPEED_HIGH,
            vid: 0x054C,
            pid: 0x0CE6,
            bcd_device: 0x0100,
            device_class: 0,
            device_subclass: 0,
            device_protocol: 0,
            configuration_value: 1,
            num_configurations: 1,
            num_interfaces: 4,
        });
        assert_eq!(read_fixed_str(&b[..DEV_PATH_MAX]), "/flexinput/1-1");
        assert_eq!(read_fixed_str(&b[DEV_PATH_MAX..DEV_PATH_MAX + BUS_ID_SIZE]), "1-1");
        let t = &b[DEV_PATH_MAX + BUS_ID_SIZE..];
        assert_eq!(be32(t, 8), SPEED_HIGH);
        assert_eq!(&t[12..18], &[0x05, 0x4C, 0x0C, 0xE6, 0x01, 0x00]);
        assert_eq!(t[23], 4);
    }
}
