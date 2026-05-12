---
phase: 01-Core_Foundation
plan: "04"
subsystem: ui
tags: [egui, egui-snarl, canvas, clipboard, copy-paste, node-graph]

requires: []
provides:
  - "Hardened paste() with pin-count bounds validation preventing panic on stale ClipboardData"
  - "7 regression tests covering copy/paste clipboard semantics in canvas/mod.rs"
affects:
  - 01-05
  - 01-06
  - 01-07
  - 01-08

tech-stack:
  added: []
  patterns:
    - "Inline #[cfg(test)] module in canvas/mod.rs for unit tests accessing private Canvas methods"
    - "Early-continue guard pattern for bounds validation before snarl.connect()"

key-files:
  created: []
  modified:
    - "crates/ui/src/canvas/mod.rs"

key-decisions:
  - "Pin-count validation added to paste() to mitigate T-04-01: stale/malformed ClipboardData wire indices dropped silently rather than panicking"
  - "Tests placed in inline #[cfg(test)] module inside canvas/mod.rs to access private copy_selected() and paste() methods without changing visibility"
  - "Boundary wire exclusion verified as already correct: copy_selected() only stores wires where both endpoints are in the selected set"

patterns-established:
  - "Bounds-check node indices before pin indices in paste() — separate early-continue guards for each level"
  - "Test helpers make_node(n_out, n_in) and add_node() for lightweight Canvas test setup without egui context"

requirements-completed:
  - F4

duration: 25min
completed: 2026-05-10
---

# Phase 01 Plan 04: Canvas Copy-Paste Hardening Summary

**Hardened `paste()` with pin-count bounds validation (T-04-01) and added 7 inline regression tests covering copy, paste, offset, internal wire reconstruction, and boundary wire exclusion.**

## Performance

- **Duration:** 25 min
- **Started:** 2026-05-10T00:00:00Z
- **Completed:** 2026-05-10T00:25:00Z
- **Tasks:** 2
- **Files modified:** 1

## Accomplishments

- Added per-pin bounds validation in `paste()`: stale or malformed `ClipboardData` wire entries are silently dropped instead of panicking via `snarl.connect()` assert (mitigates T-04-01)
- Added inline `#[cfg(test)] mod clipboard_tests` with 7 passing tests that exercise copy, paste, fresh NodeId generation, position offset, internal wire reconstruction, boundary wire exclusion, and malformed-index resilience
- Confirmed that the existing `copy_selected()` / `paste()` / keyboard shortcut handlers (Ctrl+C via `Event::Copy` + `Key::C`, Ctrl+V via `Event::Paste` + `Key::V`) were already correct and required no structural changes

## Task Commits

Each task was committed atomically:

1. **Task 1: Harden existing canvas copy/paste handlers** - `76124ea` (fix)
2. **Task 2: Add regression coverage for canvas clipboard semantics** - `ea07e89` (test)

## Files Created/Modified

- `crates/ui/src/canvas/mod.rs` - Hardened `paste()` with pin-count validation; added `clipboard_tests` module with 7 regression tests

## Decisions Made

- Used inline `#[cfg(test)]` module rather than a separate `crates/ui/tests/` file so that the private `copy_selected()` and `paste()` methods are directly accessible without changing their visibility.
- Chose silent `continue` (drop) over `log::warn!` for invalid wire entries because the UI crate has no logger configured and adding one would be out of scope for this plan.
- Boundary wire exclusion was already enforced correctly by `copy_selected()`'s filter; no code change was needed — only a regression test to lock the behavior.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 2 - Missing Critical] Added pin-index bounds validation to paste()**
- **Found during:** Task 1 (Harden existing canvas copy/paste handlers)
- **Issue:** The existing `paste()` only bounds-checked node indices (`from_idx < new_ids.len()`), but not pin indices (`from_pin`, `to_pin`). `snarl.connect()` uses `assert!` internally; an out-of-bounds pin index would panic at runtime if ClipboardData became stale.
- **Fix:** Added `from_pin < d.outputs.len()` and `to_pin < d.inputs.len()` checks sourced from the clipboard's stored NodeData before calling `snarl.connect()`.
- **Files modified:** `crates/ui/src/canvas/mod.rs`
- **Verification:** `paste_ignores_out_of_bounds_wire_indices` test passes with injected pin index 99.
- **Committed in:** `76124ea` (Task 1 commit)

---

**Total deviations:** 1 auto-fixed (Rule 2 - missing critical bounds check)
**Impact on plan:** Security correctness fix required by threat model T-04-01. No scope creep.

## Issues Encountered

None — existing keyboard shortcut dispatch, `ClipboardData` struct, `copy_selected()`, and `paste()` were all structurally sound. Only the pin-count validation gap required a fix.

## Known Stubs

None — no placeholder data or hardcoded values introduced.

## Threat Flags

None — no new network endpoints, auth paths, file access, or schema changes introduced.

## Next Phase Readiness

- Canvas clipboard is now hardened and regression-tested; ready for grouping (plan 05) and cross-boundary paste (plan 07) workflows that depend on correct copy/paste behavior.
- The `copy_selected()` method correctly excludes boundary wires, providing a stable foundation for AutoMap Splitter/Collector insertion at paste boundaries (plan 07).

---
*Phase: 01-Core_Foundation*
*Completed: 2026-05-10*
