//! Live integration check for `HidMaestroDevice` against a real device.
//!
//! `#[ignore]` by default — it needs an elevated, externally-created HIDMaestro
//! DS4 device at controller index 0 (e.g. via the `hm_shm_probe create` mode or
//! the elevated helper). Run after creating one:
//!
//! ```text
//! # (elevated) create a DS4-v2 device at index 0, keep it alive
//! hm_shm_probe create --index 0
//! # (this crate) drive it through the real VirtualDevice path
//! cargo test -p flexinput-virtual --test hidmaestro_live -- --ignored --nocapture
//! ```
//!
//! The test sweeps left-stick X via `VirtualDevice::send`/`flush` and prints the
//! frames; verify in a gamepad tester that the stick moves cleanly.

#![cfg(windows)]

use flexinput_core::Signal;
use flexinput_hidmaestro::profile::presets::DUALSHOCK_4_V2_JSON;
use flexinput_virtual::hidmaestro_device::HidMaestroDevice;
use flexinput_virtual::VirtualDevice;
use glam::Vec2;

#[test]
#[ignore = "requires an elevated, externally-created HIDMaestro DS4 device at index 0"]
fn drives_live_ds4_left_stick() {
    let mut dev = HidMaestroDevice::open("virtual.ds4", "Virtual DualShock 4", DUALSHOCK_4_V2_JSON, 0)
        .expect("valid DS4 profile");
    assert!(
        dev.is_connected(),
        "no live section at index 0 — create a device first (hm_shm_probe create --index 0)"
    );

    // Sweep left-stick X from center → right → left → center over ~3s.
    for step in 0..150 {
        let phase = (step as f32 / 150.0) * std::f32::consts::TAU;
        let x = phase.sin(); // -1..1
        dev.send("left_stick", Signal::Vec2(Vec2::new(x, 0.0)));
        dev.flush();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // Recenter on exit.
    dev.reset_outputs();
    println!("swept left-stick X through a full sine; check a gamepad tester");
}

/// Full app-path test: `create_device("virtual.hm.ds4", ...)` goes through the
/// helper manager — spawns the elevated helper (one UAC), creates the device +
/// sections, and returns a driveable `VirtualDevice`. Then drive + drop (which
/// destroys via the helper). This is the exact path the device pool uses.
///
/// `#[ignore]` — interactive (UAC) + machine-altering (creates a real device).
#[test]
#[ignore = "interactive: spawns elevated helper (UAC) and creates a real device"]
fn create_device_via_pool_path() {
    // The test binary runs from target/debug/deps/; point the helper manager at
    // the built helper exe (normally it sits next to the app exe).
    let helper = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("hidmaestro_helper.exe");
    assert!(
        helper.exists(),
        "build the helper first: cargo build -p flexinput-hidmaestro --features helper-bin --bin hidmaestro_helper"
    );
    flexinput_hidmaestro::helper::set_helper_exe(helper);

    let mut dev = flexinput_virtual::create_device("virtual.hm.ds4", 0);
    assert_eq!(dev.id(), "virtual.hm.ds4");
    assert!(
        dev.is_connected(),
        "helper-backed create should yield a connected device"
    );

    // Drive a short sweep through the trait API.
    for step in 0..100 {
        let x = ((step as f32 / 100.0) * std::f32::consts::TAU).sin();
        dev.send("left_stick", Signal::Vec2(Vec2::new(x, 0.0)));
        dev.flush();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    println!("drove virtual.hm.ds4 via create_device; dropping (helper destroys it)");
    drop(dev); // Drop → helper Destroy
    std::thread::sleep(std::time::Duration::from_millis(500));
}
