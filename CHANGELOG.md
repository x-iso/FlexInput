# Changelog

All notable changes to FlexInput are documented here. This project adheres to
[Semantic Versioning](https://semver.org/).

## [0.10.0] - 2026-06-26

This release replaces the ViGEm backend with a pure-Rust HIDMaestro virtual-device
stack (Xbox 360 / XInput, DualShock 4, DualSense), adds a driver-free Audio Stream
Haptics module, and ships a large batch of rumble, device-fidelity, and UI fixes.

### Added

#### HIDMaestro — pure-Rust virtual devices (ViGEm removal)
- Pure-Rust HIDMaestro shared-memory client and HID descriptor parser + report
  encoder (DS4 path), with plain-HID device create/teardown entirely in Rust.
- `VirtualDevice` adapter wiring HIDMaestro into the existing virtual-device API.
- Driver availability probe, installed-INF discovery, and an elevated helper that
  deploys the driver (certificate + `pnputil`), bundled into the app via self
  re-exec rather than a separate binary.
- App integration: HIDMaestro outputs in Advanced mode, device persistence
  setting, and a per-instance driver config so devices report their real VID/PID.
- Working virtual **Xbox 360 / XInput** pad, including rumble-in across all rumble
  APIs, with customizable poll rate, multi-pad support, and a forked driver that
  returns real DualSense feature reports.
- Virtual **DualSense** touchpad emit and virtual→physical forwarding of DualSense
  LEDs and adaptive triggers.
- Gyro/accel encoding, touchpad-neutral handling, profile-driven rumble, and
  friendly device names.

#### Audio Stream Haptics
- Driver-free Audio Stream Haptics module (WASAPI loopback → rumble routing).

#### Devices & rumble
- Physical DualShock 4 touchpad decoding.
- Per-device rumble shaping UI; forwarding of game rumble from HIDMaestro virtual
  pads to physical controllers.
- Single gamepad output card with model selector + rumble range (Easy mode).

### Changed
- `cargo run` / `cargo build` at the workspace root now resolve to the GUI app.
- Async virtual-device lifecycle with a progress overlay and driver reinstall flow.
- Combine feedback from multiple virtual sinks onto a single physical pad.
- Persist setting clarified as HIDMaestro-only (not ViGEm).
- Foreground-gated GPU-loss stall handling.

### Fixed
- **Rumble:** Switch Pro HD-rumble write path, legacy-rumble pins routed to the HD
  voice coil, peak-hold so a same-tick on→off pulse survives, default HD-rumble
  frequency, and physical feedback delivered even under bypass.
- **Device fidelity:** DS4/DualSense digital L2/R2 triggers, DS4 read-back falling
  through to a gilrs WGI axis scramble, DS4 IMU/touchpad byte offsets (+2/+3 too
  high), and virtual pads no longer reporting a false 100% / physical battery.
- **Own-virtual detection:** distinguish own emulated pads from real same-VID/PID
  controllers by HID instance path / USB product string; restore tagging for both
  ViGEm (Xbox/DS4) and HIDMaestro pads.
- **Teardown reliability:** remove HID children and sweep orphaned ghost children
  on teardown, clean up orphans only on first hello, survive abrupt exit without
  orphaning nodes, guarantee a single helper across close→reopen and overlap, and
  collapse teardown to one device enumeration with parallel `pnputil`.
- **Permissions:** grant the unelevated app pipe and Global SHM write access
  (fixes OS error 5).
- **Persistence:** stop destroying virtual nodes on clean exit when persist is on,
  and restore helper persist after GPU recovery even when stalled.
- **UI:** restore node-drag and param edits inside the sub-patch editor, keep live
  visuals animating in the sub-patch editor, restore last-active tab on launch with
  per-canvas pan/zoom, manual MIDI refresh to stop periodic audio disruption,
  exclude battery from AutoMap port/wire glow, and kill the startup low-battery
  warning (show physical pad battery instead).
- **Devices:** stop ~2s input gaps caused by hidapi refresh on the I/O thread.

[0.10.0]: https://github.com/x-iso/FlexInput/compare/v0.9.7...v0.10.0
