//! Run the REAL Bluetooth Classic backend, outside the app.
//!
//! ⭐ **Because a parallel probe proved the wrong thing.** `bt_classic --watch`
//! reproduced the protocol and worked perfectly — connect, key, authenticate,
//! encrypt, channels, 220 reports a second — while the application, driving the
//! same radio through the same crate, connected and then went silent. Testing a
//! copy of the logic only ever proves the copy.
//!
//! This constructs `ClassicBtBackend` itself and polls it exactly as the app's
//! I/O thread does, so anything that goes wrong here is wrong in the shipping
//! code path: the shared radio, the router's fan-out, the subscription, the
//! lease handling. Nothing is reimplemented.
//!
//! Usage:
//!   cargo run -p flexinput-devices --bin classic_watch
//!   cargo run -p flexinput-devices --bin classic_watch -- --keys D:\\some\\folder

use std::time::{Duration, Instant};

use flexinput_devices::classic_bt::{pair_control, ClassicBtBackend};
use flexinput_devices::DeviceBackend;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(dir) = args
        .iter()
        .position(|a| a == "--keys")
        .and_then(|i| args.get(i + 1))
    {
        flexinput_btle::keystore::set_dir(Some(std::path::PathBuf::from(dir)));
    }
    println!("key store: {}", flexinput_btle::keystore::path().display());
    for (addr, p) in flexinput_btle::keystore::load() {
        println!("  paired: {addr}  {}", p.name.unwrap_or_default());
    }

    // ⭐ Drives the SHIPPING pairing path, not a copy of it. `--pair` makes
    // the same request the panel's button makes, so a pairing that works here
    // is a pairing that works in the app.
    let want_pair = args.iter().any(|a| a == "--pair");

    let t0 = Instant::now();
    let mut backend = ClassicBtBackend::new();
    let ctl = pair_control();
    if want_pair {
        ctl.request();
    }
    if want_pair {
        println!("PAIRING REQUESTED - hold the controller's Sync button now
");
    } else {
        println!("backend started - switch the controller ON
");
    }

    let mut last_status = String::new();
    let mut last_phase = String::new();
    let mut devices = 0usize;
    let mut total_reports = 0u32;
    loop {
        // Exactly what the I/O thread does, at roughly its rate.
        let signals = backend.poll();
        for (dev, n) in backend.take_event_counts() {
            if n > 0 {
                total_reports += n;
                let _ = dev;
            }
        }
        let list = backend.enumerate();
        if list.len() != devices {
            devices = list.len();
            println!(
                "[{:>6.1}s] devices: {} {:?}",
                t0.elapsed().as_secs_f32(),
                devices,
                list.iter().map(|d| d.display_name.clone()).collect::<Vec<_>>()
            );
        }
        let phase = format!("{:?}", ctl.phase());
        if phase != last_phase {
            println!("[{:>6.1}s] pair: {phase}", t0.elapsed().as_secs_f32());
            last_phase = phase;
        }
        let status = format!("{:?}", ctl.status());
        if status != last_status {
            println!("[{:>6.1}s] status: {status}", t0.elapsed().as_secs_f32());
            last_status = status;
        }
        if total_reports > 0 && total_reports % 500 < 8 && !signals.is_empty() {
            println!(
                "[{:>6.1}s] ~{total_reports} reports, {} signals/poll",
                t0.elapsed().as_secs_f32(),
                signals.len()
            );
            total_reports += 8; // keep the modulo from firing repeatedly
        }
        std::thread::sleep(Duration::from_millis(8));
    }
}
