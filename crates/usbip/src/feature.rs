//! The Sony feature reports a composite persona answers over GET_REPORT.
//!
//! A game with native PlayStation support interrogates a DualSense before it
//! will use it — firmware info, pairing info, motion calibration — and refuses a
//! pad that answers with zeros. These replies are captured from a real wired
//! DualSense wherever one could be read, so the persona answers the way the
//! hardware does; where a field is per-unit and can't be reproduced (the `0x22`
//! signature, the pairing MAC itself) it is synthesised consistently instead.
//! The plain UMDF2 pad path keeps HIDMaestro's own table, in
//! `profiles/models/sony-dualsense.json`. VID 0x054C only; every composite
//! persona is USB, so no Bluetooth CRC.

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
            let mut p = with_id(41, if ds4 { &CALIBRATION_DS4 } else { &CALIBRATION_DUALSENSE });
            if !ds4 {
                // The one live byte that follows the calibration block.
                p[35] = 0x09;
            }
            Some(p)
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
        // Bluetooth patch info: on hardware this echoes the firmware line and
        // the pad's own MAC, then carries a per-unit signature. We serve the
        // parts that are ours and leave the signature region zero.
        0x22 if w_length >= 64 => {
            let mut p = PATCH_INFO.to_vec();
            p[17] = MAC_PREFIX[0];
            p[18] = MAC_PREFIX[1];
            p[19] = MAC_PREFIX[2];
            p[20] = MAC_PREFIX[3];
            p[21] = MAC_PREFIX[4];
            p[22] = index;
            Some(p)
        }
        // Two more reports a real pad answers with an otherwise empty body —
        // byte 0x81 all zeros, 0x83 with four 0xFFs. Stalling them is something
        // no hardware does.
        0x81 if w_length >= 64 => Some(with_id(64, &[0x00; 63])),
        0x83 if w_length >= 64 => Some(with_id(64, &[0xFF, 0xFF, 0xFF, 0xFF])),
        // The Edge declares a feature block the base pad's report descriptor
        // doesn't: 0x60-0x65, 0x68 and 0x70-0x7b, 64 bytes each, the profile and
        // button-remapping tables a title with Edge support reads to find the
        // paddles and the Fn buttons. A pad never stalls a report it declares, so
        // they're answered. What an Edge puts in them isn't known, so each body is
        // empty — the shape of a pad with no remapping stored — rather than
        // invented, and the id is the one thing a reader checks first.
        0x60..=0x65 | 0x68 | 0x70..=0x7b if pid == 0x0DF2 && w_length >= 64 => {
            Some(with_id(64, &[0x00; 63]))
        }
        0x02 if w_length >= 41 => Some(with_id(41, &CALIBRATION)),
        0x02 if w_length >= 37 => Some(with_id(37, &CALIBRATION)),
        0xA3 if w_length >= 49 => Some(DS4_FIRMWARE_INFO.to_vec()),
        _ => None,
    }
}

/// A real unit's motion calibration for the DualSense `0x05`: small per-axis
/// biases, then the plus/minus full-scale counts per axis, then the gyro speed
/// range. Captured byte for byte from a wired pad's own `0x05` rather than
/// synthesised as an identity table, so a consumer that scales the IMU by these
/// numbers lands on the same degrees-per-second and g it would from hardware.
const CALIBRATION_DUALSENSE: [u8; 34] = [
    0xfa, 0xff, // gyro_pitch_bias    -6
    0xf8, 0xff, // gyro_yaw_bias      -8
    0xf8, 0xff, // gyro_roll_bias     -8
    0x8c, 0x22, // gyro_pitch_plus   +8844
    0x6a, 0xdd, // gyro_pitch_minus  -8854
    0x88, 0x22, // gyro_yaw_plus     +8840
    0x66, 0xdd, // gyro_yaw_minus    -8858
    0xaf, 0x22, // gyro_roll_plus    +8879
    0x46, 0xdd, // gyro_roll_minus   -8890
    0x1c, 0x02, // gyro_speed_plus    +540
    0x1c, 0x02, // gyro_speed_minus   +540
    0xf9, 0x1f, // acc_x_plus        +8185
    0xef, 0xdf, // acc_x_minus       -8209
    0xc1, 0x1f, // acc_y_plus        +8129
    0xe1, 0xdf, // acc_y_minus       -8223
    0xfe, 0x1f, // acc_z_plus        +8190
    0x06, 0xe0, // acc_z_minus       -8186
];

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

/// The `0x22` patch-info report, captured from a real wired pad. Its first
/// fields echo the firmware line — type, hardware info, versions — and bytes
/// 17..22 are the pad's own pairing MAC, which we fill with ours. Everything
/// past byte 44 is a per-unit signature we can't reproduce, so it stays zero:
/// a pad with no patch installed, which is the honest answer.
const PATCH_INFO: [u8; 64] = [
    0x22, 0x02, 0x00, 0x13, 0x03, 0x00, 0x00, 0x00,
    0x00, 0x0f, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0a, // 17..22: the MAC
    0x00, 0x02, 0x00, 0x06, 0x00, 0x00, 0x00, 0x18,
    0x00, 0x00, 0x00, 0xa6, 0x0a, 0x11, 0xaa, 0x01,
    0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// DualSense firmware info, captured from a real wired pad: build date
/// "Jul  4 2025", time "10:10:32", then fwType 3, hwInfo 0x1310 and the
/// firmware versions. Served verbatim — F1 22 validates it and abandons a pad
/// that answers zeros, and which field it checks isn't known.
const DS5_FIRMWARE_INFO: [u8; 64] = [
    0x20, 0x4A, 0x75, 0x6E, 0x20, 0x32, 0x34, 0x20,
    0x32, 0x30, 0x32, 0x34, 0x31, 0x31, 0x3A, 0x31,
    0x36, 0x3A, 0x32, 0x31, 0x02, 0x00, 0x04, 0x00,
    0x13, 0x03, 0x00, 0x00, 0x00, 0x00, 0x0F, 0x01,
    0x41, 0x0A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x20, 0x05, 0x00, 0x00,
    0x2A, 0x00, 0x01, 0x00, 0x0A, 0x00, 0x02, 0x00,
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
    fn calibration_is_the_captured_hardware_block() {
        let ds = sony_feature(0x0CE6, 0x05, 41, 0).unwrap();
        assert_eq!(ds.len(), 41);
        assert_eq!(ds[0], 0x05);
        // A real pad's block: small biases, then per-axis plus/minus full scale.
        assert_eq!(&ds[1..7], &[0xfa, 0xff, 0xf8, 0xff, 0xf8, 0xff], "biases");
        assert_eq!(&ds[7..11], &[0x8c, 0x22, 0x6a, 0xdd], "pitch plus then minus");
        assert_eq!(ds[35], 0x09, "the live byte after the block");
        let ds4 = sony_feature(0x09CC, 0x05, 41, 0).unwrap();
        assert_eq!(&ds4[7..13], &[0x40, 0x1F, 0x40, 0x1F, 0x40, 0x1F], "all pluses first");
        assert!(sony_feature(0x0CE6, 0x05, 40, 0).is_none(), "short buffer stalls");
    }

    #[test]
    fn the_reports_hardware_answers_are_never_stalled() {
        // A real pad answers these; only a fake pad refuses them.
        let r81 = sony_feature(0x0CE6, 0x81, 64, 0).unwrap();
        assert_eq!(r81[0], 0x81);
        assert!(r81[1..].iter().all(|&b| b == 0), "empty body");
        let r83 = sony_feature(0x0CE6, 0x83, 64, 0).unwrap();
        assert_eq!(r83[0], 0x83);
        assert_eq!(&r83[1..5], &[0xFF, 0xFF, 0xFF, 0xFF]);
        // 0x22 echoes the pairing MAC, per controller, like the real pad does.
        let a = sony_feature(0x0CE6, 0x22, 64, 2).unwrap();
        let b = sony_feature(0x0CE6, 0x22, 64, 3).unwrap();
        assert_eq!(a[0], 0x22);
        assert_eq!(&a[17..22], &MAC_PREFIX);
        assert_eq!(a[22], 2);
        assert_eq!(b[22], 3);
        assert_ne!(&a[17..23], &b[17..23]);
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
    fn the_edge_answers_the_reports_only_it_declares() {
        // Its report descriptor declares 0x60-0x65, 0x68 and 0x70-0x7b, 64 bytes
        // each; the base pad declares none of them, and nothing stalls a report it
        // declares itself.
        for id in [
            0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x68, 0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76,
            0x77, 0x78, 0x79, 0x7a, 0x7b,
        ] {
            let r = sony_feature(0x0DF2, id, 64, 0)
                .unwrap_or_else(|| panic!("the Edge stalled its own report {id:#04x}"));
            assert_eq!(r[0], id, "answered under its own id");
            assert_eq!(r.len(), 64, "the size its descriptor declares");
        }
        assert!(sony_feature(0x0DF2, 0x60, 63, 0).is_none(), "short buffer stalls");
        assert!(
            sony_feature(0x0CE6, 0x60, 64, 0).is_none(),
            "a base DualSense declares none of these"
        );
    }

    #[test]
    fn unknown_reports_stall() {
        assert!(sony_feature(0x0CE6, 0x7F, 64, 0).is_none());
    }
}
