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

/// Longest gap between input notifications before a link is written off.
const INPUT_TIMEOUT: Duration = Duration::from_secs(3);

/// First and longest gap between attempts to start a scan that was refused.
const SCAN_RETRY_MIN: Duration = Duration::from_millis(200);
const SCAN_RETRY_MAX: Duration = Duration::from_secs(5);

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
    calib: StickCalib,
    orientation: OrientationTracker,
    last_input: Instant,
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
            "[jc2-dongle] no WinUSB-bound Bluetooth adapter found — bind one with \
             Zadig, or set FLEXINPUT_JC2_DONGLE=vid:pid. Joy-Cons will fall back \
             to the Windows stack."
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
    let mut scan_backoff = SCAN_RETRY_MIN;

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
                    scan_backoff = SCAN_RETRY_MIN;
                    dlog!("scan START ({} link(s) held)", links.len());
                }
                Err(e) => {
                    eprintln!("[jc2-dongle] scan enable failed: {e}");
                    dlog!("scan ENABLE FAILED: {e}");
                    // Back off briefly rather than spinning on a controller
                    // that is busy — usually a half-finished initiator, which
                    // `start_le_scan` cancels on the next attempt.
                    //
                    // ⛔ And back off FURTHER each time it keeps failing. Each
                    // attempt is a radio lease, and a lease stops the shared
                    // reader for every transport — so a scan that refuses to
                    // start, retried five times a second forever, would black
                    // out a Bluetooth Classic controller's reconnection for as
                    // long as it went on.
                    scan_retry_at = Instant::now() + scan_backoff;
                    scan_backoff = (scan_backoff * 2).min(SCAN_RETRY_MAX);
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
    let known_side = shared.known.lock().unwrap().get(&r.address).copied();
    if let Some(side) = known_side {
        if links.iter().any(|l| l.key.address == r.address) {
            return None;
        }
        return Some((r.address, r.address_type, side));
    }
    // ⛔ No log line per rejection. A passive scan in an ordinary room reports
    // hundreds of advertisements a second from devices that are not Joy-Cons,
    // and logging each one was a firehose on a thread that shares its radio.
    let md = r.manufacturer_data()?;
    // The company id is INCLUDED in this stack's manufacturer data (unlike
    // btleplug, which strips it into a map key), so VID sits at 5 and PID at 7
    // rather than 3 and 5.
    if md.len() < 9 {
        return None;
    }
    if u16::from_le_bytes([md[0], md[1]]) != protocol::NINTENDO_MANUFACTURER_ID {
        return None;
    }
    if u16::from_le_bytes([md[5], md[6]]) != protocol::NINTENDO_VID {
        return None;
    }
    let pid = u16::from_le_bytes([md[7], md[8]]);
    let side = Side::from_pid(pid)?;
    if Side::is_safe_mode(pid) {
        return None;
    }
    if links.iter().any(|l| l.key.address == r.address) {
        return None;
    }
    dlog!("adv {:02x?} {} MATCH — pid {pid:#06x}", r.address, side.display_name());
    shared.known.lock().unwrap().insert(r.address, side);
    Some((r.address, r.address_type, side))
}

/// Connect, subscribe and initialise one controller.
///
/// ⭐ **The working recipe and nothing more.** Everything here runs inside a
/// radio lease, and a lease stops the shared reader for every transport on the
/// dongle — so each extra write is time a Bluetooth Classic controller cannot
/// be heard. The protocol investigation that used to live here (probe reads,
/// attribute walks, the common and extra input streams, mirrored commands)
/// added the better part of a second per connect and found nothing usable;
/// its conclusions are recorded in `docs/BLUETOOTH_TRANSPORTS.md`.
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
    //
    // Fixed 7.5 ms, the BLE spec minimum. Lower is rejected; asking for it
    // directly measured ~140-200 Hz of input against the ~67 Hz this link used
    // to run at.
    let link = dongle.le_connect_params(address, address_type, 6, 6)?;
    let conn = link.conn_handle;
    eprintln!(
        "[jc2-dongle] {} connected, handle {conn:#06x}, interval {:.2} ms",
        side.display_name(),
        link.interval_ms(),
    );
    // The interval cannot go lower, so more reports per second can only come
    // from moving them faster on the air. Best effort: status-only, and a link
    // that stays on 1M streams fine.
    let _ = dongle.request_2m_phy(conn);

    // Raise the ATT MTU before anything else. At the 23-byte default a 63-byte
    // input report arrives fragmented and every parser offset is wrong.
    dongle.send_att(conn, &acl::exchange_mtu_request(jc::DESIRED_MTU))?;

    // Input notifications, and command responses on both response channels —
    // pairing waits on `0x001a`.
    dongle.send_att(conn, &acl::write_request(jc::HANDLE_INPUT_CCCD, &acl::CCCD_NOTIFY))?;
    dongle.send_att(
        conn,
        &acl::write_request(jc::HANDLE_CMD_RESPONSE_CCCD, &acl::CCCD_NOTIFY),
    )?;
    dongle.send_att(
        conn,
        &acl::write_request(jc::HANDLE_CMD_RESPONSE_PERSIDE_CCCD, &acl::CCCD_NOTIFY),
    )?;
    // ⛔ The vendor report-rate descriptor. WITHOUT THIS the controller streams
    // stub reports forever — counter incrementing, every field zero — which is
    // indistinguishable from a parser bug.
    dongle.send_att(
        conn,
        &acl::write_request(jc::HANDLE_INPUT_REPORT_RATE, &protocol::REPORT_RATE_PAYLOAD),
    )?;

    // ❗ Commands go to the handle the controller EXECUTES from (`0x0016`).
    // `0x0014` accepts writes and acts on none of them.
    debug_assert!(jc::executes_commands(jc::HANDLE_CMD_WRITE));
    // Fire-and-forget with a short gap. Waiting on each reply once turned a
    // millisecond handshake into ~40 s and the controller powered off mid-way.
    //
    // ⭐ The gap DRAINS rather than sleeps: while this half initialises, the
    // other one is still streaming into the dongle, and a controller whose
    // buffers fill stops being able to send.
    let cmd = |c: u8, s: u8, data: &[u8]| -> Result<(), Box<dyn std::error::Error>> {
        let frame = protocol::rumble_cmd_frame(c, s, data);
        dongle.send_att(conn, &acl::write_command(jc::HANDLE_CMD_WRITE, &frame))?;
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

    // ⭐ Pair BEFORE the rest of init, so the flash writes never land in the
    // middle of a live stream.
    //
    // ❗ The lookup is a STATEMENT, not an `if let` scrutinee. A `MutexGuard`
    // created in an `if let` condition lives until the end of the whole
    // if/else chain, so locking `paired` again inside it deadlocks the thread.
    let already_paired = shared.paired.lock().unwrap().get(&address).copied();
    if already_paired.is_none() {
        if let Some(ltk) = run_pairing(dongle, conn, side) {
            shared.paired.lock().unwrap().insert(address, ltk);
        }
    }

    // Controller-memory reads, which carry factory calibration.
    for (size, addr) in protocol::JC2_INIT_MEMORY_READS {
        cmd(
            protocol::CMD_READ_MEMORY,
            protocol::SUB_READ_MEMORY,
            &protocol::read_memory_data(*size, *addr),
        )?;
    }

    // Connection feedback: the buzz, then the player LED.
    cmd(protocol::CMD_VIBRATION, 0x02, &[0x03, 0, 0, 0])?;
    cmd(protocol::CMD_PLAYER_LEDS, 0x07, &[0x01, 0, 0, 0, 0, 0, 0, 0])?;

    // Feature select, in the captured order: INIT, then `0x11/0x03`, the
    // vibration payload `0x0a/0x08`, `0x11/0x01`, and only then the confirm.
    // Skipping it leaves `motion_len = 0` forever.
    let mask = protocol::feature::JOYCON2_DEFAULT;
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

    dlog!("init: COMPLETE");
    Ok(Link {
        key: PadKey { side, address },
        conn,
        calib: StickCalib::default(),
        orientation: OrientationTracker::default(),
        last_input: Instant::now(),
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
            _ => {}
        }
    }

    // Bounded, so a flood of input cannot keep this from returning to the scan
    // and connect logic in the caller.
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
        if n.handle != jc::HANDLE_INPUT_VALUE {
            continue;
        }
        let Some(link) = links.iter_mut().find(|l| l.conn == pkt.conn_handle) else {
            continue;
        };
        let Some(snap) = reports::parse_input(link.key.side, &n.value) else {
            link.unparsed = link.unparsed.saturating_add(1);
            if link.unparsed.is_power_of_two() {
                eprintln!(
                    "[jc2-dongle] {} {} report(s) FAILED TO PARSE, {} bytes",
                    link.key.side.display_name(),
                    link.unparsed,
                    n.value.len(),
                );
            }
            continue;
        };
        let stick = link.calib.normalize(snap.stick_raw);
        link.orientation.set_resting_drift(crate::cal::field_drift(&link.key));
        let o = link.orientation.update(&snap.motion, link.key.side);
        link.last_input = Instant::now();
        if let Some(pad) = shared.pads.lock().unwrap().get_mut(&link.key) {
            pad.streaming = true;
            pad.snapshot = snap;
            pad.stick = stick;
            pad.gyro = o.rate_dps;
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
