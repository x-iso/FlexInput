//! Guided IMU field-mapping probe — both halves, one sweep per axis.
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
//! One continuous sweep per axis, and the two sensors are separated
//! afterwards by analysis rather than by asking the operator to hold still at
//! precise moments:
//!
//! * **Accelerometer** is found by physics. Gravity has constant magnitude, so
//!   the accel triple is the one set of three fields whose vector length stays
//!   at 1 g (**4096 LSB**) through every orientation. Every candidate triple is
//!   tested and scored on how constant its magnitude is; nothing else in the
//!   report can fake that.
//! * **Gyro** is then whatever responds strongly to exactly ONE sweep and is
//!   quiet in the others — angular rate about one axis, by definition.
//!
//! Because a sweep goes one way and then the other, each gyro field's minimum
//! and maximum are the two rotation directions, which recovers its **sign**.
//!
//! Both halves are recorded during the same physical motion. Clipped into the
//! grip they are rigidly coupled and cannot rotate at different rates, so a
//! genuine gyro axis must respond on both — and their neutral accelerometer
//! readings differ only by the fixed mounting rotation, which is exactly the
//! pivot offset a shared frame has to correct for.
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

/// Byte range searched, as overlapping little-endian `i16`s. The real alignment
/// is unknown; assuming it would beg the question.
const SEARCH_START: usize = 8;
const SEARCH_END: usize = 61;

/// 1 g in accelerometer LSB, established from an earlier hardware capture.
const ONE_G: f64 = 4096.0;

struct Phase {
    name: &'static str,
    instruction: &'static str,
    secs: u64,
}

/// Index 0 is the reference; 1..=3 are the axis sweeps, in roll/pitch/yaw order.
const PHASES: &[Phase] = &[
    Phase {
        name: "neutral",
        instruction: "Lay the grip FLAT on the table, both halves attached. DO NOT TOUCH IT.",
        secs: 5,
    },
    Phase {
        name: "roll",
        instruction: "ROLL: from flat, rotate 90 deg COUNTER-CLOCKWISE, then 180 deg the other way \
                      to 90 deg CLOCKWISE, then back to flat. Slow and steady.",
        secs: 12,
    },
    Phase {
        name: "pitch",
        instruction: "PITCH: from flat, rotate DOWN 90 deg (shoulder buttons pointing at the GROUND), \
                      then 180 deg UP (shoulder buttons pointing at the CEILING), then back to flat.",
        secs: 12,
    },
    Phase {
        name: "yaw",
        instruction: "YAW: keep it FLAT on the table. Spin 90 deg COUNTER-CLOCKWISE, then 180 deg \
                      CLOCKWISE, then back to neutral.",
        secs: 12,
    },
];

const AXES: [&str; 3] = ["roll", "pitch", "yaw"];

struct Link {
    conn: u16,
    side: &'static str,
}

/// Raw reports kept per phase per link.
///
/// Whole frames rather than running statistics: the accelerometer test needs
/// three fields *from the same sample* to compute a vector magnitude, which
/// per-offset summaries throw away. ~3000 frames per half is a few hundred KB.
type Frames = Vec<Vec<u8>>;

fn main() {
    let want_mag = std::env::args().any(|a| a == "--mag");
    // Echo the RESOLVED setting, not the raw argument. Three separate rounds of
    // this investigation were wasted on flags that silently never reached the
    // process, each time producing confident-looking but meaningless results.
    // Printing what the code actually decided makes that impossible to miss.
    println!(
        "[imu] feature mask = {:#04x}{}",
        if want_mag { 0xB7 } else { 0x37 },
        if want_mag { "  (magnetometer bit REQUESTED)" } else { "  (stock; pass --mag to add magnetometer)" },
    );

    let dongle = match Dongle::open(DONGLE_VID, DONGLE_PID) {
        Ok(d) => d,
        Err(e) => { eprintln!("[imu] cannot open dongle: {e}"); std::process::exit(1); }
    };
    if let Err(e) = dongle.reset_and_init() {
        eprintln!("[imu] init failed: {e}");
        std::process::exit(1);
    }

    let links = connect_both(&dongle, want_mag);
    if links.is_empty() {
        eprintln!("[imu] no controllers connected");
        std::process::exit(1);
    }
    if links.len() < 2 {
        println!("\n[imu] NOTE: only one half connected — no cross-check between halves.\n");
    }

    println!("\n[imu] === {} phases, about 45 s total ===", PHASES.len());
    println!("[imu] Keep BOTH halves clipped into the grip throughout.\n");

    // phase -> conn -> frames
    let mut rec: Vec<HashMap<u16, Frames>> = Vec::new();
    for phase in PHASES {
        println!("\n>>> [{}] {}", phase.name, phase.instruction);
        for n in (1..=3).rev() {
            println!("    {n}…");
            std::thread::sleep(Duration::from_secs(1));
        }
        println!("    GO — {} s", phase.secs);
        let frames = record(&dongle, &links, Duration::from_secs(phase.secs));
        for l in &links {
            let f = frames.get(&l.conn);
            // Report LENGTH is printed too: if the magnetometer bit is honoured,
            // the controller has to put those readings somewhere, so a longer
            // report is the most direct evidence that the bit did anything at
            // all. Identical lengths with and without --mag means it did not.
            let len = f.and_then(|v| v.first()).map(|r| r.len()).unwrap_or(0);
            println!("    {}: {} frames, report len {len}", l.side, f.map(|v| v.len()).unwrap_or(0));
        }
        rec.push(frames);
    }

    for link in &links {
        let _ = dongle.disconnect(link.conn);
    }
    for link in &links {
        analyse(link, &rec);
    }
    cross_check(&links, &rec);
}

/// Read an `i16` field from a frame, or `None` if it does not fit.
fn field(frame: &[u8], off: usize) -> Option<i16> {
    if off + 1 < frame.len() {
        Some(i16::from_le_bytes([frame[off], frame[off + 1]]))
    } else {
        None
    }
}

/// Range of an offset across a set of frames.
fn range_of(frames: &Frames, off: usize) -> i32 {
    let mut lo = i32::MAX;
    let mut hi = i32::MIN;
    for f in frames {
        if let Some(v) = field(f, off) {
            lo = lo.min(v as i32);
            hi = hi.max(v as i32);
        }
    }
    if lo > hi { 0 } else { hi - lo }
}

/// Mean of an i16 field across frames.
fn mean_of(frames: &Frames, off: usize) -> i32 {
    let (mut sum, mut n) = (0i64, 0i64);
    for f in frames {
        if let Some(v) = field(f, off) {
            sum += v as i64;
            n += 1;
        }
    }
    if n == 0 { 0 } else { (sum / n) as i32 }
}

/// Score a consecutive 3-field i32 block as an accelerometer: how constant is
/// its vector magnitude, relative to 1 g?
///
/// This is the identification that cannot be faked. Gravity's magnitude is
/// fixed, so only the real accel triple holds a constant length through every
/// orientation; nothing else in the report has a physically pinned magnitude.
fn accel_error(all: &[Vec<u8>], base: usize, stride: usize) -> Option<f64> {
    let (mut err, mut n) = (0.0f64, 0.0f64);
    for f in all {
        let (Some(x), Some(y), Some(z)) = (
            field(f, base),
            field(f, base + stride),
            field(f, base + 2 * stride),
        ) else {
            continue;
        };
        let m = ((x as f64).powi(2) + (y as f64).powi(2) + (z as f64).powi(2)).sqrt();
        err += ((m - ONE_G) / ONE_G).powi(2);
        n += 1.0;
    }
    if n < 50.0 {
        return None;
    }
    Some((err / n).sqrt())
}

fn analyse(link: &Link, rec: &[HashMap<u16, Frames>]) {
    let empty: Frames = Vec::new();
    let ph = |i: usize| rec[i].get(&link.conn).unwrap_or(&empty);

    println!("

========== {} HALF ==========", link.side);

    let mut all: Vec<Vec<u8>> = Vec::new();
    for i in 0..PHASES.len() {
        all.extend(ph(i).iter().cloned());
    }

    // A raw frame, so the block structure can be read directly instead of
    // inferred. Cheap, and it settles layout arguments that statistics cannot.
    if let Some(f) = ph(0).last() {
        println!("
-- one NEUTRAL frame (flat on the table) --");
        for row in 0..(f.len() + 15) / 16 {
            let lo = row * 16;
            let hi = (lo + 16).min(f.len());
            let hex: Vec<String> = f[lo..hi].iter().map(|b| format!("{b:02x}")).collect();
            println!("  {lo:>3}: {}", hex.join(" "));
        }
    }

    // Accelerometer: the 3 i16 fields whose vector magnitude stays at 1 g.
    // Strides are searched rather than assumed — the block could be packed
    // (stride 2) or interleaved with another sensor (stride 4 or 6), and the
    // earlier i32 reading was simply wrong: it produced values in the hundreds
    // of millions, which is what reading across packed 16-bit fields looks like.
    let mut accel: Vec<(usize, usize, f64)> = Vec::new();
    for stride in [2usize, 4, 6] {
        for b in SEARCH_START..SEARCH_END.saturating_sub(2 * stride + 1) {
            if let Some(e) = accel_error(&all, b, stride) {
                accel.push((b, stride, e));
            }
        }
    }
    accel.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap());

    println!("
-- ACCELEROMETER (3 x i16; magnitude must hold at 4096 = 1 g) --");
    for (b, st, e) in accel.iter().take(5) {
        let verdict = if *e < 0.15 { "  <== ACCEL BLOCK" } else { "" };
        println!("  offsets [{} {} {}] stride {st}  magnitude error {:>6.1}%{}",
            b, b + st, b + 2 * st, e * 100.0, verdict);
    }
    match accel.first().filter(|(_, _, e)| *e < 0.15) {
        Some((b, st, _)) => {
            println!("  neutral (flat) means: {}={} {}={} {}={}",
                b, mean_of(ph(0), *b), b + st, mean_of(ph(0), *b + st),
                b + 2 * st, mean_of(ph(0), *b + 2 * st));
            println!("  flat on a table, one axis should read about +/-4096 and the others ~0");
        }
        None => println!("  no triple held a constant magnitude at any stride"),
    }

    // Gyro: 3 x i32 on the same stride, each field responding to ONE sweep.
    println!("
-- GYRO (i16; each field owns one rotation axis, saturating reads excluded) --");
    println!("{:>5} {:>10} {:>10} {:>10}  {:>12} {:>12}",
        "off", "roll", "pitch", "yaw", "min", "max");
    let accel_span = accel.first().filter(|(_, _, e)| *e < 0.15).map(|(b, st, _)| (*b, *st));
    let mut rows: Vec<(usize, [i32; 3], f64)> = Vec::new();
    for off in SEARCH_START..SEARCH_END.saturating_sub(1) {
        // Skip the accel fields themselves — gravity makes them respond to
        // rotation too, and they outranked real gyro axes until excluded.
        if let Some((b, st)) = accel_span {
            if [b, b + st, b + 2 * st].contains(&off) {
                continue;
            }
        }
        let rest = range_of(ph(0), off);
        let r = [range_of(ph(1), off), range_of(ph(2), off), range_of(ph(3), off)];
        let best = r.iter().copied().max().unwrap_or(0);
        // Anything spanning nearly the whole i16 range is a misaligned read,
        // not a sensor: a real axis does not saturate both rails.
        if best < 1000 || best > 60000 || best < rest * 4 {
            continue;
        }
        let others = r.iter().copied().sum::<i32>() - best;
        rows.push((off, r, best as f64 / others.max(1) as f64));
    }
    rows.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap());
    for (off, r, sel) in rows.iter().take(8) {
        let axis = (0..3).max_by_key(|i| r[*i]).unwrap();
        let (mut lo, mut hi) = (i32::MAX, i32::MIN);
        for f in ph(axis + 1) {
            if let Some(v) = field(f, *off) {
                lo = lo.min(v as i32);
                hi = hi.max(v as i32);
            }
        }
        let tag = if *sel > 2.0 { format!("  <== GYRO {}", AXES[axis]) } else { String::new() };
        println!("{off:>5} {:>10} {:>10} {:>10}  {lo:>12} {hi:>12}  sel={sel:.1}{tag}",
            r[0], r[1], r[2]);
    }
    println!("
  A gyro axis has min and max of OPPOSITE sign (the sweep went both ways).");
    println!("  Real fields sit 4 bytes apart; anything in between is a misaligned read.");
}

/// Compare the halves. They are bolted together and cannot rotate at different
/// rates, so any axis identified on only one of them is suspect; and their
/// neutral accelerometer readings differ purely by the mounting rotation.
fn cross_check(links: &[Link], rec: &[HashMap<u16, Frames>]) {
    if links.len() < 2 {
        return;
    }
    println!("\n\n========== CROSS-CHECK ==========");
    println!("Both halves saw the SAME motion, so gyro magnitudes should match per axis,");
    println!("and any difference in the neutral accel vector IS the fixed mounting offset.");
    let empty: Frames = Vec::new();
    for (ai, axis) in AXES.iter().enumerate() {
        println!("\n-- {axis} --");
        for link in links {
            let ph = |i: usize| rec[i].get(&link.conn).unwrap_or(&empty);
            let best = (SEARCH_START..SEARCH_END)
                .map(|off| (off, range_of(ph(ai + 1), off), range_of(ph(0), off)))
                .filter(|(_, act, rest)| *act > 400 && *act > rest * 4)
                .max_by_key(|(_, act, _)| *act);
            match best {
                Some((off, act, _)) => println!("  {:>5}: strongest offset {off:>3}, range {act}", link.side),
                None => println!("  {:>5}: nothing responded", link.side),
            }
        }
    }
}

fn connect_both(dongle: &Dongle, want_mag: bool) -> Vec<Link> {
    let mut links: Vec<Link> = Vec::new();
    let mut addrs: Vec<[u8; 6]> = Vec::new();
    println!("[imu] waiting for BOTH halves — wake them now (any button)");

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

fn record(dongle: &Dongle, links: &[Link], dur: Duration) -> HashMap<u16, Frames> {
    let mut out: HashMap<u16, Frames> = links.iter().map(|l| (l.conn, Vec::new())).collect();
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        if let Ok(Some(Event::DisconnectionComplete { reason, .. })) =
            dongle.read_event_timeout(Duration::from_millis(1))
        {
            eprintln!("[imu] a link dropped mid-phase (reason {reason:#04x})");
        }
        // Greedy drain: one packet per iteration starved whichever half was
        // serviced second, badly enough to make its statistics meaningless.
        for pkt in dongle.drain_acl(256) {
            if pkt.cid != acl::CID_ATT {
                continue;
            }
            let Some(n) = acl::parse_notification(&pkt.payload) else { continue };
            if n.handle != jc::HANDLE_INPUT_VALUE {
                continue;
            }
            // Demultiplex by connection handle — both halves share one pipe.
            if let Some(v) = out.get_mut(&pkt.conn_handle) {
                v.push(n.value);
            }
        }
    }
    out
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

    // Ask the controller what it ACTUALLY enabled.
    //
    // `0x0C/0x01` retrieves the feature configuration, so this is the
    // controller's own answer rather than our assumption. Until now the probe
    // subscribed to the command-response channel and then ignored it entirely,
    // which meant a rejected feature mask looked exactly like an accepted one.
    // That matters most for the magnetometer bit: the input report is a FIXED
    // 63 bytes, so magnetometer data would land in existing padding rather
    // than lengthening the report — an unchanged length proves nothing either
    // way, and the readback is the only direct evidence available.
    let _ = dongle.send_att(conn, &acl::write_command(jc::HANDLE_CMD_WRITE, &cmd_frame(0x0C, 0x01, &[])));
    drain_responses(dongle, conn, Duration::from_millis(600));
}

/// Print whatever the controller sends back on the command-response channel.
///
/// Responses on this characteristic carry their 8-byte header at offset 0x0F
/// (the leading bytes are the rumble region echoed back), so the header is
/// decoded from there: `[cmd][dir 0x01][transport][subcmd][..][len]`.
fn drain_responses(dongle: &Dongle, conn: u16, dur: Duration) {
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        let Ok(Some(pkt)) = dongle.read_acl(Duration::from_millis(20)) else { continue };
        if pkt.cid != acl::CID_ATT || pkt.conn_handle != conn {
            continue;
        }
        let Some(n) = acl::parse_notification(&pkt.payload) else { continue };
        if n.handle != jc::HANDLE_CMD_RESPONSE {
            continue;
        }
        const HDR: usize = 0x0F;
        if n.value.len() >= HDR + 8 {
            let h = &n.value[HDR..HDR + 8];
            let data = &n.value[HDR + 8..(HDR + 8 + h[5] as usize).min(n.value.len())];
            println!(
                "    <- reply cmd={:#04x} sub={:#04x} len={} data={:02x?}",
                h[0], h[3], h[5], data
            );
        } else {
            println!("    <- reply (short) {:02x?}", n.value);
        }
    }
}
