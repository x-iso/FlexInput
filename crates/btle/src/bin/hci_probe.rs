//! Go/no-go probe for the dedicated-dongle route.
//!
//! Opens a WinUSB-bound Bluetooth dongle, sends `HCI_Reset`, and waits for the
//! matching `Command Complete`. That single round-trip is the whole question:
//! if it succeeds, FlexInput can drive the radio itself and everything above
//! the transport is ordinary engineering. If it fails, the route is dead before
//! any effort goes into a GATT client.
//!
//! Then reads Local Version Information, because knowing the controller's LE
//! feature level costs one more command and decides what is possible later.
//!
//! Usage (defaults to the Realtek dongle this was developed against):
//!   cargo run -p flexinput-btle --bin hci_probe
//!   cargo run -p flexinput-btle --bin hci_probe -- 0bda a728

use flexinput_btle::{hci::Opcode, Dongle};

/// Realtek RTL8761/8852-class BT 5.4 dongle.
const DEFAULT_VID: u16 = 0x0BDA;
const DEFAULT_PID: u16 = 0xA728;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (vid, pid) = match args.len() {
        0 => (DEFAULT_VID, DEFAULT_PID),
        2 => (
            u16::from_str_radix(args[0].trim_start_matches("0x"), 16).expect("vid must be hex"),
            u16::from_str_radix(args[1].trim_start_matches("0x"), 16).expect("pid must be hex"),
        ),
        _ => {
            eprintln!("usage: hci_probe [<vid hex> <pid hex>]");
            std::process::exit(2);
        }
    };

    println!("[probe] opening {vid:04x}:{pid:04x}");
    let dongle = match Dongle::open(vid, pid) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[probe] FAILED to open: {e}");
            eprintln!("[probe] if this says access denied or not found, the dongle is probably");
            eprintln!("[probe] still bound to the Bluetooth driver rather than WinUSB.");
            std::process::exit(1);
        }
    };
    println!("[probe] claimed interface 0, events on endpoint {:#04x}", dongle.event_endpoint());

    // Reset AND unmask events. The mask matters: after a reset the LE Meta
    // event is disabled by default, so advertising reports never arrive and a
    // scan silently finds nothing.
    match dongle.reset_and_init() {
        Ok(()) => println!("[probe] HCI_Reset + event masks -> SUCCESS"),
        Err(e) => {
            eprintln!("[probe] controller init FAILED: {e}");
            std::process::exit(1);
        }
    }

    match dongle.command_sync(Opcode::READ_LOCAL_VERSION, &[]) {
        Ok(cc) if cc.succeeded() && cc.params.len() >= 9 => {
            // Return parameters, in order:
            //   0      status
            //   1      HCI_Version
            //   2..4   HCI_Revision
            //   4      LMP_Version
            //   5..7   Manufacturer_Name   <- SIG company id
            //   7..9   LMP_Subversion
            // Reading the manufacturer at 4..6 instead reported 23818 for a
            // Realtek dongle; 23818 = 0x5D0A, i.e. LMP version 0x0A with 0x5D
            // (93 = Realtek) shifted in behind it. The wrong answer looked
            // plausible enough to print, which is exactly why it is spelled out.
            let p = &cc.params;
            let hci_version = p[1];
            let lmp_version = p[4];
            let manufacturer = u16::from_le_bytes([p[5], p[6]]);
            println!(
                "[probe] local version: HCI {hci_version} ({}), LMP {lmp_version} ({}), manufacturer {manufacturer} ({})",
                bluetooth_version_name(hci_version),
                bluetooth_version_name(lmp_version),
                company_name(manufacturer),
            );
        }
        Ok(cc) => println!("[probe] local version: unexpected reply {:02x?}", cc.params),
        Err(e) => println!("[probe] local version failed: {e}"),
    }

    println!("[probe] transport works — the dongle route is viable.");
    scan(&dongle);
}

/// Nintendo's Bluetooth SIG company id, as it appears in advertisements.
const NINTENDO_COMPANY_ID: u16 = 0x0553;

/// Scan briefly and report what the dongle sees, calling out Nintendo devices.
///
/// This is the step-3 check: proving our own stack can *find* a Joy-Con 2 is
/// what makes connecting to it worth writing.
fn scan(dongle: &Dongle) {
    println!("[probe] scanning for 10s — wake a Joy-Con so it advertises");
    if let Err(e) = dongle.start_le_scan() {
        eprintln!("[probe] scan failed to start: {e}");
        return;
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut seen: std::collections::HashSet<[u8; 6]> = std::collections::HashSet::new();
    let mut nintendo = 0usize;

    while std::time::Instant::now() < deadline {
        match dongle.read_event() {
            Ok(Some(flexinput_btle::Event::LeAdvertisingReport(r))) => {
                if !seen.insert(r.address) {
                    continue; // one line per device, not per advertisement
                }
                let addr: Vec<String> = r.address.iter().map(|b| format!("{b:02x}")).collect();
                match r.manufacturer_data() {
                    Some(md) if md.len() >= 9
                        && u16::from_le_bytes([md[0], md[1]]) == NINTENDO_COMPANY_ID =>
                    {
                        nintendo += 1;
                        println!(
                            "[probe] *** NINTENDO {} rssi={} vid={:04x} pid={:04x}",
                            addr.join(":"),
                            r.rssi,
                            u16::from_le_bytes([md[5], md[6]]),
                            u16::from_le_bytes([md[7], md[8]]),
                        );
                    }
                    _ => println!("[probe]     device {} rssi={}", addr.join(":"), r.rssi),
                }
            }
            Ok(_) => continue,
            Err(e) => {
                eprintln!("[probe] read error during scan: {e}");
                break;
            }
        }
    }

    let _ = dongle.stop_le_scan();
    println!(
        "[probe] scan done: {} device(s), {nintendo} Nintendo",
        seen.len()
    );
}

/// HCI version byte → the Bluetooth release it corresponds to.
///
/// Worth printing because LE features gate on it: extended advertising needs
/// 5.0+, and this route depends on LE support being present at all.
fn company_name(id: u16) -> &'static str {
    match id {
        2 => "Intel",
        10 => "Cambridge Silicon Radio",
        15 => "Broadcom",
        29 => "Qualcomm",
        70 => "MediaTek",
        93 => "Realtek",
        _ => "unknown",
    }
}

/// HCI version byte → the Bluetooth release it corresponds to.
fn bluetooth_version_name(v: u8) -> &'static str {
    match v {
        6 => "4.0",
        7 => "4.1",
        8 => "4.2",
        9 => "5.0",
        10 => "5.1",
        11 => "5.2",
        12 => "5.3",
        13 => "5.4",
        _ => "unknown",
    }
}
