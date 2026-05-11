---
phase: 01-Core_Foundation
plan: 06
subsystem: devices
tags: [rust, xinput, rumble, force-feedback, ffi, windows, gilrs]

# Dependency graph
requires:
  - phase: 01-Core_Foundation
    plan: 01
    provides: GilrsBackend struct and DeviceBackend trait implementation

provides:
  - XInput rumble dispatch via raw extern "system" XInputSetState call in GilrsBackend::send()
  - xinput_ffi module with XINPUT_VIBRATION struct linked to xinput.dll
  - xinput_idx map populated per poll() call tracking XInput slot assignments
  - xinput_rumble cache per slot storing (left_motor_byte, right_motor_byte)

affects:
  - 01-07
  - 01-08

# Tech tracking
tech-stack:
  added: []
  patterns:
    - "Hand-declared extern system block for xinput.dll (no windows-sys dep, mirrors hidhide.rs pattern)"
    - "#[cfg(windows)] module isolation for Win32 FFI with #[repr(C)] struct"
    - "Early-return branch in send() routes XInput before PS/Switch HID path"
    - "xinput_idx rebuilt each poll() from kind_seen counter to prevent slot drift on reconnect"

key-files:
  created: []
  modified:
    - crates/devices/src/gilrs_backend.rs

key-decisions:
  - "Use hand-declared extern system block linking xinput.dll (no windows-sys dep needed — mirrors hidhide.rs)"
  - "Rebuild xinput_idx at every poll() call (not cached across frames) to stay in sync with kind_seen on device reconnect"
  - "Motor speed scaled from 0-255 to 0-65535 via saturating_mul(257) = 0xFFFF/0xFF"
  - "rumble_strong maps to w_left_motor_speed (left motor); rumble_weak maps to w_right_motor_speed (right motor)"

patterns-established:
  - "xinput_ffi: #[cfg(windows)] mod pattern for raw Win32 FFI in crates/devices (no windows-sys)"
  - "Early-return dispatch pattern in send(): XInput path intercepts before GyroManager path"

requirements-completed:
  - F1
  - F6

# Metrics
duration: 5min
completed: 2026-05-11
---

# Phase 01 Plan 06: XInput Rumble Dispatch Summary

**XInput rumble signals now routed to physical controller motors via raw XInputSetState FFI, with left motor receiving rumble_strong and right motor receiving rumble_weak**

## Performance

- **Duration:** 5 min
- **Started:** 2026-05-11T10:21:02Z
- **Completed:** 2026-05-11T10:26:11Z
- **Tasks:** 2
- **Files modified:** 1

## Accomplishments

- Added `#[cfg(windows)] mod xinput_ffi` with `XINPUT_VIBRATION` struct and `XInputSetState` extern linked to xinput.dll — same hand-declared pattern as hidhide.rs, no new Cargo dependency
- Extended `GilrsBackend` struct with `xinput_idx: HashMap<String, u32>` and `xinput_rumble: HashMap<u32, (u8, u8)>`, initialized in `try_new()`
- Added `self.xinput_idx.clear()` at top of `poll()` and XInput slot population loop inside the gamepads iteration — ensures slot index never drifts on device reconnect
- Replaced `send()` body: XInput early-return branch dispatches `rumble_strong`/`rumble_weak` via `XInputSetState` with 0-255 bytes scaled to 0-65535; PS/Switch HID path via GyroManager is completely unchanged

## Task Commits

Each task was committed atomically:

1. **Task 1: Add xinput_ffi module and new GilrsBackend fields** - `fbee513` (feat)
2. **Task 2: Populate xinput_idx in poll() and dispatch XInput rumble in send()** - `92bfdf7` (feat)

## Files Created/Modified

- `crates/devices/src/gilrs_backend.rs` - Added xinput_ffi FFI module, two new GilrsBackend fields, xinput_idx population in poll(), and XInput dispatch branch in send()

## Decisions Made

- Hand-declared `extern "system"` block used for xinput.dll (no windows-sys dependency in crates/devices Cargo.toml; same pattern as hidhide.rs)
- `xinput_idx` rebuilt at start of every `poll()` call by clearing and re-populating from kind_seen counter — prevents XInput slot drift on controller reconnect
- Motor speed byte-to-u16 conversion uses `saturating_mul(257)` (257 = 65535/255) to map 0-255 range to 0-65535 as required by XInputSetState
- `rumble_strong` pin → `w_left_motor_speed` (left motor), `rumble_weak` pin → `w_right_motor_speed` (right motor)

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered

None.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

- XInput rumble dispatch is complete and compiles on Windows (real XInputSetState) and non-Windows (cfg-guarded, no unsafe executed)
- PS/Switch HID path via GyroManager is fully preserved
- Plans 01-07 (cross-boundary paste) and 01-08 (test harness) can proceed independently

---

## Self-Check: PASSED

- File exists: `crates/devices/src/gilrs_backend.rs` - FOUND
- Commit fbee513 - FOUND
- Commit 92bfdf7 - FOUND
- `mod xinput_ffi` present - FOUND (1 occurrence)
- `xinput_idx: HashMap<String, u32>` present - FOUND
- `xinput_rumble: HashMap<u32, (u8, u8)>` present - FOUND
- `#[link(name = "xinput")]` present - FOUND
- `cargo check --package flexinput-devices` - PASSED (no errors)
- `cargo check --workspace` - PASSED (no errors)

---
*Phase: 01-Core_Foundation*
*Completed: 2026-05-11*
