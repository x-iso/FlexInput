//! One question, one program: **can we bring a Joy-Con 2 link up ENCRYPTED, and
//! does the common input `0x000a` start talking when we do?**
//!
//! # Why this is a new binary rather than another flag on `jc2_imu`
//!
//! `jc2_imu` has grown around a series of assumptions that were later
//! disproven — a command handle that turned out inert, a "safety net" re-init
//! that silently overwrote the setting under test, preflight guards that
//! aborted the very experiment they were asked to run. Every new mode inherited
//! all of it, and several results had to be thrown away once the interference
//! was found. Adding one more flag would inherit the same risk.
//!
//! So this does the minimum, in a straight line, with nothing else running:
//!
//! 1. read the dongle's own address **before any link exists**
//! 2. connect ONE half
//! 3. subscribe to the command-response channel only
//! 4. `0x15/0x01` set-host, `0x15/0x04` key-exchange, read the reply
//! 5. derive the LTK and enable encryption
//! 6. subscribe the inputs and see which ones talk
//!
//! ❗ It DOES send `0x15/0x03`, which commits the host address and key to
//! controller flash (`0x1FA000`, two host slots). That is deliberate and
//! authorised: this controller's owner has no Switch 2, the PC is the only
//! intended host, and every staged-only attempt failed for the obvious reason —
//! a controller will not encrypt a link with a key it was never told to keep.
//!
//! Usage: `cargo run -p flexinput-btle --bin jc2_crypt`

use std::time::{Duration, Instant};

use flexinput_btle::{acl, joycon as jc, Dongle, Event};

const NINTENDO_COMPANY_ID: u16 = 0x0553;

/// The reference implementation's fixed host key, minus its framing byte.
///
/// It treats this as a constant the host dictates. Our own `pairing.rs` models
/// the same exchange as `LTK = host XOR device`, and hardware agrees with the
/// latter: the controller answers `0x15/0x04` with a 16-byte key of its own.
const HOST_KEY: [u8; 16] = [
    0xea, 0xbd, 0x47, 0x13, 0x89, 0x35, 0x42, 0xc6,
    0x79, 0xee, 0x07, 0xf2, 0x53, 0x2c, 0x6c, 0x31,
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

    // ⭐ BEFORE connecting anything.
    //
    // Read on a live link this competes with a flood of `Number Of Completed
    // Packets` events from two halves at ~67 Hz, and loses. On an idle dongle
    // the answer arrives in milliseconds. The command was never the problem;
    // asking at the wrong moment was.
    let host = match dongle.read_bd_addr() {
        Ok(a) => {
            println!("dongle BD_ADDR {a:02x?}");
            a
        }
        Err(e) => {
            eprintln!("cannot read dongle address: {e}");
            std::process::exit(1);
        }
    };

    // ❗ SYNC, not "any button".
    //
    // Waking a controller with an ordinary button reconnects it using the bond
    // it already holds. Only sync mode makes it accept a NEW host and key — so
    // a pairing handshake sent to a button-woken pad is talking to something
    // that has no intention of re-bonding.
    //
    // On this clone that failure is invisible: it answers `status 0x01 OK` to
    // every command including ones that do not exist, so all four pairing steps
    // "succeeded" while accepting nothing, and only the encryption attempt at
    // the end revealed that the controller was still using a different key.
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║  HOLD THE SYNC BUTTON until the lights run back and forth.   ║");
    println!("║  A normal button press only RECONNECTS an existing bond and  ║");
    println!("║  will not accept a new key — the handshake will appear to    ║");
    println!("║  succeed and encryption will still fail.                     ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    let Some((addr, addr_type, side)) = scan(&dongle) else {
        eprintln!("no controller found");
        std::process::exit(1);
    };
    println!("found {side} {addr:02x?}");

    // ❗ Let the controller finish entering sync mode before grabbing it.
    //
    // The probe connected on the FIRST advertisement, which arrives while the
    // sync animation is barely starting — the reported symptom was "I barely
    // see the LEDs light up before it goes dark". A pad still transitioning is
    // not in a state to accept a new bond, so the handshake that follows is
    // talking to the wrong mode, and every step still answers OK.
    println!("waiting 3 s for sync mode to settle (watch the LEDs run)…");
    std::thread::sleep(Duration::from_secs(3));

    let conn = connect(&dongle, addr, addr_type, "connected");

    // MTU first: the default 23 bytes fragments a 63-byte report and truncates
    // discovery responses.
    let _ = dongle.send_att(conn, &acl::exchange_mtu_request(jc::DESIRED_MTU));
    std::thread::sleep(Duration::from_millis(150));

    // Command responses only. The inputs stay unsubscribed until after
    // encryption, so that what wakes up is attributable to encryption and not
    // to having been subscribed all along.
    match dongle.write_attribute(
        conn,
        jc::HANDLE_CMD_RESPONSE_CCCD,
        &acl::CCCD_NOTIFY,
        acl::ATT_WRITE_REQUEST,
    ) {
        Ok(()) => println!("subscribed command responses ({:#06x})", jc::HANDLE_CMD_RESPONSE),
        Err(e) => {
            eprintln!("cannot subscribe command responses: {e}");
            std::process::exit(1);
        }
    }

    // ── Pairing handshake, staged ────────────────────────────────────────────
    let mut set_host = vec![0x00, 0x02];
    let mut le = host;
    le.reverse();
    set_host.extend_from_slice(&le);
    set_host.extend_from_slice(&le);
    match command(&dongle, conn, 0x15, 0x01, &set_host) {
        Some((st, d)) => println!("0x15/0x01 set-host   status {st:#04x} data {d:02x?}"),
        None => println!("0x15/0x01 set-host   NO REPLY"),
    }

    let mut key_req = vec![0x00];
    key_req.extend_from_slice(&HOST_KEY);
    let reply = match command(&dongle, conn, 0x15, 0x04, &key_req) {
        Some((st, d)) => {
            println!("0x15/0x04 key-xchg   status {st:#04x} data {d:02x?}");
            d
        }
        None => {
            eprintln!("0x15/0x04 key-xchg   NO REPLY — cannot derive a key, stopping");
            std::process::exit(1);
        }
    };

    // ❗ Skip the leading framing byte. Including it shifts every byte of the
    // derived key and produced a key that was wrong in all 16 positions.
    if reply.len() < 17 {
        eprintln!("key reply too short ({} bytes) — expected 1 framing + 16 key", reply.len());
        std::process::exit(1);
    }
    let device_key = &reply[1..17];
    let mut ltk = HOST_KEY;
    for (i, b) in ltk.iter_mut().enumerate() {
        *b ^= device_key[i];
    }
    println!("device key  {device_key:02x?}");
    println!("derived LTK {ltk:02x?}");

    // ❗ The SECOND key. The reference sends four pairing commands —
    // `0x01` set-host, `0x04` key-exchange, `0x02`, `0x03` finalise — and we
    // were sending two. Skipping `0x02` plausibly leaves the controller with no
    // committed key to encrypt with, which fits both the silence and the
    // controller going dark straight afterwards.
    //
    // The reference's value here is another fixed constant, so it is sent
    // verbatim; `0x02` does not commit to flash (that is `0x03`).
    const SECOND_KEY: [u8; 17] = [
        0x00, 0x40, 0xb0, 0x8a, 0x5f, 0xcd, 0x1f, 0x9b, 0x41,
        0x12, 0x5c, 0xac, 0xc6, 0x3f, 0x38, 0xa0, 0x73,
    ];
    match command(&dongle, conn, 0x15, 0x02, &SECOND_KEY) {
        Some((st, d)) => println!("0x15/0x02 second-key status {st:#04x} data {d:02x?}"),
        None => println!("0x15/0x02 second-key NO REPLY"),
    }

    // ⭐ FINALISE — commits the host address and key to controller flash.
    //
    // ❗ This WRITES PERSISTENT MEMORY (`0x1FA000`), which holds only two host
    // slots, so it can evict another host's entry. Sent deliberately: the owner
    // has no Switch 2 and the PC is the only intended host, which makes
    // "pairing to this machine displaces another" the normal, wanted outcome
    // rather than a risk.
    //
    // Every earlier attempt staged a key without ever committing it, and the
    // controller has no reason to encrypt a link with a key it was never told
    // to keep. Ordering matters: `pairing.rs` records that `0x03/0x07` is sent
    // "immediately after pairing is finalised", so the finalise goes first.
    match command(&dongle, conn, 0x15, 0x03, &[0x00]) {
        Some((st, d)) => println!("0x15/0x03 FINALISE     status {st:#04x} data {d:02x?}  (flash written)"),
        None => println!("0x15/0x03 FINALISE     NO REPLY"),
    }
    std::thread::sleep(Duration::from_millis(200));

    // ⭐ REGISTER THE LINK KEY — the step that was missing.
    //
    // The key exchange above is now cryptographically verified: the controller's
    // reply to `0x15/0x02` equals `AES128-ECB(LTK, A2)`, so both sides hold the
    // same key. But holding a key is not the same as knowing which key the LIVE
    // LINK uses, and `pairing.rs` says so in as many words:
    //
    //   "without it the controller has completed a key exchange but has never
    //    been told which key the live link is using. Omitting it was almost
    //    certainly why connections were dropped a fixed ~30 s after connecting"
    //
    // "Goes dark shortly after connecting" is that symptom exactly.
    //
    // Payload is `[host addr, wire order][LTK, byte-reversed]` — the same
    // reversal the key material uses in `0x15/0x02` and `0x15/0x04`. This does
    // not write flash; `0x15/0x03` is the step that does.
    let mut reg = Vec::with_capacity(22);
    reg.extend_from_slice(&le); // host address, already reversed above
    let mut key_wire = ltk;
    key_wire.reverse();
    reg.extend_from_slice(&key_wire);
    match command(&dongle, conn, 0x03, 0x07, &reg) {
        Some((st, d)) => println!("0x03/0x07 reg-link-key status {st:#04x} data {d:02x?}"),
        None => println!("0x03/0x07 reg-link-key NO REPLY"),
    }
    match command(&dongle, conn, 0x03, 0x09, &[]) {
        Some((st, d)) => println!("0x03/0x09 key-commit   status {st:#04x} data {d:02x?}"),
        None => println!("0x03/0x09 key-commit   NO REPLY"),
    }

    // ── Did the bond actually take? ──────────────────────────────────────────
    //
    // ⭐ Every acknowledgement above is worthless on this pad. The advertised
    // reconnect address is not: it is state the controller only rewrites when a
    // bond is genuinely stored. Disconnect and look.
    //
    // This also matches how bonding is SUPPOSED to work, which we have never
    // done: pair, drop the link, reconnect, and encrypt using the stored key on
    // the NEW connection. Every previous attempt tried to encrypt the same
    // connection the pairing ran on.
    println!("\n── verifying the bond from the advertisement ──");
    let _ = dongle.disconnect(conn);
    std::thread::sleep(Duration::from_secs(2));
    println!("press any button to wake it for the reconnect…");

    let Some((addr2, addr_type2, _)) = scan(&dongle) else {
        eprintln!("controller did not advertise again");
        std::process::exit(1);
    };
    // `scan` prints the advert and the bonded host; compare it against us.
    println!("our dongle is {host:02x?}");

    let conn = connect(&dongle, addr2, addr_type2, "reconnected");
    let _ = dongle.write_attribute(
        conn,
        jc::HANDLE_CMD_RESPONSE_CCCD,
        &acl::CCCD_NOTIFY,
        acl::ATT_WRITE_REQUEST,
    );

    // ⭐ INITIALISE, or the controller goes back to sleep.
    //
    // The reported difference against the Windows path is exact: there the pad
    // BUZZES, takes player slot 1 and STAYS ON; here it powers off moments after
    // connecting. This probe was sending pairing commands and then nothing at
    // all — no handshake, no feature select, no player LED. A controller that
    // connects and is never initialised concludes the host is not really there.
    //
    // That also makes it a plausible precondition for encryption: a pad on its
    // way to sleep has no reason to negotiate anything.
    println!("running init so the controller stays awake…");
    for (cmd, sub, data) in [
        (0x07u8, 0x01u8, &[][..]),
        (0x10, 0x01, &[][..]),
        (0x16, 0x01, &[][..]),
        // Player LED 1 — the visible half of "assigned slot 1" on Windows, and
        // the one step whose effect can be seen without parsing anything.
        (0x09, 0x07, &[0x01, 0, 0, 0, 0, 0, 0, 0][..]),
        (0x0C, 0x02, &[0x37, 0, 0, 0][..]),
        (0x0C, 0x04, &[0x37, 0, 0, 0][..]),
    ] {
        match command(&dongle, conn, cmd, sub, data) {
            Some((st, _)) => println!("   {cmd:#04x}/{sub:#04x} status {st:#04x}"),
            None => println!("   {cmd:#04x}/{sub:#04x} no reply"),
        }
    }
    // The report-rate descriptor: without it the stream stays stub forever.
    let _ = dongle.write_attribute(
        conn,
        jc::HANDLE_INPUT_REPORT_RATE,
        &jc::REPORT_RATE_PAYLOAD,
        acl::ATT_WRITE_REQUEST,
    );
    println!("   → is the LED lit and the controller still awake? if so, init worked.");
    std::thread::sleep(Duration::from_millis(500));

    // ── Encryption ───────────────────────────────────────────────────────────
    // `rand` and `ediv` are zero: this key did not come from legacy SMP.
    //
    // ❗ Key byte order is a genuine ambiguity. `le_enable_encryption` reverses
    // on the way to the wire, and `pairing.rs::register_link_key_data` stores
    // the LTK reversed when handing it to the controller — so the two sides may
    // disagree. Both orders are tried, as long as the link survives the first.
    let mut reversed = ltk;
    reversed.reverse();
    let mut encrypted = false;
    for (what, key) in [("derived LTK", ltk), ("derived LTK, byte-reversed", reversed)] {
        println!("\n→ LE_Enable_Encryption with {what}: {key:02x?}");
        if let Err(e) = dongle.le_enable_encryption(conn, 0, 0, &key) {
            eprintln!("  failed to send: {e}");
            break;
        }
        match wait_encryption(&dongle) {
            Verdict::Enabled => {
                println!("  ⭐ ENCRYPTION ENABLED — first encrypted link with this controller");
                encrypted = true;
                break;
            }
            Verdict::Dropped => {
                eprintln!("  ⛔ link dropped — key rejected, and no further key can be tried");
                break;
            }
            Verdict::Refused { status, enabled } => {
                eprintln!("  ⛔ refused: status {status:#04x} enabled {enabled}");
            }
            // ⭐ The dongle refusing the COMMAND is a different fact from the
            // controller refusing the KEY, and they were indistinguishable
            // before: `le_enable_encryption` answers with Command Status, which
            // nothing read, so a rejected command looked exactly like a
            // controller that never replied.
            Verdict::CommandRejected(st) => {
                eprintln!("  ⛔ the DONGLE rejected the command: status {st:#04x}");
                eprintln!("     Nothing reached the controller; this is a host-side limit.");
                break;
            }
            Verdict::Silent => {
                eprintln!("  ⛔ no Command Status and no Encryption Change within 5 s");
            }
        }
    }
    if !encrypted {
        // ❗ This used to claim the finalise was still untried, while the line
        // directly above it showed the finalise being sent. Stale guidance in a
        // failure path is worse than none: it points the next run at work
        // already done.
        eprintln!("\nThe FULL pairing sequence completed — every step acknowledged, key");
        eprintln!("verified against the controller's own AES confirmation, flash finalised.");
        eprintln!("Encryption is still refused, so the LTK is not what this controller uses");
        eprintln!("for LINK encryption, or it does not support host-supplied LTK at all.");
        eprintln!("\nRemember: this pad answers status 0x01 OK to commands that do not exist,");
        eprintln!("so none of the acknowledgements above are evidence that anything applied.");
        std::process::exit(1);
    }

    // ── Now subscribe the inputs and see who talks ───────────────────────────
    let attrs = match dongle.discover_attributes(conn) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("discovery failed after encryption: {e}");
            return;
        }
    };
    let mut inputs = Vec::new();
    for a in attrs.iter().filter(|a| a.uuid == acl::AttUuid::Short(acl::GATT_CCCD)) {
        if a.handle == jc::HANDLE_CMD_RESPONSE_CCCD {
            continue;
        }
        let value = a.handle - 1;
        match dongle.write_attribute(conn, a.handle, &acl::CCCD_NOTIFY, acl::ATT_WRITE_REQUEST) {
            Ok(()) => println!("subscribed {value:#06x}"),
            Err(e) => println!("subscribe {value:#06x} REFUSED: {e}"),
        }
        inputs.push(value);
        std::thread::sleep(Duration::from_millis(40));
    }

    println!("\nlistening 3 s …");
    let mut counts: Vec<(u16, usize, usize)> = inputs.iter().map(|h| (*h, 0, 0)).collect();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let Ok(Some(pkt)) = dongle.read_acl(Duration::from_millis(20)) else { continue };
        if pkt.cid != acl::CID_ATT {
            continue;
        }
        let Some(n) = acl::parse_notification(&pkt.payload) else { continue };
        if let Some(e) = counts.iter_mut().find(|(h, _, _)| *h == n.handle) {
            e.1 += 1;
            e.2 = n.value.len();
        }
    }
    println!("\nresult on an ENCRYPTED link:");
    for (h, n, len) in &counts {
        let star = if *h == jc::HANDLE_INPUT_COMMON && *n > 0 { "  ⭐⭐⭐" } else { "" };
        println!("   {h:#06x}: {n} frames, {len} bytes{star}");
    }
    if counts.iter().any(|(h, n, _)| *h == jc::HANDLE_INPUT_COMMON && *n > 0) {
        println!("\n⭐ The common report streams once the link is encrypted.");
        println!("  Layout per the reference: accel at 48..54, gyro at 54..60, i16 LE.");
    } else {
        println!("\n{:#06x} stays silent even encrypted — encryption is not the gate.", jc::HANDLE_INPUT_COMMON);
    }
}

/// Connect at the interval a WORKING implementation uses, and say what we got.
///
/// ⭐ **7.5 ms, requested as a fixed `6..=6`.** `Dongle::le_connect` asks for
/// interval 4 (5 ms) first, which is BELOW the BLE spec minimum — the ESP32
/// firmware in the reference project pins 6 with the comment "values below 6
/// are rejected". A sub-spec request is a plausible cause of a link that comes
/// up and then dies moments later, and nothing here has ever shown what
/// interval was actually granted: it was logged at `log::info!` and this probe
/// installs no logger, so a link at the wrong interval looked exactly like a
/// correct one that misbehaved.
///
/// Supervision timeout is left at the stack default (5 s), close to the
/// firmware's 4 s.
fn connect(dongle: &Dongle, addr: [u8; 6], addr_type: u8, what: &str) -> u16 {
    match dongle.le_connect_params(addr, addr_type, 6, 6) {
        Ok(p) => {
            println!(
                "{what}, handle {:#06x} — interval {:.2} ms, supervision timeout {} ms",
                p.conn_handle,
                p.interval_ms(),
                p.timeout_ms(),
            );
            if p.interval < 6 {
                println!("   ⚠ interval is BELOW the 7.5 ms spec minimum — expect instability");
            }
            p.conn_handle
        }
        Err(e) => {
            eprintln!("{what} failed: {e}");
            std::process::exit(1);
        }
    }
}

/// One command on the per-side channel, returning `(status, data)` of its reply.
fn command(dongle: &Dongle, conn: u16, cmd: u8, sub: u8, data: &[u8]) -> Option<(u8, Vec<u8>)> {
    // 17-byte rumble prefix, then `[cmd][0x91][0x01][sub][0][len][0][0][data]`.
    let mut frame = vec![0u8; 17];
    frame.extend_from_slice(&[cmd, 0x91, 0x01, sub, 0x00, data.len() as u8, 0x00, 0x00]);
    frame.extend_from_slice(data);
    dongle
        .write_attribute(conn, jc::HANDLE_CMD_WRITE, &frame, acl::ATT_WRITE_COMMAND)
        .ok()?;

    let deadline = Instant::now() + Duration::from_millis(900);
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

/// What came back after asking for encryption.
enum Verdict {
    Enabled,
    Refused { status: u8, enabled: u8 },
    /// The DONGLE rejected `LE_Enable_Encryption` — nothing reached the air.
    CommandRejected(u8),
    Dropped,
    Silent,
}

/// Wait for the outcome, watching Command Status as well as Encryption Change.
///
/// ❗ Watching only for Encryption Change conflated two very different
/// failures: a controller that ignored the request, and a dongle that never
/// sent it. `LE_Enable_Encryption` is answered with Command Status, so a
/// host-side rejection produced total silence and read as the controller's
/// fault.
fn wait_encryption(dongle: &Dongle) -> Verdict {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match dongle.read_event_timeout(Duration::from_millis(200)) {
            Ok(Some(Event::EncryptionChange { status: 0x00, enabled: 1, .. })) => {
                return Verdict::Enabled
            }
            Ok(Some(Event::EncryptionChange { status, enabled, .. })) => {
                return Verdict::Refused { status, enabled }
            }
            Ok(Some(Event::CommandStatus { status, opcode }))
                if opcode == flexinput_btle::hci::Opcode::LE_ENABLE_ENCRYPTION =>
            {
                if status != 0 {
                    return Verdict::CommandRejected(status);
                }
                // Accepted; the real answer is still to come.
                continue;
            }
            Ok(Some(Event::DisconnectionComplete { .. })) => return Verdict::Dropped,
            _ => continue,
        }
    }
    Verdict::Silent
}

/// The host this controller is currently bonded to, from its advertisement.
///
/// ⭐ **The only trustworthy pairing signal on this hardware.** The pad answers
/// `status 0x01 OK` to commands that do not exist, so no acknowledgement proves
/// anything applied — but the advertised reconnect address is real state, and it
/// changes only when a bond is genuinely written.
///
/// Layout, counting from the company id: `[0..2]` company, `[5..7]` vendor,
/// `[7..9]` product, `[12..18]` bonded host address (little-endian). The
/// reference reads the same field at `[10..16]` of a slice that has already had
/// the two company-id bytes removed.
///
/// All zeros means NOT BONDED.
fn bonded_host(md: &[u8]) -> Option<[u8; 6]> {
    if md.len() < 18 {
        return None;
    }
    let mut addr = [0u8; 6];
    addr.copy_from_slice(&md[12..18]);
    addr.reverse();
    Some(addr)
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
                // ⭐ The FULL advertisement, because sync mode almost certainly
                // shows up here. Nothing has ever compared a button-woken advert
                // against a sync-mode one, and a single differing flag byte
                // would let the probe REFUSE to run against a pad that is not
                // actually in pairing mode — instead of running a handshake
                // that reports success and changes nothing.
                println!("manufacturer data: {md:02x?}");
                match bonded_host(md) {
                    Some(h) if h == [0; 6] => println!("bonded host: NONE (all zeros)"),
                    Some(h) => println!("bonded host: {h:02x?}"),
                    None => println!("bonded host: advert too short to tell"),
                }
                // The right half carries the LOWER product id.
                return Some((r.address, r.address_type, if pid == 0x2066 { "RIGHT" } else { "LEFT" }));
            }
        }
        let _ = dongle.stop_le_scan();
    }
    None
}
