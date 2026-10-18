//! The Sony feature reports a composite persona answers over GET_REPORT.
//!
//! A game with native PlayStation support interrogates a DualSense before it
//! will use it — firmware info, pairing info, then motion calibration — and
//! refuses a pad that answers with zeros. These are byte for byte the replies
//! HIDMaestro's UMDF driver (`driver.c`) and its usbip backend serve, so a
//! composite persona reads the same as FlexInput's plain virtual pads.
//! VID 0x054C only; every composite persona is USB, so no Bluetooth CRC.

/// The pairing MAC's fixed prefix: locally administered, then 'F' 'X'. Not
/// HIDMaestro's 'H' 'M', so a composite never shares a MAC with a plain
/// virtual pad of the same index.
const MAC_PREFIX: [u8; 5] = [0x02, 0x46, 0x58, 0x00, 0x00];

/// The answer to a feature GET_REPORT, or `None` to stall it. `index` makes
/// the pairing MAC unique per controller.
pub fn sony_feature(pid: u16, report_id: u8, w_length: u16, index: u8) -> Option<Vec<u8>> {
    let w_length = w_length as usize;
    let with_id = |len: usize, body: &[u8]| {
        let mut p = vec![0u8; len];
        p[0] = report_id;
        p[1..1 + body.len()].copy_from_slice(body);
        p
    };
    match report_id {
        // Motion calibration. A DS4 orders its gyro pairs differently.
        0x05 if w_length >= 41 => {
            let ds4 = matches!(pid, 0x05C4 | 0x09CC | 0x0BA0);
            Some(with_id(41, if ds4 { &CALIBRATION_DS4 } else { &CALIBRATION }))
        }
        // DualSense pairing info: the MAC at bytes 1..6.
        0x09 if w_length >= 20 => {
            let mut p = with_id(20, &MAC_PREFIX);
            p[6] = index;
            Some(p)
        }
        // Firmware info. The Edge is a different firmware line: Sony's updater
        // data records the base pad as type 0x0004 and the Edge as 0x0044.
        0x20 if w_length >= 64 => {
            let mut p = DS5_FIRMWARE_INFO.to_vec();
            if pid == 0x0DF2 {
                p[22] = 0x44; // swSeries      0x0044
                p[23] = 0x00;
                p[44] = 0x17; // updateVersion 0x0217
                p[45] = 0x02;
            }
            Some(p)
        }
        // DS4 pairing info over USB, the DS4's 0x09.
        0x12 if w_length >= 16 => {
            let mut p = with_id(16, &MAC_PREFIX);
            p[6] = index;
            Some(p)
        }
        // Bluetooth patch info: zero past the id is a pad carrying no patch.
        0x22 if w_length >= 64 => Some(with_id(64, &[])),
        0x02 if w_length >= 41 => Some(with_id(41, &CALIBRATION)),
        0x02 if w_length >= 37 => Some(with_id(37, &CALIBRATION)),
        0xA3 if w_length >= 49 => Some(DS4_FIRMWARE_INFO.to_vec()),
        _ => None,
    }
}

/// Identity calibration for the pad's nominal units (8192 per g, 16 per
/// degree/second), written after the report id.
const CALIBRATION: [u8; 34] = [
    0x00, 0x00, // gyro_pitch_bias
    0x00, 0x00, // gyro_yaw_bias
    0x00, 0x00, // gyro_roll_bias
    0x40, 0x1F, // gyro_pitch_plus    +8000
    0xC0, 0xE0, // gyro_pitch_minus   -8000
    0x40, 0x1F, // gyro_yaw_plus      +8000
    0xC0, 0xE0, // gyro_yaw_minus     -8000
    0x40, 0x1F, // gyro_roll_plus     +8000
    0xC0, 0xE0, // gyro_roll_minus    -8000
    0xF4, 0x01, // gyro_speed_plus     +500
    0xF4, 0x01, // gyro_speed_minus    +500
    0x00, 0x20, // acc_x_plus         +8192
    0x00, 0xE0, // acc_x_minus        -8192
    0x00, 0x20, // acc_y_plus         +8192
    0x00, 0xE0, // acc_y_minus        -8192
    0x00, 0x20, // acc_z_plus         +8192
    0x00, 0xE0, // acc_z_minus        -8192
];

/// The same calibration in a DualShock 4's order: pitch+ yaw+ roll+, then the
/// minuses.
const CALIBRATION_DS4: [u8; 34] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x40, 0x1F, 0x40, 0x1F, 0x40, 0x1F,
    0xC0, 0xE0, 0xC0, 0xE0, 0xC0, 0xE0,
    0xF4, 0x01, 0xF4, 0x01,
    0x00, 0x20, 0x00, 0xE0, 0x00, 0x20, 0x00, 0xE0, 0x00, 0x20, 0x00, 0xE0,
];

/// DualSense firmware info, captured from a real wired pad: build date
/// "Jul  4 2025", time "10:10:32", then fwType 3, hwInfo 0x1310 and the
/// firmware versions. Served verbatim — F1 22 validates it and abandons a pad
/// that answers zeros, and which field it checks isn't known.
const DS5_FIRMWARE_INFO: [u8; 64] = [
    0x20, 0x4A, 0x75, 0x6C, 0x20, 0x20, 0x34, 0x20,
    0x32, 0x30, 0x32, 0x35, 0x31, 0x30, 0x3A, 0x31,
    0x30, 0x3A, 0x33, 0x32, 0x03, 0x00, 0x04, 0x00,
    0x10, 0x13, 0x00, 0x00, 0x2A, 0x00, 0x10, 0x01,
    0x01, 0xC8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x30, 0x06, 0x00, 0x00,
    0x3C, 0x00, 0x01, 0x00, 0x0A, 0x00, 0x02, 0x00,
    0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// DualShock 4 firmware / hardware info (from WinUHid's WinUHidPS4.cpp).
const DS4_FIRMWARE_INFO: [u8; 49] = [
    0xA3, 0x41, 0x75, 0x67, 0x20, 0x20, 0x33, 0x20,
    0x32, 0x30, 0x31, 0x33, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x30, 0x37, 0x3A, 0x30, 0x31, 0x3A, 0x31,
    0x32, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x01, 0x00, 0x31, 0x03, 0x00, 0x00,
    0x00, 0x49, 0x00, 0x05, 0x00, 0x00, 0x80, 0x03,
    0x00,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibration_is_the_identity_for_both_orders() {
        let ds = sony_feature(0x0CE6, 0x05, 41, 0).unwrap();
        assert_eq!(ds.len(), 41);
        assert_eq!(ds[0], 0x05);
        assert_eq!(&ds[7..11], &[0x40, 0x1F, 0xC0, 0xE0], "pitch plus then minus");
        let ds4 = sony_feature(0x09CC, 0x05, 41, 0).unwrap();
        assert_eq!(&ds4[7..13], &[0x40, 0x1F, 0x40, 0x1F, 0x40, 0x1F], "all pluses first");
        assert!(sony_feature(0x0CE6, 0x05, 40, 0).is_none(), "short buffer stalls");
    }

    #[test]
    fn pairing_mac_is_per_controller() {
        let a = sony_feature(0x0CE6, 0x09, 20, 0).unwrap();
        let b = sony_feature(0x0CE6, 0x09, 20, 1).unwrap();
        assert_eq!(&a[1..6], &MAC_PREFIX);
        assert_ne!(a[6], b[6]);
    }

    #[test]
    fn edge_firmware_info_names_its_own_line() {
        let ds = sony_feature(0x0CE6, 0x20, 64, 0).unwrap();
        let edge = sony_feature(0x0DF2, 0x20, 64, 0).unwrap();
        assert_eq!(&ds[22..24], &[0x04, 0x00]);
        assert_eq!(&edge[22..24], &[0x44, 0x00]);
        assert_eq!(&edge[44..46], &[0x17, 0x02]);
        assert_eq!(ds[0], 0x20);
    }

    #[test]
    fn unknown_reports_stall() {
        assert!(sony_feature(0x0CE6, 0x7F, 64, 0).is_none());
    }
}
