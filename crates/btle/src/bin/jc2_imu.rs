//! Guided IMU field-mapping probe.
//!
//! The Joy-Con 2 motion block's packing is documented as "unknown", the
//! published layout for reports 0x07/0x08 is wrong over BLE, and the offsets in
//! `flexinput-joycon2` were reverse-engineered from an at-rest capture — which
//! can locate the accelerometer (gravity is a constant-magnitude vector) but
//! **cannot** locate the gyro, because a stationary gyro reads zero and is
//! indistinguishable from padding.
//!
//! So this does not guess. It walks through a fixed sequence of physical
//! motions, records how every candidate 16-bit field behaves in each phase, and
//! prints a table. A field that swings hard during exactly one rotation phase
//! and stays quiet in the others is that rotation's gyro axis. A field that
//! tracks orientation but not rotation is an accelerometer axis.
//!
//! Usage:
//!   cargo run -p flexinput-btle --bin jc2_imu
//!   cargo run -p flexinput-btle --bin jc2_imu -- --mag     (ask for magnetometer too)
//!
//! Put both halves in the charging grip, unplugged, so they sit flat and share
//! one rigid orientation — that makes "flat and still" a real reference rather
//! than however the controller happened to be lying.

use std::time::{Duration, Instant};

use flexinput_btle::{acl, joycon as jc, Dongle, Event};

const DONGLE_VID: u16 = 0x0BDA;
const DONGLE_PID: u16 = 0xA728;
const NINTENDO_COMPANY_ID: u16 = 0x0553;

/// Byte range searched for motion fields.
///
/// Starts past the buttons/stick header and runs to the end of the 63-byte
/// report. Every offset in it is interpreted as a little-endian `i16`, including
/// overlapping ones — the real alignment is unknown, so assuming it would beg
/// the question.
const SEARCH_START: usize = 8;
const SEARCH_END: usize = 61;

/// One step of the guided sequence.
struct Phase {
    name: &'static str,
    instruction: &'static str,
    secs: u64,
}

const PHASES: &[Phase] = &[
    Phase {
        name: "still",
        instruction: "Lay the grip FLAT on the table and DO NOT TOUCH IT",
        secs: 6,
    },
    Phase {
        name: "roll",
        instruction: "ROLL only: tip it left and right, like turning a steering wheel that lies flat",
        secs: 8,
    },
    Phase {
        name: "pitch",
        instruction: "PITCH only: tilt the far edge up and down, like nodding",
        secs: 8,
    },
    Phase {
        name: "yaw",
        instruction: "YAW only: keep it flat on the table and spin it, like a compass needle",
        secs: 8,
    },
    Phase {
        name: "still2",
        instruction: "Lay it FLAT and still again (confirms the rest reading is repeatable)",
        secs: 6,
    },
];

/// Per-offset statistics for one phase.
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
        if self.seen {
            self.max - self.min
        } else {
            0
        }
    }
    fn mean(&self) -> i32 {
        if self.n == 0 {
            0
        } else {
            (self.sum / self.n as i64) as i32
        }
    }
}

fn main() {
    let want_mag = std::env::args().any(|a| a == "--mag");

    let dongle = match Dongle::open(DONGLE_VID, DONGLE_PID) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[imu] cannot open dongle: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = dongle.reset_and_init() {
        eprintln!("[imu] init failed: {e}");
        std::process::exit(1);
    }

    let Some((addr, addr_type, pid)) = find(&dongle) else {
        eprintln!("[imu] no Joy-Con found — wake one and retry");
        std::process::exit(1);
    };
    let side = if pid == 0x2066 { "RIGHT" } else { "LEFT" };
    println!("[imu] using the {side} half ({pid:04x})");

    let conn = match dongle.le_connect(addr, addr_type) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[imu] connect failed: {e}");
            std::process::exit(1);
        }
    };
    init(&dongle, conn, want_mag);

    // Baseline report length: if asking for the magnetometer makes the
    // controller send MORE data, that alone answers whether this half has one.
    println!("\n[imu] === guided sequence ===");
    if want_mag {
        println!("[imu] magnetometer bit REQUESTED (0xB7) — watch for fields that only");
        println!("[imu] come alive in this mode compared with a run without --mag\n");
    }

    let mut phases: Vec<(String, Vec<Stat>)> = Vec::new();
    for phase in PHASES {
        println!("\n>>> {} — {} ({} s)", phase.name.to_uppercase(), phase.instruction, phase.secs);
        for n in (1..=3).rev() {
            println!("    starting in {n}…");
            std::thread::sleep(Duration::from_secs(1));
        }
        println!("    GO");
        let stats = record(&dongle, conn, Duration::from_secs(phase.secs));
        println!("    done ({} samples)", stats.1);
        phases.push((phase.name.to_string(), stats.0));
    }

    let _ = dongle.disconnect(conn);
    report(&phases);
}

/// Collect per-offset statistics for one phase.
fn record(dongle: &Dongle, conn: u16, dur: Duration) -> (Vec<Stat>, u32) {
    let mut stats = vec![Stat::default(); SEARCH_END];
    let mut samples = 0u32;
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        // Drain events so a disconnect does not go unnoticed mid-phase.
        if let Ok(Some(Event::DisconnectionComplete { reason, .. })) =
            dongle.read_event_timeout(Duration::from_millis(1))
        {
            eprintln!("[imu] link dropped mid-phase (reason {reason:#04x})");
            break;
        }
        let Ok(Some(pkt)) = dongle.read_acl(Duration::from_millis(20)) else { continue };
        // Filter by connection handle, not just channel: the dongle can hold
        // both halves at once, and mixing two controllers' motion into one set
        // of statistics would produce a field map that describes neither.
        if pkt.cid != acl::CID_ATT || pkt.conn_handle != conn {
            continue;
        }
        let Some(n) = acl::parse_notification(&pkt.payload) else { continue };
        if n.handle != jc::HANDLE_INPUT_VALUE {
            continue;
        }
        samples += 1;
        for off in SEARCH_START..SEARCH_END.min(n.value.len().saturating_sub(1)) {
            let v = i16::from_le_bytes([n.value[off], n.value[off + 1]]);
            stats[off].push(v);
        }
    }
    (stats, samples)
}

/// Print the table that identifies each field.
fn report(phases: &[(String, Vec<Stat>)]) {
    println!("\n\n========== IMU FIELD MAP ==========");
    println!("Each row is one candidate i16 offset (little-endian, overlapping).");
    println!("Read it like this:");
    println!("  * big range in ONE rotation phase only  -> that axis's GYRO");
    println!("  * changes with orientation, quiet when still -> ACCELEROMETER");
    println!("  * large range even in 'still'            -> noise, or not a field at all");
    println!("  * mean ~0 when still, ~+/-4096 on one axis -> accel (1 g = 4096 LSB)\n");

    let names: Vec<&str> = phases.iter().map(|(n, _)| n.as_str()).collect();
    print!("{:>6} ", "off");
    for n in &names {
        print!("{n:>14} ");
    }
    println!();

    // Rank by how much a field moves during rotation relative to how much it
    // moves at rest — a field that is merely noisy scores near zero.
    let still_idx = 0usize;
    let mut rows: Vec<(usize, i64)> = Vec::new();
    for off in SEARCH_START..SEARCH_END {
        let still = phases[still_idx].1[off].range().max(1) as i64;
        let active: i64 = phases[1..4].iter().map(|(_, s)| s[off].range() as i64).sum();
        rows.push((off, active / still));
    }
    rows.sort_by(|a, b| b.1.cmp(&a.1));

    for (off, score) in rows.iter().take(24) {
        print!("{off:>6} ");
        for (_, s) in phases {
            print!("{:>6}/{:<7} ", s[*off].range(), s[*off].mean());
        }
        println!(" score={score}");
    }
    println!("\n(columns are range/mean; showing the 24 most rotation-responsive offsets)");
    println!("Offsets overlap by one byte, so a real 16-bit field usually appears as a");
    println!("strong row with weaker neighbours either side — take the strongest.");
}

fn find(dongle: &Dongle) -> Option<([u8; 6], u8, u16)> {
    println!("[imu] scanning — wake a Joy-Con");
    dongle.start_le_scan().ok()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut found = None;
    while Instant::now() < deadline {
        if let Ok(Some(Event::LeAdvertisingReport(r))) = dongle.read_event() {
            if let Some(md) = r.manufacturer_data() {
                if md.len() >= 9 && u16::from_le_bytes([md[0], md[1]]) == NINTENDO_COMPANY_ID {
                    found = Some((r.address, r.address_type, u16::from_le_bytes([md[7], md[8]])));
                    break;
                }
            }
        }
    }
    let _ = dongle.stop_le_scan();
    found
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
    let _ = dongle.send_att(
        conn,
        &acl::write_request(jc::HANDLE_CMD_RESPONSE_CCCD, &acl::CCCD_NOTIFY),
    );

    let send = |c: u8, s: u8, d: &[u8]| {
        let _ = dongle.send_att(conn, &acl::write_command(jc::HANDLE_CMD_WRITE, &cmd_frame(c, s, d)));
        std::thread::sleep(Duration::from_millis(30));
    };
    send(0x07, 0x01, &[]);
    send(0x10, 0x01, &[]);
    send(0x16, 0x01, &[]);
    for (size, addr) in [
        (0x40u8, 0x013000u32),
        (0x40, 0x013080),
        (0x40, 0x1FC040),
        (0x10, 0x013040),
        (0x18, 0x013100),
        (0x20, 0x013060),
    ] {
        let mut d = vec![size, 0x7E, 0x00, 0x00];
        d.extend_from_slice(&addr.to_le_bytes());
        send(0x02, 0x04, &d);
    }
    send(0x09, 0x07, &[0x01, 0, 0, 0, 0, 0, 0, 0]);

    // 0x37 is what official software sends; 0xB7 adds the magnetometer bit,
    // which it never sets. If a half has no magnetometer the extra bit is
    // expected to be ignored rather than rejected.
    let features: u8 = if want_mag { 0xB7 } else { 0x37 };
    send(0x0C, 0x02, &[features, 0, 0, 0]);
    send(0x0C, 0x04, &[features, 0, 0, 0]);

    let _ = dongle.send_att(
        conn,
        &acl::write_request(jc::HANDLE_INPUT_REPORT_RATE, &jc::REPORT_RATE_PAYLOAD),
    );
    std::thread::sleep(Duration::from_millis(200));
}
