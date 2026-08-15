//! Joy-Con 2 BLE protocol constants and command framing.
//!
//! Source: <https://github.com/ndeadly/switch2_controller_research>
//! (`bluetooth_interface.md`, `commands.md`, `hid_reports.md`, `descriptors.md`).
//!
//! Switch 2 controllers are Bluetooth LE but implement none of the standard
//! profiles: no HID-over-GATT, no SMP. Everything below is Nintendo's own.

use uuid::{uuid, Uuid};

pub const NINTENDO_VID: u16 = 0x057E;
/// Bluetooth SIG company identifier for Nintendo, as it appears in the
/// manufacturer-data AD field. This is the ONLY reliable way to spot a Switch 2
/// controller during discovery — the advertisement carries no service UUIDs and
/// no name a generic driver could match on.
pub const NINTENDO_MANUFACTURER_ID: u16 = 0x0553;

/// Product IDs, from `descriptors.md`. Note the ordering: **R is the lower id**.
/// Getting this backwards silently swaps every button map, so it is asserted in
/// the tests at the bottom of this file.
pub const PID_JOYCON2_R: u16 = 0x2066;
pub const PID_JOYCON2_L: u16 = 0x2067;
/// "Safe mode" ids — the controller exposes these after a failed firmware
/// update. Recognised so we can report the state instead of silently ignoring
/// the pad, but no input is available in this mode.
pub const PID_JOYCON2_R_SAFE: u16 = 0x2070;
pub const PID_JOYCON2_L_SAFE: u16 = 0x2071;

/// Which half of a Joy-Con 2 pair a peripheral is. Each half is an independent
/// BLE peripheral with its own service UUIDs, so this drives characteristic
/// selection as well as the button map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub fn from_pid(pid: u16) -> Option<Self> {
        match pid {
            PID_JOYCON2_L | PID_JOYCON2_L_SAFE => Some(Self::Left),
            PID_JOYCON2_R | PID_JOYCON2_R_SAFE => Some(Self::Right),
            _ => None,
        }
    }

    pub fn is_safe_mode(pid: u16) -> bool {
        matches!(pid, PID_JOYCON2_L_SAFE | PID_JOYCON2_R_SAFE)
    }

    /// Stable slug used to build FlexInput device ids (`jc2:joycon2_l:0`).
    pub fn slug(self) -> &'static str {
        match self {
            Self::Left => "joycon2_l",
            Self::Right => "joycon2_r",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Left => "Joy-Con 2 (L)",
            Self::Right => "Joy-Con 2 (R)",
        }
    }
}

// ── GATT attributes ───────────────────────────────────────────────────────────
//
// The research doc indexes these by handle; btleplug addresses them by UUID, so
// the handle each one corresponds to is noted for cross-referencing the
// initialisation tables in `bluetooth_interface.md`.

/// Vendor service holding a single WRITE characteristic that official software
/// pokes with `01 00` before anything else. Purpose unknown; replicated because
/// the controller may gate later steps on it.
pub const SVC_PRELUDE: Uuid = uuid!("00c5af5d-1964-4e30-8f51-1956f96bd280");
/// Handle `0x0005`, WRITE. First write of the init sequence.
pub const CHR_PRELUDE_WRITE: Uuid = uuid!("00c5af5d-1964-4e30-8f51-1956f96bd282");

/// The main vendor service. Stands in for HID-over-GATT, which these
/// controllers deliberately do not implement.
pub const SVC_MAIN: Uuid = uuid!("ab7de9be-89fe-49ad-828f-118f09df7fd0");

/// Handle `0x000a`, READ|NOTIFY. Common input report 0x05, shared by every
/// Switch 2 controller. We do not subscribe to it — report 0x07/0x08 on
/// `CHR_INPUT_*` is the richer per-controller stream and carries the same
/// buttons plus mouse and motion.
pub const CHR_INPUT_COMMON: Uuid = uuid!("ab7de9be-89fe-49ad-828f-118f09df7fd2");

/// Handle `0x000e`, READ|NOTIFY. Per-side input report (0x07 for L, 0x08 for R).
pub const CHR_INPUT_L: Uuid = uuid!("cc1bbbb5-7354-4d32-a716-a81cb241a32a");
pub const CHR_INPUT_R: Uuid = uuid!("d5a9e01e-2ffc-4cca-b20c-8b67142bf442");

/// Handle `0x0012`, WRITE-NO-RESPONSE. Rumble-only output report.
pub const CHR_RUMBLE_L: Uuid = uuid!("289326cb-a471-485d-a8f4-240c14f18241");
pub const CHR_RUMBLE_R: Uuid = uuid!("fa19b0fb-cd1f-46a7-84a1-bbb09e00c149");

/// Handle `0x0014`, WRITE-NO-RESPONSE. Plain command channel: header at offset
/// 0 with no rumble prefix. Responses arrive on [`CHR_CMD_RESP_BASIC`].
pub const CHR_CMD_BASIC: Uuid = uuid!("649d4ac9-8eb7-4e6c-af44-1ea54fe5f005");

/// Handle `0x0016`, WRITE-NO-RESPONSE. Combined rumble + command channel.
/// This is what official software uses for the whole init sequence, so we do
/// too — the controller has been observed to gate pairing on this path.
/// Commands here are prefixed with [`JC2_CMD_PREFIX_LEN`] zero bytes.
pub const CHR_RUMBLE_CMD_L: Uuid = uuid!("ce49a830-dced-48ae-931e-c8cf88aadbea");
pub const CHR_RUMBLE_CMD_R: Uuid = uuid!("65a724b3-f1e7-4a61-8078-a342376b27ff");

/// Handle `0x001a`, NOTIFY. Command responses for [`CHR_CMD_BASIC`],
/// header at offset 0.
pub const CHR_CMD_RESP_BASIC: Uuid = uuid!("c765a961-d9d8-4d36-a20a-5315b111836a");

/// Handle `0x001e`, NOTIFY. Command responses for [`CHR_RUMBLE_CMD_L`] /
/// [`CHR_RUMBLE_CMD_R`], header at [`CMD_RESP_HEADER_OFFSET`].
pub const CHR_CMD_RESP_EXT_L: Uuid = uuid!("63a3810f-aec7-474b-9010-3d52403cb996");
pub const CHR_CMD_RESP_EXT_R: Uuid = uuid!("640ca58e-0e88-410c-a7f3-426faf2b690b");

/// Handle `0x0022`, NOTIFY. Purpose unknown. Official software enables it
/// during init, so we do as well and simply discard the payloads.
pub const CHR_UNKNOWN_NOTIFY: Uuid = uuid!("d3bd69d2-841c-4241-ab15-f86f406d2a80");

/// Vendor descriptor on the input characteristic (handle `0x0010`). Official
/// software writes `85 00` here as the second-to-last init step; the research
/// doc labels it "Set Report Rate?". Writing it is what moves the controller
/// off its idle reporting cadence, so it is not optional.
pub const DSC_REPORT_RATE: Uuid = uuid!("679d5510-5a24-4dee-9557-95df80486ecb");
/// Payload for the descriptor above, copied verbatim from the captured init.
pub const REPORT_RATE_PAYLOAD: [u8; 2] = [0x85, 0x00];

impl Side {
    pub fn input_char(self) -> Uuid {
        match self {
            Self::Left => CHR_INPUT_L,
            Self::Right => CHR_INPUT_R,
        }
    }

    pub fn rumble_char(self) -> Uuid {
        match self {
            Self::Left => CHR_RUMBLE_L,
            Self::Right => CHR_RUMBLE_R,
        }
    }

    pub fn rumble_cmd_char(self) -> Uuid {
        match self {
            Self::Left => CHR_RUMBLE_CMD_L,
            Self::Right => CHR_RUMBLE_CMD_R,
        }
    }

    pub fn cmd_resp_ext_char(self) -> Uuid {
        match self {
            Self::Left => CHR_CMD_RESP_EXT_L,
            Self::Right => CHR_CMD_RESP_EXT_R,
        }
    }

    /// Report id carried by this side's input notifications.
    pub fn input_report_id(self) -> u8 {
        match self {
            Self::Left => 0x07,
            Self::Right => 0x08,
        }
    }
}

// ── Command framing ───────────────────────────────────────────────────────────

/// Byte 1 of the header: host → device.
const DIR_REQUEST: u8 = 0x91;
/// Byte 1 of the header: device → host. Used to validate responses.
pub const DIR_RESPONSE: u8 = 0x01;
/// Byte 2 of the header. `0x00` = USB, `0x01` = Bluetooth.
const TRANSPORT_BT: u8 = 0x01;

pub const CMD_HEADER_LEN: usize = 8;

/// Joy-Con 2 writes to `0x0016` are laid out as
/// `[report id 0x00][16 bytes HD rumble][command header][command data]`,
/// so a command-only write is preceded by 17 zero bytes. Pro Controller 2 and
/// the GameCube pad use different prefix lengths — this constant is Joy-Con
/// specific, which is why it lives next to the Joy-Con-only characteristics.
pub const JC2_CMD_PREFIX_LEN: usize = 0x11;

/// Responses on handle `0x001e` are prefixed with 15 bytes before the header.
pub const CMD_RESP_HEADER_OFFSET: usize = 0x0F;

// Command ids used by the initialisation sequence.
pub const CMD_READ_MEMORY: u8 = 0x02;
pub const CMD_PAIRING_EXTRA: u8 = 0x03;
pub const CMD_UNKNOWN_07: u8 = 0x07;
pub const CMD_PLAYER_LEDS: u8 = 0x09;
pub const CMD_VIBRATION: u8 = 0x0A;
pub const CMD_FEATURE_SELECT: u8 = 0x0C;
pub const CMD_UNKNOWN_10: u8 = 0x10;
pub const CMD_UNKNOWN_11: u8 = 0x11;
pub const CMD_PAIRING: u8 = 0x15;
pub const CMD_UNKNOWN_16: u8 = 0x16;

// `0x15` pairing subcommands.
pub const SUB_PAIR_EXCHANGE_ADDRS: u8 = 0x01;
pub const SUB_PAIR_CONFIRM_LTK: u8 = 0x02;
pub const SUB_PAIR_FINALISE: u8 = 0x03;
pub const SUB_PAIR_EXCHANGE_KEYS: u8 = 0x04;

// `0x0c` feature-select subcommands: 0x02 arms the flags, 0x04 confirms them.
pub const SUB_FEATURE_INIT: u8 = 0x02;
pub const SUB_FEATURE_CONFIRM: u8 = 0x04;

/// Feature flag bits (command `0x0C`). Joy-Con 2 is initialised with `0x37` by
/// official software: buttons, sticks, IMU, mouse and rumble — everything
/// except the magnetometer.
pub mod feature {
    pub const BUTTONS: u8 = 0x01;
    pub const STICKS: u8 = 0x02;
    pub const IMU: u8 = 0x04;
    pub const MOUSE: u8 = 0x10;
    pub const RUMBLE: u8 = 0x20;
    pub const MAGNETOMETER: u8 = 0x80;

    /// What official software sends to a Joy-Con 2.
    pub const JOYCON2_DEFAULT: u8 = BUTTONS | STICKS | IMU | MOUSE | RUMBLE; // 0x37

    /// `JOYCON2_DEFAULT` plus the magnetometer (0xB7).
    ///
    /// Official software does NOT set this bit, so whether a given half even
    /// has a magnetometer is an open question — Joy-Con 2 is described as
    /// 9-axis, but it is unclear whether both halves carry one or only the
    /// right. Asking for it and seeing whether new bytes come alive in the
    /// report is the cheapest way to find out; if a half has none, the extra
    /// bit is expected to be ignored rather than rejected.
    pub const JOYCON2_WITH_MAGNETOMETER: u8 = JOYCON2_DEFAULT | MAGNETOMETER;
}

/// Build an 8-byte command header followed by its data.
///
/// Layout (`commands.md`): `[cmd][dir][transport][subcmd][unknown][len][0][0]`.
pub fn command(cmd: u8, subcmd: u8, data: &[u8]) -> Vec<u8> {
    debug_assert!(data.len() <= u8::MAX as usize, "command data too long");
    let mut out = Vec::with_capacity(CMD_HEADER_LEN + data.len());
    out.extend_from_slice(&[
        cmd,
        DIR_REQUEST,
        TRANSPORT_BT,
        subcmd,
        0x00,
        data.len() as u8,
        0x00,
        0x00,
    ]);
    out.extend_from_slice(data);
    out
}

/// Subcommand for `CMD_READ_MEMORY`.
pub const SUB_READ_MEMORY: u8 = 0x04;

/// HID output report carrying host→controller commands over USB.
///
/// From `descriptors.md`: Joy-Con 2 (both halves) define output report `0x01`
/// and input report `0x05`, both 63 bytes, under Usage Page Generic Desktop /
/// Usage Game Pad — which is why Windows binds a HID gamepad for the USB
/// interface with no driver of ours involved. (Pro Controller 2 uses output
/// `0x02` and the NSO GameCube pad `0x03`; both are out of scope here.)
pub const USB_OUTPUT_REPORT_ID: u8 = 0x01;
/// HID input report carrying controller→host state over USB.
pub const USB_INPUT_REPORT_ID: u8 = 0x05;
/// Report payload length, excluding the leading report id.
pub const USB_REPORT_LEN: usize = 63;

/// `0x03/0x03` — "Enable USB HID Reports". Activates HID input over USB.
pub const SUB_USB_ENABLE_HID_REPORTS: u8 = 0x03;
/// `0x03/0x0D` — "Initialise USB".
///
/// Per `commands.md`, **required before the controller will send input reports
/// over USB**. This is why a Joy-Con on the charging grip enumerates as a
/// healthy HID gamepad and then streams absolutely nothing: the device is fine,
/// it simply has not been told to start. Its payload is the host's Bluetooth
/// address, byte-reversed — the same encoding the link-key registration uses.
pub const SUB_USB_INIT: u8 = 0x0D;

/// Payload for `0x03/0x0D`: the host BD_ADDR in reverse wire order.
pub fn usb_init_data(host: &[u8; 6]) -> Vec<u8> {
    let mut out = host.to_vec();
    out.reverse();
    out
}

/// Build a full USB HID output report for a command.
///
/// Same body as the Bluetooth framing — a 16-byte rumble region followed by the
/// 8-byte header — but byte 0 carries the HID report id instead of the zero
/// that BLE writes, and the whole thing is padded to the descriptor's fixed
/// report length. Anything shorter is rejected by the HID stack.
pub fn usb_cmd_frame(cmd: u8, subcmd: u8, data: &[u8]) -> Vec<u8> {
    let mut out = rumble_cmd_frame(cmd, subcmd, data);
    out[0] = USB_OUTPUT_REPORT_ID;
    out.resize(USB_REPORT_LEN + 1, 0);
    out
}

/// `0x03/0x01` — "Bluetooth Wake". Per `commands.md`: "Starts broadcasting
/// Bluetooth LE advertisements to wake the console when argument is nonzero."
///
/// Re-armed periodically because Windows reclaims the link on a ~30 s timer no
/// matter what we do, and a controller that is not advertising when that
/// happens cannot be recovered — Windows adds it to the filter accept list and
/// scans, but finds nothing, and our own scan loop fares no better. Keeping the
/// advertisement alive is what turns an unavoidable drop into a recoverable one.
pub const SUB_BT_WAKE_ADVERTISE: u8 = 0x01;

/// `0x03/0x02` — "Bluetooth Cancel". Per `commands.md`: "Terminates active
/// Bluetooth LE advertising, though player LEDs continue cycling indefinitely."
///
/// Sent because the controller wakes into a *search for a console* state and
/// advertises to be found. Nothing we currently send tells it that search is
/// over, so its wake window plausibly just expires — which would look exactly
/// like the observed behaviour: the pad sleeps ~28 s after connecting whether
/// or not we run a full init, and afterwards stops advertising altogether.
/// Writes no flash.
pub const SUB_BT_CANCEL_ADVERTISING: u8 = 0x02;

/// Build the data payload for a `0x02/0x04` controller-memory read.
///
/// Wire format, decoded from the captured `40 7e 00 00 00 30 01 00`
/// ("read 0x40 bytes from 0x13000"): `[size][0x7E][0][0][address u32 LE]`.
pub fn read_memory_data(size: u8, address: u32) -> Vec<u8> {
    let mut data = vec![size, 0x7E, 0x00, 0x00];
    data.extend_from_slice(&address.to_le_bytes());
    data
}

/// The controller-memory reads official software performs during Joy-Con 2
/// initialisation, as `(size, address)`.
///
/// These are not optional housekeeping. Other implementations report that
/// without the calibration read the controller streams *stub* reports — button
/// fields stuck at zero — so a connection can look healthy while carrying no
/// input. The blocks hold factory stick and IMU calibration; we currently send
/// the reads and log the replies without decoding them, which is enough to get
/// real reports flowing.
pub const JC2_INIT_MEMORY_READS: &[(u8, u32)] = &[
    (0x40, 0x013000),
    (0x40, 0x013080),
    (0x40, 0x1FC040),
    (0x10, 0x013040),
    (0x18, 0x013100),
    (0x20, 0x013060),
];

/// Wrap a command for the `0x0016` rumble+command characteristic by prefixing
/// the zeroed report-id and rumble region.
pub fn rumble_cmd_frame(cmd: u8, subcmd: u8, data: &[u8]) -> Vec<u8> {
    let body = command(cmd, subcmd, data);
    let mut out = vec![0u8; JC2_CMD_PREFIX_LEN];
    out.extend_from_slice(&body);
    out
}

/// A parsed command response header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponseHeader {
    pub cmd: u8,
    pub subcmd: u8,
    /// Byte 5. The doc calls this "Data Length/ACK"; captured responses carry
    /// `0x78` here rather than a length, so it is surfaced raw and not trusted
    /// as a length.
    pub ack: u8,
}

/// Locate and parse the response to a specific command.
///
/// `expected_offset` is where the header should be for the characteristic the
/// payload came from ([`CMD_RESP_HEADER_OFFSET`] for handle `0x001e`, 0 for
/// `0x001a`). The research doc's `0x001e` table has an off-by-one between its
/// "Unknown, size 0xE" row and the header at `0xF`, so rather than trusting the
/// offset blindly we validate there and fall back to scanning for the header.
///
/// Validation matches on the command id as well as the direction byte.
/// Direction alone is far too weak: the header's transport byte is also `0x01`
/// on Bluetooth, so a window shifted one byte late passes a direction-only
/// check and silently yields a header built from the wrong bytes.
pub fn parse_response(
    payload: &[u8],
    expected_offset: usize,
    expected_cmd: u8,
) -> Option<(ResponseHeader, &[u8])> {
    let is_header = |off: usize| -> bool {
        payload.len() >= off + CMD_HEADER_LEN
            && payload[off] == expected_cmd
            && payload[off + 1] == DIR_RESPONSE
    };

    let off = if is_header(expected_offset) {
        expected_offset
    } else {
        (0..=payload.len().saturating_sub(CMD_HEADER_LEN)).find(|&o| is_header(o))?
    };

    Some((
        ResponseHeader {
            cmd: payload[off],
            subcmd: payload[off + 3],
            ack: payload[off + 5],
        },
        &payload[off + CMD_HEADER_LEN..],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one constant most likely to be transcribed backwards, and the
    /// failure is silent (every button lands on the wrong half).
    #[test]
    fn right_joycon_has_the_lower_pid() {
        assert!(PID_JOYCON2_R < PID_JOYCON2_L);
        assert_eq!(Side::from_pid(0x2066), Some(Side::Right));
        assert_eq!(Side::from_pid(0x2067), Some(Side::Left));
        assert_eq!(Side::from_pid(0x2069), None, "Pro Controller 2 is out of scope");
    }

    #[test]
    fn safe_mode_pids_map_to_a_side_and_are_flagged() {
        assert_eq!(Side::from_pid(PID_JOYCON2_L_SAFE), Some(Side::Left));
        assert!(Side::is_safe_mode(PID_JOYCON2_L_SAFE));
        assert!(!Side::is_safe_mode(PID_JOYCON2_L));
    }

    /// Byte-for-byte against the captured init sequence in `bluetooth_interface.md`.
    #[test]
    fn command_framing_matches_captured_traffic() {
        // Set player LEDs: `09 91 01 07 00 08 00 00 | 01 00 00 00 00 00 00 00`
        assert_eq!(
            command(CMD_PLAYER_LEDS, 0x07, &[0x01, 0, 0, 0, 0, 0, 0, 0]),
            vec![0x09, 0x91, 0x01, 0x07, 0x00, 0x08, 0x00, 0x00,
                 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        );
        // Initialise feature flags 0x37: `0c 91 01 02 00 04 00 00 | 37 00 00 00`
        assert_eq!(
            command(CMD_FEATURE_SELECT, SUB_FEATURE_INIT, &[0x37, 0, 0, 0]),
            vec![0x0c, 0x91, 0x01, 0x02, 0x00, 0x04, 0x00, 0x00, 0x37, 0x00, 0x00, 0x00],
        );
        // A command with no data still carries a full header and a zero length.
        assert_eq!(
            command(CMD_UNKNOWN_07, 0x01, &[]),
            vec![0x07, 0x91, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00],
        );
    }

    #[test]
    fn usb_frame_is_report_sized_and_carries_the_bluetooth_body() {
        let frame = usb_cmd_frame(CMD_PAIRING_EXTRA, SUB_USB_INIT, &[1, 2, 3, 4, 5, 6]);
        // Report id + the descriptor's fixed 63-byte payload. A short write is
        // rejected outright by the HID stack.
        assert_eq!(frame.len(), USB_REPORT_LEN + 1);
        assert_eq!(frame[0], USB_OUTPUT_REPORT_ID);
        // The 16-byte rumble region stays zeroed; the header starts right after.
        assert!(frame[1..JC2_CMD_PREFIX_LEN].iter().all(|b| *b == 0));
        assert_eq!(
            &frame[JC2_CMD_PREFIX_LEN..JC2_CMD_PREFIX_LEN + 8],
            &[CMD_PAIRING_EXTRA, 0x91, 0x01, SUB_USB_INIT, 0x00, 0x06, 0x00, 0x00],
        );
        // Body identical to the Bluetooth framing apart from that first byte,
        // which is the whole point of sharing `rumble_cmd_frame`.
        let ble = rumble_cmd_frame(CMD_PAIRING_EXTRA, SUB_USB_INIT, &[1, 2, 3, 4, 5, 6]);
        assert_eq!(&frame[1..ble.len()], &ble[1..]);
    }

    #[test]
    fn usb_init_payload_reverses_the_host_address() {
        assert_eq!(
            usb_init_data(&[0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]),
            vec![0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa],
        );
    }

    /// Byte-for-byte against `02 91 01 04 00 08 00 00 | 40 7e 00 00 00 30 01 00`
    /// ("read 0x40 bytes from 0x13000") in the captured init sequence.
    #[test]
    fn memory_read_matches_captured_traffic() {
        assert_eq!(
            read_memory_data(0x40, 0x013000),
            vec![0x40, 0x7e, 0x00, 0x00, 0x00, 0x30, 0x01, 0x00],
        );
        assert_eq!(
            command(CMD_READ_MEMORY, SUB_READ_MEMORY, &read_memory_data(0x40, 0x013000)),
            vec![0x02, 0x91, 0x01, 0x04, 0x00, 0x08, 0x00, 0x00,
                 0x40, 0x7e, 0x00, 0x00, 0x00, 0x30, 0x01, 0x00],
        );
        // The odd one out in the list, addressed high in flash.
        assert_eq!(
            read_memory_data(0x40, 0x1FC040),
            vec![0x40, 0x7e, 0x00, 0x00, 0x40, 0xc0, 0x1f, 0x00],
        );
    }

    #[test]
    fn joycon2_default_features_are_0x37() {
        assert_eq!(feature::JOYCON2_DEFAULT, 0x37);
    }

    #[test]
    fn rumble_cmd_frame_prefixes_seventeen_zero_bytes() {
        let f = rumble_cmd_frame(CMD_UNKNOWN_07, 0x01, &[]);
        assert_eq!(&f[..JC2_CMD_PREFIX_LEN], &[0u8; JC2_CMD_PREFIX_LEN]);
        assert_eq!(&f[JC2_CMD_PREFIX_LEN..], &[0x07, 0x91, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn parse_response_reads_the_extended_offset() {
        // `15 01 01 04 10 78 00 00 | 01 5c f6 …` at offset 0x0F.
        let mut payload = vec![0u8; CMD_RESP_HEADER_OFFSET];
        payload.extend_from_slice(&[0x15, 0x01, 0x01, 0x04, 0x10, 0x78, 0x00, 0x00]);
        payload.extend_from_slice(&[0x01, 0x5c, 0xf6, 0xee]);

        let (hdr, data) =
            parse_response(&payload, CMD_RESP_HEADER_OFFSET, CMD_PAIRING).expect("header");
        assert_eq!(hdr.cmd, CMD_PAIRING);
        assert_eq!(hdr.subcmd, SUB_PAIR_EXCHANGE_KEYS);
        assert_eq!(hdr.ack, 0x78);
        assert_eq!(data, &[0x01, 0x5c, 0xf6, 0xee]);
    }

    /// The fallback scan is the whole reason a real capture that disagrees with
    /// the doc by a byte doesn't take the pairing flow down.
    #[test]
    fn parse_response_falls_back_to_scanning_when_the_offset_is_wrong() {
        let mut payload = vec![0u8; CMD_RESP_HEADER_OFFSET - 1];
        payload.extend_from_slice(&[0x09, 0x01, 0x01, 0x07, 0x10, 0x78, 0x00, 0x00, 0xAA]);

        let (hdr, data) =
            parse_response(&payload, CMD_RESP_HEADER_OFFSET, CMD_PLAYER_LEDS).expect("header");
        assert_eq!(hdr.cmd, CMD_PLAYER_LEDS);
        assert_eq!(hdr.subcmd, 0x07);
        assert_eq!(data, &[0xAA]);
    }

    /// Regression: the header's transport byte is `0x01` on Bluetooth, exactly
    /// like the direction byte. A validator that only checks direction accepts
    /// the window starting one byte late and returns a header assembled from
    /// the wrong fields — which is precisely what this payload triggers.
    #[test]
    fn parse_response_does_not_lock_onto_a_one_byte_late_window() {
        let mut payload = vec![0u8; CMD_RESP_HEADER_OFFSET - 1];
        payload.extend_from_slice(&[0x09, 0x01, 0x01, 0x07, 0x10, 0x78, 0x00, 0x00, 0xAA]);

        let (hdr, _) =
            parse_response(&payload, CMD_RESP_HEADER_OFFSET, CMD_PLAYER_LEDS).expect("header");
        assert_eq!(hdr.cmd, CMD_PLAYER_LEDS, "must not read the transport byte as the cmd id");
        assert_ne!(hdr.cmd, DIR_RESPONSE);
    }

    #[test]
    fn parse_response_rejects_a_payload_with_no_header() {
        assert!(parse_response(&[0u8; 40], 0, CMD_PLAYER_LEDS).is_none());
    }

    /// A response for a different command must not be mistaken for ours, or a
    /// pairing step would consume the previous step's late reply.
    #[test]
    fn parse_response_ignores_another_commands_reply() {
        let mut payload = vec![0u8; CMD_RESP_HEADER_OFFSET];
        payload.extend_from_slice(&[CMD_VIBRATION, 0x01, 0x01, 0x02, 0x10, 0x78, 0x00, 0x00]);
        assert!(parse_response(&payload, CMD_RESP_HEADER_OFFSET, CMD_PAIRING).is_none());
        assert!(parse_response(&payload, CMD_RESP_HEADER_OFFSET, CMD_VIBRATION).is_some());
    }
}
