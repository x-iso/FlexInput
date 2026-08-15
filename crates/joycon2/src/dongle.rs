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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flexinput_btle::{acl, joycon as jc, Dongle, Event};

use crate::hub::{PadKey, PadState};
use crate::protocol::{self, Side};
use crate::reports::{self, GyroBias, PadSnapshot, StickCalib};

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
}

/// A live connection to one half.
struct Link {
    key: PadKey,
    conn: u16,
    calib: StickCalib,
    gyro_bias: GyroBias,
    last_input: Instant,
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

    pub fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::Relaxed);
    }
}

impl Drop for Joycon2DongleHub {
    fn drop(&mut self) {
        self.shutdown();
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

fn run(shared: Arc<Shared>) {
    let (vid, pid) = configured_dongle();
    let dongle = match Dongle::open(vid, pid) {
        Ok(d) => d,
        Err(e) => {
            // Not an error worth shouting about: most users have no dongle
            // bound to WinUSB, and the other transports still work.
            log::info!("joycon2-dongle: no dongle at {vid:04x}:{pid:04x} ({e})");
            return;
        }
    };
    if let Err(e) = dongle.reset_and_init() {
        eprintln!("[jc2-dongle] controller init failed: {e}");
        return;
    }
    eprintln!("[jc2-dongle] dongle {vid:04x}:{pid:04x} ready");

    let mut links: Vec<Link> = Vec::new();
    let mut last_scan = Instant::now() - SCAN_GAP;

    while !shared.shutdown.load(Ordering::Relaxed) {
        if links.len() < MAX_LINKS && last_scan.elapsed() >= SCAN_GAP {
            last_scan = Instant::now();
            if let Some((addr, addr_type, side)) = discover(&dongle, &links) {
                match connect_and_init(&dongle, addr, addr_type, side) {
                    Ok(link) => {
                        register(&shared, &link);
                        links.push(link);
                    }
                    Err(e) => eprintln!("[jc2-dongle] {} connect failed: {e}", side.display_name()),
                }
            }
        }

        pump(&dongle, &shared, &mut links);

        if links.is_empty() {
            // Nothing to service; avoid spinning the CPU between scans.
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    for link in &links {
        let _ = dongle.disconnect(link.conn);
    }
}

/// Scan for a Joy-Con 2 that is not already connected.
fn discover(dongle: &Dongle, links: &[Link]) -> Option<([u8; 6], u8, Side)> {
    if let Err(e) = dongle.start_le_scan() {
        // Surfaced, not swallowed: a refused scan-enable is how "no controllers
        // ever appear" happens, and it gives no other symptom.
        eprintln!("[jc2-dongle] scan enable failed: {e}");
        return None;
    }
    let deadline = Instant::now() + SCAN_WINDOW;
    let mut found = None;
    while Instant::now() < deadline {
        if let Ok(Some(Event::LeAdvertisingReport(r))) = dongle.read_event_timeout(Duration::from_millis(100)) {
            let Some(md) = r.manufacturer_data() else { continue };
            // Company id is INCLUDED in this stack's manufacturer data (unlike
            // btleplug, which strips it into a map key), so VID sits at 5 and
            // PID at 7 rather than 3 and 5.
            if md.len() < 9 || u16::from_le_bytes([md[0], md[1]]) != protocol::NINTENDO_MANUFACTURER_ID {
                continue;
            }
            if u16::from_le_bytes([md[5], md[6]]) != protocol::NINTENDO_VID {
                continue;
            }
            let pid = u16::from_le_bytes([md[7], md[8]]);
            let Some(side) = Side::from_pid(pid) else { continue };
            if protocol::Side::is_safe_mode(pid) {
                continue;
            }
            if links.iter().any(|l| l.key.address == r.address) {
                continue;
            }
            found = Some((r.address, r.address_type, side));
            break;
        }
    }
    let _ = dongle.stop_le_scan();
    found
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
    let conn = dongle.le_connect(address, address_type)?;
    eprintln!(
        "[jc2-dongle] {} connected, handle {conn:#06x}",
        side.display_name()
    );

    // Raise the ATT MTU before anything else. At the 23-byte default a 63-byte
    // input report arrives fragmented and every parser offset is wrong.
    dongle.send_att(conn, &acl::exchange_mtu_request(jc::DESIRED_MTU))?;
    dongle.send_att(
        conn,
        &acl::write_request(jc::HANDLE_INPUT_CCCD, &acl::CCCD_NOTIFY),
    )?;
    dongle.send_att(
        conn,
        &acl::write_request(jc::HANDLE_CMD_RESPONSE_CCCD, &acl::CCCD_NOTIFY),
    )?;

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
    cmd(protocol::CMD_FEATURE_SELECT, 0x02, &[protocol::feature::JOYCON2_DEFAULT, 0, 0, 0])?;
    cmd(protocol::CMD_FEATURE_SELECT, 0x04, &[protocol::feature::JOYCON2_DEFAULT, 0, 0, 0])?;

    // The vendor report-rate descriptor. WITHOUT THIS the controller streams
    // stub reports forever — counter incrementing, every field zero — which is
    // indistinguishable from a parser bug. It is the single most expensive
    // omission in this sequence.
    dongle.send_att(
        conn,
        &acl::write_request(jc::HANDLE_INPUT_REPORT_RATE, &protocol::REPORT_RATE_PAYLOAD),
    )?;

    Ok(Link {
        key: PadKey { side, address },
        conn,
        calib: StickCalib::default(),
        gyro_bias: GyroBias::default(),
        last_input: Instant::now(),
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
            events: 0,
        },
    );
}

/// Service every live link: drain ACL, demultiplex by connection handle, and
/// drop links that go quiet or disconnect.
fn pump(dongle: &Dongle, shared: &Arc<Shared>, links: &mut Vec<Link>) {
    // Events first — a disconnect must be noticed before its handle is reused.
    while let Ok(Some(evt)) = dongle.read_event_timeout(Duration::from_millis(1)) {
        if let Event::DisconnectionComplete { conn_handle, reason } = evt {
            if let Some(pos) = links.iter().position(|l| l.conn == conn_handle) {
                let link = links.remove(pos);
                eprintln!(
                    "[jc2-dongle] {} disconnected (reason {reason:#04x})",
                    link.key.side.display_name()
                );
                shared.pads.lock().unwrap().remove(&link.key);
            }
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
        if n.handle != jc::HANDLE_INPUT_VALUE {
            continue;
        }
        let Some(link) = links.iter_mut().find(|l| l.conn == pkt.conn_handle) else {
            continue;
        };
        let Some(snap) = reports::parse_input(link.key.side, &n.value) else { continue };
        let stick = link.calib.normalize(snap.stick_raw);
        let gyro = link.gyro_bias.correct(snap.motion.gyro);
        link.last_input = Instant::now();
        if let Some(pad) = shared.pads.lock().unwrap().get_mut(&link.key) {
            pad.streaming = true;
            pad.snapshot = snap;
            pad.stick = stick;
            pad.gyro = gyro;
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
