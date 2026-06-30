# Changelog

All notable changes to FlexInput are documented here. This project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed

- **Smoother virtual mouse with mixed output:** the Virtual Keyboard & Mouse
  emission loop now scales motion by the *real* elapsed time each tick instead of
  assuming a perfect interval, so cursor speed no longer lurches under scheduler
  jitter when a virtual gamepad is flushing concurrently. The loop also runs at
  1 kHz (was 500 Hz), halving the integer-pixel stair-step so slow stick-aim
  reads smoother.
- **Physical-mouse suppression is now configurable and game-aware.** It is
  automatically forced OFF in "mixed mode" (a virtual gamepad active alongside
  the keyboard/mouse), since games that warp/recenter the cursor each frame would
  otherwise make virtual mouse aim stutter. New Settings: a master on/off toggle
  and an adjustable release window (50–2000 ms, default 500).
- The virtual-mouse emission thread now runs at `TIME_CRITICAL`, and its per-tick
  motion is clamped to ≤4 ms of travel, so an occasional scheduler gap no longer
  discharges as a single cursor jump under heavy game load.

### Added

- **Braid mixed output (experimental):** optional Settings toggle that makes the
  virtual-gamepad and keyboard/mouse outputs **submit in strict alternation** (a
  shared turn token) so a gamepad HID report and a mouse `SendInput` never land
  in the same instant. Neither stream is muted or zeroed — the mouse accumulates
  between its turns (no motion lost) and an idle mouse just passes its turn, so it
  never chops the pad. Pacing is a per-lane rate: **Real-time** (fastest, lowest
  latency — limited only by the polling/mouse rate) or 500 / 250 / 125 Hz. For
  empirically probing games whose input arbiter behaves differently under
  simultaneous mixed output (confirmed to recover a game that lost mouse input
  intermittently under FlexInput). Off by default; effect is game-specific.

## [0.10.2] - 2026-06-27

Touchpad output bindings for Remapper/Lean, a combiner mapping fix, and a
HIDMaestro driver-uninstall path with on-demand install from Easy mode.

### Added

- **Touchpad / swipe / mic output bindings** in the Remapper and Lean (3DOF→2D)
  "Special" picker. The picker is now a button (mouse-clickable cells + gamepad
  nav, same popup for both) offering the three DualSense touch zones, touchpad
  click, horizontal/vertical analog swipe (gated to analog inputs), and the
  DualSense mic button. The engine synthesizes real touch points from these
  bindings, stacking up to the two the hardware supports.
- **Uninstall HIDMaestro driver** path: tears down all live virtual device nodes,
  then removes every installed driver package via the elevated helper (new
  `UninstallDriver` IPC request + `deploy::uninstall_driver`, with an
  `Uninstall`/`Uninstalling` device-op and progress state).
- Easy mode gamepad output card stays **enabled when the driver is absent** —
  selecting a model installs HIDMaestro on demand (one admin prompt) via the
  normal create path, with a hint shown.

### Fixed

- **Combiner SORT** now picks the first *asserted* port (with fallback to the
  first port), so a Remapper's mapped output is no longer clobbered by a raw
  pass-through bus port — fixes broken gamepad button→button remapping inside a
  sub-patch (you'd get neither button, or both lighting up).
- **Touchpad combo logic:** buttons in a touch combo only *gate* the finger
  (activate it), while analog inputs drive the swipe axes — no longer "stuck at
  full value." Opposite cardinals of one axis cover both halves; a combo can map
  e.g. button + left-stick (all directions) to both touchpad-point axes.
- **Gate-button suppression for multi-axis touch combos:** a combo mixing
  opposite cardinals of one axis (which can never be simultaneously held) now
  correctly consumes its gate button from pass-through while active, instead of
  leaking it through.

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
