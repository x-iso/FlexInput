//! End-to-end link test: connect a Joy-Con 2 through our own dongle and see
//! how long the link survives.
//!
//! This is the measurement that decides whether the dedicated-dongle route
//! solves the problem. Through the Windows stack, a Joy-Con 2 link dies after
//! **31.1 seconds**, every time, because Windows reclaims unpaired BLE links on
//! a timer. If this holds past ~35 s, that constraint is gone.
//!
//! Usage:
//!   cargo run -p flexinput-btle --bin jc2_link
//!   cargo run -p flexinput-btle --bin jc2_link -- c84805fd1b78
//!
//! Wake the controller (any button, or hold sync) so it advertises.

use std::time::{Duration, Instant};

use flexinput_btle::{acl, joycon, Dongle, Event};

const DONGLE_VID: u16 = 0x0BDA;
const DONGLE_PID: u16 = 0xA728;
const NINTENDO_COMPANY_ID: u16 = 0x0553;

/// How long to hold the link before declaring success and disconnecting.
///
/// Comfortably past the 31.1 s Windows cutoff — long enough that surviving it
/// cannot be a coincidence, short enough to keep the test quick.
const HOLD_TARGET: Duration = Duration::from_secs(90);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let wanted: Option<[u8; 6]> = args.first().map(|s| parse_addr(s));

    let dongle = match Dongle::open(DONGLE_VID, DONGLE_PID) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[link] cannot open dongle: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = dongle.reset_and_init() {
        eprintln!("[link] controller init failed: {e}");
        std::process::exit(1);
    }
    println!("[link] dongle ready");

    let (addr, addr_type) = match wanted {
        Some(a) => (a, 0x00),
        None => match find_joycon(&dongle) {
            Some(found) => found,
            None => {
                eprintln!("[link] no Joy-Con found — wake one and try again");
                std::process::exit(1);
            }
        },
    };
    println!("[link] connecting to {}", fmt_addr(&addr));

    let conn = match dongle.le_connect(addr, addr_type) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[link] connect failed: {e}");
            std::process::exit(1);
        }
    };
    println!("[link] connected, handle {conn:#06x}");

    // Raise the ATT MTU first. At the 23-byte default a 63-byte input report
    // would arrive fragmented and every parser offset would be wrong.
    //
    // The MTU exchange doubles as the ATT round-trip test: it is the one
    // request every GATT server must answer regardless of handles, so a reply
    // proves the ACL path works and no reply localises the fault to transport
    // rather than to a wrong handle.
    send(&dongle, conn, "MTU request", &acl::exchange_mtu_request(joycon::DESIRED_MTU));

    // Subscribe to input notifications. Write Request (acknowledged), so a
    // failed subscribe shows up as an error instead of silence.
    send(&dongle, conn, "subscribe input", &acl::write_request(joycon::HANDLE_INPUT_CCCD, &acl::CCCD_NOTIFY));
    // Also subscribe to the command-response characteristic: init replies land
    // there, and its silence vs the input channel's distinguishes "wrong input
    // handle" from "no ATT at all".
    send(&dongle, conn, "subscribe cmd-resp", &acl::write_request(joycon::HANDLE_CMD_RESPONSE_CCCD, &acl::CCCD_NOTIFY));
    println!("[link] holding…");

    hold(&dongle, conn);
}

/// Write an ATT PDU, reporting the outcome.
///
/// `write_bulk` succeeding says only that the dongle accepted the bytes over
/// USB — never that they reached the peer. Logging it anyway is what separates
/// "we never sent" from "we sent and got no answer".
fn send(dongle: &Dongle, conn: u16, what: &str, pdu: &[u8]) {
    match dongle.send_att(conn, pdu) {
        Ok(()) => println!("[link] -> {what} ({} bytes)", pdu.len()),
        Err(e) => eprintln!("[link] -> {what} FAILED: {e}"),
    }
}

/// Pump the link, reporting throughput and reacting to a disconnect.
fn hold(dongle: &Dongle, conn: u16) {
    let start = Instant::now();
    let mut notifications = 0u64;
    let mut last_report = Instant::now();
    let mut last_notification: Option<Instant> = None;
    let mut mtu: Option<u16> = None;
    let mut dumped = 0u32;
    // Counts EVERY inbound ACL packet, on any channel. Zero here means nothing
    // is coming back at all, which is a transport fault; a non-zero count with
    // no notifications means ATT works and a handle is wrong. Those need
    // completely different fixes, so they must be distinguishable.
    let mut acl_in = 0u64;
    let mut other_dumped = 0u32;

    loop {
        if start.elapsed() >= HOLD_TARGET {
            println!(
                "[link] HELD {:.1}s with {notifications} notifications — target reached",
                start.elapsed().as_secs_f32()
            );
            break;
        }

        // Events first: a disconnect must be noticed promptly, and it is the
        // one outcome this whole test is about.
        match dongle.read_event_timeout(Duration::from_millis(5)) {
            Ok(Some(Event::DisconnectionComplete { reason, .. })) => {
                println!(
                    "[link] *** DISCONNECTED after {:.1}s — reason {reason:#04x} ({})",
                    start.elapsed().as_secs_f32(),
                    disconnect_reason(reason),
                );
                println!("[link] {notifications} notifications received");
                return;
            }
            Ok(Some(Event::EncryptionChange { status, enabled, .. })) => {
                println!("[link] encryption change: status={status} enabled={enabled}");
            }
            Ok(_) | Err(_) => {}
        }

        match dongle.read_acl(Duration::from_millis(20)) {
            Ok(Some(pkt)) if pkt.cid != acl::CID_ATT => {
                acl_in += 1;
                if other_dumped < 4 {
                    other_dumped += 1;
                    println!("[link] <- non-ATT cid={:#06x}: {:02x?}", pkt.cid, pkt.payload);
                }
            }
            Ok(Some(pkt)) => {
                acl_in += 1;
                if let Some(n) = acl::parse_notification(&pkt.payload) {
                    if n.handle == joycon::HANDLE_INPUT_VALUE {
                        notifications += 1;
                        last_notification = Some(Instant::now());
                        if dumped < 3 {
                            dumped += 1;
                            println!("[link] input report ({} bytes): {:02x?}", n.value.len(), n.value);
                        }
                    }
                } else if pkt.payload.first() == Some(&acl::ATT_EXCHANGE_MTU_RESPONSE)
                    && pkt.payload.len() >= 3
                {
                    let m = u16::from_le_bytes([pkt.payload[1], pkt.payload[2]]);
                    mtu = Some(m);
                    println!("[link] ATT MTU negotiated: {m}");
                } else if pkt.payload.first() == Some(&acl::ATT_ERROR_RESPONSE) {
                    // Handle, opcode and reason are all in here — an error is a
                    // GOOD sign at this stage: it proves ATT round-trips.
                    println!("[link] <- ATT error: {:02x?}", pkt.payload);
                } else if other_dumped < 6 {
                    other_dumped += 1;
                    println!("[link] <- ATT pdu: {:02x?}", pkt.payload);
                }
            }
            Ok(_) => {}
            Err(e) => {
                println!("[link] ACL read error after {:.1}s: {e}", start.elapsed().as_secs_f32());
                break;
            }
        }

        if last_report.elapsed() >= Duration::from_secs(5) {
            last_report = Instant::now();
            let hz = notifications as f32 / start.elapsed().as_secs_f32();
            let quiet = last_notification
                .map(|t| format!("{:.1}s ago", t.elapsed().as_secs_f32()))
                .unwrap_or_else(|| "never".into());
            println!(
                "[link] up {:.0}s  notifications={notifications} ({hz:.0} Hz)  acl_in={acl_in}  last={quiet}  mtu={}",
                start.elapsed().as_secs_f32(),
                mtu.map(|m| m.to_string()).unwrap_or_else(|| "default".into()),
            );
        }
    }

    let _ = dongle.disconnect(conn);
}

/// Scan until a Nintendo controller turns up.
fn find_joycon(dongle: &Dongle) -> Option<([u8; 6], u8)> {
    println!("[link] scanning — wake a Joy-Con now");
    if let Err(e) = dongle.start_le_scan() {
        eprintln!("[link] scan failed: {e}");
        return None;
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut found = None;
    while Instant::now() < deadline {
        if let Ok(Some(Event::LeAdvertisingReport(r))) = dongle.read_event() {
            if let Some(md) = r.manufacturer_data() {
                if md.len() >= 9 && u16::from_le_bytes([md[0], md[1]]) == NINTENDO_COMPANY_ID {
                    let pid = u16::from_le_bytes([md[7], md[8]]);
                    println!("[link] found {} pid={pid:04x}", fmt_addr(&r.address));
                    found = Some((r.address, r.address_type));
                    break;
                }
            }
        }
    }
    let _ = dongle.stop_le_scan();
    found
}

fn parse_addr(s: &str) -> [u8; 6] {
    let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    assert_eq!(hex.len(), 12, "address must be 12 hex digits");
    let mut out = [0u8; 6];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("hex");
    }
    out
}

fn fmt_addr(a: &[u8; 6]) -> String {
    a.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

/// Standard HCI disconnect reasons — the byte that ended the Windows
/// investigation, so it is spelled out rather than printed raw.
fn disconnect_reason(r: u8) -> &'static str {
    match r {
        0x05 => "authentication failure",
        0x08 => "supervision timeout",
        0x13 => "remote user terminated",
        0x14 => "remote device low resources",
        0x15 => "remote device powered off",
        0x16 => "terminated by local host",
        0x3B => "unacceptable connection parameters",
        0x3D => "MIC failure",
        0x3E => "connection failed to be established",
        _ => "unknown",
    }
}
