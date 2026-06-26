//! Throwaway probe: validate WASAPI process-loopback haptics capture.
//!
//! Opens a loopback capture on a target process and prints the live per-side
//! amplitude + normalized frequency derived from its audio. Use it to confirm
//! the capture + DSP produce sane haptic targets from a real game / media player
//! BEFORE wiring the amp/freq to the Switch Pro HD-rumble encoder.
//!
//!   cargo run -p flexinput-devices --bin loopback_probe -- <pid>
//!
//! Get <pid> from Task Manager (Details tab) for the game / media player whose
//! audio you want to feel. (Foreground / by-name targeting comes with the real
//! UI integration; this probe stays dependency-free.)

#[cfg(windows)]
fn main() {
    use std::time::{Duration, Instant};
    use flexinput_devices::loopback_haptic::LoopbackHaptic;

    let pid: u32 = match std::env::args().nth(1).and_then(|a| a.parse().ok()) {
        Some(p) => p,
        None => {
            eprintln!("usage: loopback_probe <pid>   (PID from Task Manager > Details)");
            std::process::exit(1);
        }
    };

    println!("[probe] opening loopback capture for pid {pid} (include child tree)…");
    let cap = match LoopbackHaptic::open(pid, true) {
        Some(c) => c,
        None => {
            eprintln!("[probe] failed to start capture thread");
            std::process::exit(1);
        }
    };

    println!("[probe] streaming. Play audio in the target app. Ctrl+C to stop.");
    let mut last = Instant::now();
    loop {
        if last.elapsed() >= Duration::from_millis(200) {
            let p = cap.params();
            let to_hz = |n: f32| 80.0 + n * (500.0 - 80.0);
            let bar = |v: f32| {
                let n = (v.clamp(0.0, 1.0) * 20.0).round() as usize;
                format!("{}{}", "#".repeat(n), "-".repeat(20 - n))
            };
            println!(
                "L amp {:.3} [{}] @ {:>3.0}Hz   R amp {:.3} [{}] @ {:>3.0}Hz",
                p.l_amp, bar(p.l_amp), to_hz(p.l_freq),
                p.r_amp, bar(p.r_amp), to_hz(p.r_freq),
            );
            last = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("loopback_probe is Windows-only.");
}
