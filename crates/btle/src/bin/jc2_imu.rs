//! Guided IMU field-mapping probe — both halves at once.
//!
//! The motion block's packing is documented as "unknown", the published layout
//! for reports 0x07/0x08 is wrong over BLE, and the offsets currently in
//! `flexinput-joycon2` came from an AT-REST capture. That can locate the
//! accelerometer, because gravity is a constant-magnitude vector, but it can
//! never locate the gyro: a stationary gyro reads zero and is indistinguishable
//! from padding.
//!
//! # Method
//!
//! Both halves are connected first, and nothing starts until both are
//! streaming — with the halves clipped into the (unplugged) charging grip they
//! are rigidly coupled, so one physical motion drives both and their readings
//! are directly comparable. That coupling is the strongest cross-check
//! available here: a genuine gyro axis must respond on BOTH halves with equal
//! magnitude, because they physically cannot rotate at different rates.
//!
//! Each axis is exercised as **90° counter-clockwise → hold → 90° clockwise →
//! hold**, and motion phases are recorded separately from hold phases. That
//! separation is what tells the two sensors apart:
//!
//! * a **gyro** axis spikes while MOVING and returns to rest while HOLDING;
//! * an **accelerometer** axis is quiet while moving at constant rate but
//!   settles to a *different value* in each hold, because gravity has rotated
//!   in the sensor's frame.
//!
//! Recording counter-clockwise and clockwise separately also recovers the
//! **sign** of each axis, which a symmetric shake cannot: the two directions
//! must produce opposite-signed peaks.
//!
//! Usage:
//!   cargo run -p flexinput-btle --bin jc2_imu
//!   cargo run -p flexinput-btle --bin jc2_imu -- --mag

use std::collections::HashMap;
use std::time::{Duration, Instant};

use flexinput_btle::{acl, joycon as jc, Dongle, Event};

const DONGLE_VID: u16 = 0x0BDA;
const DONGLE_PID: u16 = 0xA728;
const NINTENDO_COMPANY_ID: u16 = 0x0553;

/// Byte range searched, interpreted as overlapping little-endian `i16`s.
/// The real alignment is unknown; assuming it would beg the question.
const SEARCH_START: usize = 8;
const SEARCH_END: usize = 61;

/// What a phase is for, which decides how its numbers are read.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// Stationary reference. Establishes the noise floor and the neutral value.
    Neutral,
    /// Rotating. Gyro axes spike here.
    Move,
    /// Held at 90°. Accelerometer axes settle to a new value here.
    Hold,
}

struct Phase {
    name: &'static str,
    axis: usize, // 0 roll, 1 pitch, 2 yaw; usize::MAX for neutral
    kind: Kind,
    ccw: bool,
    instruction: &'static str,
    secs: u64,
}

const AXES: [&str; 3] = ["roll", "pitch", "yaw"];

const PHASES: &[Phase] = &[
    Phase { name: "neutral", axis: usize::MAX, kind: Kind::Neutral, ccw: false,
        instruction: "Grip FLAT on the table, both halves attached. DO NOT TOUCH.", secs: 5 },

    Phase { name: "roll_ccw", axis: 0, kind: Kind::Move, ccw: true,
        instruction: "ROLL 90 deg COUNTER-CLOCKWISE: raise the LEFT edge until it stands on its side", secs: 4 },
    Phase { name: "roll_ccw_hold", axis: 0, kind: Kind::Hold, ccw: true,
        instruction: "HOLD it there, steady", secs: 3 },
    Phase { name: "roll_cw", axis: 0, kind: Kind::Move, ccw: false,
        instruction: "ROLL back through flat and 90 deg CLOCKWISE: raise the RIGHT edge", secs: 4 },
    Phase { name: "roll_cw_hold", axis: 0, kind: Kind::Hold, ccw: false,
        instruction: "HOLD it there, steady", secs: 3 },

    Phase { name: "pitch_ccw", axis: 1, kind: Kind::Move, ccw: true,
        instruction: "Return flat. PITCH 90 deg CCW: tip the FAR edge DOWN / near edge up", secs: 4 },
    Phase { name: "pitch_ccw_hold", axis: 1, kind: Kind::Hold, ccw: true,
        instruction: "HOLD it there, steady", secs: 3 },
    Phase { name: "pitch_cw", axis: 1, kind: Kind::Move, ccw: false,
        instruction: "PITCH back through flat and 90 deg CW: tip the FAR edge UP", secs: 4 },
    Phase { name: "pitch_cw_hold", axis: 1, kind: Kind::Hold, ccw: false,
        instruction: "HOLD it there, steady", secs: 3 },

    Phase { name: "yaw_ccw", axis: 2, kind: Kind::Move, ccw: true,
        instruction: "Lay it FLAT again. YAW 90 deg CCW: spin it left, flat on the table", secs: 4 },
    Phase { name: "yaw_ccw_hold", axis: 2, kind: Kind::Hold, ccw: true,
        instruction: "HOLD it there, steady", secs: 3 },
    Phase { name: "yaw_cw", axis: 2, kind: Kind::Move, ccw: false,
        instruction: "YAW back through neutral and 90 deg CW: spin it right", secs: 4 },
    Phase { name: "yaw_cw_hold", axis: 2, kind: Kind::Hold, ccw: false,
        instruction: "HOLD it there, steady", secs: 3 },

    Phase { name: "neutral2", axis: usize::MAX, kind: Kind::Neutral, ccw: false,
        instruction: "Return to FLAT neutral and let go (checks the rest reading repeats)", secs: 5 },
];

#[derive(Clone, Copy, Default)]
struct Stat {
    min: i32,
    max: i32,
    sum: i64,
    n: u32,
    seen: bool,
}

impl Stat {
    fn push(&mut self, v: i16) {
        let v = v as i32;
        if !self.seen {
            self.min = v;
            self.max = v;
            self.seen = true;
        }
        self.min = self.min.min(v);
        self.max = self.max.max(v);
        self.sum += v as i64;
        self.n += 1;
    }
    fn range(&self) -> i32 {
        if self.seen { self.max - self.min } else { 0 }
    }
    fn mean(&self) -> i32 {
        if self.n == 0 { 0 } else { (self.sum / self.n as i64) as i32 }
    }
    /// Largest absolute excursion, keeping its sign — this is what recovers
    /// axis direction from a counter-clockwise vs clockwise comparison.
    fn signed_peak(&self) -> i32 {
        if !self.seen { 0 } else if self.max.abs() >= self.min.abs() { self.max } else { self.min }
    }
}

struct Link {
    conn: u16,
    side: &'static str,
}

fn main() {
    let want_mag = std::env::args().any(|a| a == "--mag");

    let dongle = match Dongle::open(DONGLE_VID, DONGLE_PID) {
        Ok(d) => d,
        Err(e) => { eprintln!("[imu] cannot open dongle: {e}"); std::process::exit(1); }
    };
    if let Err(e) = dongle.reset_and_init() {
        eprintln!("[imu] init failed: {e}");
        std::process::exit(1);
    }

    // Both halves before anything starts. Mapping one at a time would mean two
    // separate physical runs whose motions cannot be compared, which throws
    // away the rigid coupling that makes this measurement trustworthy.
    let links = connect_both(&dongle, want_mag);
    if links.is_empty() {
        eprintln!("[imu] no controllers connected");
        std::process::exit(1);
    }
    if links.len() < 2 {
        println!("\n[imu] WARNING: only one half connected — the cross-check between");
        println!("[imu] halves will not be available. Wake the other and rerun for the full map.\n");
    }

    println!("\n[imu] === guided sequence: ~55 s, follow each instruction ===");
    println!("[imu] Keep BOTH halves clipped into the grip the whole time.\n");

    // phase -> conn -> per-offset stats
    let mut recorded: Vec<HashMap<u16, Vec<Stat>>> = Vec::new();
    for phase in PHASES {
        println!("\n>>> {}", phase.instruction);
        for n in (1..=3).rev() {
            println!("    {n}…");
            std::thread::sleep(Duration::from_secs(1));
        }
        // Phase name echoed so a run's console output can be lined up against
        // the table afterwards without counting steps.
        println!("    GO [{}] ({} s)", phase.name, phase.secs);
        let (stats, samples) = record(&dongle, &links, Duration::from_secs(phase.secs));
        println!("    done: {samples:?}");
        recorded.push(stats);
    }

    for link in &links {
        let _ = dongle.disconnect(link.conn);
    }
    for link in &links {
        report(link, &recorded);
    }
    cross_check(&links, &recorded);
}

/// Connect and initialise every Joy-Con we can find, up to a pair.
fn connect_both(dongle: &Dongle, want_mag: bool) -> Vec<Link> {
    let mut links: Vec<Link> = Vec::new();
    let mut addrs: Vec<[u8; 6]> = Vec::new();
    println!("[imu] looking for BOTH halves — wake them now (any button)");

    let deadline = Instant::now() + Duration::from_secs(40);
    while links.len() < 2 && Instant::now() < deadline {
        let Some((addr, addr_type, pid)) = scan_once(dongle, &addrs) else { continue };
        let side = if pid == 0x2066 { "RIGHT" } else { "LEFT" };
        match dongle.le_connect(addr, addr_type) {
            Ok(conn) => {
                init(dongle, conn, want_mag);
                println!("[imu] {side} connected ({} of 2)", links.len() + 1);
                addrs.push(addr);
                links.push(Link { conn, side });
            }
            Err(e) => eprintln!("[imu] {side} connect failed: {e}"),
        }
    }
    links
}

fn scan_once(dongle: &Dongle, known: &[[u8; 6]]) -> Option<([u8; 6], u8, u16)> {
    dongle.start_le_scan().ok()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut found = None;
    while Instant::now() < deadline {
        if let Ok(Some(Event::LeAdvertisingReport(r))) =
            dongle.read_event_timeout(Duration::from_millis(100))
        {
            let Some(md) = r.manufacturer_data() else { continue };
            if md.len() < 9 || u16::from_le_bytes([md[0], md[1]]) != NINTENDO_COMPANY_ID {
                continue;
            }
            if known.contains(&r.address) {
                continue;
            }
            found = Some((r.address, r.address_type, u16::from_le_bytes([md[7], md[8]])));
            break;
        }
    }
    let _ = dongle.stop_le_scan();
    found
}

/// Record one phase across every link at once.
fn record(
    dongle: &Dongle,
    links: &[Link],
    dur: Duration,
) -> (HashMap<u16, Vec<Stat>>, HashMap<&'static str, u32>) {
    let mut stats: HashMap<u16, Vec<Stat>> =
        links.iter().map(|l| (l.conn, vec![Stat::default(); SEARCH_END])).collect();
    let mut samples: HashMap<&'static str, u32> = links.iter().map(|l| (l.side, 0)).collect();

    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        if let Ok(Some(Event::DisconnectionComplete { reason, .. })) =
            dongle.read_event_timeout(Duration::from_millis(1))
        {
            eprintln!("[imu] a link dropped mid-phase (reason {reason:#04x})");
        }
        let Ok(Some(pkt)) = dongle.read_acl(Duration::from_millis(10)) else { continue };
        if pkt.cid != acl::CID_ATT {
            continue;
        }
        let Some(n) = acl::parse_notification(&pkt.payload) else { continue };
        if n.handle != jc::HANDLE_INPUT_VALUE {
            continue;
        }
        // Demultiplex by connection handle: both halves stream into the same
        // ACL pipe, and mixing them would describe neither.
        let Some(s) = stats.get_mut(&pkt.conn_handle) else { continue };
        if let Some(link) = links.iter().find(|l| l.conn == pkt.conn_handle) {
            *samples.entry(link.side).or_default() += 1;
        }
        for off in SEARCH_START..SEARCH_END.min(n.value.len().saturating_sub(1)) {
            s[off].push(i16::from_le_bytes([n.value[off], n.value[off + 1]]));
        }
    }
    (stats, samples)
}

/// Classification of one candidate offset.
struct Verdict {
    off: usize,
    label: String,
    rest: i32,
    best_move: i32,
    best_hold: i32,
    axis: usize,
    ccw_peak: i32,
    cw_peak: i32,
}

fn classify(conn: u16, rec: &[HashMap<u16, Vec<Stat>>]) -> Vec<Verdict> {
    let stat = |pi: usize, off: usize| -> Stat {
        rec[pi].get(&conn).map(|v| v[off]).unwrap_or_default()
    };
    let neutral_idx: Vec<usize> = PHASES.iter().enumerate()
        .filter(|(_, p)| p.kind == Kind::Neutral).map(|(i, _)| i).collect();

    let mut out = Vec::new();
    for off in SEARCH_START..SEARCH_END {
        // Noise floor and neutral reference, from both stationary phases.
        let rest = neutral_idx.iter().map(|i| stat(*i, off).range()).max().unwrap_or(0);
        let neutral_mean = stat(neutral_idx[0], off).mean();

        let mut best_move = 0;
        let mut best_hold = 0;
        let mut axis = 0usize;
        let mut ccw_peak = 0;
        let mut cw_peak = 0;

        for a in 0..3 {
            let mv: i32 = PHASES.iter().enumerate()
                .filter(|(_, p)| p.axis == a && p.kind == Kind::Move)
                .map(|(i, _)| stat(i, off).range()).max().unwrap_or(0);
            // How far the HELD value drifts from neutral — the accelerometer's
            // signature, since gravity has rotated in the sensor frame.
            let hold: i32 = PHASES.iter().enumerate()
                .filter(|(_, p)| p.axis == a && p.kind == Kind::Hold)
                .map(|(i, _)| (stat(i, off).mean() - neutral_mean).abs()).max().unwrap_or(0);
            if mv > best_move {
                best_move = mv;
                axis = a;
                ccw_peak = PHASES.iter().enumerate()
                    .find(|(_, p)| p.axis == a && p.kind == Kind::Move && p.ccw)
                    .map(|(i, _)| stat(i, off).signed_peak()).unwrap_or(0);
                cw_peak = PHASES.iter().enumerate()
                    .find(|(_, p)| p.axis == a && p.kind == Kind::Move && !p.ccw)
                    .map(|(i, _)| stat(i, off).signed_peak()).unwrap_or(0);
            }
            best_hold = best_hold.max(hold);
        }

        let floor = (rest * 4).max(300);
        let label = if best_move > floor && best_move > best_hold * 2 {
            format!("GYRO {}", AXES[axis])
        } else if best_hold > floor {
            "ACCEL".to_string()
        } else {
            "-".to_string()
        };
        out.push(Verdict { off, label, rest, best_move, best_hold, axis, ccw_peak, cw_peak });
    }
    out.sort_by_key(|v| -(v.best_move.max(v.best_hold)));
    out
}

fn report(link: &Link, rec: &[HashMap<u16, Vec<Stat>>]) {
    println!("\n\n========== {} HALF — FIELD MAP ==========", link.side);
    println!("{:>5} {:>11} {:>7} {:>9} {:>9} {:>9} {:>9}",
        "off", "verdict", "rest", "move", "hold", "ccw_peak", "cw_peak");
    for v in classify(link.conn, rec).iter().take(18) {
        println!("{:>5} {:>11} {:>7} {:>9} {:>9} {:>9} {:>9}",
            v.off, v.label, v.rest, v.best_move, v.best_hold, v.ccw_peak, v.cw_peak);
    }
    println!("\nGYRO rows: ccw_peak and cw_peak should have OPPOSITE signs. If they do not,");
    println!("the field is probably not a gyro axis, or the motion was not clean.");
    println!("ACCEL rows: 1 g = 4096 LSB, so a 90 deg tilt moves an axis by about that much.");
    println!("Offsets overlap by a byte, so a real field is a strong row with weak neighbours.");
}

/// Compare the two halves.
///
/// They are bolted together, so they cannot rotate at different rates: a real
/// gyro axis MUST respond on both with similar magnitude. Anything that only
/// lights up on one half is a mis-identification. Their accelerometer readings
/// at neutral differ only by the fixed mounting orientation, which is exactly
/// the pivot offset needed to bring both into one frame.
fn cross_check(links: &[Link], rec: &[HashMap<u16, Vec<Stat>>]) {
    if links.len() < 2 {
        return;
    }
    println!("\n\n========== CROSS-CHECK (halves are rigidly coupled) ==========");
    for a in 0..3 {
        println!("\n-- {} --", AXES[a]);
        for link in links {
            let best = classify(link.conn, rec).into_iter()
                .filter(|v| v.label.starts_with("GYRO") && v.axis == a)
                .max_by_key(|v| v.best_move);
            match best {
                Some(v) => println!(
                    "  {:>5}: offset {:>3}  move={:<7} ccw={:<7} cw={:<7}",
                    link.side, v.off, v.best_move, v.ccw_peak, v.cw_peak),
                None => println!("  {:>5}: no gyro axis identified", link.side),
            }
        }
    }

    println!("\n-- neutral accelerometer values (the fixed mounting offset) --");
    println!("Same physical orientation, so any difference between the halves IS the");
    println!("mounting rotation, and is what a shared frame has to correct for.");
    for link in links {
        let accel: Vec<String> = classify(link.conn, rec).into_iter()
            .filter(|v| v.label == "ACCEL")
            .take(4)
            .map(|v| {
                let m = rec[0].get(&link.conn).map(|s| s[v.off].mean()).unwrap_or(0);
                format!("off{}={m}", v.off)
            })
            .collect();
        println!("  {:>5}: {}", link.side, accel.join("  "));
    }
}

fn cmd_frame(cmd: u8, sub: u8, data: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; 17];
    out.extend_from_slice(&[cmd, 0x91, 0x01, sub, 0x00, data.len() as u8, 0x00, 0x00]);
    out.extend_from_slice(data);
    out
}

fn init(dongle: &Dongle, conn: u16, want_mag: bool) {
    let _ = dongle.send_att(conn, &acl::exchange_mtu_request(jc::DESIRED_MTU));
    let _ = dongle.send_att(conn, &acl::write_request(jc::HANDLE_INPUT_CCCD, &acl::CCCD_NOTIFY));
    let _ = dongle.send_att(conn, &acl::write_request(jc::HANDLE_CMD_RESPONSE_CCCD, &acl::CCCD_NOTIFY));

    let send = |c: u8, s: u8, d: &[u8]| {
        let _ = dongle.send_att(conn, &acl::write_command(jc::HANDLE_CMD_WRITE, &cmd_frame(c, s, d)));
        std::thread::sleep(Duration::from_millis(30));
    };
    send(0x07, 0x01, &[]);
    send(0x10, 0x01, &[]);
    send(0x16, 0x01, &[]);
    for (size, addr) in [
        (0x40u8, 0x013000u32), (0x40, 0x013080), (0x40, 0x1FC040),
        (0x10, 0x013040), (0x18, 0x013100), (0x20, 0x013060),
    ] {
        let mut d = vec![size, 0x7E, 0x00, 0x00];
        d.extend_from_slice(&addr.to_le_bytes());
        send(0x02, 0x04, &d);
    }
    send(0x09, 0x07, &[0x01, 0, 0, 0, 0, 0, 0, 0]);

    // 0x37 is what official software sends; 0xB7 adds the magnetometer bit it
    // never sets. A half without one is expected to ignore the extra bit.
    let features: u8 = if want_mag { 0xB7 } else { 0x37 };
    send(0x0C, 0x02, &[features, 0, 0, 0]);
    send(0x0C, 0x04, &[features, 0, 0, 0]);

    let _ = dongle.send_att(
        conn,
        &acl::write_request(jc::HANDLE_INPUT_REPORT_RATE, &jc::REPORT_RATE_PAYLOAD),
    );
    std::thread::sleep(Duration::from_millis(150));
}
