//! Live check against an installed usbip-win2: serve a composite DualSense (or
//! Edge), attach it, keep it up for a while reporting what the host does with
//! it, then detach.
//!
//!   cargo run -p flexinput-usbip --example attach_probe -- [seconds] [edge]
//!
//! While it runs, Windows should show "DualSense Wireless Controller" under
//! Sound (a 4-channel output and a microphone) and as a game controller. Play
//! audio to that output, or a game's haptics, and the PCM counters move.

#[cfg(windows)]
fn main() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use flexinput_usbip::{profiles, vhci, EmulatedDevice, HOST};

    let args: Vec<String> = std::env::args().collect();
    let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(30);
    let edge = args.iter().any(|a| a == "edge");
    let json = if edge { profiles::DUALSENSE_EDGE_COMPOSITE_JSON } else { profiles::DUALSENSE_COMPOSITE_JSON };

    if !vhci::is_available() {
        eprintln!("usbip-win2's host controller isn't present.");
        std::process::exit(2);
    }
    println!("usbip-win2 request layout: {:?}", vhci::installed_layout());

    // Before serving anything (see vhci::sweep_stale).
    vhci::sweep_stale(None);
    let dev = EmulatedDevice::new(json, if edge { "probe.edge" } else { "probe.dualsense" }, 0)
        .expect("composite profile");
    println!("serving {} on {HOST}:{} as {}, serial {:?}", dev.descriptors().profile_id, dev.port(), dev.busid(), dev.descriptors().serial());

    let bytes = Arc::new(AtomicU64::new(0));
    let peak = Arc::new(AtomicU64::new(0));
    {
        let (bytes, peak) = (Arc::clone(&bytes), Arc::clone(&peak));
        dev.set_pcm_sink(Some(Box::new(move |pcm: &[u8]| {
            bytes.fetch_add(pcm.len() as u64, Ordering::Relaxed);
            // Loudest sample on the haptic channels (3 and 4).
            for f in pcm.chunks_exact(8) {
                for ch in [2usize, 3] {
                    let v = i16::from_le_bytes([f[ch * 2], f[ch * 2 + 1]]).unsigned_abs() as u64;
                    peak.fetch_max(v, Ordering::Relaxed);
                }
            }
        })));
    }

    let started = Instant::now();
    let vport = match vhci::attach(HOST, dev.port(), dev.busid()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("attach failed: {e}");
            std::process::exit(1);
        }
    };
    println!("attached on controller port {vport} in {:?}", started.elapsed());

    // A neutral report: sticks centred, hat released, touch fingers up,
    // battery full (the plain encoder's neutral state, by hand).
    let mut report = [0u8; 64];
    report[0] = 0x01;
    report[1..5].fill(0x80);
    report[8] = 0x08;
    report[33] = 0x80;
    report[37] = 0x80;
    report[53] = 0x0A;
    if edge {
        report[49] = 0x80;
    }

    let mut outputs = 0usize;
    let mut next_line = Instant::now() + Duration::from_secs(1);
    while started.elapsed() < Duration::from_secs(secs) {
        dev.submit_input(&report);
        outputs += dev.take_outputs().len();
        if Instant::now() >= next_line {
            next_line += Duration::from_secs(1);
            println!(
                "t={:>3}s connected={} speaker/haptics open={} pcm={} B haptic peak={} output reports={}",
                started.elapsed().as_secs(),
                dev.is_connected(),
                dev.output_stream_open(),
                bytes.load(Ordering::Relaxed),
                peak.swap(0, Ordering::Relaxed),
                outputs,
            );
        }
        std::thread::sleep(Duration::from_millis(4));
    }

    match vhci::detach(vport) {
        Ok(()) => println!("detached"),
        Err(e) => eprintln!("detach failed: {e}"),
    }
    std::thread::sleep(Duration::from_millis(300));
    println!("connected after detach: {}", dev.is_connected());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("usbip-win2 is Windows-only");
}
