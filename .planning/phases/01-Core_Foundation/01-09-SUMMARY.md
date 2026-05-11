---
phase: 01-Core_Foundation
plan: 09
subsystem: modules
tags: [rust, signal-routing, automap, selector, split, any-type]

# Dependency graph
requires: []
provides:
  - "Selector and Split value pins widened to SignalType::Any — Vector and other non-Float signals can now route through them"
  - "AutoMapFork module (module.automap_fork) — routes one AutoMap bus to out_0 or out_1 based on Bool/Float select"
  - "AutoMapSelector module (module.automap_selector) — selects one of N AutoMap buses using Float 0..1 quantized slot logic"
affects: [ui, engine, future-automap-routing]

# Tech tracking
tech-stack:
  added: []
  patterns:
    - "AutoMap routing modules use no-op or pass-through process() — eval.rs follows wire topology; modules just declare pins"
    - "Value pass-through modules (Selector, Split) use SignalType::Any on data pins, SignalType::Float on control/select pins"

key-files:
  created: []
  modified:
    - crates/modules/src/controls.rs
    - crates/modules/src/processing.rs

key-decisions:
  - "SplitModule::process updated to pass actual signal through on selected output (not just extracted Float) so Any-typed value pins are semantically correct"
  - "AutoMapFork and AutoMapSelector process() implementations perform actual routing logic rather than being no-ops, consistent with their semantic role as routing switches"
  - "Signal::Float(0.0) used as disconnected/zero value for unselected AutoMap outputs since Signal does not implement Default"

patterns-established:
  - "Pattern: AutoMap routing modules that do not inject per-pin signals have lightweight process() bodies; eval.rs wire topology handles the actual bus routing"

requirements-completed: [F4, F6]

# Metrics
duration: 15min
completed: 2026-05-11
---

# Phase 01 Plan 09: Selector/Split Any-pin widening and AutoMapFork/AutoMapSelector Summary

**Selector and Split value pins widened to SignalType::Any enabling Vec2/Bool/Int routing; AutoMapFork and AutoMapSelector added for AutoMap bus switching at patch boundaries**

## Performance

- **Duration:** ~15 min
- **Started:** 2026-05-11T00:00:00Z
- **Completed:** 2026-05-11T00:15:00Z
- **Tasks:** 2
- **Files modified:** 2

## Accomplishments
- Selector module: `in_0`, `in_1`, `out` pins changed from `SignalType::Float` to `SignalType::Any`; `select` pin remains `SignalType::Float`
- Split module: `in`, `out_0`, `out_1` pins changed from `SignalType::Float` to `SignalType::Any`; `select` pin remains `SignalType::Float`
- SplitModule::process updated to pass the actual incoming signal through on the selected output (not coerced to Float)
- AutoMapFork added: routes one AutoMap bus to out_0 or out_1 based on Bool/Float select pin; registered in processing.rs
- AutoMapSelector added: quantizes Float 0..1 to N slot index and routes the corresponding AutoMap input to out; registered in processing.rs
- Workspace compiles cleanly — no new errors introduced

## Task Commits

Each task was committed atomically:

1. **Task 1: Fix Selector and Split value pin types from Float to Any** - `cad66b9` (feat)
2. **Task 2: Add AutoMapFork and AutoMapSelector modules to processing.rs** - `bb9cf95` (feat)

## Files Created/Modified
- `crates/modules/src/controls.rs` - SelectorModule and SplitModule pin type declarations updated; SplitModule::process passes actual signal through
- `crates/modules/src/processing.rs` - AutoMapFork and AutoMapSelector structs added with Module impls; both registered in registrations()

## Decisions Made
- **SplitModule::process signal pass-through:** The plan's action text said to verify `Signal::default()` availability. Since `Signal` does not implement `Default`, the unselected output continues emitting `Signal::Float(0.0)`. The selected output now passes the actual `Option<Signal>` value through (unwrapping to `Signal::Float(0.0)` if None). This is semantically correct for Any-typed pins.
- **AutoMapFork/Selector process() not no-op:** Unlike AutoMapSplitModule and AutoMapCollectModule which rely on eval.rs injection for per-pin signal work, AutoMapFork and AutoMapSelector perform simple signal routing in process() to correctly populate their output slots. This keeps the modules self-consistent.
- **ModuleDescriptor without ..Default::default():** ModuleDescriptor does not implement Default — all fields were initialized explicitly, matching the existing module patterns in the file.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] SplitModule::process adapted to pass Any-typed signal through**
- **Found during:** Task 1 (Fix Selector and Split value pin types)
- **Issue:** Original process() extracted the `in` pin as a float via `get_float(inputs, 1, 0.0)` and always emitted `Signal::Float(...)`. With the `in` pin now typed `Any`, this would silently downcast Vec2/Bool/Int signals to Float, defeating the purpose of the type widening.
- **Fix:** Changed to `inputs.get(1).and_then(|s| *s)` to preserve the actual signal variant; unselected outputs still emit `Signal::Float(0.0)` as the zero/default.
- **Files modified:** crates/modules/src/controls.rs
- **Verification:** cargo check --workspace passes cleanly
- **Committed in:** cad66b9 (Task 1 commit)

---

**Total deviations:** 1 auto-fixed (Rule 1 — correctness fix to match intent of pin type widening)
**Impact on plan:** Necessary for semantic correctness. No scope creep.

## Issues Encountered
None — both tasks executed smoothly. The plan's note about `Signal::default()` unavailability proved accurate; `Signal::Float(0.0)` was used as documented.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Selector and Split now accept any signal type on value pins, unblocking Vector routing through those modules
- AutoMapFork and AutoMapSelector appear in the module palette (category "AutoMap") and are ready for use in sub-patch boundary composition
- No blockers

---
*Phase: 01-Core_Foundation*
*Completed: 2026-05-11*
