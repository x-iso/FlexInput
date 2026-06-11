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
use flexinput_hidmaestro::Profile;
use flexinput_virtual::hidmaestro_device::HidMaestroDevice;
use flexinput_virtual::VirtualDevice;
use glam::Vec2;

#[test]
#[ignore = "requires an elevated, externally-created HIDMaestro DS4 device at index 0"]
fn drives_live_ds4_left_stick() {
    let profile = Profile::from_json(DUALSHOCK_4_V2_JSON).unwrap();
    let mut dev = HidMaestroDevice::open("virtual.ds4", "Virtual DualShock 4", profile, 0);
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
