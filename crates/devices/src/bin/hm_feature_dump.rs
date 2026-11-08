//! Diagnostic: dump GET_FEATURE responses from a PHYSICAL Sony pad (DualSense /
//! DS4) so we know exactly what feature-report data a real device answers.
//!
//! Why: virtual DS/DualSense game-authenticity (#5). Strict games validate "is
//! this a real pad?" via GET_FEATURE round-trips (calibration 0x05, pairing/MAC
//! 0x09, firmware/version 0x20, …). FlexInput's HIDMaestro virtual answers those
//! INSIDE the prebuilt driver (no SHM feature channel), so before deciding
//! whether a driver fork is worth it we need to know (a) what a real pad returns,
//! and (b) — by comparison later — whether our virtual already passes enough.
//!
//! This is a throwaway probe (like the hm_xusb_probe / hm_xinput_read bins): it
//! only reads from the physical device, never writes. Run with a real DualSense
//! or DS4 plugged in over USB:
//!
//!   cargo run -p flexinput-devices --bin hm_feature_dump
//!
//! Optional: pass report IDs (decimal or 0xHH) to dump only those, e.g.
//!   cargo run -p flexinput-devices --bin hm_feature_dump -- 0x05 0x09 0x20

use hidapi::{HidApi, HidDevice};

const SONY_VID: u16 = 0x054C;
// DS4 (gen1 0x05C4, gen2/slim+pro 0x09CC); DualSense (0x0CE6) + Edge (0x0DF2).
const DS4_PIDS: &[u16] = &[0x05C4, 0x09CC];
const DUALSENSE_PIDS: &[u16] = &[0x0CE6, 0x0DF2];

/// Feature report IDs worth probing on a Sony pad. The ones that matter for
/// authentication are called out; the rest round out the picture. A pad returns
/// an error (and we print it) for IDs it doesn't implement.
///
/// DualSense (hid-playstation / nondebug docs):
///   0x05 calibration (IMU) — the big authentication one (DS4Windows reads it)
///   0x09 pairing info / Bluetooth MAC + host MAC
///   0x20 firmware / hardware version
///   0x22, 0x80..0x83 various vendor/feature blocks
/// DS4:
///   0x02 calibration (USB), 0x12 pairing info, 0xa3 firmware date/version,
///   0x81 MAC. (0x05 is an OUTPUT report on DS4, not feature.)
const PROBE_IDS_DUALSENSE: &[u8] = &[0x05, 0x09, 0x20, 0x22, 0x80, 0x81, 0x82, 0x83];
const PROBE_IDS_DS4: &[u8] = &[0x02, 0x12, 0x81, 0xa3, 0xa4, 0x06, 0x07];

fn main() {
    let api = match HidApi::new() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("hidapi init failed: {e}");
            std::process::exit(1);
        }
    };

    // Optional CLI filter: explicit report IDs to probe instead of the defaults.
    let id_filter: Vec<u8> = std::env::args()
        .skip(1)
        .filter_map(|a| parse_id(&a))
        .collect();

    let mut found_any = false;
    for info in api.device_list() {
        if info.vendor_id() != SONY_VID {
            continue;
        }
        let pid = info.product_id();
        let is_ds4 = DS4_PIDS.contains(&pid);
        let is_ds = DUALSENSE_PIDS.contains(&pid);
        if !is_ds4 && !is_ds {
            continue;
        }
        // Accept FlexInput's own virtuals too: the HIDMaestro node carries
        // 'hidmaestro' in its instance path (root-enumerated, no VID_ segment)
        // and the composite USB/IP persona matches VID_054C normally.
        let path_lower = info.path().to_string_lossy().to_lowercase();
        let is_virtual = path_lower.contains("hidmaestro");
        // Only the gamepad usage page interface carries the feature reports we
        // want; on Windows a pad can also expose audio/other collections.
        // usage_page 0x01 (Generic Desktop) / usage 0x05 (Gamepad).
        if info.usage_page() != 0x0001 || info.usage() != 0x0005 {
            // Still note it so the user knows it was skipped (BT vs USB shape
            // differs); don't probe non-gamepad collections.
            continue;
        }

        found_any = true;
        let kind = if is_ds { "DualSense" } else { "DS4" };
        let product = info.product_string().unwrap_or("<no product string>");
        let path = info.path().to_string_lossy().to_string();

        let dev = match api.open_path(info.path()) {
            Ok(d) => d,
            Err(e) => {
                println!("\n=== {kind}  VID:{SONY_VID:04X} PID:{pid:04X}  \"{product}\" ===");
                println!("    path: {path}");
                println!("    open_path failed: {e}");
                continue;
            }
        };

        // Physical or virtual. The path can't always say: a plain HIDMaestro
        // node is root-enumerated (`HID\HIDCLASS\…`, no VID_ segment), but a
        // composite USB/IP persona is a USB device like any wired pad, its path
        // carrying VID_054C&MI_03 too. So the pairing MAC decides first, as in
        // hm_report_record: FlexInput's personas answer 02:46:58 ('F' 'X'),
        // HIDMaestro-family virtual pads 02:48:4D ('H' 'M').
        let origin = match pairing_mac_prefix(&dev, is_ds) {
            Some([0x02, 0x46, 0x58]) => "VIRTUAL (FlexInput USB persona)",
            Some([0x02, 0x48, 0x4D]) => "VIRTUAL (HIDMaestro)",
            _ if is_virtual => "VIRTUAL (HIDMaestro)",
            _ if path.to_uppercase().contains("VID_054C") => "PHYSICAL",
            _ => "VIRTUAL (other)",
        };
        println!(
            "\n=== {kind} [{origin}]  VID:{:04X} PID:{:04X}  \"{product}\" ===",
            SONY_VID, pid
        );
        println!("    path: {path}");

        let ids: Vec<u8> = if !id_filter.is_empty() {
            id_filter.clone()
        } else if is_ds {
            PROBE_IDS_DUALSENSE.to_vec()
        } else {
            PROBE_IDS_DS4.to_vec()
        };

        for &rid in &ids {
            dump_feature(&dev, rid);
        }
    }

    if !found_any {
        eprintln!(
            "No physical Sony DS4/DualSense gamepad interface found.\n\
             Plug one in over USB and re-run. (BT-only may expose a different \
             usage; this probe targets the Generic-Desktop/Gamepad collection.)"
        );
        std::process::exit(2);
    }
}

/// GET_FEATURE for one report id and hexdump the result. hidapi requires the
/// first buffer byte to be the report id; the returned length includes it.
/// The first three bytes of the pad's pairing MAC, read from its pairing
/// report: `0x09` on a DualSense, `0x12` on a DS4 over USB. `None` when the pad
/// doesn't answer.
fn pairing_mac_prefix(dev: &HidDevice, is_dualsense: bool) -> Option<[u8; 3]> {
    let (id, len) = if is_dualsense { (0x09u8, 20) } else { (0x12u8, 16) };
    let mut buf = vec![0u8; len];
    buf[0] = id;
    match dev.get_feature_report(&mut buf) {
        Ok(n) if n >= 4 => Some([buf[1], buf[2], buf[3]]),
        _ => None,
    }
}

fn dump_feature(dev: &HidDevice, report_id: u8) {
    // 64 is the max DualSense feature size; oversize is harmless (driver caps it).
    let mut buf = vec![0u8; 64];
    buf[0] = report_id;
    match dev.get_feature_report(&mut buf) {
        Ok(n) if n > 0 => {
            buf.truncate(n);
            println!("  0x{report_id:02X}  ({n} bytes):");
            print_hex(&buf, 4);
        }
        Ok(_) => println!("  0x{report_id:02X}  -> 0 bytes (empty / not supported)"),
        Err(e) => println!("  0x{report_id:02X}  -> ERROR: {e}"),
    }
}

/// Hexdump `data` indented by `indent` spaces, 16 bytes/row with an ASCII gutter.
fn print_hex(data: &[u8], indent: usize) {
    let pad = " ".repeat(indent);
    for (row, chunk) in data.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' })
            .collect();
        println!("{pad}{:04x}  {:<47}  {ascii}", row * 16, hex.join(" "));
    }
}

/// Parse a report id given as decimal ("5") or hex ("0x05" / "0xA3").
fn parse_id(s: &str) -> Option<u8> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u8::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<u8>().ok()
    }
}
