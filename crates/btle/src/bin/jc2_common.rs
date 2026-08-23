//! One question: **does the common input `0x000a` wake up if the init is sent
//! to `0x0014` WITH the 17-byte rumble prefix?**
//!
//! # Why this combination and no other
//!
//! Two facts have been established separately and never combined:
//!
//! * on this pad the per-side command channel `0x0016` requires a **17-byte
//!   prefix** (report id + rumble region) before the command header — without it
//!   the stream stays stub
//! * the reference implementations drive a genuine Joy-Con 2 through `0x0014`
//!   (`649d4ac9-…`), sending the header at offset 0 with **no prefix**
//!
//! Every attempt on `0x0014` here used the bare framing, concluded it was inert
//! (a player-LED test on it moved nothing), and moved on. But "inert" and
//! "correctly framed for a different device" are the same observation. If this
//! clone wants the prefix on BOTH channels, every `0x0014` command we ever sent
//! was malformed — which would explain why `0x000a` was never configured, in a
//! way that nothing else has.
//!
//! ❗ **No encryption.** The reference ESP32 firmware states outright that
//! Switch 2 controllers use a plain, unencrypted, unbonded link and that
//! initiating security makes them drop the link. Every drop seen while testing
//! encryption here was self-inflicted.
//!
//! Usage: `cargo run -p flexinput-btle --bin jc2_common`

use std::time::{Duration, Instant};

use flexinput_btle::{acl, joycon as jc, Dongle, Event};

const NINTENDO_COMPANY_ID: u16 = 0x0553;

/// motion | mouse | magnetometer — the whole IMU behind one gate.
const FEATURE_MASK: u8 = 0x94;

/// `sw2_init_commands`, verbatim from the reference.
const INIT: &[(u8, u8, &[u8])] = &[
    (0x03, 0x0d, &[0x01, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]),
    (0x07, 0x01, &[]),
    (0x16, 0x01, &[]),
    (0x15, 0x03, &[0x00]),
    (0x0c, 0x02, &[FEATURE_MASK, 0, 0, 0]),
    (0x11, 0x03, &[]),
    (0x0a, 0x08, &[
        0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0x35, 0x00, 0x46, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ]),
    (0x0c, 0x04, &[FEATURE_MASK, 0, 0, 0]),
    (0x03, 0x0a, &[0x09, 0x00, 0x00, 0x00]),
    (0x10, 0x01, &[]),
    (0x01, 0x0c, &[]),
    (0x01, 0x01, &[0x00, 0x00, 0x00, 0x00]),
    (0x09, 0x07, &[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]),
];

fn main() {
    let dongle = match flexinput_btle::open_preferred() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cannot open dongle: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = dongle.reset_and_init() {
        eprintln!("dongle init failed: {e}");
        std::process::exit(1);
    }

    println!("wake ONE half with any button (no sync needed — nothing is paired here)");
    let Some((addr, addr_type, side)) = scan(&dongle) else {
        eprintln!("no controller found");
        std::process::exit(1);
    };
    println!("found {side} {addr:02x?}");

    // Two passes over one connection would leave the first pass's state in
    // place, so each framing gets its own fresh link.
    for prefixed in [true, false] {
        let label = if prefixed { "WITH 17-byte prefix" } else { "BARE (reference framing)" };
        println!("\n══════════════════════════════════════════════════════════");
        println!("  init to {:#06x} {label}", jc::HANDLE_CMD_WRITE_COMMON);
        println!("══════════════════════════════════════════════════════════");

        let conn = match dongle.le_connect_params(addr, addr_type, 6, 6) {
            Ok(p) => {
                println!("connected {:#06x}, interval {:.2} ms", p.conn_handle, p.interval_ms());
                p.conn_handle
            }
            Err(e) => {
                eprintln!("connect failed: {e} — wake the controller and retry");
                std::process::exit(1);
            }
        };

        let _ = dongle.send_att(conn, &acl::exchange_mtu_request(jc::DESIRED_MTU));
        std::thread::sleep(Duration::from_millis(150));

        // Command responses first, so the init's replies are visible. The INPUT
        // characteristics stay unsubscribed until after the init — the order
        // both reference implementations use.
        let _ = dongle.write_attribute(
            conn,
            jc::HANDLE_CMD_RESPONSE_CCCD,
            &acl::CCCD_NOTIFY,
            acl::ATT_WRITE_REQUEST,
        );

        let mut acked = 0usize;
        for (cmd, sub, data) in INIT {
            if send(&dongle, conn, *cmd, *sub, data, prefixed).is_some() {
                acked += 1;
            }
        }
        // Set Input Mode 0x30 — a RAW write, not a framed command.
        let mode: [u8; 11] = [0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0x03, 0x30];
        let mut frame = Vec::new();
        if prefixed {
            frame.resize(17, 0);
        }
        frame.extend_from_slice(&mode);
        let _ = dongle.write_attribute(
            conn,
            jc::HANDLE_CMD_WRITE_COMMON,
            &frame,
            acl::ATT_WRITE_COMMAND,
        );
        println!("  {acked}/{} init commands answered, input mode 0x30 sent", INIT.len());
        std::thread::sleep(Duration::from_millis(300));

        // NOW subscribe the inputs.
        let attrs = dongle.discover_attributes(conn).unwrap_or_default();
        let mut watched = Vec::new();
        for a in attrs.iter().filter(|a| a.uuid == acl::AttUuid::Short(acl::GATT_CCCD)) {
            if a.handle == jc::HANDLE_CMD_RESPONSE_CCCD {
                continue;
            }
            let _ = dongle.write_attribute(conn, a.handle, &acl::CCCD_NOTIFY, acl::ATT_WRITE_REQUEST);
            watched.push((a.handle - 1, 0usize, 0usize));
            std::thread::sleep(Duration::from_millis(30));
        }
        // The rate descriptors: without them the per-side stream stays stub, and
        // the common one has an identical descriptor that nothing used to write.
        for a in attrs.iter().filter(|a| a.uuid.to_string() == UUID_REPORT_RATE) {
            let _ = dongle.write_attribute(
                conn,
                a.handle,
                &jc::REPORT_RATE_PAYLOAD,
                acl::ATT_WRITE_REQUEST,
            );
        }

        println!("  listening 3 s …");
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            let Ok(Some(pkt)) = dongle.read_acl(Duration::from_millis(20)) else { continue };
            if pkt.cid != acl::CID_ATT {
                continue;
            }
            let Some(n) = acl::parse_notification(&pkt.payload) else { continue };
            if let Some(e) = watched.iter_mut().find(|(h, _, _)| *h == n.handle) {
                e.1 += 1;
                e.2 = n.value.len();
            }
        }
        for (h, n, len) in &watched {
            let tag = if *h == jc::HANDLE_INPUT_COMMON && *n > 0 { "   ⭐⭐⭐ THE COMMON REPORT" } else { "" };
            println!("    {h:#06x}: {n:>4} frames, {len} bytes{tag}");
        }
        if watched.iter().any(|(h, n, _)| *h == jc::HANDLE_INPUT_COMMON && *n > 0) {
            println!("\n⭐ {:#06x} STREAMS with the init sent {label}.", jc::HANDLE_INPUT_COMMON);
            println!("  Reference layout: buttons@4, sticks@10..16, mag@25, accel@48..54, gyro@54..60.");
            return;
        }

        let _ = dongle.disconnect(conn);
        std::thread::sleep(Duration::from_secs(2));
        if prefixed {
            println!("\n  no luck — wake the controller again for the bare-framing pass");
            let _ = scan(&dongle);
        }
    }
    println!("\n⛔ Neither framing woke {:#06x}.", jc::HANDLE_INPUT_COMMON);
}

const UUID_REPORT_RATE: &str = "679d5510-5a24-4dee-9557-95df80486ecb";

/// Send one command, optionally with the 17-byte rumble prefix, and wait for
/// its reply on the common response channel.
fn send(
    dongle: &Dongle,
    conn: u16,
    cmd: u8,
    sub: u8,
    data: &[u8],
    prefixed: bool,
) -> Option<(u8, Vec<u8>)> {
    let mut frame = Vec::new();
    if prefixed {
        frame.resize(17, 0);
    }
    frame.extend_from_slice(&[cmd, 0x91, 0x01, sub, 0x00, data.len() as u8, 0x00, 0x00]);
    frame.extend_from_slice(data);
    dongle
        .write_attribute(conn, jc::HANDLE_CMD_WRITE_COMMON, &frame, acl::ATT_WRITE_COMMAND)
        .ok()?;

    let deadline = Instant::now() + Duration::from_millis(400);
    while Instant::now() < deadline {
        let Ok(Some(pkt)) = dongle.read_acl(Duration::from_millis(20)) else { continue };
        if pkt.cid != acl::CID_ATT || pkt.conn_handle != conn {
            continue;
        }
        let Some(n) = acl::parse_notification(&pkt.payload) else { continue };
        if n.handle == jc::HANDLE_CMD_RESPONSE && n.value.len() >= 8 && n.value[0] == cmd {
            return Some((n.value[1], n.value[8..].to_vec()));
        }
    }
    None
}

fn scan(dongle: &Dongle) -> Option<([u8; 6], u8, &'static str)> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if dongle.start_le_scan().is_err() {
            std::thread::sleep(Duration::from_millis(300));
            continue;
        }
        let window = Instant::now() + Duration::from_secs(3);
        while Instant::now() < window {
            if let Ok(Some(Event::LeAdvertisingReport(r))) =
                dongle.read_event_timeout(Duration::from_millis(100))
            {
                let Some(md) = r.manufacturer_data() else { continue };
                if md.len() < 9 || u16::from_le_bytes([md[0], md[1]]) != NINTENDO_COMPANY_ID {
                    continue;
                }
                let pid = u16::from_le_bytes([md[7], md[8]]);
                let _ = dongle.stop_le_scan();
                return Some((r.address, r.address_type, if pid == 0x2066 { "RIGHT" } else { "LEFT" }));
            }
        }
        let _ = dongle.stop_le_scan();
    }
    None
}
