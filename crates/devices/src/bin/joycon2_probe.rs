//! Diagnostic: dump the USB-HID interface of a Joy-Con 2 (or a third-party
//! clone such as the Mobapad M12-S) so we can map its report layout before
//! committing to a FlexInput backend design.
//!
//! Why: Joy-Con 2 is two different devices depending on transport.
//!   * Over BLE it speaks a CUSTOM vendor GATT service
//!     (`ab7de9be-89fe-49ad-828f-118f09df7fd0`), NOT HID-over-GATT `0x1812`.
//!     Windows bonds it, finds no service it can bind a driver to, and drops
//!     the link — so `hidapi` can never see it wirelessly. That path needs a
//!     real BLE backend (btleplug) and is deliberately out of scope here.
//!   * Over USB (dock or cable) it enumerates as a normal composite device:
//!     interface 0 is a standard HID gamepad that Windows drives out of the
//!     box, and interface 1 is a vendor interface with no driver. The yellow
//!     bang on "Joy-Con (R)" in Other Devices is that second interface
//!     (`CM_PROB_FAILED_INSTALL`) and is EXPECTED — it is not why input does
//!     or doesn't work.
//!
//! So the wired path is readable today with the `hidapi` we already depend on.
//! This probe confirms that and dumps enough to write the parser.
//!
//! Plug a Joy-Con 2 in over USB and run:
//!
//!   cargo run -p flexinput-devices --bin joycon2_probe
//!
//! By default this only reads. `--init` makes it WRITE an activation command
//! first — see `send_init` for why that is needed and what the risk is.
//!
//! Optional args:
//!   --pid 0x2066   only consider this product id (default: any Nintendo VID)
//!   --index N      open the Nth matching interface instead of autoselecting
//!   --list         enumerate and exit, don't stream
//!   --raw          print every report, not just ones that changed
//!   --init         send the documented activation command before streaming
//!   --init-hex "…" send this hex byte string instead (implies --init), so
//!                  variants can be tried without a rebuild

use hidapi::{HidApi, HidDevice};
use std::time::{Duration, Instant};

const NINTENDO_VID: u16 = 0x057E;

/// Generic Desktop usage page, and the two usages a pad presents itself under.
/// We autoselect on these so we open the gamepad collection rather than any
/// vendor-defined one the composite device also exposes.
const USAGE_PAGE_GENERIC_DESKTOP: u16 = 0x01;
const USAGE_JOYSTICK: u16 = 0x04;
const USAGE_GAMEPAD: u16 = 0x05;

/// hidapi's own cap on a report descriptor (HID_API_MAX_REPORT_DESCRIPTOR_SIZE).
const MAX_DESCRIPTOR: usize = 4096;

/// The only OUTPUT report this device declares (63 payload bytes). Commands go
/// here; there is no 0x80-prefixed control report like the original Joy-Con had.
const OUT_REPORT_ID: u8 = 0x01;
const OUT_REPORT_LEN: usize = 63;

/// Activation command, taken verbatim from the documented Joy-Con 2 BLE command
/// characteristic (`649d4ac9-…`). The wired command channel is assumed to carry
/// the same protocol — unverified, which is the whole point of trying it. Note
/// the `07`, which plausibly names input report 7, the one carrying buttons and
/// the stick.
const JC2_INIT: &[u8] = &[
    0x09, 0x91, 0x01, 0x07, 0x00, 0x08, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

struct Args {
    pid: Option<u16>,
    index: Option<usize>,
    list_only: bool,
    raw: bool,
    init: bool,
    init_hex: Option<Vec<u8>>,
}

fn main() {
    let args = parse_args();

    let api = match HidApi::new() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("hidapi init failed: {e}");
            std::process::exit(1);
        }
    };

    // Collect every Nintendo-VID interface, in a stable order, so `--index`
    // means something reproducible between runs.
    let mut matches: Vec<&hidapi::DeviceInfo> = api
        .device_list()
        .filter(|i| i.vendor_id() == NINTENDO_VID)
        .filter(|i| args.pid.is_none_or(|p| i.product_id() == p))
        .collect();
    matches.sort_by_key(|i| (i.product_id(), i.interface_number(), i.usage()));

    if matches.is_empty() {
        eprintln!("No Nintendo-VID (0x{NINTENDO_VID:04X}) HID interfaces found.");
        eprintln!("Is the controller connected over USB? BLE will NOT show up here — see the module docs.");
        std::process::exit(1);
    }

    println!("Found {} Nintendo-VID interface(s):\n", matches.len());
    for (n, i) in matches.iter().enumerate() {
        println!("  [{n}] pid=0x{:04X} iface={} usage_page=0x{:04X} usage=0x{:02X}",
            i.product_id(), i.interface_number(), i.usage_page(), i.usage());
        println!("      product : {}", i.product_string().unwrap_or("<none>"));
        println!("      serial  : {}", i.serial_number().unwrap_or("<none>"));
        println!("      path    : {}", i.path().to_string_lossy());
        println!();
    }

    if args.list_only {
        return;
    }

    // Autoselect the gamepad collection unless the caller pinned an index.
    let chosen = match args.index {
        Some(n) => match matches.get(n) {
            Some(i) => *i,
            None => {
                eprintln!("--index {n} out of range (0..{})", matches.len() - 1);
                std::process::exit(1);
            }
        },
        None => matches
            .iter()
            .find(|i| {
                i.usage_page() == USAGE_PAGE_GENERIC_DESKTOP
                    && (i.usage() == USAGE_GAMEPAD || i.usage() == USAGE_JOYSTICK)
            })
            .copied()
            .unwrap_or_else(|| {
                println!("(no gamepad-usage interface; falling back to [0])\n");
                matches[0]
            }),
    };

    println!("Opening pid=0x{:04X} iface={} usage=0x{:02X} ...",
        chosen.product_id(), chosen.interface_number(), chosen.usage());

    let dev = match chosen.open_device(&api) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("open failed: {e}");
            eprintln!("If another process already claimed it exclusively (Steam, Switch2Connect, HidHide), close it first.");
            std::process::exit(1);
        }
    };

    dump_report_descriptor(&dev);

    if args.init || args.init_hex.is_some() {
        let payload = args.init_hex.as_deref().unwrap_or(JC2_INIT);
        send_init(&dev, payload);
    }

    stream(&dev, args.raw);
}

/// Write an activation command to the device's OUTPUT report.
///
/// Why this exists: the descriptor declares a perfectly good gamepad, but the
/// controller sends nothing at all until told to start — the same design as the
/// original Joy-Con, which stays silent until it receives subcommand 0x03 to set
/// the input report mode. Reading alone therefore cannot get us a byte map.
///
/// Risk: this is the one part of the probe that is not read-only. The default
/// payload is a documented command for this device class, not a fuzzed guess, so
/// the realistic failure mode is "device ignores it". A controller that ends up
/// in an odd state should recover on a power cycle (undock / re-dock). Do not
/// point `--init-hex` at random bytes and expect the same guarantee.
fn send_init(dev: &HidDevice, payload: &[u8]) {
    // hidapi wants the report ID as byte 0, then exactly the declared payload
    // length — short writes get rejected by the Windows HID stack.
    let mut buf = vec![0u8; OUT_REPORT_LEN + 1];
    buf[0] = OUT_REPORT_ID;
    let n = payload.len().min(OUT_REPORT_LEN);
    buf[1..=n].copy_from_slice(&payload[..n]);
    if payload.len() > OUT_REPORT_LEN {
        println!("\n(warning: payload truncated to {OUT_REPORT_LEN} bytes)");
    }

    let hex: Vec<String> = payload[..n].iter().map(|b| format!("{b:02x}")).collect();
    println!("\n=== sending activation on output report 0x{OUT_REPORT_ID:02x} ===");
    println!("  payload: {}", hex.join(" "));

    match dev.write(&buf) {
        Ok(written) => println!("  wrote {written} bytes OK"),
        Err(e) => {
            println!("  write FAILED: {e}");
            println!("  (streaming anyway — the device may still report on its own)");
        }
    }
}

/// The report descriptor is the authoritative layout — it tells us report IDs,
/// field sizes and usages without guessing from byte diffs. Community docs for
/// Joy-Con 2 cover the BLE notify format, which is a DIFFERENT layout from this
/// USB one, so do not assume the published offsets apply here.
fn dump_report_descriptor(dev: &HidDevice) {
    let mut buf = vec![0u8; MAX_DESCRIPTOR];
    match dev.get_report_descriptor(&mut buf) {
        Ok(n) => {
            println!("\n=== HID report descriptor ({n} bytes) ===");
            for (row, chunk) in buf[..n].chunks(16).enumerate() {
                let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
                println!("  {:04x}  {}", row * 16, hex.join(" "));
            }
            println!("\n  (paste into an HID descriptor decoder to get the field map)");
        }
        Err(e) => println!("\n(report descriptor unavailable: {e})"),
    }
}

/// Stream input reports. By default we only print a report when it differs from
/// the previous one and mark the bytes that moved, which makes mapping buttons a
/// matter of pressing one and reading the marker. A running tally of every
/// offset that has EVER changed is printed periodically — that set is the real
/// output of this probe, since it separates live fields from padding.
fn stream(dev: &HidDevice, raw: bool) {
    println!("\n=== streaming (Ctrl-C to stop) ===");
    println!("changed bytes are shown as [xx]\n");

    // A 64-byte buffer covers the 63-byte reports these controllers are
    // documented to send, with room for a leading report ID.
    let mut buf = [0u8; 64];
    let mut prev: Option<Vec<u8>> = None;
    let mut ever_changed = vec![false; buf.len()];
    let mut last_summary = Instant::now();
    let mut reports: u64 = 0;
    let started = Instant::now();

    loop {
        let n = match dev.read_timeout(&mut buf, 1000) {
            Ok(0) => continue, // timeout, no report waiting
            Ok(n) => n,
            Err(e) => {
                eprintln!("read failed: {e}");
                return;
            }
        };
        reports += 1;
        let cur = buf[..n].to_vec();

        let changed: Vec<bool> = match &prev {
            None => vec![true; n],
            Some(p) => (0..n).map(|i| p.get(i) != cur.get(i)).collect(),
        };
        for (i, c) in changed.iter().enumerate() {
            if *c {
                if let Some(slot) = ever_changed.get_mut(i) {
                    *slot = true;
                }
            }
        }

        let differs = prev.as_ref().is_none_or(|p| *p != cur);
        if raw || differs {
            let rendered: Vec<String> = cur
                .iter()
                .zip(&changed)
                .map(|(b, c)| if *c { format!("[{b:02x}]") } else { format!(" {b:02x} ") })
                .collect();
            println!("len={n:<3} {}", rendered.join(""));
        }
        prev = Some(cur);

        if last_summary.elapsed() >= Duration::from_secs(2) {
            let live: Vec<String> = ever_changed
                .iter()
                .take(n)
                .enumerate()
                .filter(|(_, c)| **c)
                .map(|(i, _)| i.to_string())
                .collect();
            let hz = reports as f64 / started.elapsed().as_secs_f64();
            println!("\n  -- {reports} reports, ~{hz:.0} Hz | live offsets: {} --\n", live.join(","));
            last_summary = Instant::now();
        }
    }
}

fn parse_args() -> Args {
    let mut a = Args {
        pid: None,
        index: None,
        list_only: false,
        raw: false,
        init: false,
        init_hex: None,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut it = argv.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--list" => a.list_only = true,
            "--raw" => a.raw = true,
            "--init" => a.init = true,
            "--pid" => a.pid = it.next().and_then(|v| parse_u16(v)),
            "--index" => a.index = it.next().and_then(|v| v.parse().ok()),
            "--init-hex" => a.init_hex = it.next().and_then(|v| parse_hex_bytes(v)),
            other => eprintln!("(ignoring unknown arg {other})"),
        }
    }
    a
}

/// Parse a loose hex byte string — "09 91 01", "099101" and "0x09,0x91" all work.
fn parse_hex_bytes(s: &str) -> Option<Vec<u8>> {
    let cleaned: String = s
        .replace("0x", " ")
        .replace("0X", " ")
        .chars()
        .map(|c| if c.is_ascii_hexdigit() { c } else { ' ' })
        .collect();
    let tokens: Vec<&str> = cleaned.split_whitespace().collect();

    // Either space-separated byte tokens, or one unbroken run of hex digits.
    if tokens.len() == 1 && tokens[0].len() > 2 {
        let run = tokens[0];
        if run.len() % 2 != 0 {
            eprintln!("--init-hex: odd number of hex digits");
            return None;
        }
        return (0..run.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&run[i..i + 2], 16).ok())
            .collect();
    }
    tokens
        .iter()
        .map(|t| u8::from_str_radix(t, 16).ok())
        .collect()
}

fn parse_u16(s: &str) -> Option<u16> {
    let t = s.trim();
    match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => u16::from_str_radix(hex, 16).ok(),
        None => t.parse().ok(),
    }
}
