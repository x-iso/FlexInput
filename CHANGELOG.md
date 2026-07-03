# Changelog

All notable changes to FlexInput are documented here. This project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.10.7] - 2026-07-03

A new Vec Reshaper module for fighting analog-stick "diagonal stickiness" —
directional reshaping of a Vec2 with a visual editor.

### Added

- **Vec Reshaper module** (Processing category, Vec2 → Vec2). Reshapes a stick
  vector as a function of DIRECTION, which the radially-symmetric Vec Response
  Curve cannot do. Two orthogonal controls: a per-direction **Boundary** that
  sets the reachable output envelope (1.0 = circle, √2 = the square's corner, so
  a round stick can be expanded to fill a square for games that expect square
  response), and a per-direction **Gain** that accelerates/decelerates within
  that envelope (push diagonals faster to kill diagonal stickiness). One quadrant
  is edited; the rest mirror it (4-way, or X-mirror for asymmetric up/down).
- **Visual editor** on the node body: a direction→value curve (grid + snap on
  both axes) with a Gain/Boundary toggle, plus a live 2D pad showing the unit
  circle, the reshaped envelope, and a smooth **stretch-field gradient** (blue =
  accelerated, red = decelerated, transparent at neutral) so the internal shaping
  is visible without moving the stick. Live input→output dots trace the current
  deflection. Presets: Circle, Square, Diag+. Every element is individually
  pinnable in Easy mode and gamepad-navigable.

Gamepad navigation now reaches every Audio Stream Haptics control in Easy mode.

### Fixed

- **Audio Stream Haptics pinned elements are now editable via gamepad
  navigation.** The module was missing from the Easy-mode nav dispatch, so its
  pinned rows were selectable but not editable. All calibration rows (Volume,
  Release, Crossover, Amplitude floor/ceiling/curve, Balance, Swap, Rumble mix)
  and the capture-mode block (App/Focused/System + include-children) now route
  through the unified multi-field editor. The scope's EQ points are dot-editable
  through the same curve-dot path as Response Curve widgets (South enters,
  LS/dpad highlights a dot, RT/LT add/remove at the cursor, South edits a dot).

## [0.10.5] - 2026-07-01

SDL3 gamepad support for controllers FlexInput doesn't handle natively, and the
extra rear-paddle / misc buttons those pads expose, mappable through the AutoMap
system.

### Added

- **SDL3 gamepad backend** for controllers FlexInput doesn't parse natively
  (Steam Controller, 8BitDo, arcade sticks, third-party pads). gilrs and the
  raw-HID path keep the pads they handle well (Xbox/XInput, DualShock 4,
  DualSense, Switch Pro) with their tuned gyro/touchpad/HD-haptic overrides; SDL
  is enumerated only for pads that classify as generic, filtered by VID/PID so no
  controller is surfaced twice. For those pads it relays sticks, buttons, analog
  triggers, gyro/accel, touchpad, and the extra paddle/misc buttons. SDL is built
  from source and linked statically — no extra DLL to ship.
- **Extra buttons in the AutoMap system.** Rear paddles (`btn_paddle_l1/r1/l2/r2`)
  and misc buttons (`btn_misc1..6`) are now part of the canonical AutoMap pin set,
  so they can be mapped to anything via Remapper and other AutoMap modules. Rear
  paddles render a generic labeled icon (PL1/PR1/PL2/PR2); labels are
  device-agnostic for now.

## [0.10.4] - 2026-07-01

HidHide masking of remapped physical controllers, exact XInput player-slot
control, same-family physical/virtual pad fixes, Audio Stream Haptics raw
analysis outputs, and mixed-output smoothing.

### Added

- **HidHide masking** of remapped physical controllers via the elevated
  HIDMaestro helper, so a game sees only the virtual pad and not the physical
  device behind it. Masking is reconciled on device/patch changes and toggleable
  from Settings.
- **Exact XInput player-slot control:** a slot-reorder engine plus on-canvas and
  Easy-mode slot indicators, with safe virtual re-arrival so a re-created pad
  reclaims its slot. Resolves physical pads reading from the wrong slot after
  focus loss.
- **Audio Stream Haptics — raw analysis output pins.** Six new Float outputs
  after the AutoMap passthrough expose the raw two-band decomposition *before*
  the carrier/modulator (AM/RM) blend: per-band/per-side envelope followers
  (`LF/HF EF L/R`) and each band's carrier frequency in Hz (`LF/HF Hz`). Wire
  them to scopes/readouts or drive other modules from the audio analysis.
- **Audio Stream Haptics — pinnable capture-mode block.** The App/Focused/System
  selector (with its process picker and status line) can now be pinned to a
  sub-patch body like the calibration rows.
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

### Fixed

- **Physical/virtual same-family pad crossing.** A physical controller no longer
  freezes or reads from the wrong device when a virtual pad of the same family is
  present (DualSense gilrs-walk vs hidapi index crossing; XInput slot/Steam
  consolidation). Physical XInput is now read directly via `XInputGetState` so it
  survives focus loss, and the physical pad is correlated to its real slot.

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
