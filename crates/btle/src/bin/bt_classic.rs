//! BR/EDR (Bluetooth Classic) probe — stage 1 of classic gamepad support.
//!
//! # Why this exists before any of the interesting code
//!
//! Classic Bluetooth is a **second radio protocol**, not an extension of the LE
//! stack this crate already has. Everything built so far — scan, connect,
//! ATT/GATT, LE encryption — talks to devices that advertise continuously and
//! are reached by address on a fixed channel. A classic device is silent until
//! the host asks the whole room at once (INQUIRY), is connected by PAGING with
//! its clock offset, and then speaks L2CAP channels negotiated through SDP.
//! Sharing a USB transport and an event loop is most of what the two have in
//! common.
//!
//! That makes the first question not "how do we pair a controller" but two much
//! smaller ones, and both have to be answered on real hardware before the rest
//! is worth writing:
//!
//! 1. **Can this dongle do BR/EDR at all?** Many USB Bluetooth adapters are
//!    dual-mode, but not all, and an LE-only one answers every classic command
//!    with `Unknown HCI Command` — indistinguishable from a bug in our command
//!    encoding unless the capability was checked first.
//! 2. **Does an inquiry actually find a controller?** A gamepad only answers
//!    while it is discoverable (pairing mode), and its Class of Device says
//!    whether it presents as a gamepad. Seeing a real controller's address and
//!    class in a list is the milestone that makes paging worth implementing.
//!
//! ❗ This probe establishes NO lasting connection and pairs with nothing. It
//! does page each device it believes is a gamepad, because that is what asking
//! a name costs — a name request is a real radio conversation, not a field in
//! the inquiry reply. Nothing else is touched: headsets and phones are listed
//! from their inquiry response alone and never contacted.
//!
//! That much is worth the cost, because "some gamepad" and "Pro Controller" are
//! very different levels of confidence to start paging on.
//!
//! Usage:
//!   cargo run -p flexinput-btle --bin bt_classic
//!   cargo run -p flexinput-btle --bin bt_classic -- --secs 20
//!   cargo run -p flexinput-btle --bin bt_classic -- --pair da:2d:16:0f:01:69
//!   cargo run -p flexinput-btle --bin bt_classic -- --pair <addr> --keys D:\\sync
//!   cargo run -p flexinput-btle --bin bt_classic -- --hid <addr>
//!   cargo run -p flexinput-btle --bin bt_classic -- --watch
//!   cargo run -p flexinput-btle --bin bt_classic -- --list
//!   cargo run -p flexinput-btle --bin bt_classic -- --forget <addr>
//!
//! Several controllers can be paired to one dongle — pair each in turn and they
//! all reconnect. `--list` shows what is stored, `--forget` removes one.
//!
//! `--hid` pairs (or reuses a stored key), opens the two HID L2CAP channels and
//! prints the input reports as they arrive — the point at which a classic
//! controller is actually talking to us.
//!
//! `--pair` pages the address, completes Secure Simple Pairing, brings the link
//! up encrypted, and writes the resulting link key to `bt-classic-keys.json`.
//! ❗ Pairing REPLACES whatever host the controller was bonded to — a Switch Pro
//! paired here stops reconnecting to its console until re-paired there.
//!
//! Put the controller in pairing mode first — on a Switch Pro, hold the small
//! Sync button next to the USB port until the player lights run back and forth.

use std::collections::HashMap;

use std::time::Duration;

use flexinput_btle::{keystore, l2cap, Dongle, Event, Opcode};

const DONGLE_VID: u16 = 0x0BDA;
const DONGLE_PID: u16 = 0xA728;

/// Human-readable major device class, from the Class of Device bits.
fn major_class_name(major: u8) -> &'static str {
    match major {
        0x00 => "Miscellaneous",
        0x01 => "Computer",
        0x02 => "Phone",
        0x03 => "Network AP",
        0x04 => "Audio/Video",
        0x05 => "Peripheral",
        0x06 => "Imaging",
        0x07 => "Wearable",
        0x08 => "Toy",
        0x09 => "Health",
        _ => "Uncategorised",
    }
}

/// The peripheral minor class, which is where "gamepad" actually lives.
fn peripheral_kind(cod: [u8; 3]) -> &'static str {
    match (cod[0] >> 2) & 0x0F {
        0x01 => "joystick",
        0x02 => "gamepad",
        0x03 => "remote control",
        0x04 => "sensing device",
        0x05 => "digitiser tablet",
        0x06 => "card reader",
        _ => "other",
    }
}

/// Open the two HID channels and print input reports until interrupted.
///
/// ⭐ **Control first, then interrupt, and the order matters.** The HID profile
/// expects the control channel to exist before the interrupt one; opening them
/// the other way round is accepted by some devices and quietly refused by
/// others, which is the kind of difference that costs an evening.
///
/// ❗ SDP is deliberately skipped. It would be needed to read the device's
/// REPORT DESCRIPTOR — the map from report bytes to buttons and axes — but the
/// PSMs themselves are fixed by the profile, and FlexInput already knows the
/// Switch Pro report layout. Descriptor parsing is what a GENERIC classic pad
/// would need, and that is a separate job from proving this one works.
fn stream_hid(dongle: &Dongle, conn: u16) {
    let mut log = |m: &str| println!("[bt]   {m}");
    let control = match dongle.l2cap_connect(conn, l2cap::PSM_HID_CONTROL, 0x0040, &mut log) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[bt] ⛔ HID control channel failed: {e}");
            return;
        }
    };
    let interrupt = match dongle.l2cap_connect(conn, l2cap::PSM_HID_INTERRUPT, 0x0041, &mut log) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[bt] ⛔ HID interrupt channel failed: {e}");
            return;
        }
    };
    println!(
        "[bt] ✅ HID channels open — control {:#06x}->{:#06x}, interrupt {:#06x}->{:#06x}",
        control.local_cid, control.remote_cid, interrupt.local_cid, interrupt.remote_cid,
    );
    println!("[bt] press buttons — reports below. Ctrl+C to stop.\n");

    let mut count = 0usize;
    let mut last = Vec::new();
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_secs(60) {
        let Ok(Some(pkt)) = dongle.read_acl(std::time::Duration::from_millis(200)) else {
            continue;
        };
        if pkt.conn_handle != conn {
            continue;
        }
        if pkt.cid == l2cap::CID_SIGNALLING {
            // Keep answering configuration and disconnection traffic, or the
            // controller tears the channels down mid-stream.
            if let Some(sig) = l2cap::parse_signal(&pkt.payload) {
                if sig.code == l2cap::SIG_DISCONNECTION_REQUEST {
                    println!("[bt] remote closed a channel — stopping");
                    break;
                }
            }
            continue;
        }
        if pkt.cid != interrupt.local_cid {
            continue;
        }
        count += 1;
        // Only print CHANGES after the first few: a controller at rest repeats
        // the same report at its full rate, and a scrolling wall of identical
        // bytes hides the one thing being looked for.
        if count <= 3 || pkt.payload != last {
            println!(
                "  #{count:<5} {:>3} bytes  {:02x?}",
                pkt.payload.len(),
                &pkt.payload[..pkt.payload.len().min(32)],
            );
            last = pkt.payload.clone();
        }
    }
    println!("\n[bt] {count} input report(s) in 60 s");
    if count == 0 {
        println!("     Channels opened but nothing arrived. On a Switch Pro that");
        println!("     usually means it is waiting for a Set Report to start full");
        println!("     input mode — the next thing to implement.");
    }
}

/// Live trace: page-scan, accept anything paired, and narrate every step.
///
/// ⭐ **Built because reasoning about this stopped working.** Six rounds of
/// fixing one symptom at a time produced six builds that did not work, because
/// every layer failed silently and the only signal was "it does not connect".
/// This prints what the radio actually reports — every HCI event, every L2CAP
/// step — so a failure names itself instead of being guessed at.
fn watch(dongle: &Dongle) {
    use std::time::Instant;
    let t0 = Instant::now();
    let say = |m: String| {
        println!("[{:>6.1}s] {m}", t0.elapsed().as_secs_f32());
        // Flushed so a live trace is live rather than block-buffered.
        use std::io::Write;
        let _ = std::io::stdout().flush();
    };

    let known = keystore::load();
    say(format!("key store: {}", keystore::path().display()));
    if known.is_empty() {
        say("⛔ NOTHING PAIRED — an incoming link will be refused.".into());
    }
    for (a, p) in &known {
        say(format!("paired: {a}  {}", p.name.clone().unwrap_or_default()));
    }
    if let Ok(a) = dongle.read_bd_addr() {
        say(format!("this dongle: {}", keystore::format_addr(a)));
    }

    // Clear anything a previous run stranded, then listen.
    for h in 1u16..=8 {
        let _ = dongle.disconnect(h);
    }
    std::thread::sleep(Duration::from_millis(300));
    let _ = dongle.drain_events();
    match dongle.set_scan_enable(0x02) {
        Ok(()) => say("page scan ENABLED — the controller can now call us".into()),
        Err(e) => say(format!("⛔ page scan FAILED: {e}")),
    }
    let _ = dongle.set_page_timeout(8.0);

    let mut links: Vec<(u16, [u8; 6])> = Vec::new();
    let mut hid: Vec<(u16, u16)> = Vec::new(); // (conn, interrupt local cid)
    let mut reports = 0usize;
    let mut last_report = Instant::now();

    say("listening — switch the controller ON now".into());
    loop {
        // Events
        while let Ok(Some(evt)) = dongle.read_event_timeout(Duration::from_millis(2)) {
            match evt {
                Event::ConnectionRequest { address, class_of_device, .. } => {
                    let a = keystore::format_addr(address);
                    say(format!("⭐ INCOMING from {a} (class {class_of_device:02x?})"));
                    if !known.contains_key(&a) {
                        say(format!("   ⛔ {a} is NOT in the key store — ignoring"));
                        continue;
                    }
                    match dongle.accept_connection(address) {
                        Ok(()) => say("   accepting…".into()),
                        Err(e) => say(format!("   ⛔ accept failed: {e}")),
                    }
                }
                Event::ConnectionComplete { status, conn_handle, address } => {
                    let a = keystore::format_addr(address);
                    if status == 0 {
                        say(format!("   ✅ link up: {a} handle {conn_handle:#06x}"));
                        links.push((conn_handle, address));
                        let _ = dongle
                            .send_command(Opcode::AUTHENTICATION_REQUESTED,
                                          &conn_handle.to_le_bytes());
                    } else {
                        say(format!("   ⛔ link FAILED for {a}: status {status:#04x}"));
                    }
                }
                Event::LinkKeyRequest { address } => {
                    let a = keystore::format_addr(address);
                    match known.get(&a) {
                        Some(p) => {
                            say(format!("   key requested for {a} — supplying"));
                            let mut d = Vec::new();
                            d.extend_from_slice(&address);
                            d.extend_from_slice(&p.key);
                            let _ = dongle.command_sync(Opcode::LINK_KEY_REQUEST_REPLY, &d);
                        }
                        None => {
                            say(format!("   ⛔ key requested for {a} — WE HAVE NONE"));
                            let _ = dongle.command_sync(
                                Opcode::LINK_KEY_REQUEST_NEGATIVE_REPLY, &address);
                        }
                    }
                }
                Event::IoCapabilityRequest { address } => {
                    say("   io-capability asked — NoInputNoOutput".into());
                    let mut d = Vec::new();
                    d.extend_from_slice(&address);
                    d.extend_from_slice(&[0x03, 0x00, 0x00]);
                    let _ = dongle.command_sync(Opcode::IO_CAPABILITY_REQUEST_REPLY, &d);
                }
                Event::UserConfirmationRequest { address, .. } => {
                    say("   confirming pairing".into());
                    let _ = dongle
                        .command_sync(Opcode::USER_CONFIRMATION_REQUEST_REPLY, &address);
                }
                Event::LinkKeyNotification { address, key, .. } => {
                    let a = keystore::format_addr(address);
                    say(format!("   ⭐ NEW link key for {a} — saving"));
                    let _ = keystore::put(address, key, None, dongle.read_bd_addr().ok());
                }
                Event::AuthenticationComplete { status, conn_handle } => {
                    if status == 0 {
                        say("   authenticated — encrypting".into());
                        let mut d = Vec::new();
                        d.extend_from_slice(&conn_handle.to_le_bytes());
                        d.push(0x01);
                        let _ = dongle.send_command(Opcode::SET_CONNECTION_ENCRYPTION, &d);
                    } else {
                        say(format!("   ⛔ AUTH FAILED: status {status:#04x} \
                                     (the stored key is probably not this dongle's)"));
                    }
                }
                Event::EncryptionChange { status, conn_handle: _, enabled } => {
                    if status == 0 && enabled != 0 {
                        // ⭐ We do NOT open the channels. The controller called
                        // US, so it is the initiator and will send its own
                        // L2CAP Connection Requests — see the signalling
                        // handler below. Sending ours here is what deadlocked
                        // the link until it timed out.
                        say("   ✅ encrypted — waiting for the controller to open \
                             its HID channels".into());
                    } else {
                        say(format!("   ⛔ ENCRYPTION FAILED: status {status:#04x}"));
                    }
                }
                Event::DisconnectionComplete { conn_handle, reason } => {
                    say(format!("   link {conn_handle:#06x} DROPPED (reason {reason:#04x})"));
                    links.retain(|(h, _)| *h != conn_handle);
                    hid.retain(|(h, _)| *h != conn_handle);
                }
                Event::Other { code, .. } => {
                    if code != 0x13 && code != 0x0e && code != 0x0f {
                        say(format!("   (event {code:#04x})"));
                    }
                }
                _ => {}
            }
        }
        // ACL
        while let Ok(Some(pkt)) = dongle.read_acl(Duration::from_millis(2)) {
            if let Some((_, cid)) = hid.iter().find(|(h, c)| *h == pkt.conn_handle && *c == pkt.cid) {
                let _ = cid;
                reports += 1;
                if reports == 1 || reports % 500 == 0 {
                    say(format!("   📥 input report #{reports} ({} bytes)", pkt.payload.len()));
                }
                last_report = Instant::now();
            } else if pkt.cid == l2cap::CID_SIGNALLING {
                if let Some(sig) = l2cap::parse_signal(&pkt.payload) {
                    say(format!("   l2cap signal code {:#04x} on {:#06x}",
                                sig.code, pkt.conn_handle));
                    // ⭐ The device asking for a channel. Grant it, give it one
                    // of our ids, and immediately configure our side.
                    if sig.code == l2cap::SIG_CONNECTION_REQUEST {
                        if let Some((psm, their_cid)) =
                            l2cap::parse_connection_request(&sig.data)
                        {
                            let our_cid = 0x0040 + (hid.len() as u16) * 2
                                + if psm == l2cap::PSM_HID_INTERRUPT { 1 } else { 0 };
                            say(format!(
                                "   ⭐ device wants PSM {psm:#06x} (its cid {their_cid:#06x}) \
                                 — granting cid {our_cid:#06x}"));
                            let resp = l2cap::encode_signal(
                                l2cap::SIG_CONNECTION_RESPONSE, sig.identifier,
                                &l2cap::connection_response(our_cid, their_cid, 0));
                            let _ = dongle.send_att_raw(
                                pkt.conn_handle, l2cap::CID_SIGNALLING, &resp);
                            // Configure our side straight away.
                            let cfg = l2cap::encode_signal(
                                l2cap::SIG_CONFIGURE_REQUEST, sig.identifier.wrapping_add(1),
                                &l2cap::configure_request(their_cid, 672));
                            let _ = dongle.send_att_raw(
                                pkt.conn_handle, l2cap::CID_SIGNALLING, &cfg);
                            if psm == l2cap::PSM_HID_INTERRUPT {
                                hid.push((pkt.conn_handle, our_cid));
                                say("   ✅ interrupt channel granted — expecting input".into());
                            }
                        }
                    } else if sig.code == l2cap::SIG_CONFIGURE_REQUEST {
                        if let Some((dest, opts)) = l2cap::parse_configure_request(&sig.data) {
                            let reply = l2cap::encode_signal(
                                l2cap::SIG_CONFIGURE_RESPONSE, sig.identifier,
                                &l2cap::configure_response(dest, &opts));
                            let _ = dongle.send_att_raw(
                                pkt.conn_handle, l2cap::CID_SIGNALLING, &reply);
                            say("   → answered config request".into());
                        }
                    } else if sig.code == l2cap::SIG_DISCONNECTION_REQUEST {
                        let reply = l2cap::encode_signal(
                            l2cap::SIG_DISCONNECTION_RESPONSE, sig.identifier, &sig.data);
                        let _ = dongle.send_att_raw(
                            pkt.conn_handle, l2cap::CID_SIGNALLING, &reply);
                        say("   → answered disconnect request".into());
                    }
                }
            } else {
                say(format!("   acl on cid {:#06x} handle {:#06x} ({} bytes)",
                            pkt.cid, pkt.conn_handle, pkt.payload.len()));
            }
        }
        if !hid.is_empty() && last_report.elapsed() > Duration::from_secs(5) {
            say("   ⚠ channel open but NO INPUT for 5 s".into());
            last_report = Instant::now();
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let secs: f32 = args
        .iter()
        .position(|a| a == "--secs")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(10.0);

    // Managing the store needs no radio at all, so it happens before the
    // dongle is opened — which matters when the Joy-Con 2 hub is holding it.
    if let Some(d) = args.iter().position(|a| a == "--keys").and_then(|i| args.get(i + 1)) {
        keystore::set_dir(Some(std::path::PathBuf::from(d)));
    }
    if args.iter().any(|a| a == "--list") {
        let all = keystore::load();
        println!("[bt] key store: {}", keystore::path().display());
        if all.is_empty() {
            println!("[bt] nothing paired yet.");
        }
        for (addr, p) in &all {
            // First four bytes only: enough to tell two entries apart, without
            // printing a shared secret in full to a terminal or a screenshot.
            let head: String = p.key[..4].iter().map(|b| format!("{b:02x}")).collect();
            let name = p.name.clone().unwrap_or_else(|| "(no name recorded)".into());
            println!("  {addr}  {name:<24}  key {head}…");
        }
        return;
    }
    if let Some(addr) = args
        .iter()
        .position(|a| a == "--forget")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| keystore::parse_addr(v))
    {
        match keystore::forget(addr) {
            Ok(true) => println!(
                "[bt] {} removed. ❗ The CONTROLLER still has its side of the \n\
                 [bt] bond — re-pair it to use it again.",
                keystore::format_addr(addr)
            ),
            Ok(false) => println!("[bt] {} was not in the store.", keystore::format_addr(addr)),
            Err(e) => eprintln!("[bt] could not update the store: {e}"),
        }
        return;
    }

    let hid_target = args
        .iter()
        .position(|a| a == "--hid")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| keystore::parse_addr(v));
    let pair_target = args
        .iter()
        .position(|a| a == "--pair")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| keystore::parse_addr(v));

    let dongle = match Dongle::open(DONGLE_VID, DONGLE_PID) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[bt] cannot open dongle: {e}");
            eprintln!("     Is it bound to WinUSB (Zadig), and not in use by the app?");
            std::process::exit(1);
        }
    };
    if let Err(e) = dongle.reset_and_init() {
        eprintln!("[bt] init failed: {e}");
        std::process::exit(1);
    }

    // ── 1. Capability ────────────────────────────────────────────────────
    match dongle.supports_bredr() {
        Ok(true) => println!("[bt] ✅ this dongle reports BR/EDR support"),
        Ok(false) => {
            println!("[bt] ⛔ this dongle is LE-ONLY — it reports BR/EDR Not Supported.");
            println!("     Classic gamepads cannot be paired with it at any amount of");
            println!("     effort; a dual-mode adapter is required.");
            std::process::exit(2);
        }
        Err(e) => {
            eprintln!("[bt] could not read local features: {e}");
            std::process::exit(1);
        }
    }
    if let Ok(addr) = dongle.read_bd_addr() {
        println!("[bt] local address {}", keystore::format_addr(addr));
    }
    // ⭐ Answer incoming pages for the whole session. A bonded controller calls
    // its host rather than waiting to be called, and `HCI_Reset` leaves this
    // off — which is what made an already-paired pad blink and search forever.
    if let Err(e) = dongle.set_scan_enable(0x02) {
        println!("[bt] (page scan not enabled: {e})");
    }

    if args.iter().any(|a| a == "--watch") {
        watch(&dongle);
        return;
    }

    // ── 2. Pair, if asked ────────────────────────────────────────────────
    //
    // An inquiry is still run first even when pairing: paging needs the page
    // scan repetition mode and clock offset, and those come from an inquiry
    // response. A remembered address is not enough on its own.
    if let Some(target) = hid_target.or(pair_target) {
        let stream = hid_target.is_some();
        println!("[bt] looking for {} to pair with…", keystore::format_addr(target));
        println!("[bt] key store: {}", keystore::path().display());
        // Same early stop as the app: page while the controller is still
        // listening for one, rather than after the whole inquiry has run.
        let found = dongle
            .inquiry_until(secs, &mut |r| r.address == target)
            .unwrap_or_default();
        let Some(r) = found.iter().find(|r| r.address == target) else {
            eprintln!("[bt] {} did not answer the inquiry.", keystore::format_addr(target));
            eprintln!("     It must be in PAIRING MODE — hold the Sync button.");
            std::process::exit(1);
        };
        let known = keystore::get(target);
        if known.is_some() {
            println!("[bt] a link key for this device is already stored — reusing it");
        }
        let mut log = |m: &str| println!("[bt]   {m}");
        match dongle.page_and_pair(
            target,
            r.page_scan_repetition_mode,
            r.clock_offset,
            known,
            // Someone has to walk over and hold the Sync button.
            std::time::Duration::from_secs(25),
            &mut log,
        ) {
            Ok(link) => {
                println!("[bt] ✅ paired and encrypted, handle {:#06x}", link.conn_handle);
                if let Some(k) = link.link_key {
                    if known != Some(k) {
                        // ⭐ Ask the controller its name WHILE it is connected.
                        // This is the only moment it is cheap — the link is
                        // already up — and a stored name is what makes the
                        // pairing list readable when everything is switched
                        // off later.
                        let name = dongle
                            .remote_name(target, r.page_scan_repetition_mode, r.clock_offset)
                            .ok()
                            .filter(|n| !n.is_empty());
                        if let Some(n) = &name {
                            println!("[bt] name: {n:?}");
                        }
                        let adapter = dongle.read_bd_addr().ok();
                        match keystore::put(target, k, name.as_deref(), adapter) {
                            Ok(p) => println!("[bt] link key saved to {}", p.display()),
                            // ❗ Loud, because the bond has ALREADY replaced the
                            // controller's previous host. A key that failed to
                            // save means re-pairing, and silence here would let
                            // that be discovered later as a mystery.
                            Err(e) => eprintln!(
                                "[bt] ⛔ COULD NOT SAVE THE LINK KEY: {e}\n                                 [bt]    The controller is paired but this host will \
                                 not remember it."
                            ),
                        }
                    }
                }
                if stream {
                    stream_hid(&dongle, link.conn_handle);
                } else {
                    println!("[bt] Re-run with --hid to open the HID channels.");
                }
                let _ = dongle.disconnect(link.conn_handle);
                println!("[bt] link closed cleanly.");
            }
            Err(e) => {
                eprintln!("[bt] ⛔ pairing failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    // ── 3. Inquiry ───────────────────────────────────────────────────────
    //
    // Ask for RSSI first. Controllers default to the ORIGINAL inquiry-result
    // format, which has no RSSI field at all — so without this every device
    // reports "n/a" and it reads like a decode bug rather than a mode nobody
    // selected. Best-effort: a controller that refuses just keeps the old form.
    if let Err(e) = dongle.set_inquiry_mode(0x01) {
        println!("[bt] (no RSSI: {e})");
    }
    println!("[bt] inquiring for {secs:.0} s — put the controller in PAIRING MODE now");
    let found = match dongle.inquiry(secs) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[bt] inquiry failed: {e}");
            std::process::exit(1);
        }
    };

    // A device answers once per inquiry cycle, so the same address arrives
    // several times. Fold them, keeping the strongest RSSI seen — that is the
    // useful summary, and the repeat count itself says how reliably it answers.
    let mut seen: HashMap<[u8; 6], (usize, Option<i8>, [u8; 3], u8, u16)> = HashMap::new();
    for r in &found {
        let e = seen.entry(r.address).or_insert((
            0,
            r.rssi,
            r.class_of_device,
            r.page_scan_repetition_mode,
            r.clock_offset,
        ));
        e.0 += 1;
        if let (Some(best), Some(now)) = (e.1, r.rssi) {
            if now > best {
                e.1 = Some(now);
            }
        }
    }

    println!("\n[bt] {} device(s), {} raw responses:", seen.len(), found.len());
    if seen.is_empty() {
        println!("     Nothing answered. A classic device is INVISIBLE unless it is");
        println!("     discoverable, so this most likely means the controller is not");
        println!("     in pairing mode — or it reconnected to a console instead.");
    }
    let mut rows: Vec<_> = seen.into_iter().collect();
    // Gamepads first, then by signal strength: the thing being looked for
    // should not be buried among headphones and phones.
    rows.sort_by_key(|(_, (_, rssi, cod, _, _))| {
        let is_pad = (cod[1] & 0x1F) == 0x05 && matches!((cod[0] >> 2) & 0x0F, 1 | 2);
        (!is_pad, -(rssi.unwrap_or(-127) as i32))
    });
    for (addr, (hits, rssi, cod, psrm, clk)) in rows {
        let major = cod[1] & 0x1F;
        let mut what = major_class_name(major).to_string();
        if major == 0x05 {
            what = format!("Peripheral / {}", peripheral_kind(cod));
        }
        let mark = if major == 0x05 && matches!((cod[0] >> 2) & 0x0F, 1 | 2) {
            "🎮"
        } else {
            "  "
        };
        let rssi_s = rssi.map(|v| format!("{v:>4} dBm")).unwrap_or_else(|| "   n/a".into());
        // ⭐ Name only the gamepads. A name request PAGES the device — it is a
        // real radio conversation, several seconds each — so doing it for every
        // headset in the building would make the probe useless and touch
        // devices that were never the point.
        let name = if mark == "🎮" {
            match dongle.remote_name(addr, psrm, clk) {
                Ok(n) if !n.is_empty() => format!("  \"{n}\""),
                Ok(_) => "  (no name)".to_string(),
                Err(e) => format!("  (name unavailable: {e})"),
            }
        } else {
            String::new()
        };
        println!(
            "  {mark} {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}  {rssi_s}  x{hits:<3} \
             cod {cod:02x?}  {what}{name}",
            addr[0], addr[1], addr[2], addr[3], addr[4], addr[5],
        );
    }

    println!("\n[bt] To pair one of the 🎮 devices above:");
    println!("       cargo run -p flexinput-btle --bin bt_classic -- --pair <address>");
    println!("     ❗ That REPLACES the host it is currently bonded to.");
    println!("     Several controllers can share this dongle — pair each in turn.");
    let paired = keystore::load();
    if !paired.is_empty() {
        println!("\n[bt] already paired ({}): {}", paired.len(),
            paired.keys().cloned().collect::<Vec<_>>().join(", "));
    }
}
