//! Joy-Con 2 over FlexInput's own BLE stack, driving a dedicated USB dongle.
//!
//! # Why this is the preferred transport
//!
//! Windows reclaims **unpaired** BLE links on a ~30 s timer. That is measured,
//! not inferred: an HCI capture shows `HCI_Disconnect` with reason `0x16`
//! ("Terminated by Local Host") at exactly 31.1 s, while input notifications
//! were still arriving every 15 ms. Nothing available to a GATT client
//! prevents it — not `MaintainConnection`, not constant traffic, not WinRT
//! pairing, not a hand-injected registry link key. Windows also cannot bond
//! with these controllers at all: SMP ends in `Confirm Value Failed` because
//! they use a non-zero legacy TK that WinRT has no API to supply.
//!
//! Owning the radio removes the problem rather than working around it. The same
//! controller on the same machine holds **90 s at 64 Hz with zero drops** here.
//!
//! # Shape
//!
//! One thread owns the dongle and every link on it. That is deliberate: a
//! `rusb` handle is awkward to share, and interleaving scan / connect / ACL
//! drain in a single loop keeps all connection state in one place with no
//! locking beyond the snapshot the caller reads.
//!
//! Handles and the init order are documented in [`crate::protocol`] and in
//! `flexinput_btle::joycon`; both were recovered from an HCI capture of the
//! Windows stack driving the controller, so no GATT discovery is needed.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flexinput_btle::{acl, joycon as jc, Dongle, Event};

use crate::hub::{PadKey, PadState};
use crate::protocol::{self, Side};
use crate::reports::{self, OrientationTracker, PadSnapshot, StickCalib};

/// Most halves to hold at once. Two is a full pair.
const MAX_LINKS: usize = 2;


/// Spacing between the fire-and-forget init writes.
///
/// Waiting on each reply is what once turned a millisecond handshake into ~40 s
/// in the Bluetooth backend, during which the controller powered itself off
/// having never seen a completed init.
const INIT_GAP: Duration = Duration::from_millis(30);

/// How long to wait for a Write Response during init.
///
/// ⛔ **Init is a race against [`INPUT_TIMEOUT`], and I lost it.**
///
/// Acknowledged writes were introduced so a refused command would say so
/// instead of vanishing, which was right — but at 400-600 ms each, four of
/// them plus a 600 ms discovery walk put more than two seconds of blocking
/// waits in front of a three-second watchdog. Observed as controllers
/// reconnecting in a loop, and on one attempt as every single write returning
/// NO REPLY: a link too busy being set up to answer anything.
///
/// A controller that is going to acknowledge does it in milliseconds. Waiting
/// longer does not make a refusal more likely to arrive; it only makes the
/// link less likely to survive long enough to hear it.
const ATT_ACK_WAIT: Duration = Duration::from_millis(120);

/// Longest gap between input notifications before a link is written off.
const INPUT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Default)]
struct Shared {
    pads: Mutex<HashMap<PadKey, PadState>>,
    shutdown: AtomicBool,
    /// Transport state, published so the WinRT hub can stand down — see
    /// [`Joycon2DongleHub::state_flag`] and [`DONGLE_PROBING`].
    state: Arc<AtomicU8>,
    /// Set by the worker as its last act, so shutdown can tell "finished
    /// tearing the links down" from "stuck".
    finished: AtomicBool,
    /// Addresses already identified as Joy-Cons, and which half each is.
    ///
    /// ⭐ Once a controller has been recognised, it never needs recognising
    /// again. Identification depends on manufacturer data that only rides in
    /// the SCAN RESPONSE, so every reconnect otherwise waits for another one —
    /// and a Joy-Con sleeps and wakes constantly, so that wait is paid over and
    /// over. Remembering the address turns a wake into an immediate connect.
    ///
    /// Only ever populated from a report that DID carry the data, so this
    /// caches a fact the controller told us rather than a guess.
    known: Mutex<HashMap<[u8; 6], Side>>,
    /// Controllers already paired during this run.
    ///
    /// ❗ **Pairing WRITES CONTROLLER FLASH**, twice — the finalise step and the
    /// link-key commit. Without this the dongle re-ran the whole handshake on
    /// every connect, and since a Joy-Con sleeps and reconnects on a button
    /// press that meant a fresh pair of flash writes every time the user woke
    /// it. The Bluetooth hub already caches for exactly this reason and the
    /// research notes say the `0x15` commands are omitted on reconnection.
    ///
    /// In memory only, so it survives a reconnect but not an app restart —
    /// which still removes the great majority of writes.
    paired: Mutex<HashMap<[u8; 6], [u8; 16]>>,
}

/// A live connection to one half.
struct Link {
    key: PadKey,
    conn: u16,
    /// Resolved from THIS controller's table — see `resolve_handles`. Matching
    /// notifications against the captured constants instead would drop a
    /// stream that discovery had just found at a different number.
    handles: Handles,
    calib: StickCalib,
    orientation: OrientationTracker,
    /// ⏳ TEMPORARY — slow timer for the 0x000a presence line.
    common_probe: Instant,
    /// ⏳ TEMPORARY — slow timer for the whole-frame dump.
    frame_dump: Instant,
    /// Latest real gyro (and its accel) from the common input, if it streams.
    common_motion: Option<([i16; 3], Option<[i16; 3]>)>,
    last_input: Instant,
    /// Reports parsed on this link, for the backoff in the motion diagnostic.
    reports: u32,
    /// Notifications on the previously unsubscribed streams — see
    /// `jc::HANDLE_INPUT_EXTRA`.
    extra_reports: u32,
    /// Command replies seen. Logged during init and then left alone.
    replies: u32,
    /// Notifications seen on the COMMON input characteristic, which has never
    /// produced one. Counted separately so the first is unmistakable.
    common_reports: u32,
    /// Reports that failed to parse. Silent failure here reads as a dead
    /// controller everywhere downstream, so it is counted and reported.
    unparsed: u32,
}

/// Joy-Con 2 transport over a dedicated BLE dongle.
pub struct Joycon2DongleHub {
    shared: Arc<Shared>,
    /// Kept so shutdown can WAIT for the thread to tear the links down.
    ///
    /// ❗ Without this the handle was dropped on spawn, so closing the app set
    /// the shutdown flag and exited immediately — the thread was killed before
    /// it could send a single HCI Disconnect. The controller then held a link
    /// to a process that no longer existed until its supervision timeout, and
    /// the next run found a controller that was connected to nobody and would
    /// not advertise. That is a large part of "it takes several attempts".
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Default for Joycon2DongleHub {
    fn default() -> Self {
        Self::new()
    }
}

impl Joycon2DongleHub {
    /// Start the dongle thread. Harmless when no dongle is present: the open
    /// fails, one line is logged, and the thread exits.
    pub fn new() -> Self {
        let shared: Arc<Shared> = Arc::default();
        let t = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("jc2-dongle".into())
            .spawn(move || run(t))
            .expect("spawn jc2-dongle thread");
        Self {
            shared,
            thread: Mutex::new(Some(thread)),
        }
    }

    pub fn pads(&self) -> Vec<PadState> {
        let mut v: Vec<PadState> = self.shared.pads.lock().unwrap().values().cloned().collect();
        v.sort_by_key(|p| p.key);
        v
    }

    pub fn take_event_counts(&self) -> Vec<(PadKey, u32)> {
        let mut pads = self.shared.pads.lock().unwrap();
        pads.iter_mut()
            .map(|(k, p)| (*k, std::mem::take(&mut p.events)))
            .collect()
    }

    /// Live transport state for the WinRT hub to defer to.
    ///
    /// Exposed so that hub can refuse to touch Joy-Cons while the dongle owns
    /// them. A BLE peripheral accepts exactly ONE connection, so the two
    /// transports are not additive — they are rivals for the same hardware, and
    /// whichever connects first locks the other out entirely.
    pub fn state_flag(&self) -> Arc<AtomicU8> {
        // A live handle rather than a snapshot: the hub reads it on every scan
        // pass, and a dongle can appear or disappear at any time (unplugged
        // mid-session, or plugged in after the app started).
        Arc::clone(&self.shared.state)
    }

    pub fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::Relaxed);
    }
}

impl Drop for Joycon2DongleHub {
    fn drop(&mut self) {
        self.shutdown();
        // Wait for the teardown. The loop tests the shutdown flag every
        // iteration and its longest sleep is 50 ms, so this costs a fraction of
        // a second on exit and buys a controller that is actually disconnected
        // rather than one still holding a link to a dead process.
        // ❗ BOUNDED. A plain `join()` here hangs the whole application when the
        // worker is stuck — which is exactly the state worth exiting from, and
        // the state the user hit: a controller mid-init, the thread blocked,
        // and closing FlexInput simply never completing.
        //
        // Waiting on a flag rather than the thread means a healthy shutdown
        // still tears the links down properly, and an unhealthy one costs a
        // second and then lets go. Leaking a detached thread at process exit is
        // free; hanging is not.
        let deadline = Instant::now() + Duration::from_millis(1200);
        while !self.shared.finished.load(Ordering::Relaxed) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if self.shared.finished.load(Ordering::Relaxed) {
            if let Some(t) = self.thread.lock().unwrap().take() {
                let _ = t.join();
            }
        } else {
            eprintln!("[jc2-dongle] worker did not finish in time — leaving it detached");
        }
    }
}

/// Feature mask to send during init, or `None` to skip the command entirely.
///
/// Defaults to sending [`protocol::feature::JOYCON2_DEFAULT`]. Skipping it was
/// tried, on the grounds that no working reference sends one to enable motion,
/// and this controller then withholds motion completely — see the body.
///
/// `FLEXINPUT_JC2_FEATURES` takes a hex byte (`2f`, `37`, `10`, `04`, …) to try
/// a different mask, or `off` to skip the command, without a rebuild.
fn feature_override() -> Option<u8> {
    let Ok(raw) = std::env::var("FLEXINPUT_JC2_FEATURES") else {
        // ⭐ Default is to SEND it. Skipping was tried and the controller then
        // reports `motion_len = 0` forever: input notifications keep arriving
        // with the counter climbing, but the motion block is absent entirely.
        // So on this hardware the feature command is what enables motion at
        // all — it is not merely selecting a format.
        return Some(protocol::feature::JOYCON2_DEFAULT);
    };
    let raw = raw.trim().trim_start_matches("0x");
    if raw.eq_ignore_ascii_case("off") || raw.is_empty() {
        return None;
    }
    match u8::from_str_radix(raw, 16) {
        Ok(v) => Some(v),
        Err(_) => {
            eprintln!("[jc2-dongle] FLEXINPUT_JC2_FEATURES={raw:?} is not a hex byte — skipping");
            None
        }
    }
}

/// Walk the controller's whole attribute table and print it.
///
/// ⭐ **Looking for a service nobody knew to look for.** Decompiling the vendor's
/// own configuration app shows it does not use the Nintendo service at all. It
/// talks to its controllers over a private GATT service:
///
/// * service `0000FF00-0000-1000-8000-00805F9B34FB`
/// * write   `0000FF01-…`
/// * notify  `0000FF02-…`
///
/// with frames `[len][cmd][content…][SN]`, `len = content.len() + 3`. Its
/// command `0x6A` carries a family of motion sub-commands — mapping type,
/// axis ratio, dead zone, XY reversal — and separate gyroscope alignment
/// commands at `0x41`/`0x42`/`0x43`.
///
/// Whether this controller exposes that service while it is running the Switch 2
/// BLE profile is unknown and is exactly what this prints. If it does, there is
/// a whole command channel available that this project has never spoken to, and
/// "motion mapping type" is the most promising switch left.
///
/// Off by default because a full walk costs a second of init and normal use does
/// not need it. `FLEXINPUT_JC2_GATT_SCAN=1` turns it on.
fn scan_gatt(dongle: &Dongle, conn: u16) {
    if std::env::var("FLEXINPUT_JC2_GATT_SCAN").is_err() {
        return;
    }
    eprintln!("[jc2-dongle] GATT scan: walking the attribute table");
    // Let the MTU exchange settle and clear anything already queued, so the
    // first response we match is genuinely ours. Reading a stale packet is what
    // made the previous version stop after a single attribute and then report
    // "not found" from a walk that had enumerated one handle.
    std::thread::sleep(Duration::from_millis(200));
    let _ = dongle.drain_acl(256);

    let mut start = 0x0001u16;
    let mut found_vendor = false;
    let mut seen = 0usize;
    // Characteristic-declaration handles (UUID 0x2803), read for properties
    // after the walk finishes — reading mid-walk would interleave responses.
    let mut decls: Vec<u16> = Vec::new();
    // Ask one handle at a time rather than open-ended. It costs more round
    // trips but every reply is unambiguous, and an unsupported or empty range
    // cannot be mistaken for the end of the table.
    while start < 0x0100 {
        if dongle
            .send_att(conn, &acl::find_information_request(start, 0x00FF))
            .is_err()
        {
            break;
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut infos: Option<Vec<acl::AttrInfo>> = None;
        let mut att_error = false;
        while Instant::now() < deadline && infos.is_none() && !att_error {
            for pkt in dongle.drain_acl(64) {
                if pkt.cid != acl::CID_ATT || pkt.payload.is_empty() {
                    continue;
                }
                // An "Attribute Not Found" error IS the end of the table, and
                // is the only reliable terminator — distinguishing it from
                // silence matters, because silence means the scan is broken.
                if pkt.payload[0] == acl::ATT_ERROR_RESPONSE {
                    eprintln!("[jc2-dongle] GATT scan: end of table at {start:#06x}");
                    att_error = true;
                    break;
                }
                if let Some(v) = acl::parse_find_information_response(&pkt.payload) {
                    if !v.is_empty() {
                        infos = Some(v);
                        break;
                    }
                }
            }
        }
        if att_error {
            break;
        }
        let Some(infos) = infos else {
            eprintln!("[jc2-dongle] GATT scan: NO REPLY past {start:#06x} — scan incomplete");
            break;
        };
        for info in &infos {
            let mark = match info.uuid {
                acl::AttUuid::Short(u) if (0xFF00..=0xFF1F).contains(&u) => "   <== VENDOR SERVICE",
                acl::AttUuid::Long(_) => "   (128-bit)",
                _ => "",
            };
            if mark.starts_with("   <==") {
                found_vendor = true;
            }
            if matches!(info.uuid, acl::AttUuid::Short(0x2803)) {
                decls.push(info.handle);
            }
            eprintln!("[jc2-dongle]   {:#06x}  {:?}{mark}", info.handle, info.uuid);
            seen += 1;
        }
        let last = infos.last().map(|i| i.handle).unwrap_or(start);
        if last < start {
            break;
        }
        start = last.saturating_add(1);
    }
    // ⭐ Now READ every characteristic declaration and print its properties.
    //
    // Enumerating handles says what exists; it does not say what any of it can
    // DO. Subscribing `0x0026` and `0x0022` produced no notifications, and with
    // fire-and-forget CCCD writes there is no way to tell "the stream is idle"
    // from "this characteristic does not support notify and the write was
    // rejected". The declaration answers it directly: one byte of property bits
    // per characteristic, from the controller itself.
    //
    // Declaration value is `[properties][value handle LE][uuid]`.
    for decl in decls {
        if dongle.send_att(conn, &acl::read_request(decl)).is_err() {
            continue;
        }
        let deadline = Instant::now() + Duration::from_millis(300);
        let mut value: Option<Vec<u8>> = None;
        while Instant::now() < deadline && value.is_none() {
            for pkt in dongle.drain_acl(64) {
                if pkt.cid == acl::CID_ATT {
                    if let Some(v) = acl::parse_read_response(&pkt.payload) {
                        value = Some(v);
                        break;
                    }
                }
            }
        }
        let Some(v) = value else { continue };
        if v.len() < 3 {
            continue;
        }
        let props = v[0];
        let vh = u16::from_le_bytes([v[1], v[2]]);
        let mut names = Vec::new();
        for (bit, name) in [
            (acl::PROP_READ, "READ"),
            (acl::PROP_WRITE_NO_RESPONSE, "WRITE_NR"),
            (acl::PROP_WRITE, "WRITE"),
            (acl::PROP_NOTIFY, "NOTIFY"),
        ] {
            if props & bit != 0 {
                names.push(name);
            }
        }
        eprintln!(
            "[jc2-dongle]   decl {decl:#06x} -> value {vh:#06x}  props {props:#04x} [{}]",
            names.join("|"),
        );
    }

    eprintln!(
        "[jc2-dongle] GATT scan done — {seen} attributes, Mobapad vendor service {}",
        if found_vendor {
            "PRESENT"
        } else if seen < 8 {
            "UNKNOWN (scan enumerated too little to say)"
        } else {
            "not found"
        },
    );
}

/// Send one command on the working per-side handle and wait for its reply.
///
/// The rest of init is deliberately fire-and-forget — waiting on each reply
/// once turned a millisecond handshake into ~40 s in the Bluetooth backend, and
/// the controller powered itself off before it finished. Pairing is the one
/// sequence that CANNOT work that way: each step consumes the previous step's
/// response, so there is nothing to do but wait.
///
/// Replies arrive on [`jc::HANDLE_CMD_RESPONSE`] (`0x001a`), which is already
/// subscribed. Matching is on the command id, not merely on "something
/// arrived": input notifications and vendor events share this drain.
fn cmd_and_wait(
    dongle: &Dongle,
    conn: u16,
    cmd: u8,
    sub: u8,
    data: &[u8],
    timeout: Duration,
) -> Option<Vec<u8>> {
    let frame = protocol::rumble_cmd_frame(cmd, sub, data);
    dongle
        .send_att(conn, &acl::write_command(jc::HANDLE_CMD_WRITE, &frame))
        .ok()?;

    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        for pkt in dongle.drain_acl(64) {
            if pkt.cid != acl::CID_ATT {
                continue;
            }
            let Some(n) = acl::parse_notification(&pkt.payload) else { continue };
            if n.handle != jc::HANDLE_CMD_RESPONSE {
                continue;
            }
            if let Some((hdr, body)) =
                protocol::parse_response(&n.value, protocol::CMD_RESP_HEADER_OFFSET, cmd)
            {
                if hdr.subcmd == sub {
                    return Some(body.to_vec());
                }
            }
        }
    }
    None
}

/// Run the LTK pairing handshake over the dongle.
///
/// ⭐ **The dongle path has never done this, and the Windows path always has.**
/// That asymmetry is worth closing on its own — a controller paired over one
/// transport and not the other behaves differently for reasons nothing in the
/// code explains — but the specific reason to try it now is that two
/// characteristics declare `[READ|NOTIFY]` and refuse both:
///
/// ```text
///   0x000a  ab7de9be-…-7fd2   Read Not Permitted, never notifies
///   0x0026  ab7de9be-…-7fde   Read Not Permitted, never notifies
/// ```
///
/// A peripheral gating attributes behind an authenticated relationship is
/// exactly what that looks like, and the transport that HAS such a relationship
/// is the one we never gave those attributes to. This is the cheapest way to
/// find out, and it costs one handshake.
///
/// ❗ **The finalise step writes controller flash**, which is why this is opt-out
/// rather than unconditional: `FLEXINPUT_JC2_DONGLE_PAIR=off` skips it. A real
/// Joy-Con buzzes when it lands, which is a better success signal than any log
/// line — the Windows path produces that buzz and this one never has.
///
/// Failure is non-fatal at every step. Pairing is an enhancement to a link that
/// already streams input, and a controller that refuses it must still work.
fn run_pairing(dongle: &Dongle, conn: u16, side: Side) -> Option<[u8; 16]> {
    use protocol::{CMD_PAIRING, SUB_PAIR_CONFIRM_LTK, SUB_PAIR_EXCHANGE_ADDRS,
                   SUB_PAIR_EXCHANGE_KEYS, SUB_PAIR_FINALISE};
    use crate::pairing;

    if std::env::var("FLEXINPUT_JC2_DONGLE_PAIR")
        .is_ok_and(|v| v.eq_ignore_ascii_case("off") || v == "0")
    {
        eprintln!("[jc2-dongle] pairing SKIPPED (FLEXINPUT_JC2_DONGLE_PAIR=off)");
        return None;
    }

    let name = side.display_name();
    // ❗ A synthetic host rather than giving up. `read_bd_addr` has failed on
    // this dongle before, and skipping the whole handshake over it would answer
    // nothing — the question is whether pairing changes what the controller
    // exposes, and the controller only stores whatever host address it is
    // handed. A made-up one still exercises every step; it only matters for
    // reconnect, where the pad advertises the host it is bonded to.
    let host = match dongle.read_bd_addr() {
        Ok(a) => a,
        Err(e) => {
            eprintln!(
                "[jc2-dongle] {name} pairing: cannot read local BD_ADDR ({e}) —                  using a synthetic host, reconnect binding will be wrong"
            );
            [0x02, 0x00, 0x00, 0xFE, 0xED, 0x01]
        }
    };
    eprintln!("[jc2-dongle] {name} pairing: host {host:02x?}");
    let wait = Duration::from_millis(800);

    // 1. Addresses.
    //
    // ❗ Every step reports its own failure. These used to be bare `?`, which
    // returned with nothing logged — so a handshake that died at step one was
    // indistinguishable from one that never ran, and "no authentication
    // happening" could not be told from "authentication refused".
    let Some(resp) = cmd_and_wait(
        dongle, conn, CMD_PAIRING, SUB_PAIR_EXCHANGE_ADDRS,
        &pairing::exchange_addresses_data(&[host]), wait,
    ) else {
        eprintln!("[jc2-dongle] {name} PAIRING FAILED: no reply to address exchange");
        return None;
    };
    if let Some(addr) = pairing::parse_controller_address(&resp) {
        eprintln!("[jc2-dongle] {name} pairing: controller address {addr:02x?}");
    }

    // 2. Keys. A1 is arbitrary; `uuid` v4 is already a dependency and is backed
    //    by a proper CSPRNG, so it doubles as the random source.
    let a1: [u8; 16] = *uuid::Uuid::new_v4().as_bytes();
    let Some(resp) = cmd_and_wait(
        dongle, conn, CMD_PAIRING, SUB_PAIR_EXCHANGE_KEYS,
        &pairing::exchange_keys_data(&a1), wait,
    ) else {
        eprintln!("[jc2-dongle] {name} PAIRING FAILED: no reply to key exchange");
        return None;
    };
    let Some(b1) = pairing::parse_key_response(&resp) else {
        eprintln!("[jc2-dongle] {name} PAIRING FAILED: malformed key response {resp:02x?}");
        return None;
    };
    let ltk = pairing::derive_ltk(&a1, &b1);

    // 3. Challenge / confirmation.
    let a2: [u8; 16] = *uuid::Uuid::new_v4().as_bytes();
    let Some(resp) = cmd_and_wait(
        dongle, conn, CMD_PAIRING, SUB_PAIR_CONFIRM_LTK,
        &pairing::confirm_ltk_data(&a2), wait,
    ) else {
        eprintln!("[jc2-dongle] {name} PAIRING FAILED: no reply to LTK challenge");
        return None;
    };
    match pairing::parse_key_response(&resp) {
        Some(b2) => match pairing::check_confirmation(
            &pairing::expected_confirmation(&ltk, &a2),
            &b2,
        ) {
            pairing::Confirmation::Match => {
                eprintln!("[jc2-dongle] {name} pairing: LTK confirmed")
            }
            pairing::Confirmation::MatchReversed => {
                eprintln!("[jc2-dongle] {name} pairing: LTK confirmed (byte-reversed)")
            }
            // Advisory, not fatal: the controller decides whether pairing is
            // accepted, and a mismatch most likely means our byte-order reading
            // of the exchange is off rather than that the LTK is wrong.
            pairing::Confirmation::Mismatch => {
                eprintln!("[jc2-dongle] {name} pairing: LTK confirmation MISMATCH, continuing")
            }
        },
        None => eprintln!("[jc2-dongle] {name} pairing: malformed challenge response"),
    }

    // 4. Finalise — THIS writes controller flash.
    let _ = cmd_and_wait(
        dongle, conn, CMD_PAIRING, SUB_PAIR_FINALISE, &pairing::finalise_data(), wait,
    );
    // 5. Register the link key, then commit it. `0x09` is the second flash
    //    write; reconnects re-send `0x07` alone.
    let registered = cmd_and_wait(
        dongle, conn, protocol::CMD_PAIRING_EXTRA, pairing::SUB_REGISTER_LINK_KEY,
        &pairing::register_link_key_data(&host, &ltk), wait,
    );
    let committed = cmd_and_wait(
        dongle, conn, protocol::CMD_PAIRING_EXTRA, pairing::SUB_LINK_KEY_COMMIT, &[], wait,
    );
    eprintln!(
        "[jc2-dongle] {name} PAIRED — LTK {ltk:02x?} registered={} committed={} \
         (controller flash written)",
        registered.is_some(),
        committed.is_some(),
    );
    Some(ltk)
}

/// Read the readable vendor characteristics and dump them.
///
/// ⭐ **`0x0026` does not have to notify for us to see what is in it.** The
/// declaration read says `props 0x12 [READ|NOTIFY]` — the same properties as
/// the two known input streams — so a plain Read Request returns its current
/// value whether or not it is streaming. That sidesteps the whole question of
/// what starts it.
///
/// The vendor service's readable attributes, from the attribute-table walk:
///
/// ```text
///   0x000a  ab7de9be-…-7fd2   READ|NOTIFY   common input, never notifies here
///   0x0026  ab7de9be-…-7fde   READ|NOTIFY   third stream, never notifies
///   0x0002  00c5af5d-…-d281   READ          first service
///   0x0006  00c5af5d-…-d283   READ          first service
/// ```
///
/// Run AFTER init, because a stream that only fills once motion is enabled
/// would read as zeros beforehand and look empty for the wrong reason.
///
/// What to look for: this controller's accelerometer is 4096 LSB/g, so any
/// buffer holding motion shows a value near ±4096 on one axis with the pad at
/// rest. That signature is unmistakable and needs no decode.
fn probe_readable(dongle: &Dongle, conn: u16) {
    if std::env::var("FLEXINPUT_JC2_GATT_SCAN").is_err() {
        return;
    }
    let _ = dongle.drain_acl(256);
    for handle in [jc::HANDLE_INPUT_EXTRA, jc::HANDLE_INPUT_COMMON, 0x0002, 0x0006] {
        // Twice, a moment apart: a live sensor buffer changes between reads and
        // a static one does not, which distinguishes "stream that needs
        // starting" from "constant descriptor" without decoding anything.
        for pass in 0..2 {
            if dongle.send_att(conn, &acl::read_request(handle)).is_err() {
                continue;
            }
            let deadline = Instant::now() + Duration::from_millis(400);
            let mut got = None;
            while Instant::now() < deadline && got.is_none() {
                for pkt in dongle.drain_acl(64) {
                    if pkt.cid != acl::CID_ATT || pkt.payload.is_empty() {
                        continue;
                    }
                    if pkt.payload[0] == acl::ATT_ERROR_RESPONSE {
                        eprintln!("[jc2-dongle] read {handle:#06x} -> ATT error {:02x?}", pkt.payload);
                        got = Some(Vec::new());
                        break;
                    }
                    if let Some(v) = acl::parse_read_response(&pkt.payload) {
                        got = Some(v);
                        break;
                    }
                }
            }
            match got {
                Some(v) if !v.is_empty() => eprintln!(
                    "[jc2-dongle] ⭐ READ {handle:#06x} pass {pass} ({} bytes): {:02x?}",
                    v.len(),
                    &v[..v.len().min(64)],
                ),
                Some(_) => {}
                None => eprintln!("[jc2-dongle] read {handle:#06x} -> no reply"),
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }
}

/// The handles this controller actually uses, resolved from its own table.
///
/// ⛔ **Handle numbers are not part of a protocol. They are one device's
/// GATT layout, and we had them hardcoded.**
///
/// `0x000a`, `0x000e`, `0x0016` and the rest were captured by walking the
/// attribute table of ONE controller — a third-party M12-S grip — and then
/// written into `joycon.rs` as constants. A GATT server is free to lay its
/// table out however it likes; the numbers are assigned in the order attributes
/// are declared, and nothing requires two devices to agree. Subscribing by
/// number on a device with a different layout writes `CCCD_NOTIFY` to whatever
/// attribute happens to occupy that slot.
///
/// That failure is completely silent. Every write here is fire-and-forget, so
/// an `Attribute Not Found` or `Write Not Permitted` in reply goes unread, and
/// the symptom is a characteristic that was "subscribed and never notified" —
/// which is exactly what was recorded against `0x000a` on both the reference
/// grip and, in a field log, on retail Joy-Con 2s. Reported working in another
/// project on the same hardware, which is what makes it ours rather than the
/// controller's.
///
/// So the table is walked and the characteristics are found by UUID, which IS
/// part of the protocol. The constants remain as a fallback for a controller
/// that will not answer discovery at all.
#[derive(Debug, Clone, Copy)]
struct Handles {
    /// Per-side input, report 0x07/0x08 — the stream we already decode.
    input: u16,
    /// Common input, report 0x05 — where a real gyro would be.
    common: Option<u16>,
    /// Whether the common input declares NOTIFY at all.
    common_notifiable: bool,
    /// Command channel, shared with rumble.
    cmd: u16,
}

impl Default for Handles {
    fn default() -> Self {
        Self {
            input: jc::HANDLE_INPUT_VALUE,
            common: Some(jc::HANDLE_INPUT_COMMON),
            common_notifiable: true,
            cmd: jc::HANDLE_CMD_WRITE,
        }
    }
}

/// Find the characteristics by UUID, falling back to the captured numbers.
///
/// ❗ Failure is not fatal. A controller that refuses discovery still works on
/// the old constants — they were right for at least one device — so this
/// upgrades where it can and never blocks a connection.
fn resolve_handles(dongle: &Dongle, conn: u16, side: Side) -> Handles {
    let mut h = Handles::default();
    // ⭐ An escape hatch that needs no rebuild. Discovery runs during setup,
    // before the input subscription, so if it ever costs a link again this is
    // how to prove it in one run rather than one round trip.
    // ⛔ **Opt-IN, because it costs connect time and has never once
    // disagreed with the constants.**
    //
    // Across every log so far it has resolved input, cmd and common to exactly
    // the assumed handles, on two different controllers — while spending its
    // whole budget doing so. Connect time is not free: it is measured against a
    // three-second input watchdog, and everything spent here is spent before
    // the stream starts. The question it was added to answer has been answered.
    //
    // `FLEXINPUT_JC2_DISCOVER=on` brings it back for the next controller that
    // might genuinely differ.
    if !std::env::var("FLEXINPUT_JC2_DISCOVER")
        .is_ok_and(|v| v.eq_ignore_ascii_case("on"))
    {
        return h;
    }
    // ❗ Budgeted, and TIMED. This happens before the input subscription, so
    // whatever it spends is time the controller is connected and silent — and
    // the link watchdog is three seconds. See `discover_characteristics`.
    let began = Instant::now();
    // ⛔ Find Information, NOT Read By Type. A Joy-Con 2 grip does not answer
    // Read By Type at all — "no reply within 2 s", every connect — which both
    // wasted the budget and, before it was bounded, cost the link. The GATT
    // scan has always walked the table this way and always worked.
    let attrs = match dongle.discover_attributes(conn, Duration::from_millis(600)) {
        Ok(a) if !a.is_empty() => a,
        other => {
            let why = match other {
                Ok(_) => "the table was empty".to_string(),
                Err(e) => e.to_string(),
            };
            eprintln!("[jc2-dongle] {} handle discovery failed ({why}) — using captured numbers",
                side.display_name());
            crate::dlog::imu(format_args!(
                "{} handle discovery FAILED ({why}) — falling back to hardcoded \
                 handles, which may belong to a different controller's layout",
                side.display_name(),
            ));
            return h;
        }
    };

    // ⛔ **The whole table, into the log.**
    //
    // Subscribing the common input means writing CCCD_NOTIFY to
    // `HANDLE_INPUT_COMMON + 1`, on the GATT convention that a CCCD follows the
    // value it configures. That write is acknowledged every time — but a write
    // to ANY writable attribute is acknowledged, so the acknowledgement has
    // never been evidence that 0x000b is a CCCD at all. If a vendor descriptor
    // sits there instead, we have been enabling notifications on nothing and
    // being told it worked, which fits every observation so far.
    //
    // The walk already has the answer in hand; it was throwing it away and
    // keeping three handles. Printing it costs nothing and settles what 0x000b
    // and 0x000c actually are — 0x2902 is a CCCD, anything else is not.
    for a in &attrs {
        crate::dlog::imu(format_args!(
            "{} attr {:#06x} = {}",
            side.display_name(),
            a.handle,
            a.uuid,
        ));
    }

    // ⛔ **Can 0x000a notify AT ALL?**
    //
    // Everything checks out and nothing arrives: the handle is right, 0x000b
    // really is a `0x2902` CCCD, the write is acknowledged, the layout matches
    // the per-side pair that works. The one property never measured is whether
    // the characteristic declares NOTIFY in the first place — and this
    // project's own notes contradict each other on it, one calling 0x000a
    // `READ|NOTIFY` and another `Read Not Permitted`.
    //
    // A characteristic declaration (`0x2803`) sits immediately before its
    // value and holds `properties | value_handle | uuid`. Reading the two
    // declarations and comparing them answers it outright: if the common input
    // lacks bit 0x10 while the per-side one has it, it cannot notify however it
    // is configured, and every attempt to make it has been addressed to a
    // characteristic that was never going to.
    let read_decl = |value_handle: u16, what: &str| {
        let decl = value_handle.saturating_sub(1);
        if dongle.send_att(conn, &acl::read_request(decl)).is_err() {
            return;
        }
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            for pkt in dongle.drain_acl(32) {
                if pkt.cid != acl::CID_ATT || pkt.payload.is_empty() {
                    continue;
                }
                let Some(v) = acl::parse_read_response(&pkt.payload) else { continue };
                let props = v.first().copied().unwrap_or(0);
                crate::dlog::imu(format_args!(
                    "{} {what} declaration {decl:#06x}: properties {props:#04x}                      (read {} write-no-rsp {} write {} NOTIFY {} indicate {}) raw {:02x?}",
                    side.display_name(),
                    props & 0x02 != 0,
                    props & 0x04 != 0,
                    props & 0x08 != 0,
                    props & 0x10 != 0,
                    props & 0x20 != 0,
                    &v[..v.len().min(24)],
                ));
                return;
            }
        }
        crate::dlog::imu(format_args!(
            "{} {what} declaration {decl:#06x}: NO REPLY to the read",
            side.display_name()
        ));
    };
    let _ = dongle.drain_acl(64);
    read_decl(jc::HANDLE_INPUT_COMMON, "common input");
    read_decl(jc::HANDLE_INPUT_VALUE, "per-side input (known good)");

    // ⭐ The TYPE of a characteristic value attribute IS the characteristic's
    // UUID, so a Find Information walk resolves handles on its own.
    let find = |want: uuid::Uuid| -> Option<u16> {
        attrs
            .iter()
            .find(|a| match a.uuid {
                // ATT carries 128-bit UUIDs little-endian; `uuid::Uuid` bytes
                // are big-endian, so one side has to be reversed to compare.
                acl::AttUuid::Long(raw) => {
                    let mut be = raw;
                    be.reverse();
                    uuid::Uuid::from_bytes(be) == want
                }
                acl::AttUuid::Short(_) => false,
            })
            .map(|a| a.handle)
    };

    let per_side = match side {
        Side::Left => protocol::CHR_INPUT_L,
        Side::Right => protocol::CHR_INPUT_R,
    };
    if let Some(v) = find(per_side) {
        h.input = v;
    }
    let cmd = match side {
        Side::Left => protocol::CHR_RUMBLE_CMD_L,
        Side::Right => protocol::CHR_RUMBLE_CMD_R,
    };
    if let Some(v) = find(cmd) {
        h.cmd = v;
    }
    // ❗ The walk is capped at 600 ms and was spending all of it. Stopping as
    // soon as the three characteristics are in hand takes a fixed cost off
    // every connect, and connect time is what the link watchdog measures.
    match find(protocol::CHR_INPUT_COMMON) {
        Some(v) => {
            h.common = Some(v);
            // Find Information does not carry properties; the declaration walk
            // did. Not knowing is not a reason to refuse to subscribe.
            h.common_notifiable = true;
        }
        None => {
            // ⭐ Absent from the table is a different fact from present and
            // silent, and only one of them is our bug.
            h.common = None;
        }
    }

    // ⭐ Logged with the ASSUMED values beside them. A mismatch here is the
    // whole hypothesis, and it is invisible unless both numbers are shown.
    crate::dlog::imu(format_args!(
        "{} handles resolved in {} ms — input {:#06x} (assumed {:#06x}) · cmd {:#06x} \
         (assumed {:#06x}) · common {} (assumed {:#06x}){}",
        side.display_name(),
        began.elapsed().as_millis(),
        h.input,
        jc::HANDLE_INPUT_VALUE,
        h.cmd,
        jc::HANDLE_CMD_WRITE,
        match h.common {
            Some(v) => format!("{v:#06x}"),
            None => "ABSENT FROM THIS CONTROLLER'S TABLE".to_string(),
        },
        jc::HANDLE_INPUT_COMMON,
        if h.common.is_some() && !h.common_notifiable {
            " — common input declares NO NOTIFY property"
        } else {
            ""
        },
    ));
    h
}

/// `FLEXINPUT_JC2_DONGLE=vid:pid` if set, otherwise whatever is actually here.
///
/// ⛔ **This used to fall back to a literal `0bda:a728` without ever asking
/// what was plugged in.** Any adapter bound to WinUSB speaks the same HCI —
/// nothing in this stack is Realtek-specific — so hardcoding one vendor's id
/// meant the transport looked for a device most users do not own, failed to
/// open it, and announced that no dongle was available while one sat beside it
/// unopened. See `flexinput_btle::preferred_dongle`.
fn configured_dongle() -> Option<(u16, u16)> {
    let parse = || -> Option<(u16, u16)> {
        let raw = std::env::var("FLEXINPUT_JC2_DONGLE").ok()?;
        let (v, p) = raw.split_once(':')?;
        Some((
            u16::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok()?,
            u16::from_str_radix(p.trim().trim_start_matches("0x"), 16).ok()?,
        ))
    };
    parse().or_else(flexinput_btle::preferred_dongle)
}

/// Dongle transport state, shared with the WinRT hub.
///
/// Three states, not a bool, because "no dongle yet" and "no dongle at all"
/// must be told apart. Opening the device takes long enough that a bool would
/// read `false` during startup, letting the Windows stack scan and auto-connect
/// a remembered controller before the dongle ever got a chance — the exact race
/// this is here to close.
pub const DONGLE_PROBING: u8 = 0;
pub const DONGLE_ACTIVE: u8 = 1;
pub const DONGLE_ABSENT: u8 = 2;

fn run(shared: Arc<Shared>) {
    // Open the log FIRST, before anything can fail.
    //
    // It used to be created lazily on the first `dlog!`, which meant that every
    // way this thread can give up early — no dongle, open failure, a claim
    // refused — produced no file at all. Those are exactly the runs worth
    // reading, and "the log is nowhere to be found" is an unhelpful thing for
    // a diagnostic to say about itself.
    dlog!("dongle thread start");

    // Whatever happens below, the hub must eventually be released; without this
    // an early return would leave it standing down forever and Joy-Cons would
    // stop working entirely on machines with no dongle.
    struct ReleaseUnlessActive(Arc<AtomicU8>);
    impl Drop for ReleaseUnlessActive {
        fn drop(&mut self) {
            // ❗ Publishing ABSENT is not housekeeping — it is the signal that
            // lets the WinRT hub start scanning, after which Windows
            // auto-connects any remembered controller and the dongle can no
            // longer even SEE it. Announce it, because from the outside it
            // looks like "the dongle stopped working for no reason".
            if self.0.swap(DONGLE_ABSENT, Ordering::Relaxed) == DONGLE_ACTIVE {
                eprintln!(
                    "[jc2-dongle] dongle thread exiting — Joy-Cons handed back to the \
                     Windows stack, which will grab them within seconds"
                );
            }
        }
    }
    let _release = ReleaseUnlessActive(Arc::clone(&shared.state));

    let Some((vid, pid)) = configured_dongle() else {
        eprintln!(
            "[jc2-dongle] no WinUSB-bound Bluetooth adapter found — bind one              with Zadig, or set FLEXINPUT_JC2_DONGLE=vid:pid.
             [jc2-dongle] Joy-Cons will fall back to the Windows stack."
        );
        dlog!("no WinUSB-bound adapter present");
        return;
    };
    // ⭐ SHARED, not owned. This hub and the Bluetooth Classic transport run on
    // the same adapter at the same time, which is what a dual-mode radio is
    // for — see `flexinput_btle::radio`. Whichever asks first opens and
    // initialises it; the other joins. Ownership used to be a startup race
    // whose loser reported "another process holds it" about its own process.
    //
    // ❗ Reads come from `sub`; anything that sends and waits for a reply goes
    // through `radio.with_dongle`, which holds the router off for the length of
    // the conversation.
    let Some(radio) = flexinput_btle::radio::shared(vid, pid) else {
        eprintln!(
            "[jc2-dongle] no usable dongle {vid:04x}:{pid:04x} — is it bound to \
             WinUSB via Zadig?\n\
             [jc2-dongle] Joy-Cons will fall back to the Windows stack."
        );
        dlog!("no usable dongle {vid:04x}:{pid:04x}");
        return;
    };
    let sub = flexinput_btle::radio::subscribe(&radio);
    eprintln!("[jc2-dongle] dongle {vid:04x}:{pid:04x} ready (shared)");
    // From here on the WinRT hub must leave Joy-Cons alone; cleared on the way
    // out so unplugging the dongle hands them back rather than stranding them.
    shared.state.store(DONGLE_ACTIVE, Ordering::Relaxed);

    let mut links: Vec<Link> = Vec::new();
    // Scanning is a STATE, not a blocking call.
    //
    // It used to be a 2 s blocking `discover()` on this same thread, which left
    // any already-connected half completely unserviced for the whole window —
    // observed as a single controller cycling between 67 Hz and 0 Hz every few
    // seconds, and only settling once the second half connected and scanning
    // stopped. ACL now drains continuously while the scan runs.
    let mut scanning = false;
    let mut scan_retry_at = Instant::now();

    while !shared.shutdown.load(Ordering::Relaxed) {
        // ⭐ SCAN CONTINUOUSLY until every half is connected. No windowing.
        //
        // This used to scan for 2 s, stop, and rest 3 s — and with a link held
        // the radio itself only listened half of each window on top of that.
        // Roughly 20% of the time actually spent listening, against a Joy-Con
        // that advertises in a short burst after a button wake. Missing four
        // bursts out of five is the whole "it takes several attempts, and it is
        // far worse when one Joy-Con is already connected" complaint.
        //
        // The rest existed to protect live links, on the theory that scanning
        // was what dropped them. That theory was tested and disproved: links
        // still dropped at ~29 s with scanning fully disabled, so the airtime
        // was being given up for nothing. A controller can scan and hold a
        // connection at once; the cost is a little link jitter, which is
        // enormously preferable to not connecting at all.
        //
        // Stopping is now driven by state, not by a timer: scan while a half is
        // missing, stop when the pair is complete.
        let want_scan = links.len() < MAX_LINKS;
        if want_scan && !scanning && Instant::now() >= scan_retry_at {
            match radio.with_dongle(|d| d.start_le_scan_duty(true)) {
                Ok(()) => {
                    scanning = true;
                    dlog!("scan START ({} link(s) held)", links.len());
                }
                Err(e) => {
                    eprintln!("[jc2-dongle] scan enable failed: {e}");
                    dlog!("scan ENABLE FAILED: {e}");
                    // Back off briefly rather than spinning on a controller
                    // that is busy — usually a half-finished initiator, which
                    // `start_le_scan` cancels on the next attempt.
                    scan_retry_at = Instant::now() + Duration::from_millis(200);
                }
            }
        } else if !want_scan && scanning {
            let _ = radio.with_dongle(|d| d.stop_le_scan());
            scanning = false;
            dlog!("scan STOP — both halves connected");
        }

        let found = pump(&sub, &shared, &mut links, scanning);

        if let Some((addr, addr_type, side)) = found {
            // Scanning must stop while connecting: a controller cannot scan and
            // initiate at the same time.
            let _ = radio.with_dongle(|d| d.stop_le_scan());
            scanning = false;
            dlog!("connect ATTEMPT {addr:02x?} {} type {addr_type}", side.display_name());
            let t0 = Instant::now();
            match radio.with_dongle(|d| connect_and_init(&shared, d, addr, addr_type, side)) {
                // Reject a handle already in use: it means a stale Connection
                // Complete was returned rather than a new link, and both pads
                // would then mirror one controller.
                Ok(link) if links.iter().any(|l| l.conn == link.conn) => {
                    eprintln!(
                        "[jc2-dongle] {} got in-use handle {:#06x} — discarding",
                        side.display_name(),
                        link.conn,
                    );
                    dlog!(
                        "connect REJECTED after {} ms — handle {:#06x} already in use",
                        t0.elapsed().as_millis(), link.conn,
                    );
                    radio.with_dongle(|d| d.cancel_pending_connect());
                    let now = Instant::now();
                    for l in links.iter_mut() {
                        l.last_input = now;
                    }
                }
                Ok(link) => {
                    eprintln!("[jc2-dongle] {} handle {:#06x}", side.display_name(), link.conn);
                    // ❗ The link(s) already held have been unserviced for the
                    // whole of that init. Their `last_input` is stale through no
                    // fault of the controller, and INPUT_TIMEOUT is shorter than
                    // an init takes — so without this the first Joy-Con is
                    // written off within moments of the second connecting.
                    let now = Instant::now();
                    for l in links.iter_mut() {
                        l.last_input = now;
                    }
                    dlog!(
                        "connect OK after {} ms — handle {:#06x}",
                        t0.elapsed().as_millis(), link.conn,
                    );
                    register(&shared, &link);
                    links.push(link);
                }
                Err(e) => {
                    eprintln!("[jc2-dongle] {} connect failed: {e}", side.display_name());
                    dlog!("connect FAILED after {} ms: {e}", t0.elapsed().as_millis());
                    // ❗ A failed `LE_Create_Connection` leaves the controller
                    // INITIATING, and while it initiates it refuses to scan
                    // with "Command Disallowed". Without this the next scan
                    // enable fails, and the one after that, until something
                    // else happens to clear it — which reads as "it just stops
                    // finding anything".
                    radio.with_dongle(|d| d.cancel_pending_connect());
                    let now = Instant::now();
                    for l in links.iter_mut() {
                        l.last_input = now;
                    }
                }
            }
        }

        if links.is_empty() && !scanning {
            // Nothing to service; do not spin the CPU between scan windows.
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    // Tear every link down, and WAIT for the controller to acknowledge.
    //
    // Sending Disconnect and closing the USB handle in the same breath loses
    // the command: the dongle never gets to transmit it, and the controller is
    // left believing the link is live until supervision timeout. Draining until
    // each handle reports Disconnection Complete is what makes the next run
    // find an advertising controller instead of a silent one.
    if !links.is_empty() {
        eprintln!("[jc2-dongle] disconnecting {} link(s)", links.len());
        // ⭐ ONE lease for the whole teardown. Disconnecting and then waiting
        // for each confirmation is a conversation, and the router must not eat
        // the Disconnection Completes we are waiting on.
        let lease = radio.exclusive();
        let dongle = lease.dongle();
        for link in &links {
            let _ = dongle.disconnect(link.conn);
        }
        let deadline = Instant::now() + Duration::from_millis(600);
        let mut open: Vec<u16> = links.iter().map(|l| l.conn).collect();
        while !open.is_empty() && Instant::now() < deadline {
            match dongle.read_event_timeout(Duration::from_millis(50)) {
                Ok(Some(Event::DisconnectionComplete { conn_handle, .. })) => {
                    open.retain(|h| *h != conn_handle);
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        if open.is_empty() {
            eprintln!("[jc2-dongle] all links closed cleanly");
        } else {
            eprintln!("[jc2-dongle] {} link(s) did not confirm disconnect", open.len());
        }
    }
    dlog!("dongle thread exit");
    shared.finished.store(true, Ordering::Relaxed);
}


/// Decide whether an advertising report is a Joy-Con 2 worth connecting to.
fn advert_match(
    shared: &Arc<Shared>,
    r: &flexinput_btle::hci::AdvReport,
    links: &[Link],
) -> Option<([u8; 6], u8, Side)> {
    // A controller we have already identified needs no manufacturer data.
    // Copied out to a local before branching — the guard would otherwise live
    // for the whole block, and this function locks `known` again further down.
    // See the deadlock note in `connect_and_init`.
    let known_side = shared.known.lock().unwrap().get(&r.address).copied();
    if let Some(side) = known_side {
        if links.iter().any(|l| l.key.address == r.address) {
            return None;
        }
        dlog!("adv {:02x?} {} KNOWN — matching without scan response",
              r.address, side.display_name());
        return Some((r.address, r.address_type, side));
    }
    // Every rejection below is logged with its reason. They were all bare
    // `return None`, so a controller that advertised and was turned away looked
    // exactly like one that never advertised at all.
    let Some(md) = r.manufacturer_data() else {
        // `event_type` 0x00 is ADV_IND, 0x04 is SCAN_RSP. Logged because which
        // one carries the identifying payload is the whole question here.
        dlog!(
            "adv {:02x?} rssi {} type {} — no manufacturer data, {} data bytes",
            r.address, r.rssi, r.event_type, r.data.len(),
        );
        return None;
    };
    // The company id is INCLUDED in this stack's manufacturer data (unlike
    // btleplug, which strips it into a map key), so VID sits at 5 and PID at 7
    // rather than 3 and 5.
    if md.len() < 9 {
        dlog!("adv {:02x?} — manufacturer data only {} bytes: {md:02x?}", r.address, md.len());
        return None;
    }
    let company = u16::from_le_bytes([md[0], md[1]]);
    if company != protocol::NINTENDO_MANUFACTURER_ID {
        dlog!("adv {:02x?} — company {company:#06x}, not Nintendo", r.address);
        return None;
    }
    let vid = u16::from_le_bytes([md[5], md[6]]);
    if vid != protocol::NINTENDO_VID {
        dlog!("adv {:02x?} — VID {vid:#06x}, not Nintendo", r.address);
        return None;
    }
    let pid = u16::from_le_bytes([md[7], md[8]]);
    let Some(side) = Side::from_pid(pid) else {
        dlog!("adv {:02x?} — PID {pid:#06x} is not a known half", r.address);
        return None;
    };
    if Side::is_safe_mode(pid) {
        dlog!("adv {:02x?} {} — SAFE MODE (PID {pid:#06x}), refusing",
              r.address, side.display_name());
        return None;
    }
    if links.iter().any(|l| l.key.address == r.address) {
        dlog!("adv {:02x?} {} — already connected", r.address, side.display_name());
        return None;
    }
    dlog!(
        "adv {:02x?} {} MATCH — pid {pid:#06x} rssi {} addr_type {} data {md:02x?}",
        r.address, side.display_name(), r.rssi, r.address_type,
    );
    shared.known.lock().unwrap().insert(r.address, side);
    Some((r.address, r.address_type, side))
}

/// Connect, subscribe and initialise one controller.
fn connect_and_init(
    shared: &Arc<Shared>,
    dongle: &Dongle,
    address: [u8; 6],
    address_type: u8,
    side: Side,
) -> Result<Link, Box<dyn std::error::Error>> {
    // The advertisement's address type is carried through rather than assumed
    // public: a controller using a random address is unreachable if we get it
    // wrong, and the failure looks like "connect times out" with no clue why.
    // Fixed 7.5 ms, the BLE spec minimum, rather than `le_connect`'s attempt at
    // 5 ms followed by a fallback.
    //
    // 5 ms is BELOW the spec minimum; the reference ESP32 firmware pins 6 with
    // the note that lower values are rejected. Asking for 6 directly measured
    // ~140-200 Hz of input reports against the ~67 Hz this link has always run
    // at — two to three times the motion samples, for free, and the same change
    // removed the intermittent failure to find the second half.
    let link = dongle.le_connect_params(address, address_type, 6, 6)?;
    let conn = link.conn_handle;
    eprintln!(
        "[jc2-dongle] {} link: interval {:.2} ms, supervision timeout {} ms",
        side.display_name(),
        link.interval_ms(),
        link.timeout_ms(),
    );
    eprintln!(
        "[jc2-dongle] {} connected, handle {conn:#06x}",
        side.display_name()
    );

    // Raise the ATT MTU before anything else. At the 23-byte default a 63-byte
    // input report arrives fragmented and every parser offset is wrong.
    dongle.send_att(conn, &acl::exchange_mtu_request(jc::DESIRED_MTU))?;

    // Before any of the Nintendo init, ask what this controller actually
    // exposes — see `scan_gatt`. No-op unless FLEXINPUT_JC2_GATT_SCAN is set.
    scan_gatt(dongle, conn);

    // ⛔ Resolve the handles from THIS controller's table before writing to any
    // of them — see `resolve_handles`. Subscribing by a number captured from a
    // different device configures whatever attribute happens to sit there.
    let handles = resolve_handles(dongle, conn, side);

    // ⭐ **Whether the per-side stream is subscribed at all is now a choice.**
    //
    // ⛔ The reference subscribes its input characteristic LAST, after the
    // motion enable, and subscribes only ONE. FlexInput subscribes the per-side
    // input FIRST and then asks for the common one too — and the common one has
    // never sent a byte, on any controller, even with every write accepted.
    //
    // ❗ "Accepted" there means an ATT Write Response came back: the write
    // reached the attribute. It says nothing about the controller acting on it.
    // A plausible reading of the silence is that the controller emits ONE input
    // stream and picks it from what is already subscribed when the enable
    // lands — in which case subscribing the per-side report first settles the
    // question before the enable is even sent.
    //
    // FLEXINPUT_JC2_COMMON=only tests exactly that: skip the per-side
    // subscription entirely and take the common stream or nothing. Off by
    // default, because the per-side path is the one that currently works and a
    // hypothesis should not cost anyone their controllers.
    let common_only = std::env::var("FLEXINPUT_JC2_COMMON")
        .is_ok_and(|v| v.eq_ignore_ascii_case("only"));
    if common_only {
        crate::dlog::imu(format_args!(
            "{} FLEXINPUT_JC2_COMMON=only — per-side input NOT subscribed;              the common stream is the only source this run",
            side.display_name()
        ));
    } else {
        dongle.send_att(
            conn,
            &acl::write_request(handles.input + 1, &acl::CCCD_NOTIFY),
        )?;
    }
    dongle.send_att(
        conn,
        &acl::write_request(jc::HANDLE_CMD_RESPONSE_CCCD, &acl::CCCD_NOTIFY),
    )?;

    // ⭐ Also subscribe the COMMON input, and give it the report-rate descriptor
    // the per-side one gets.
    //
    // This is the one combination never tried. The common input was written off
    // as "unreachable by notify" after it stayed silent through every ordering,
    // mask and mode — but that testing only ever wrote the CCCD. It never wrote
    // the report-rate descriptor, and this file's own comment thirty lines below
    // records what happens when that step is skipped: "WITHOUT THIS the
    // controller streams stub reports forever". Each input characteristic has
    // its own descriptor, and the common one has never been written in the life
    // of this project.
    //
    // It matters because the two working PC implementations both read motion
    // from THIS characteristic, at offsets that are all zero in the per-side
    // report we do receive — accel at 0x30, gyro at 0x36, six contiguous i16.
    // The per-side stream has now been shown to contain no angular rate at all:
    // its accel padding bytes stay exactly zero through a full 360 deg sweep, so
    // they are padding rather than a gyro at rest, and nothing past the accel
    // block is ever non-zero.
    //
    // Failing is free — an unsupported handle returns an ATT error, which the
    // send path already tolerates, and the per-side stream is untouched either
    // way.
    // ⭐ And the PER-SIDE response channel, which answers the handle we send
    // commands to and has never been subscribed — see the constant's docs.
    dongle.send_att(
        conn,
        &acl::write_request(jc::HANDLE_CMD_RESPONSE_PERSIDE_CCCD, &acl::CCCD_NOTIFY),
    )?;
    dlog!("init: feature-select done, writing report-rate descriptor");
    // The vendor report-rate descriptor. WITHOUT THIS the controller streams
    // stub reports forever — counter incrementing, every field zero — which is
    // indistinguishable from a parser bug. It is the single most expensive
    // omission in this sequence.
    dongle.send_att(
        conn,
        &acl::write_request(jc::HANDLE_INPUT_REPORT_RATE, &protocol::REPORT_RATE_PAYLOAD),
    )?;

    dlog!("init: report-rate written, stream should start now");
    probe_readable(dongle, conn);
    // ⛔ **Moved to the END of init, which is where the reference puts it.**
    //
    // This ran FIRST — before pairing, the memory reads, the LED, and before
    // the report-rate descriptor that actually starts a stream. Acknowledged
    // every time, and the common characteristic never sent a byte. The
    // reference sends its enable after the info and calibration reads and
    // subscribes its input LAST, and that ordering was described plainly in the
    // source I took the command from; I implemented the command and ignored
    // where it sat.
    //
    // ❗ Not proof that order is the missing piece — subscribing only the
    // common stream did not start it either, so exclusivity is already ruled
    // out. But sending a setup command before setup has happened is not a test
    // of anything, and this is the cheap half of the remaining difference.
    // ⭐ **Turn the common stream ON before subscribing to it.**
    //
    // ⛔ This is the step that was missing, and the reason `0x000a` was
    // recorded as "subscribed and silent" for the whole gyro search. FlexInput
    // enables features with `0x0C` on the per-side rumble+command channel,
    // which visibly works — it starts the per-side report. It simply enables a
    // DIFFERENT stream. Every working PC implementation sends `0x03` on the
    // BASIC command channel instead, twice: subcommand `0x00` to initialise
    // and `0x01` to enable, each carrying the feature flags.
    //
    // The report that follows carries a real angular rate at 0x30/0x36, which
    // is what every workaround downstream exists to substitute for.
    //
    // ❗ Order matters and is taken from the reference: enable FIRST, subscribe
    // second. Both writes are acknowledged so a refusal is visible rather than
    // inferred from silence, and neither is fatal — a controller that refuses
    // still streams the per-side report exactly as before.
    // ❗ Written to the DISCOVERY log as well, so its position in the real
    // sequence is observable rather than inferred. Two attempts to place this
    // step by reading timestamps were wrong: the diagnostic log and the
    // discovery log have different clock origins, and the `dlog!` calls around
    // it live in different functions, so neither file nor the source order
    // answers "when does this actually run".
    // ⭐ The common input, subscribed at its RESOLVED handle and with the reply
    // actually read.
    //
    // ⛔ Both of these were fire-and-forget at a hardcoded number. If the
    // number was wrong the write landed on an unrelated attribute; if it was
    // refused the error response was never read. Either way the outcome was a
    // characteristic recorded as "subscribed and silent" — the exact note that
    // sat against `0x000a` for the whole gyro search, on two different
    // controllers, while another project read motion from it on the same
    // hardware.
    //
    // A CCCD sits immediately after the value handle it configures, and the
    // vendor rate descriptor after that; those offsets are GATT convention and
    // hold wherever the base handle lands.
    if let Some(common) = handles.common {
        for (what, handle, payload) in [
            ("CCCD", common + 1, &acl::CCCD_NOTIFY[..]),
            ("rate", common + 2, &protocol::REPORT_RATE_PAYLOAD[..]),
        ] {
            match dongle.att_request(
                conn,
                &acl::write_request(handle, payload),
                acl::ATT_WRITE_RESPONSE,
                ATT_ACK_WAIT,
            ) {
                Ok(Some(_)) => crate::dlog::imu(format_args!(
                    "{} common input {what} write {handle:#06x} acknowledged",
                    side.display_name()
                )),
                Ok(None) => crate::dlog::imu(format_args!(
                    "{} common input {what} write {handle:#06x} — NO REPLY                      (the attribute may not exist here)",
                    side.display_name()
                )),
                Err(e) => crate::dlog::imu(format_args!(
                    "{} common input {what} write {handle:#06x} REFUSED: {e}",
                    side.display_name()
                )),
            }
        }
    } else {
        crate::dlog::imu(format_args!(
            "{} common input NOT PRESENT in this controller's table — nothing              to subscribe to, so report 0x05 cannot be a gyro source here",
            side.display_name()
        ));
    }

    // ⭐ The third input stream and the spare notify characteristic, both found
    // by walking the attribute table — see `jc::HANDLE_INPUT_EXTRA`. Same
    // subscribe-then-set-a-rate pattern the two known inputs need.
    dongle.send_att(
        conn,
        &acl::write_request(jc::HANDLE_INPUT_EXTRA_CCCD, &acl::CCCD_NOTIFY),
    )?;
    dongle.send_att(
        conn,
        &acl::write_request(jc::HANDLE_INPUT_EXTRA_RATE, &protocol::REPORT_RATE_PAYLOAD),
    )?;
    dongle.send_att(
        conn,
        &acl::write_request(jc::HANDLE_NOTIFY_EXTRA2_CCCD, &acl::CCCD_NOTIFY),
    )?;

    // ❗ [`jc::HANDLE_CMD_WRITE`] must be the handle the controller EXECUTES
    // from (`0x0016`), not the one the reference's UUID table points at
    // (`0x0014`). Repointing this at `0x0014` sent the entire sequence below —
    // handshake, memory reads, feature-select — into a handle that accepts
    // writes and discards them. The link came up, reports streamed, and every
    // one of them was a stub: `motion_len = 0`, all fields zero. Downstream
    // that reads as a dead accelerometer, several layers from the cause.
    debug_assert!(jc::executes_commands(jc::HANDLE_CMD_WRITE));
    // ⛔ **The last structural difference from the working reference.**
    //
    // Everything else about the common input now checks out and it is still
    // silent: the handle is right, 0x000b is a real CCCD, the rate descriptor
    // matches the per-side one, the write is acknowledged, and its declaration
    // reads properties 0x12 — READ|NOTIFY, byte for byte identical to the
    // per-side input that streams perfectly.
    //
    // So the characteristic can notify and nothing we send makes it. The one
    // thing left that differs: the reference drives its WHOLE session through
    // `649d4ac9-…` (0x0014) with bare frames, while this drives
    // `65a724b3-…` (0x0016) with a 17-byte rumble prefix and only mirrors the
    // feature-select onto 0x0014.
    //
    // ❗ 0x0014 was once written off as inert — commands there "moved nothing"
    // while the same command on 0x0016 visibly worked. But that was judged on
    // LEDs and buzzes, which are per-side effects, and a stream-selection bit
    // need not have any visible effect at all. Inert for rumble does not mean
    // inert for everything.
    //
    // FLEXINPUT_JC2_CMD=common routes the entire init there, unprefixed, the
    // way the reference does. If the LEDs stop responding it is inert and this
    // is settled; if the common input starts streaming, it never was.
    let cmd_common = std::env::var("FLEXINPUT_JC2_CMD")
        .is_ok_and(|v| v.eq_ignore_ascii_case("common"));
    crate::dlog::imu(format_args!(
        "{} command channel: {} ({})",
        side.display_name(),
        if cmd_common { "0x0014 BARE (reference)" } else { "0x0016 prefixed (default)" },
        if cmd_common { "FLEXINPUT_JC2_CMD=common" } else { "set FLEXINPUT_JC2_CMD=common to try the reference path" },
    ));
    let cmd = |c: u8, s: u8, data: &[u8]| -> Result<(), Box<dyn std::error::Error>> {
        let (handle, frame) = if cmd_common {
            (jc::HANDLE_CMD_WRITE_COMMON, protocol::command(c, s, data))
        } else {
            (jc::HANDLE_CMD_WRITE, protocol::rumble_cmd_frame(c, s, data))
        };
        dongle.send_att(conn, &acl::write_command(handle, &frame))?;
        // ⭐ Keep draining while we wait, rather than sleeping blind.
        //
        // Initialising the SECOND controller takes about three seconds, and for
        // all of it the first one was going unserviced — its notifications
        // piling up in the dongle's ACL buffer with nobody reading them. A
        // controller whose buffers fill stops being able to send, which looks
        // from the outside like the first Joy-Con dying the moment the second
        // one connects.
        //
        // The data drained here belongs to a link this function knows nothing
        // about, so it is discarded; a few dropped reports during init is
        // nothing next to stalling the transport.
        let until = Instant::now() + INIT_GAP;
        while Instant::now() < until {
            let _ = dongle.drain_acl(64);
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    };

    // Undocumented handshake steps official software always sends first.
    cmd(protocol::CMD_UNKNOWN_07, 0x01, &[])?;
    cmd(protocol::CMD_UNKNOWN_10, 0x01, &[])?;
    cmd(protocol::CMD_UNKNOWN_16, 0x01, &[])?;

    // Controller-memory reads. These carry factory calibration; other
    // implementations report that skipping them leaves the controller emitting
    // stub reports indefinitely.
    // ⭐ Pair BEFORE the rest of init, not after.
    //
    // It used to run last, after the report-rate descriptor had already started
    // the input stream — so two controller-flash writes landed in the middle of
    // a live stream. The controller then showed its player LED (connected) while
    // FlexInput listed nothing at all: reports had stopped and never resumed,
    // and the log simply ended.
    //
    // The Bluetooth hub has always paired before finishing init, and its logs
    // read `paired …` then `init complete` then `steady state`. Matching that
    // order keeps the stream-start as the last thing that happens, which is
    // also what makes it recoverable — nothing after it can disturb it.
    // ❗ The lookup is a STATEMENT, not an `if let` scrutinee, and that is not
    // style. A `MutexGuard` created in an `if let` condition lives until the end
    // of the whole if/else-if chain, so
    //
    //     if let Some(prev) = shared.paired.lock().unwrap().get(..).copied() {
    //     } else if let Some(ltk) = run_pairing(..) {
    //         shared.paired.lock().unwrap().insert(..);   // deadlock
    //     }
    //
    // takes the same non-reentrant lock twice and blocks the thread forever.
    // It hung exactly here: the console printed `PAIRED`, the very next log
    // line never arrived, the controller kept its link, and closing the app
    // stranded it because the teardown is at the end of a thread that could no
    // longer reach it.
    let already_paired = shared.paired.lock().unwrap().get(&address).copied();
    match already_paired {
        Some(prev) => eprintln!(
            "[jc2-dongle] {} already paired this run, skipping the flash writes (LTK {prev:02x?})",
            side.display_name(),
        ),
        None => {
            if let Some(ltk) = run_pairing(dongle, conn, side) {
                shared.paired.lock().unwrap().insert(address, ltk);
            }
        }
    }

    dlog!("init: pairing done, starting memory reads");
    for (size, addr) in protocol::JC2_INIT_MEMORY_READS {
        cmd(
            protocol::CMD_READ_MEMORY,
            protocol::SUB_READ_MEMORY,
            &protocol::read_memory_data(*size, *addr),
        )?;
    }

    dlog!("init: memory reads done");

    // ⭐ CONNECTION FEEDBACK — this is the buzz, and it is a COMMAND.
    //
    // The Bluetooth hub sends `0x0a/0x02` (play a canned vibration preset) as
    // the first step after the memory reads, and the dongle path never has. A
    // Joy-Con paired over the Windows stack buzzes; over the dongle it does
    // not — and that difference was reasonably, but wrongly, read as evidence
    // that dongle pairing was being refused.
    //
    // It is not evidence of anything about pairing. Pairing reports a genuine
    // cryptographic agreement at every step: the controller returns its device
    // key, and the AES confirmation it sends back matches the one derived from
    // the LTK. That cannot be faked by a controller that ignored the exchange.
    // The buzz was simply a command nobody was sending.
    cmd(protocol::CMD_VIBRATION, 0x02, &[0x03, 0, 0, 0])?;
    dlog!("init: connection feedback (buzz) sent");

    cmd(protocol::CMD_PLAYER_LEDS, 0x07, &[0x01, 0, 0, 0, 0, 0, 0, 0])?;
    dlog!("init: player LED set");

    // ⭐ The feature-select command is now OPTIONAL, and off by default.
    //
    // Three independent reference implementations decode the gyroscope at
    // offset 0x36 of a 63-byte report, and NONE of them sends a feature command
    // to enable motion. The upstream one calls it in exactly one place:
    //
    //     if CONFIG.mouse_config.enabled:
    //         await self.enableFeatures(FEATURE_MOUSE)
    //
    // So on a standard controller the IMU is present by default, and this
    // command's job is to turn the MOUSE on — not the motion.
    //
    // Every init this project has ever run sent it, always with the mouse bit
    // set. And the report we receive carries mouse deltas at 0x09, a motion
    // block at 0x10 that matches no published layout, and zeroes across
    // 0x30..0x3C where the standard accelerometer and gyroscope live. That is
    // consistent with the command switching the controller into an alternate
    // report format in which the raw IMU is simply not carried.
    //
    // It would also explain why sweeping the mask changed nothing: if the
    // layout latches on the first feature-select regardless of payload, then
    // every mask ever tested was applied AFTER the format was already chosen.
    //
    // `FLEXINPUT_JC2_FEATURES=2f` (any hex byte) restores the old behaviour with
    // that mask, so both paths are one restart apart rather than a rebuild.
    match feature_override() {
        Some(mask) => {
            eprintln!("[jc2-dongle] feature-select ENABLED, mask {mask:#04x} (mouse needs 0x10)");
            // ⛔ **Into the diagnostic log, not just stderr.**
            //
            // The mask is the variable under test and it was only ever printed
            // to a console nobody keeps, so a run could not say which
            // experiment it was. A log that records the result but not the
            // condition answers nothing.
            crate::dlog::imu(format_args!(
                "{} feature-select mask {mask:#04x} \
                 (buttons {} sticks {} imu {} imu_raw {} mouse {} rumble {} mag {}) \
                 -> per-side {:#06x}, bare -> common {:#06x}",
                side.display_name(),
                mask & protocol::feature::BUTTONS != 0,
                mask & protocol::feature::STICKS != 0,
                mask & protocol::feature::IMU != 0,
                mask & protocol::feature::IMU_RAW != 0,
                mask & protocol::feature::MOUSE != 0,
                mask & protocol::feature::RUMBLE != 0,
                mask & protocol::feature::MAGNETOMETER != 0,
                jc::HANDLE_CMD_WRITE,
                jc::HANDLE_CMD_WRITE_COMMON,
            ));
            // ❗ The full captured sequence, in order, not just the two
            // feature-select frames.
            //
            // The hub sends INIT, then `0x11/0x03`, then the 20-byte vibration
            // payload `0x0a/0x08`, then `0x11/0x01`, and only THEN the confirm.
            // Its own comment records that official software always sends the
            // vibration data before the final confirm, and that omitting it was
            // a real regression once already. The dongle path was sending the
            // two feature frames back to back with all four intervening steps
            // missing — a shorter sequence than the one this controller was
            // captured responding to.
            cmd(protocol::CMD_FEATURE_SELECT, protocol::SUB_FEATURE_INIT, &[mask, 0, 0, 0])?;
            cmd(protocol::CMD_UNKNOWN_11, 0x03, &[])?;
            cmd(
                protocol::CMD_VIBRATION,
                0x08,
                &[
                    0x01, 0x59, 0x09, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0x35,
                    0x00, 0x46, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                ],
            )?;
            cmd(protocol::CMD_UNKNOWN_11, 0x01, &[])?;
            cmd(protocol::CMD_FEATURE_SELECT, protocol::SUB_FEATURE_CONFIRM, &[mask, 0, 0, 0])?;

            // ⭐ Mirror it onto the COMMON command characteristic, bare.
            //
            // Never tried in this combination. The reference implementations
            // write commands here (UUID 649d4ac9-…) with NO 17-byte prefix, and
            // read motion from the common input, where the standard accel/gyro
            // block lives. This project drives the per-side channel instead,
            // because that is the one that visibly executes — but "0x0014 is
            // inert" was concluded from init commands whose only evidence of
            // success was a reply, and a feature bit can take effect without
            // one.
            //
            // Costs two ATT writes. If the handle ignores them, nothing changes;
            // if it does not, the common input may start carrying the standard
            // report, which the notification handler already dumps.
            for sub in [0x02u8, 0x04] {
                let bare = protocol::command(protocol::CMD_FEATURE_SELECT, sub, &[mask, 0, 0, 0]);
                dongle.send_att(conn, &acl::write_command(jc::HANDLE_CMD_WRITE_COMMON, &bare))?;
                std::thread::sleep(INIT_GAP);
            }
        }
        None => {
            eprintln!("[jc2-dongle] feature-select SKIPPED by FLEXINPUT_JC2_FEATURES=off");
            crate::dlog::imu(format_args!(
                "{} feature-select SKIPPED (FLEXINPUT_JC2_FEATURES=off)",
                side.display_name()
            ));
        }
    }

    dlog!("init: COMPLETE");

    Ok(Link {
        key: PadKey { side, address },
        conn,
        handles,
        calib: StickCalib::default(),
        orientation: OrientationTracker::default(),
        common_probe: Instant::now(),
        // ❗ Dated in the PAST so the first frame dumps immediately. Set to
        // "now", the first dump was ten seconds away and two short test runs
        // produced no frame at all — a diagnostic that cannot answer a
        // five-second run is not a diagnostic.
        frame_dump: Instant::now() - Duration::from_secs(10),
        common_motion: None,
        last_input: Instant::now(),
        reports: 0,
        extra_reports: 0,
        replies: 0,
        common_reports: 0,
        unparsed: 0,
    })
}

fn register(shared: &Arc<Shared>, link: &Link) {
    shared.pads.lock().unwrap().insert(
        link.key,
        PadState {
            key: link.key,
            display_name: link.key.side.display_name().to_string(),
            connected: true,
            streaming: false,
            snapshot: PadSnapshot::default(),
            stick: (0.0, 0.0),
            gyro: [0.0; 3],
                orientation: [0.0; 3],
                orientation_quat: [0.0, 0.0, 0.0, 1.0],
            field_rate: [0.0; 3],
            yaw_rate: 0.0,
            pin_rate: [0.0; 3],
            events: 0,
        },
    );
}

/// Service every live link: drain ACL, demultiplex by connection handle, and
/// drop links that go quiet or disconnect.
fn pump(
    sub: &flexinput_btle::radio::Subscriber,
    shared: &Arc<Shared>,
    links: &mut Vec<Link>,
    scanning: bool,
) -> Option<([u8; 6], u8, Side)> {
    let mut found = None;
    // ⭐ From the shared radio's fan-out, not from the dongle directly. One
    // reader feeds every transport; reading the transport here would consume
    // the other one's traffic as well as ours.
    //
    // Events first — a disconnect must be noticed before its handle is reused.
    while let Some(evt) = sub.recv_event(Duration::from_millis(1)) {
        match evt {
            Event::DisconnectionComplete { conn_handle, reason } => {
                dlog!("DISCONNECT handle {conn_handle:#06x} reason {reason:#04x}");
                if let Some(pos) = links.iter().position(|l| l.conn == conn_handle) {
                    let link = links.remove(pos);
                    eprintln!(
                        "[jc2-dongle] {} disconnected (reason {reason:#04x})",
                        link.key.side.display_name()
                    );
                    shared.pads.lock().unwrap().remove(&link.key);
                }
            }
            Event::LeAdvertisingReport(r) if scanning && found.is_none() => {
                found = advert_match(shared, &r, links);
            }
            // ⭐ Reports that arrive when we are NOT looking, logged too.
            //
            // `found.is_none()` means only the first match per pass is taken,
            // and `scanning` gates the rest — so a controller whose whole
            // advertising burst lands in a window where either was false is
            // discarded without a trace. That is precisely the "woke it and
            // nothing happened" case, and it is invisible from the console.
            Event::LeAdvertisingReport(r) => {
                dlog!(
                    "adv {:02x?} rssi {} IGNORED — scanning={scanning} already_found={}",
                    r.address, r.rssi, found.is_some(),
                );
            }
            other => dlog!("event {other:?}"),
        }
    }

    // Drain EVERYTHING waiting, not one packet per pass.
    //
    // Reading a single packet per iteration with a blocking timeout capped the
    // whole process at roughly 90 packets a second shared between both links,
    // and whichever half was serviced first took nearly all of it — the right
    // half was observed at 6 Hz against the left's 60 Hz. Two halves at a 5 ms
    // connection interval need 400 packets a second, so this has to be greedy.
    // Same fan-out. Bounded so one very chatty link cannot keep this loop from
    // returning to its callers' scan and connect handling.
    let mut drained = 0;
    while let Some(pkt) = sub.recv_acl(Duration::from_millis(1)) {
        drained += 1;
        if drained > 256 {
            break;
        }
        if pkt.cid != acl::CID_ATT {
            continue;
        }
        let Some(n) = acl::parse_notification(&pkt.payload) else { continue };
        // ⭐ Announce ANY traffic on the common input, loudly and once.
        //
        // This characteristic has been assumed dead for the whole project, on
        // evidence that never included writing its report-rate descriptor. If a
        // single notification arrives here it changes what this controller is
        // believed to be capable of, and the raw bytes are printed because that
        // is the report both working PC implementations parse motion from —
        // accel at 0x30, gyro at 0x36. A hex dump is enough to confirm or kill
        // it on sight: byte 0x34 should read about 4096 with the pad face-up.
        // ⭐ Anything on the previously unsubscribed streams, dumped raw. If the
        // gyro is anywhere outside the per-side report, this is where it shows.
        if n.handle == jc::HANDLE_INPUT_EXTRA || n.handle == jc::HANDLE_NOTIFY_EXTRA2 {
            if let Some(link) = links.iter_mut().find(|l| l.conn == pkt.conn_handle) {
                link.extra_reports = link.extra_reports.saturating_add(1);
                if link.extra_reports.is_power_of_two() {
                    eprintln!(
                        "[jc2-dongle] {} ⭐ EXTRA STREAM {:#06x} #{} ({} bytes): {:02x?}",
                        link.key.side.display_name(),
                        n.handle,
                        link.extra_reports,
                        n.value.len(),
                        &n.value[..n.value.len().min(64)],
                    );
                }
            }
            continue;
        }
        if links
            .iter()
            .any(|l| l.conn == pkt.conn_handle && l.handles.common == Some(n.handle))
        {
            if let Some(link) = links.iter_mut().find(|l| l.conn == pkt.conn_handle) {
                link.common_reports = link.common_reports.saturating_add(1);
                // ⭐ **The real angular rate, kept for the next per-side
                // report.**
                //
                // Merged rather than used wholesale: the per-side report is
                // what buttons, sticks, mouse and battery already come from and
                // it works, so this changes only the one field that was
                // missing. `OrientationTracker::update` ALREADY prefers
                // `motion.gyro` over the constructed axes when it is present —
                // so supplying it here retires the differentiated heading, the
                // x4 yaw gain and the smoothing window in one step, with no
                // change to the pipeline below.
                if let Some(common) = reports::parse_common_input(link.key.side, &n.value) {
                    link.common_motion = common.motion.gyro.map(|g| (g, common.motion.std_accel));
                }
                if link.common_reports.is_power_of_two() {
                    eprintln!(
                        "[jc2-dongle] {} ⭐ COMMON INPUT #{} ({} bytes): {:02x?}",
                        link.key.side.display_name(),
                        link.common_reports,
                        n.value.len(),
                        &n.value[..n.value.len().min(64)],
                    );
                    // ⏳ TEMPORARY, and the single most valuable line this log
                    // can carry.
                    //
                    // ⭐ **This characteristic is where a real gyro would be.**
                    // The per-side stream has been shown to contain no angular
                    // rate at all, so every rate FlexInput reports is
                    // differentiated from a fused heading — which is why yaw
                    // needs a x4 fudge for its frequency response. Both working
                    // PC implementations read motion from HERE instead, as six
                    // contiguous i16 at 0x30 (accel) and 0x36 (gyro).
                    //
                    // ❗ It has never notified on the reference M12-S grip, so
                    // nobody knows whether retail hardware gates it differently.
                    // If these lines appear on a genuine Joy-Con 2, the whole
                    // constructed-axis approach can be replaced by the real
                    // thing. Decoded inline because "did it notify" and "is
                    // there sensible motion in it" are one question in practice.
                    let i16_at = |off: usize| -> Option<i16> {
                        let b = n.value.get(off..off + 2)?;
                        Some(i16::from_le_bytes([b[0], b[1]]))
                    };
                    let six = (0..6)
                        .map(|i| i16_at(0x30 + i * 2))
                        .collect::<Option<Vec<_>>>();
                    match six {
                        Some(v) => crate::dlog::imu(format_args!(
                            "{} COMMON INPUT (0x000a) #{} len {} | accel {:>7} {:>7} {:>7}                              | gyro {:>7} {:>7} {:>7} | raw {:02x?}",
                            link.key.side.display_name(),
                            link.common_reports,
                            n.value.len(),
                            v[0], v[1], v[2], v[3], v[4], v[5],
                            &n.value[..n.value.len().min(64)],
                        )),
                        None => crate::dlog::imu(format_args!(
                            "{} COMMON INPUT (0x000a) #{} len {} — TOO SHORT for the                              0x30/0x36 motion block | raw {:02x?}",
                            link.key.side.display_name(),
                            link.common_reports,
                            n.value.len(),
                            &n.value[..n.value.len().min(64)],
                        )),
                    }
                }
            }
            continue;
        }
        // ⭐ Print the controller's command REPLIES. We have never read them.
        //
        // Every init command here is fire-and-forget, on the reasoning that
        // waiting on replies once turned a millisecond handshake into 40
        // seconds. That is still the right call for TIMING — but it means the
        // controller has been answering into a void this whole time. A reply to
        // the feature-select is the one place a refused or clamped mask would
        // announce itself, and "the mask had no effect" versus "the mask was
        // rejected" are very different problems.
        //
        // Cheap: replies only arrive during init, so this is a handful of lines
        // per connection and then silence.
        if n.handle == jc::HANDLE_CMD_RESPONSE || n.handle == jc::HANDLE_CMD_RESPONSE_PERSIDE {
            if let Some(link) = links.iter_mut().find(|l| l.conn == pkt.conn_handle) {
                link.replies = link.replies.saturating_add(1);
                if link.replies <= 24 {
                    eprintln!(
                        "[jc2-dongle] {} <- reply #{} on {:#06x} ({} bytes): {:02x?}",
                        link.key.side.display_name(),
                        link.replies,
                        n.handle,
                        n.value.len(),
                        &n.value[..n.value.len().min(32)],
                    );
                }
            }
            continue;
        }
        if n.handle != jc::HANDLE_INPUT_VALUE {
            continue;
        }
        let Some(link) = links.iter_mut().find(|l| l.conn == pkt.conn_handle) else {
            continue;
        };
        // ⏳ TEMPORARY. The WHOLE frame, periodically.
        //
        // ⛔ The previous version of this logged a "frame shape" from byte
        // 0x0f and reported two alternating shapes. There is only one: byte
        // 0x0f is the motion TIMESTAMP, which steps by four every report, and
        // the motion-length byte for the left half sits one earlier at 0x0e
        // where it reads a constant 30. A discriminator invented from a
        // counter will always look like two formats.
        //
        // ❗ So this prints the entire 63 bytes instead of a summary of them.
        // Every offset question so far — accel stride, the per-side one-byte
        // shift, whether retail lays the block out the same way — is answerable
        // from the raw frame and from nothing less. Once every ten seconds
        // costs a few hundred bytes and no judgement about what matters.
        if link.frame_dump.elapsed() >= Duration::from_secs(10) {
            link.frame_dump = Instant::now();
            // ⭐ The verdict alongside the bytes. Whether 0x30..0x3c is zero
            // is the entire question, and counting zeros by eye across
            // sixty-three bytes of hex is how a run gets misread.
            let block_empty = n
                .value
                .get(0x30..0x3c)
                .map(|b| b.iter().all(|v| *v == 0))
                .unwrap_or(true);
            crate::dlog::imu(format_args!(
                "{} FRAME len {} - standard block 0x30..0x3c {} - {:02x?}",
                link.key.side.display_name(),
                n.value.len(),
                if block_empty { "STILL ZERO" } else { "POPULATED" },
                &n.value[..],
            ));
        }
        let Some(snap) = reports::parse_input(link.key.side, &n.value) else {
            link.unparsed = link.unparsed.saturating_add(1);
            if link.unparsed.is_power_of_two() {
                eprintln!(
                    "[jc2-dongle] {} {} report(s) FAILED TO PARSE, {} bytes: {:02x?}",
                    link.key.side.display_name(),
                    link.unparsed,
                    n.value.len(),
                    &n.value[..n.value.len().min(24)],
                );
            }
            continue;
        };
        // ⭐ Inject the real rate, if the common stream is running.
        //
        // ❗ `mut` from here on purely for this. Everything downstream is
        // unchanged: the tracker already prefers `motion.gyro` when it exists,
        // so this is the single line that switches the whole orientation path
        // from a differentiated heading to a rate sensor.
        let mut snap = snap;
        if let Some((gyro, accel)) = link.common_motion {
            snap.motion.gyro = Some(gyro);
            if let Some(a) = accel {
                snap.motion.std_accel = Some(a);
            }
        }
        let stick = link.calib.normalize(snap.stick_raw);
        // Orientation from gravity (roll, pitch) and the heading field (yaw),
        // differenced into a rate. No zero-rate correction step any more:
        // both sources are absolute, so there is no bias to accumulate.
        // Pick up a calibration measured on THIS controller, if the user has
        // captured one. Pushed every report rather than at connect: a capture
        // that finishes while the pad is streaming must take effect at once,
        // and an unchanged value costs one read lock.
        // ⏳ TEMPORARY. ⛔ Silence in a log is ambiguous — "0x000a never
        // notified" and "this build does not log it" look identical, and the
        // first is the finding. So absence is stated, on a slow timer.
        if link.common_probe.elapsed() >= Duration::from_secs(30) {
            link.common_probe = Instant::now();
            crate::dlog::imu(format_args!(
                "{} common input (0x000a): {} report(s) so far{}",
                link.key.side.display_name(),
                link.common_reports,
                if link.common_reports == 0 {
                    " — SUBSCRIBED AND SILENT, so rates stay differentiated from                      the fused heading"
                } else {
                    ""
                },
            ));
        }
        link.orientation.set_resting_drift(crate::cal::field_drift(&link.key));
        let o = link.orientation.update(&snap.motion, link.key.side);
        let gyro = o.rate_dps;

        // ⭐ MOTION DIAGNOSTIC. The dongle path had no report dump at all,
        // which is why "the accel pins are dead" could not be told apart from
        // "the controller is not sending motion" without guessing.
        //
        // Prints the PARSED values, not raw bytes: raw bytes still need a human
        // to apply the offsets, and getting those offsets wrong is one of the
        // failure modes this is meant to catch. `motion_len` is included
        // because a short block silently skips the whole motion parse — that
        // guard failing is invisible everywhere else.
        //
        // Doubling backoff: the first reports appear immediately, then it goes
        // quiet on its own rather than flooding a 67 Hz stream forever.
        link.reports = link.reports.saturating_add(1);
        if link.reports.is_power_of_two() {
            let a = snap.motion.accel;
            let mag = ((a[0] * a[0] + a[1] * a[1] + a[2] * a[2]) as f32).sqrt();
            // ⭐ Whether the RAW gyro block is live is the single most useful
            // fact this line can carry. It is present only when the feature
            // mask enables it, and its absence is otherwise invisible: the
            // fallback path still produces plausible-looking numbers from
            // integrated angles, so "motion works" and "motion is real" look
            // the same downstream.
            let imu = match (snap.motion.gyro, snap.motion.std_accel) {
                (Some(g), Some(sa)) => format!("RAW gyro={g:?} accel={sa:?}"),
                _ => "raw IMU block ABSENT (feature bit refused?)".to_string(),
            };
            eprintln!(
                "[jc2-dongle] {} #{} len={} accel={a:?} |a|={mag:.0} ({:.2} g) \
                 heading={} rate={:.1?} | {imu}",
                link.key.side.display_name(),
                link.reports,
                snap.motion_len,
                mag / reports::ACCEL_LSB_PER_G,
                snap.motion.angle[reports::HEADING_AXIS],
                gyro,
            );
        }
        link.last_input = Instant::now();
        if let Some(pad) = shared.pads.lock().unwrap().get_mut(&link.key) {
            pad.streaming = true;
            pad.snapshot = snap;
            pad.stick = stick;
            pad.gyro = gyro;
            pad.field_rate = o.field_rate_dps;
            pad.yaw_rate = o.yaw_rate_dps;
            pad.pin_rate = o.pin_rate_dps;
            pad.orientation = o.euler_rad;
            pad.orientation_quat = o.quat_xyzw;
            pad.events = pad.events.saturating_add(1);
        }
    }

    // Liveness watchdog: a link can stop delivering without a disconnect event,
    // and a pad frozen at its last values is worse than one that disappears.
    let mut i = 0;
    while i < links.len() {
        if links[i].last_input.elapsed() > INPUT_TIMEOUT {
            let link = links.remove(i);
            eprintln!(
                "[jc2-dongle] {} went quiet — dropping",
                link.key.side.display_name()
            );
            // Dropping a quiet link is a write; the confirmation arrives on the
            // fan-out like any other event.
            let _ = sub.radio().with_dongle(|d| d.disconnect(link.conn));
            shared.pads.lock().unwrap().remove(&link.key);
        } else {
            i += 1;
        }
    }

    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// ⛔ No vendor id is hardcoded as the answer.
    ///
    /// With no override set this must report whatever discovery finds — which
    /// on a machine with no WinUSB-bound adapter is nothing at all. Asserting a
    /// particular Realtek here is what let the hardcoded fallback survive: the
    /// test passed on the one machine where the wrong answer was right.
    fn the_dongle_is_discovered_rather_than_assumed() {
        std::env::remove_var("FLEXINPUT_JC2_DONGLE");
        assert_eq!(configured_dongle(), flexinput_btle::preferred_dongle());

        std::env::set_var("FLEXINPUT_JC2_DONGLE", "0x1234:0x5678");
        assert_eq!(configured_dongle(), Some((0x1234, 0x5678)));
        std::env::remove_var("FLEXINPUT_JC2_DONGLE");
    }

    #[test]
    fn a_pair_is_the_link_limit() {
        assert_eq!(MAX_LINKS, 2, "two halves make one controller");
    }
}
