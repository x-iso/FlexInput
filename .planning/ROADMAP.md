# FlexInput Roadmap

## Phase 1: Core foundation and Windows device integration

**Goal:** Establish a stable real-time signal processing foundation, robust Windows device I/O, and patch persistence so FlexInput can reliably route physical and MIDI inputs to virtual outputs.

**Requirements:** [F1, F2, F3, F4, F5, F6, F7]

**Plans:** 8 plans
- [ ] 01-01-PLAN.md — Enumerate physical controller output pins and feedback metadata
- [ ] 01-02-PLAN.md — Add physical output modules and sink routing
- [ ] 01-03-PLAN.md — Verify patch persistence and backward compatibility
- [ ] 01-04-PLAN.md — Refine copy/paste and selection buffer workflows
- [ ] 01-05-PLAN.md — Group selected modules into sub-patches with AutoMap wiring
- [ ] 01-06-PLAN.md — Add XInput force-feedback dispatch via raw extern system link
- [ ] 01-07-PLAN.md — Cross-boundary copy/paste with app-level clipboard
- [ ] 01-08-PLAN.md — Establish minimal test infrastructure across device and UI crates

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


