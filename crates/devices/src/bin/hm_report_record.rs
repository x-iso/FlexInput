//! Diagnostic: record live DualSense input reports (report 0x01) from every
//! Sony gamepad collection on the machine at once — physical pads, FlexInput's
//! virtuals (plain HIDMaestro and the composite USB/IP persona) and any other
//! remapper's virtual pad (PadForge) — and summarize each stream so the same
//! gesture can be compared byte-for-byte across all of them.
//!
//! Why: with feature-report answers conformed (see hm_feature_dump), a game
//! that consumes one pad's motion and refuses another's is keyed on report
//! CONTENT — which bytes move, how motion is scaled, how the sequence number
//! advances, what cadence reports arrive at — not on the descriptor or the
//! feature stubs. This probe is read-only; it never writes to a device.
//!
//!   cargo run -p flexinput-devices --bin hm_report_record -- [seconds]
//!
//! Default 6 s. Tilt ONE pad slowly and steadily while it runs; a good run
//! also keeps the other pads still, so "quiet" streams stay quiet.
//!
//! Buffer layout assumed: buffer[0] is the report number the HID transport
//! strips, and the profile's report byte N — the `extendedReport` byte index,
//! e.g. stick X at 1, sequence at 7, gyro pitch at 16 — sits at buffer[N+1].

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice};

const SONY_VID: u16 = 0x054C;

/// One captured frame: arrival time and the raw bytes as hidapi handed them.
struct Frame {
    t_ms: u128,
    buf: Vec<u8>,
}

fn main() {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(6);

    let api = match HidApi::new() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("hidapi init failed: {e}");
            std::process::exit(1);
        }
    };

    let mut targets = Vec::new();
    for info in api.device_list() {
        if info.vendor_id() != SONY_VID {
            continue;
        }
        if info.usage_page() != 0x0001 || info.usage() != 0x0005 {
            continue;
        }
        // Classify by the pairing MAC (feature 0x09): a real pad's burned-in
        // MAC, FlexInput's persona answers 02:46:58 ('F''X'), and HIDMaestro
        // -family personas (PadForge) answer 02:48:4D ('H''M'). The instance
        // path cannot tell a vhci persona from a physical pad — both carry
        // VID_054C&MI_03 — so the MAC is the only reliable tag.
        let who = match api.open_path(info.path()) {
            Ok(d) => {
                let mut b = vec![0u8; 20];
                b[0] = 0x09;
                match d.get_feature_report(&mut b) {
                    Ok(n) if n >= 7 => match &b[1..4] {
                        [0x02, 0x46, 0x58] => "FLEX-PERSONA",
                        [0x02, 0x48, 0x4D] => "OTHER-PERSONA",
                        _ => "PHYSICAL",
                    },
                    _ => "UNKNOWN",
                }
            }
            Err(_) => "UNOPENABLE",
        };
        targets.push((who.to_string(), info.path()));
    }
    if targets.is_empty() {
        eprintln!("no Sony gamepad collections visible to HID apps (HidHide? nothing plugged in?)");
        std::process::exit(2);
    }

    println!("recording {seconds}s from {} devices — tilt one pad slowly, keep the rest still:", targets.len());
    for (o, p) in &targets {
        println!("  {o:<16} {}", p.to_string_lossy());
    }

    let stop = Arc::new(AtomicBool::new(false));
    let mut streams = Vec::new();
    for (origin, path) in targets {
        let dev = match api.open_path(path) {
            Ok(d) => d,
            Err(e) => {
                println!("  open failed for {}: {e}", path.to_string_lossy());
                continue;
            }
        };
        let stop = Arc::clone(&stop);
        let (tx, rx) = std::sync::mpsc::channel::<Frame>();
        thread::spawn(move || record(dev, stop, tx));
        streams.push((origin, rx));
    }

    thread::sleep(Duration::from_secs(seconds));
    stop.store(true, Ordering::Relaxed);
    thread::sleep(Duration::from_millis(400));

    for (origin, rx) in streams {
        let frames: Vec<Frame> = rx.try_iter().collect();
        summarize(&origin, &frames);
    }
}

/// The per-device reader: one read per report, 150 ms timeout so the thread
/// notices the stop. DualSense reports arrive as 65-byte buffers (0x01).
fn record(dev: HidDevice, stop: Arc<AtomicBool>, tx: std::sync::mpsc::Sender<Frame>) {
    let dev = dev;
    let start = Instant::now();
    let mut buf = vec![0u8; 128];
    while !stop.load(Ordering::Relaxed) {
        match dev.read(&mut buf) {
            Ok(n) if n > 0 => {
                let _ = tx.send(Frame { t_ms: start.elapsed().as_millis(), buf: buf[..n].to_vec() });
            }
            Ok(_) => continue,
            Err(e) => {
                eprintln!("read error: {e}");
                return;
            }
        }
    }
}

fn summarize(origin: &str, frames: &[Frame]) {
    println!("\n=== {origin}: {} frames ===", frames.len());
    if frames.is_empty() {
        println!("  silent (no reports while the device was open)");
        return;
    }
    let len = frames[0].buf.len();
    // The pad's report 0x01 may arrive with (65) or without (64) a leading
    // report-ID byte; the profile's byte N sits at buf[N + shift].
    let shift = if len == 65 { 1 } else { 0 };
    println!(
        "  rate {} Hz, len {len}, report ids: {:?}",
        frames.len() * 1000 / frames.last().unwrap().t_ms.max(1) as usize,
        frames.iter().map(|f| f.buf[0]).collect::<BTreeSet<u8>>()
    );

    // Bytes that move across the window.
    let mut changed: BTreeSet<usize> = BTreeSet::new();
    for i in 0..len {
        if frames.iter().any(|f| f.buf[i] != frames[0].buf[i]) {
            changed.insert(i);
        }
    }
    println!("  bytes that moved: {}", fmt_ranges(&changed));

    // Per-byte table: what a real pad streams, field by field. A constant byte
    // prints its value (a virtual pad should match it); a moving byte prints its
    // span and, where the little-endian run is monotonic, the per-frame step —
    // which is how the sequence counter, the report timestamp and the touch-packet
    // counter show up.
    println!("  byte table (idx: value | span/steps):");
    for i in (0..len).step_by(4) {
        let mut row = format!("   {:>3}: ", i);
        for j in i..(i + 4).min(len) {
            let vals: Vec<u8> = frames.iter().map(|f| f.buf[j]).collect();
            let first = vals[0];
            if vals.iter().all(|&v| v == first) {
                row.push_str(&format!("{first:02X}          "));
            } else {
                let lo = vals.iter().copied().min().unwrap();
                let hi = vals.iter().copied().max().unwrap();
                row.push_str(&format!("{lo:02X}..{hi:02X}({:3}) ", vals.len()));
            }
        }
        println!("{row}");
    }

    // Structure of the vendor block: a few raw frames, so a byte's character
    // (static, slow, per-report, per-frame noise) reads off the page.
    println!("  frames (first 3, last 1):");
    let mut shown = 0;
    for i in [0, 1, 2, frames.len().saturating_sub(1)] {
        if i >= frames.len() {
            continue;
        }
        let f = &frames[i];
        let hex: String = f.buf.iter().map(|b| format!("{b:02x} ")).collect();
        println!("   [{shown}] {hex}");
        shown += 1;
    }

    // Per-byte character: mean |delta| between consecutive frames and whether the
    // byte ever repeats its previous value. A real pad's vendor block mixes
    // per-report counters, 3 MHz clocks, slow drift and per-frame noise, and the
    // virtual pad has to reproduce that character, not just the motion fields.
    println!("  per-byte character (idx: static | mean|d| | repeats%):");
    for i in 0..len {
        let mut sum = 0i64;
        let mut rep = 0usize;
        for w in frames.windows(2) {
            let d = (w[1].buf[i] as i16 - w[0].buf[i] as i16).abs();
            sum += d as i64;
            if w[1].buf[i] == w[0].buf[i] {
                rep += 1;
            }
        }
        let n = frames.len() - 1;
        let kind = if rep == n {
            "static".to_string()
        } else if rep > n * 9 / 10 {
            "slow".to_string()
        } else if rep < n / 10 {
            "noise".to_string()
        } else {
            "mixed".to_string()
        };
        println!("   {i:>3}: {kind:<6} mean|d|={} repeats={}/{}", sum / n as i64, rep, n);
    }
    // Monotonic LE runs (timestamp / counters): report the mean per-frame step.
    for width in [4usize, 2] {
        for i in 0..=len.saturating_sub(width) {
            if !changed.contains(&i) {
                continue;
            }
            let get = |f: &Frame| -> u64 {
                let mut b = [0u8; 8];
                b[..width].copy_from_slice(&f.buf[i..i + width]);
                u64::from_le_bytes(b)
            };
            let first = get(&frames[0]);
            let last = get(&frames[frames.len() - 1]);
            let span = last.wrapping_sub(first);
            let frames_n = frames.len() - 1;
            if frames_n > 8 && span > 0 && span as u128 * 10 / frames_n as u128 > 0 {
                let step = span as f64 / frames_n as f64;
                // A counter is monotonic if every step is non-negative.
                let mono = frames.windows(2).all(|w| get(&w[1]).wrapping_sub(get(&w[0])) < (1u64 << (8 * width - 1)));
                if mono {
                    println!("  LE{width}@{i}: monotonic {first:#x} -> {last:#x}, mean step {step:.1}");
                }
            }
        }
    }

    // Sequence number (report byte 7): increments and coverage.
    let sh = shift;
    if len > 8 + sh {
        let seqs: Vec<u8> = frames.iter().map(|f| f.buf[7 + sh]).collect();
        let incs: Vec<usize> = seqs.windows(2).map(|w| w[1].wrapping_sub(w[0]) as usize).collect();
        let uniq: BTreeSet<u8> = seqs.iter().copied().collect();
        println!(
            "  seq: first {} last {} unique {} increments {:?}",
            seqs[0],
            seqs[seqs.len() - 1],
            uniq.len(),
            incs.iter().copied().collect::<BTreeSet<_>>()
        );
    }

    // Motion fields: report bytes 16..28 (gyro pitch/yaw/roll, accel x/y/z).
    if len >= 28 + sh {
        let g = frames.iter().map(|f| i16le(&f.buf, 16 + sh)).collect::<Vec<i16>>();
        let a = frames.iter().map(|f| i16le(&f.buf, 22 + sh)).collect::<Vec<i16>>();
        let m = |v: &[i16]| (v.iter().copied().min().unwrap(), v.iter().copied().max().unwrap());
        println!(
            "  gyro@16..  first {:>6} {:>6} {:>6}   range [{}, {}]",
            g[0],
            i16le(&frames[0].buf, 18 + sh),
            i16le(&frames[0].buf, 20 + sh),
            m(&g).0,
            m(&g).1
        );
        println!(
            "  accel@22.. first {:>6} {:>6} {:>6}   range [{}, {}]",
            a[0],
            i16le(&frames[0].buf, 24 + sh),
            i16le(&frames[0].buf, 26 + sh),
            m(&a).0,
            m(&a).1
        );
    }
}

fn i16le(b: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([b[at], b[at + 1]])
}

/// Compress 0..len into "3-8, 17, 20-27" style ranges.
fn fmt_ranges(set: &BTreeSet<usize>) -> String {
    let mut out = String::new();
    let mut iter = set.iter().peekable();
    while let Some(&start) = iter.next() {
        let mut end = start;
        while let Some(&&n) = iter.peek() {
            if n == end + 1 {
                end = n;
                iter.next();
            } else {
                break;
            }
        }
        if !out.is_empty() {
            out.push_str(", ");
        }
        if end == start {
            out.push_str(&start.to_string());
        } else {
            out.push_str(&format!("{start}-{end}"));
        }
    }
    if out.is_empty() {
        out.push_str("(none)");
    }
    out
}
