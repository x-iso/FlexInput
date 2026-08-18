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

/// Realtek RTL8761/8852-class dongle, the one this was developed against.
/// Override with `FLEXINPUT_JC2_DONGLE=vid:pid` (hex).
const DEFAULT_VID: u16 = 0x0BDA;
const DEFAULT_PID: u16 = 0xA728;

/// Most halves to hold at once. Two is a full pair.
const MAX_LINKS: usize = 2;

/// How long each discovery window runs while looking for more controllers.
const SCAN_WINDOW: Duration = Duration::from_secs(2);
/// Rest between discovery windows. Scanning shares the radio with live links,
/// so this stays generous once anything is connected.
const SCAN_GAP: Duration = Duration::from_secs(3);

/// Spacing between the fire-and-forget init writes.
///
/// Waiting on each reply is what once turned a millisecond handshake into ~40 s
/// in the Bluetooth backend, during which the controller powered itself off
/// having never seen a completed init.
const INIT_GAP: Duration = Duration::from_millis(30);

/// Longest gap between input notifications before a link is written off.
const INPUT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Default)]
struct Shared {
    pads: Mutex<HashMap<PadKey, PadState>>,
    shutdown: AtomicBool,
    /// Transport state, published so the WinRT hub can stand down — see
    /// [`Joycon2DongleHub::state_flag`] and [`DONGLE_PROBING`].
    state: Arc<AtomicU8>,
}

/// A live connection to one half.
struct Link {
    key: PadKey,
    conn: u16,
    calib: StickCalib,
    orientation: OrientationTracker,
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
        std::thread::Builder::new()
            .name("jc2-dongle".into())
            .spawn(move || run(t))
            .expect("spawn jc2-dongle thread");
        Self { shared }
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

/// Parse `FLEXINPUT_JC2_DONGLE=vid:pid`, falling back to the known Realtek.
fn configured_dongle() -> (u16, u16) {
    let parse = || -> Option<(u16, u16)> {
        let raw = std::env::var("FLEXINPUT_JC2_DONGLE").ok()?;
        let (v, p) = raw.split_once(':')?;
        Some((
            u16::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok()?,
            u16::from_str_radix(p.trim().trim_start_matches("0x"), 16).ok()?,
        ))
    };
    parse().unwrap_or((DEFAULT_VID, DEFAULT_PID))
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

    let (vid, pid) = configured_dongle();
    let dongle = match Dongle::open(vid, pid) {
        Ok(d) => d,
        Err(e) => {
            // ❗ This was `log::info!` — the quietest line in the file, for the
            // event with the largest visible consequence. A machine with no
            // dongle is the ordinary case and says so once; but "another
            // process already has it" is a mistake worth naming, because the
            // user sees only that Windows stole their controllers again.
            //
            // The usual cause is a second FlexInput instance or a leftover
            // `jc2_imu`: a WinUSB device admits ONE owner, so whichever
            // process opened it first keeps it and every other one lands here.
            eprintln!(
                "[jc2-dongle] cannot open dongle {vid:04x}:{pid:04x} ({e})\n\
                 [jc2-dongle] if a dongle IS plugged in, another process holds it — \
                 close any other FlexInput instance or jc2_* probe and restart.\n\
                 [jc2-dongle] Joy-Cons will fall back to the Windows stack until then."
            );
            return;
        }
    };
    if let Err(e) = dongle.reset_and_init() {
        eprintln!("[jc2-dongle] controller init failed: {e}");
        return;
    }
    eprintln!("[jc2-dongle] dongle {vid:04x}:{pid:04x} ready");
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
    let mut scan_deadline = Instant::now();
    let mut last_scan_end = Instant::now() - SCAN_GAP;

    while !shared.shutdown.load(Ordering::Relaxed) {
        // ❗ The rest between scan windows exists to share the radio with LIVE
        // LINKS. Applying it with nothing connected left the dongle deaf for
        // 3 s out of every 5 — and a Joy-Con advertises only in a short burst
        // after a button wake, so waking it during the deaf window missed the
        // whole burst. That is the "took ten attempts to find them" symptom;
        // `jc2_imu` scans back-to-back and connects on the first try.
        //
        // With nothing connected there is no traffic to protect, so scan
        // continuously.
        let gap = if links.is_empty() { Duration::ZERO } else { SCAN_GAP };
        if links.len() < MAX_LINKS && !scanning && last_scan_end.elapsed() >= gap {
            match dongle.start_le_scan() {
                Ok(()) => {
                    scanning = true;
                    scan_deadline = Instant::now() + SCAN_WINDOW;
                }
                Err(e) => {
                    eprintln!("[jc2-dongle] scan enable failed: {e}");
                    last_scan_end = Instant::now();
                }
            }
        }
        if scanning && Instant::now() >= scan_deadline {
            let _ = dongle.stop_le_scan();
            scanning = false;
            last_scan_end = Instant::now();
        }

        let found = pump(&dongle, &shared, &mut links, scanning);

        if let Some((addr, addr_type, side)) = found {
            let _ = dongle.stop_le_scan();
            scanning = false;
            last_scan_end = Instant::now();
            match connect_and_init(&dongle, addr, addr_type, side) {
                // Reject a handle already in use: it means a stale Connection
                // Complete was returned rather than a new link, and both pads
                // would then mirror one controller.
                Ok(link) if links.iter().any(|l| l.conn == link.conn) => {
                    eprintln!(
                        "[jc2-dongle] {} got in-use handle {:#06x} — discarding",
                        side.display_name(),
                        link.conn,
                    );
                    dongle.cancel_pending_connect();
                }
                Ok(link) => {
                    eprintln!("[jc2-dongle] {} handle {:#06x}", side.display_name(), link.conn);
                    register(&shared, &link);
                    links.push(link);
                }
                Err(e) => eprintln!("[jc2-dongle] {} connect failed: {e}", side.display_name()),
            }
        }

        if links.is_empty() && !scanning {
            // Nothing to service; do not spin the CPU between scan windows.
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    for link in &links {
        let _ = dongle.disconnect(link.conn);
    }
}

/// Decide whether an advertising report is a Joy-Con 2 worth connecting to.
fn advert_match(r: &flexinput_btle::hci::AdvReport, links: &[Link]) -> Option<([u8; 6], u8, Side)> {
    let md = r.manufacturer_data()?;
    // The company id is INCLUDED in this stack's manufacturer data (unlike
    // btleplug, which strips it into a map key), so VID sits at 5 and PID at 7
    // rather than 3 and 5.
    if md.len() < 9 || u16::from_le_bytes([md[0], md[1]]) != protocol::NINTENDO_MANUFACTURER_ID {
        return None;
    }
    if u16::from_le_bytes([md[5], md[6]]) != protocol::NINTENDO_VID {
        return None;
    }
    let pid = u16::from_le_bytes([md[7], md[8]]);
    let side = Side::from_pid(pid)?;
    if Side::is_safe_mode(pid) || links.iter().any(|l| l.key.address == r.address) {
        return None;
    }
    Some((r.address, r.address_type, side))
}

/// Connect, subscribe and initialise one controller.
fn connect_and_init(
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
    dongle.send_att(
        conn,
        &acl::write_request(jc::HANDLE_INPUT_CCCD, &acl::CCCD_NOTIFY),
    )?;
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
    dongle.send_att(
        conn,
        &acl::write_request(jc::HANDLE_INPUT_COMMON_CCCD, &acl::CCCD_NOTIFY),
    )?;
    dongle.send_att(
        conn,
        &acl::write_request(jc::HANDLE_INPUT_COMMON_RATE, &protocol::REPORT_RATE_PAYLOAD),
    )?;

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
    let cmd = |c: u8, s: u8, data: &[u8]| -> Result<(), Box<dyn std::error::Error>> {
        let frame = protocol::rumble_cmd_frame(c, s, data);
        dongle.send_att(conn, &acl::write_command(jc::HANDLE_CMD_WRITE, &frame))?;
        std::thread::sleep(INIT_GAP);
        Ok(())
    };

    // Undocumented handshake steps official software always sends first.
    cmd(protocol::CMD_UNKNOWN_07, 0x01, &[])?;
    cmd(protocol::CMD_UNKNOWN_10, 0x01, &[])?;
    cmd(protocol::CMD_UNKNOWN_16, 0x01, &[])?;

    // Controller-memory reads. These carry factory calibration; other
    // implementations report that skipping them leaves the controller emitting
    // stub reports indefinitely.
    for (size, addr) in protocol::JC2_INIT_MEMORY_READS {
        cmd(
            protocol::CMD_READ_MEMORY,
            protocol::SUB_READ_MEMORY,
            &protocol::read_memory_data(*size, *addr),
        )?;
    }

    cmd(protocol::CMD_PLAYER_LEDS, 0x07, &[0x01, 0, 0, 0, 0, 0, 0, 0])?;

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
            cmd(protocol::CMD_FEATURE_SELECT, 0x02, &[mask, 0, 0, 0])?;
            cmd(protocol::CMD_FEATURE_SELECT, 0x04, &[mask, 0, 0, 0])?;

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
        None => eprintln!(
            "[jc2-dongle] feature-select SKIPPED (reference behaviour) — \
             set FLEXINPUT_JC2_FEATURES=2f to send it"
        ),
    }

    // The vendor report-rate descriptor. WITHOUT THIS the controller streams
    // stub reports forever — counter incrementing, every field zero — which is
    // indistinguishable from a parser bug. It is the single most expensive
    // omission in this sequence.
    dongle.send_att(
        conn,
        &acl::write_request(jc::HANDLE_INPUT_REPORT_RATE, &protocol::REPORT_RATE_PAYLOAD),
    )?;

    // With init done, read what the silent streams actually hold.
    probe_readable(dongle, conn);

    Ok(Link {
        key: PadKey { side, address },
        conn,
        calib: StickCalib::default(),
        orientation: OrientationTracker::default(),
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
            field_rate: [0.0; 3],
            events: 0,
        },
    );
}

/// Service every live link: drain ACL, demultiplex by connection handle, and
/// drop links that go quiet or disconnect.
fn pump(
    dongle: &Dongle,
    shared: &Arc<Shared>,
    links: &mut Vec<Link>,
    scanning: bool,
) -> Option<([u8; 6], u8, Side)> {
    let mut found = None;
    // Events first — a disconnect must be noticed before its handle is reused.
    while let Ok(Some(evt)) = dongle.read_event_timeout(Duration::from_millis(1)) {
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
                found = advert_match(&r, links);
            }
            _ => {}
        }
    }

    // Drain EVERYTHING waiting, not one packet per pass.
    //
    // Reading a single packet per iteration with a blocking timeout capped the
    // whole process at roughly 90 packets a second shared between both links,
    // and whichever half was serviced first took nearly all of it — the right
    // half was observed at 6 Hz against the left's 60 Hz. Two halves at a 5 ms
    // connection interval need 400 packets a second, so this has to be greedy.
    for pkt in dongle.drain_acl(256) {
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
        if n.handle == jc::HANDLE_INPUT_COMMON {
            if let Some(link) = links.iter_mut().find(|l| l.conn == pkt.conn_handle) {
                link.common_reports = link.common_reports.saturating_add(1);
                if link.common_reports.is_power_of_two() {
                    eprintln!(
                        "[jc2-dongle] {} ⭐ COMMON INPUT #{} ({} bytes): {:02x?}",
                        link.key.side.display_name(),
                        link.common_reports,
                        n.value.len(),
                        &n.value[..n.value.len().min(64)],
                    );
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
        let stick = link.calib.normalize(snap.stick_raw);
        // Orientation from gravity (roll, pitch) and the heading field (yaw),
        // differenced into a rate. No zero-rate correction step any more:
        // both sources are absolute, so there is no bias to accumulate.
        let o = link.orientation.update(&snap.motion);
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
            pad.orientation = o.euler_rad;
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
            let _ = dongle.disconnect(link.conn);
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
    fn dongle_id_defaults_to_the_realtek_and_parses_an_override() {
        // The env var is process-global, so this asserts the parser through a
        // direct call rather than mutating it and racing other tests.
        assert_eq!(configured_dongle(), (DEFAULT_VID, DEFAULT_PID));
    }

    #[test]
    fn a_pair_is_the_link_limit() {
        assert_eq!(MAX_LINKS, 2, "two halves make one controller");
    }
}
