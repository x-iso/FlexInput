---
phase: 01-Core_Foundation
plan: 08
subsystem: testing
tags: [rust, cargo-test, serde, egui-snarl, slab, layouts, clipboard, UiPatch]

# Dependency graph
requires:
  - phase: 01-Core_Foundation
    provides: layouts.rs pub fn outputs_for/inputs_for, UiPatch struct, canvas clipboard logic
provides:
  - Device layout integration tests (6 tests) in crates/devices/tests/output_enumeration.rs
  - UiPatch public API with round-trip compatibility tests (2 tests) in crates/ui/tests/patch_compat.rs
  - In-module canvas clipboard tests (3 named + 7 pre-existing = 10 total) in canvas/mod.rs
  - Fixture file crates/ui/tests/fixtures/compat_v1_basic.json
affects: [01-Core_Foundation plans 01-01 through 01-07 that depend on test infrastructure]

# Tech tracking
tech-stack:
  added: []
  patterns:
    - "Integration tests in crates/<name>/tests/<name>.rs — auto-discovered by Cargo, no Cargo.toml changes"
    - "Slab<T> serializes as JSON object (map), not array — affects egui-snarl Snarl<T> fixture format"
    - "In-module #[cfg(test)] mod for testing private fn — avoids visibility changes to copy_selected/paste"
    - "UiPatch pub with pub fields + re-export via lib.rs for integration test access"

key-files:
  created:
    - crates/devices/tests/output_enumeration.rs
    - crates/ui/tests/patch_compat.rs
    - crates/ui/tests/fixtures/compat_v1_basic.json
  modified:
    - crates/ui/src/canvas/mod.rs (UiPatch struct pub + pub fields; 3 new named clipboard tests)
    - crates/ui/src/lib.rs (pub use canvas::UiPatch re-export)

key-decisions:
  - "Made UiPatch fully pub (struct + all fields) rather than pub(crate) because integration tests in tests/ are a separate crate and cannot access pub(crate) items"
  - "Re-exported UiPatch from flexinput_ui crate root via lib.rs so patch_compat.rs can import it with use flexinput_ui::UiPatch"
  - "Fixed fixture JSON: snarl.nodes field must be {} (empty object/map) not [] (array) because Slab<T> serializes as a map"
  - "Added 3 new named test functions to existing clipboard_tests mod rather than creating a separate tests mod"

patterns-established:
  - "Pattern: snarl fixture format uses nodes:{} (map keyed by slab index) and wires:[] (sequence)"
  - "Pattern: integration test crates import pub symbols only — pub(crate) is insufficient"

requirements-completed: [F1, F4, F5]

# Metrics
duration: 25min
completed: 2026-05-10
---

# Phase 1 Plan 08: Test Infrastructure Summary

**Test harness established: 6 device layout tests, 2 UiPatch round-trip tests, 10 canvas clipboard tests — all passing with zero new Cargo dependencies**

## Performance

- **Duration:** ~25 min
- **Started:** 2026-05-10T18:08:00Z
- **Completed:** 2026-05-10T18:33:46Z
- **Tasks:** 2
- **Files modified:** 5 (3 created, 2 modified)

## Accomplishments
- Created device layout integration test suite (6 tests) covering rumble, lightbar, adaptive trigger, HD rumble, automap_out, and Generic non-panic
- Made UiPatch pub with all fields pub, re-exported from crate root, enabling integration tests to deserialize .fxp files
- Created correct JSON fixture (snarl uses Slab which serializes as map `{}`, not array `[]`)
- Added 3 named canvas clipboard tests required by plan acceptance criteria to existing clipboard_tests module

## Task Commits

1. **Task 1: Verify pub visibility on layouts functions and create device integration tests** - `c4c727d` (feat)
2. **Task 2: Create UiPatch fixture and round-trip compatibility test; add canvas clipboard in-module tests** - `38307ec` (feat)

## Files Created/Modified
- `crates/devices/tests/output_enumeration.rs` - 6 integration tests for device pin layout tables
- `crates/ui/tests/patch_compat.rs` - 2 patch round-trip compatibility tests
- `crates/ui/tests/fixtures/compat_v1_basic.json` - minimal UiPatch JSON fixture with correct snarl format
- `crates/ui/src/canvas/mod.rs` - UiPatch struct + fields made pub; 3 named clipboard tests added
- `crates/ui/src/lib.rs` - pub use canvas::UiPatch re-export added

## Decisions Made
- Made `UiPatch` fully `pub` (not `pub(crate)`) because Rust integration tests in `tests/` are compiled as a separate crate and can only access `pub` items. The plan suggested `pub(crate)` would suffice for integration tests — this is incorrect; `pub` is required.
- Re-exported `UiPatch` via `pub use canvas::UiPatch` in `lib.rs` since the `canvas` module remains private.
- The fixture JSON required `"nodes": {}` (empty map) not `"nodes": []` (empty array) because egui-snarl's `Snarl<T>` uses `slab::Slab<Node<T>>` internally, and `Slab` serializes as a JSON map keyed by slot index.
- Added plan 01-08's three named tests (`copy_selected_captures_nodes`, `paste_inserts_at_offset`, `paste_with_empty_clipboard_is_noop`) to the existing `clipboard_tests` mod using the already-defined `make_node` helper, avoiding `Default` impl issues.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] UiPatch fields must be pub for integration test field access**
- **Found during:** Task 2 (creating patch_compat.rs)
- **Issue:** Plan said make UiPatch `pub(crate)` but integration tests need `pub` struct AND `pub` fields — compiler error E0616 (private field access from separate crate)
- **Fix:** Changed `struct UiPatch` to `pub struct UiPatch` with all fields marked `pub`; added `pub use canvas::UiPatch` in lib.rs
- **Files modified:** crates/ui/src/canvas/mod.rs, crates/ui/src/lib.rs
- **Verification:** cargo test --package flexinput-ui --test patch_compat exits 0 with 2 tests passing
- **Committed in:** 38307ec (Task 2 commit)

**2. [Rule 1 - Bug] Fixture JSON format: snarl.nodes must be map not array**
- **Found during:** Task 2 (running patch_compat tests)
- **Issue:** Plan fixture template used `"nodes": []` but slab::Slab<T> serializes as a JSON map; serde error "invalid type: sequence, expected a map"
- **Fix:** Changed fixture and inline test string to `"nodes": {}` (empty JSON object)
- **Files modified:** crates/ui/tests/fixtures/compat_v1_basic.json, crates/ui/tests/patch_compat.rs
- **Verification:** Both patch_compat tests pass
- **Committed in:** 38307ec (Task 2 commit)

**3. [Rule 1 - Bug] NodeData does not implement Default — used make_node() helper instead**
- **Found during:** Task 2 (adding canvas clipboard tests)
- **Issue:** Plan template used `NodeData { module_id: "...".into(), ..Default::default() }` but NodeData has no Default impl; compiler error E0277
- **Fix:** Used the already-defined `make_node(1, 0)` helper from the same clipboard_tests module
- **Files modified:** crates/ui/src/canvas/mod.rs
- **Verification:** All 10 in-module canvas tests pass
- **Committed in:** 38307ec (Task 2 commit)

---

**Total deviations:** 3 auto-fixed (all Rule 1 bugs — plan template errors vs actual codebase)
**Impact on plan:** All fixes were necessary for compilation and correctness. No scope creep. Tests now match exact named acceptance criteria.

## Issues Encountered
- None beyond the three auto-fixed compilation errors above.

## Known Stubs
None — all tests exercise real code paths with no placeholder assertions.

## Threat Flags
None — no new network endpoints, auth paths, file access patterns, or schema changes introduced.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Test harness is fully operational for all subsequent plan test tasks
- `cargo test --workspace` exits 0 with 22 passing tests across all crates
- Device layout tests, UiPatch round-trip tests, and canvas clipboard tests all verified
- No blockers for remaining Phase 1 plans

## Self-Check: PASSED

- FOUND: crates/devices/tests/output_enumeration.rs
- FOUND: crates/ui/tests/patch_compat.rs
- FOUND: crates/ui/tests/fixtures/compat_v1_basic.json
- FOUND commit c4c727d (Task 1)
- FOUND commit 38307ec (Task 2)
- `pub fn outputs_for` confirmed in layouts.rs
- `pub struct UiPatch` confirmed in canvas/mod.rs
- `fn copy_selected_captures_nodes` confirmed in canvas/mod.rs
- `fn patch_round_trip_v1` confirmed in patch_compat.rs

---
*Phase: 01-Core_Foundation*
*Completed: 2026-05-10*
