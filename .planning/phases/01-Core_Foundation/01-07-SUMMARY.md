---
phase: 01-Core_Foundation
plan: 07
subsystem: ui
tags: [egui, clipboard, cross-boundary-paste, automap, canvas, subpatch]

# Dependency graph
requires:
  - phase: 01-04
    provides: same-canvas paste hardening, ClipboardData struct, copy_selected/paste fn

provides:
  - pub(crate) ClipboardData accessible to app.rs
  - Canvas::clipboard() and Canvas::set_clipboard() accessors for cross-boundary paste
  - Canvas::insert_automap_bridge() helper for D-04 item 3 boundary bridge insertion
  - FlexInputApp::app_clipboard field shared across all Canvas instances
  - Cross-boundary clipboard seeding/sync in FlexInputApp::update() and show_subpatch_editors()
  - insert_automap_bridge called on cross-boundary Ctrl+V in outer and inner canvas paths
  - Five clipboard contract tests in clipboard_tests module

affects: [01-08, any plan touching SubPatchEditor clipboard or copy/paste behavior]

# Tech tracking
tech-stack:
  added: []
  patterns:
    - App-level clipboard field synced from/to per-Canvas clipboard after each show()
    - Cross-boundary paste seeding: seed target canvas before show() when canvas.clipboard is None but app_clipboard is Some
    - Bridge node insertion gated on Ctrl+V detection via ctx.input()/vctx.input() before Canvas::show()
    - Inner SubPatchEditor viewport uses vctx (not ctx) for input detection

key-files:
  created: []
  modified:
    - crates/ui/src/canvas/mod.rs
    - crates/ui/src/app.rs

key-decisions:
  - "ClipboardData promoted to pub(crate) instead of private; Canvas accessors are the only public surface — internal nodes/wires vec remain private"
  - "insert_automap_bridge inserts ALL canonical AutoMap pins as Phase 1 approximation (D-04 item 3); non-AutoMap boundary wires are dropped by paste() semantics, not by bridge logic"
  - "Ctrl+V detection at app level uses egui Event::Key + Event::Paste pattern matching to match Canvas::show() shortcut handling"
  - "Inner SubPatchEditor viewport detects Ctrl+V via vctx (the viewport-specific context), not ctx, to correctly capture events in the inner window"
  - "app_clipboard snapshot (clone) taken before viewport closure to avoid borrow conflicts with app inside the closure"
  - "4 missing tests merged into existing clipboard_tests module rather than creating a duplicate mod tests block"

patterns-established:
  - "Cross-boundary clipboard: seed target.clipboard before show() from app_clipboard when target.clipboard is None"
  - "Cross-boundary bridge: check Ctrl+V from ctx/vctx before show(), call insert_automap_bridge if seeding occurred AND Ctrl+V pressed"
  - "App-level clipboard sync: after every canvas show(), if canvas.clipboard() is Some, update self.app_clipboard"

requirements-completed: [F4]

# Metrics
duration: 45min
completed: 2026-05-11
---

# Phase 01 Plan 07: Cross-Boundary Clipboard Summary

**App-level clipboard shared across outer tabs and SubPatchEditor inner canvases, with AutoMap Splitter/Collector bridge node insertion (D-04 item 3) on cross-boundary Ctrl+V paste**

## Performance

- **Duration:** ~45 min
- **Started:** 2026-05-11T00:00:00Z
- **Completed:** 2026-05-11T01:00:00Z
- **Tasks:** 4 (Tasks 1 and 2 were partially pre-done; Tasks 3 and 4 completed by this executor)
- **Files modified:** 2

## Accomplishments

- app_clipboard field added to FlexInputApp; synced after every canvas show() call; used to seed target canvas before show() when target has no local clipboard
- insert_automap_bridge() called on cross-boundary Ctrl+V in both the outer canvas path and the inner SubPatchEditor viewport path
- 4 clipboard contract tests (fresh_canvas_has_no_clipboard, set_clipboard_makes_clipboard_accessible, paste_after_set_clipboard_inserts_node, paste_calls_push_undo) added to clipboard_tests module; all 21 ui tests pass
- cargo check --workspace exits 0; cargo test --package flexinput-ui exits 0

## Task Commits

Each task was committed atomically:

1. **Task 1+4 canvas: Expose ClipboardData, add accessors, insert_automap_bridge** - `b636729` (feat) — [pre-existing commit]
2. **Task 2: app_clipboard field and cross-boundary paste logic** — [included in b636729 pre-existing commit]
3. **Task 3: In-module tests for cross-boundary clipboard semantics** - `e97fd0d` (test)
4. **Task 4 app.rs: insert_automap_bridge call on cross-boundary paste** - `421cd60` (feat)

## Files Created/Modified

- `crates/ui/src/canvas/mod.rs` - ClipboardData pub(crate), clipboard()/set_clipboard() accessors, insert_automap_bridge() helper, 4 new clipboard contract tests in clipboard_tests module
- `crates/ui/src/app.rs` - app_clipboard field on FlexInputApp, cross-boundary seeding/sync in update() and show_subpatch_editors(), insert_automap_bridge call on cross-boundary Ctrl+V in both outer and inner canvas paths

## Decisions Made

- insert_automap_bridge inserts ALL canonical automap::ALL_PINS as Phase 1 approximation; D-04 item 3 describes "wires that cross the boundary" but at paste time we don't yet know which pins are actually used, so all pins are bridged as a conservative default
- Tests merged into existing `clipboard_tests` module (not a new `tests` module) to avoid duplication since `copy_selected_populates_clipboard` already existed there
- Ctrl+V detection uses both `Event::Key` and `Event::Paste` variants, matching the existing canvas shortcut handler pattern

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Merged tests into clipboard_tests rather than creating duplicate mod tests**
- **Found during:** Task 3 (test implementation)
- **Issue:** copy_selected_populates_clipboard already existed in clipboard_tests; adding a new mod tests block would create a duplicate test name (compile error) or redundant duplicate test
- **Fix:** Added 4 missing tests (fresh_canvas_has_no_clipboard, set_clipboard_makes_clipboard_accessible, paste_after_set_clipboard_inserts_node, paste_calls_push_undo) to existing clipboard_tests module
- **Files modified:** crates/ui/src/canvas/mod.rs
- **Verification:** cargo test shows 21 tests pass, no FAILED
- **Committed in:** e97fd0d

**2. [Rule 1 - Bug] Used vctx instead of ctx for inner viewport Ctrl+V detection**
- **Found during:** Task 4 (insert_automap_bridge in show_subpatch_editors)
- **Issue:** The SubPatchEditor renders in its own immediate viewport via ctx.show_viewport_immediate(); keyboard events in that window are on vctx (the viewport context), not ctx (the main context). Using ctx.input() would miss Ctrl+V typed in the inner window.
- **Fix:** Detected Ctrl+V via vctx.input() inside the viewport closure, using inner_needs_clipboard_seed flag (computed before closure) to know whether cross-boundary seeding occurred
- **Files modified:** crates/ui/src/app.rs
- **Verification:** cargo check --workspace exits 0
- **Committed in:** 421cd60

---

**Total deviations:** 2 auto-fixed (1 merge-instead-of-duplicate, 1 vctx vs ctx correctness)
**Impact on plan:** Both fixes necessary for correctness. No scope creep.

## Issues Encountered

None beyond the deviations documented above.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

- Cross-boundary clipboard infrastructure is complete; plan 01-08 (test harness gaps) can use the clipboard accessors
- app_clipboard survives SubPatchEditor open/close cycles as required by the plan's must_haves
- Same-canvas paste behavior unchanged (canvas.clipboard() is non-None for same-canvas copy/paste, so seeding is never applied in that case)

## Self-Check

**Files exist:**
- `crates/ui/src/canvas/mod.rs` - confirmed (modified)
- `crates/ui/src/app.rs` - confirmed (modified)

**Commits exist:**
- `b636729` - confirmed (pre-existing)
- `e97fd0d` - confirmed
- `421cd60` - confirmed

## Self-Check: PASSED

---
*Phase: 01-Core_Foundation*
*Completed: 2026-05-11*
