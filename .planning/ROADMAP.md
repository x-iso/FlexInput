# FlexInput Roadmap

## Phase 1: Core foundation and Windows device integration ✓ Complete (2026-05-11)

**Goal:** Establish a stable real-time signal processing foundation, robust Windows device I/O, and patch persistence so FlexInput can reliably route physical and MIDI inputs to virtual outputs.

**Requirements:** [F1, F2, F3, F4, F5, F6, F7]

**Plans:** 9 plans
- [x] 01-01-PLAN.md — Enumerate physical controller output pins and feedback metadata
- [x] 01-02-PLAN.md — Add physical output modules and sink routing
- [x] 01-03-PLAN.md — Verify patch persistence and backward compatibility
- [x] 01-04-PLAN.md — Refine copy/paste and selection buffer workflows
- [x] 01-05-PLAN.md — Group selected modules into sub-patches with AutoMap wiring
- [x] 01-06-PLAN.md — Add XInput force-feedback dispatch via raw extern system link
- [x] 01-07-PLAN.md — Cross-boundary copy/paste with app-level clipboard and AutoMap bridge
- [x] 01-08-PLAN.md — Establish minimal test infrastructure across device and UI crates
- [x] 01-09-PLAN.md — Fix Selector/Split value pin types to Any; add AutoMapFork and AutoMapSelector modules

## Phase 2: Reliability, diagnostics, and test coverage

**Goal:** Improve runtime robustness with diagnostic feedback, graceful fallback behavior, and initial automated test coverage for core engine and device subsystems.

**Requirements:** [F7, F8, NFR1, NFR2, NFR3]

**Plans:** 0 plans
- [ ] 02-01-PLAN.md — Add diagnostics, error handling, and initial test suite

## Phase 3: UX polish and advanced mapping features

**Goal:** Polish the visual editor, improve workflow for patch creation, and extend mapping usability for complex controller layouts.

**Requirements:** [F4, F5, F6, F8, NFR2]

**Plans:** 0 plans
- [ ] 03-01-PLAN.md — Refine UI workflows, patch loading, and advanced mapping support


