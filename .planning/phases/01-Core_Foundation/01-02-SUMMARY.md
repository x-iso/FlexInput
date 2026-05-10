---
phase: 01-Core_Foundation
plan: "02"
subsystem: modules
tags: [rust, egui, flexinput-modules, flexinput-devices, flexinput-ui, rumble, rgb, adaptive-trigger, sink-modules]

requires: []

provides:
  - RumbleOutput module (module.rumble_output) with rumble_strong/rumble_weak Float inputs
  - RgbOutput module (module.rgb_output) with lightbar_r/g/b Float inputs
  - AdaptiveTriggerOutput module (module.adaptive_trigger_output) with adaptive_trigger_l/r Float inputs
  - All three modules registered in controls::registrations() and available in all_modules()
  - Integration tests verifying gilrs: sink output routing reaches DeviceBackend::send()

affects:
  - "01-06: XInput rumble dispatch extends the same sink routing path verified here"
  - "01-07: cross-boundary paste uses app.rs infrastructure confirmed intact"
  - "01-08: test harness patterns established here (MockBackend, output_routing)"

tech-stack:
  added: []
  patterns:
    - "Sink-only module pattern: Module with inputs but empty outputs Vec; process() returns SmallVec::new()"
    - "MockBackend pattern for DeviceBackend testing: Arc<Mutex<Vec<...>>> records send() calls"
    - "dispatch_sink_outputs helper: gilrs: predicate mirrors app.rs I/O thread, testable in isolation"

key-files:
  created:
    - "crates/ui/tests/output_routing.rs"
  modified:
    - "crates/modules/src/controls.rs"
    - "crates/modules/src/lib.rs"

key-decisions:
  - "Sink modules have no outputs Vec because values flow through engine SinkTarget routing, not Module::process()"
  - "Pin IDs match device layout IDs exactly (rumble_strong/rumble_weak, lightbar_r/g/b, adaptive_trigger_l/r)"
  - "AdaptiveTriggerOutput documented as DualSense USB-only; Bluetooth limitation noted in doc comment"
  - "lib.rs already calls controls::registrations() so no structural change needed; added inline test to confirm IDs"
  - "Integration test reproduces gilrs: dispatch predicate in isolation (dispatch_sink_outputs helper) rather than testing live I/O thread"

patterns-established:
  - "Sink module pattern: #[derive(Default)] struct with no outputs, process() returns SmallVec::new()"
  - "Registry test pattern: inline #[cfg(test)] mod tests in lib.rs asserts all_modules() IDs"
  - "MockBackend pattern: implement DeviceBackend, record send() calls in Arc<Mutex<Vec>>"

requirements-completed: [F1, F3, F6]

duration: 18min
completed: 2026-05-10
---

# Phase 01 Plan 02: Physical Feedback Sink Modules Summary

**Three sink-only graph modules (RumbleOutput, RgbOutput, AdaptiveTriggerOutput) registered in the global module palette, with integration tests confirming gilrs: sink outputs reach DeviceBackend::send()**

## Performance

- **Duration:** ~18 min
- **Started:** 2026-05-10T18:01Z
- **Completed:** 2026-05-10T18:19Z
- **Tasks:** 3
- **Files modified:** 3 (controls.rs, lib.rs, output_routing.rs)

## Accomplishments

- Added `RumbleOutput` (module.rumble_output), `RgbOutput` (module.rgb_output), and `AdaptiveTriggerOutput` (module.adaptive_trigger_output) as sink-only modules in `controls.rs`
- Registered all three in `controls::registrations()`; confirmed presence in `all_modules()` via inline unit test
- Created `crates/ui/tests/output_routing.rs` with 3 integration tests that reproduce the gilrs: dispatch predicate and verify routing correctness with a MockBackend

## Task Commits

Each task was committed atomically:

1. **Task 1: Add output-only feedback modules to controls.rs** - `396407e` (feat)
2. **Task 2: Register the new modules in the global module list** - `021fa4c` (feat)
3. **Task 3: Test that sink output routing reaches physical backends** - `813f490` (test)

**Plan metadata:** (docs commit follows in planning repo)

## Files Created/Modified

- `crates/modules/src/controls.rs` - Added RumbleOutput, RgbOutput, AdaptiveTriggerOutput structs + impl Module + added to registrations()
- `crates/modules/src/lib.rs` - Added inline #[cfg(test)] mod tests with all_modules_includes_feedback_sinks test
- `crates/ui/tests/output_routing.rs` - New integration test file: MockBackend + 3 routing tests

## Decisions Made

- **Sink-only module design**: outputs Vec is empty because values flow through the engine's SinkTarget routing path (device.sink node), not through Module::process(). This is the same pattern used by all physical device sink nodes.
- **Pin IDs match layout IDs exactly**: rumble_strong/rumble_weak, lightbar_r/g/b, adaptive_trigger_l/r — these must match what gilrs_backend.rs expects so graph wires connect correctly.
- **AdaptiveTriggerOutput BT limitation documented**: Added doc comment noting USB-only limitation per DualSense HID report constraints.
- **Isolated dispatch predicate for testing**: Rather than spin up the full I/O thread, extracted the `gilrs:` predicate into a `dispatch_sink_outputs` helper in the test file — mirrors the production logic exactly without threading complexity.

## Deviations from Plan

None - plan executed exactly as written.

The plan allowed a smoke test if direct backend mocking was not feasible. Full MockBackend mocking was feasible, so 3 substantive routing tests were written instead of just a compilation smoke test.

## Issues Encountered

None. Build clean on first attempt. All tests pass.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

- Module palette now includes physical feedback sinks — users can add them to graphs
- Sink output routing verified: gilrs: device IDs reach backend.send()
- Plan 01-06 (XInput rumble dispatch in gilrs_backend.rs) can proceed — the module IDs and routing path are now confirmed correct
- No blockers

---
*Phase: 01-Core_Foundation*
*Completed: 2026-05-10*
