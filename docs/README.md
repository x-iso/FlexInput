# FlexInput Documentation Blueprint

## Overview

This directory contains comprehensive reference documentation for the FlexInput codebase, designed to serve as a single source of truth for developers (human or LLM) working on the project. The documents cover architecture, subsystems, conventions, and troubleshooting guidance.

**Purpose:** Enable accurate understanding of how things work without making false assumptions from incomplete context.

---

## Document Index

### Core Architecture

| Document | Description | Key Topics |
|----------|-------------|------------|
| [ARCHITECTURE_BLUEPRINT.md](./ARCHITECTURE_BLUEPRINT.md) | High-level system overview | Three-thread design, crate structure, signal flow, key decisions |
| [ENGINE_INTERNALS.md](./ENGINE_INTERNALS.md) | Processing thread deep dive | Graph evaluation pipeline, sub-patch recursion, state management |
| [AUTOMAP_SYSTEM.md](./AUTOMAP_SYSTEM.md) | Cross-device mapping system | Canonical pins, resolve_mapping, feedback routing, sink resolution |

### Module System

| Document | Description | Key Topics |
|----------|-------------|------------|
| [MODULES_REFERENCE.md](./MODULES_REFERENCE.md) | All 10 module categories | Registration pattern, evaluation flow, adding new modules |

### User Interface

| Document | Description | Key Topics |
|----------|-------------|------------|
| [UI_ARCHITECTURE.md](./UI_ARCHITECTURE.md) | egui-based UI system | Canvas, overlays, Easy/Advanced modes, gamepad nav, settings |

### Device Handling

| Document | Description | Key Topics |
|----------|-------------|------------|
| [DEVICES_REFERENCE.md](./DEVICES_REFERENCE.md) | Physical + virtual devices | Backends (gilrs, SDL3, MIDI), HIDMaestro, calibration, polling thread |

### Network Subsystem

| Document | Description | Key Topics |
|----------|-------------|------------|
| [NETWORK_REFERENCE.md](./NETWORK_REFERENCE.md) | Multi-instance routing | Transport tiers (LAN/PSK/P2P), frame format, haptic feedback |

### Data Formats

| Document | Description | Key Topics |
|----------|-------------|------------|
| [PATCH_FORMATS.md](./PATCH_FORMATS.md) | Persistence formats | .fxp, .fxsp, .fxc, workspace.json, recovery.json, migration |

### Development Practices

| Document | Description | Key Topics |
|----------|-------------|------------|
| [DEVELOPMENT_GUIDELINES.md](./DEVELOPMENT_GUIDELINES.md) | Conventions & best practices | Coding standards, thread safety, testing, debugging, git workflow |

---

## Quick Reference Guides

### "I need to understand how X works"

| Question | Document | Section |
|----------|----------|---------|
| How does the graph get evaluated? | ENGINE_INTERNALS.md | Graph Evaluation Pipeline |
| How are devices mapped across families? | AUTOMAP_SYSTEM.md | Cross-Family Mapping |
| How do I add a new module? | MODULES_REFERENCE.md | Adding New Modules |
| How does the UI communicate with the engine? | UI_ARCHITECTURE.md | Thread Communication |
| How are patches saved/loaded? | PATCH_FORMATS.md | Patch File Format (.fxp) |
| How do I debug thread issues? | DEVELOPMENT_GUIDELINES.md | Debugging Techniques |

### "I'm working on feature X"

| Feature Area | Primary Document | Secondary References |
|--------------|------------------|---------------------|
| New signal processing module | MODULES_REFERENCE.md | ENGINE_INTERNALS.md (compute_node) |
| UI widget or panel | UI_ARCHITECTURE.md | canvas/viewer/*.rs files |
| Device backend integration | DEVICES_REFERENCE.md | crates/devices/src/*.rs |
| Network transport addition | NETWORK_REFERENCE.md | crates/net/src/transport/*.rs |
| Patch format migration | PATCH_FORMATS.md | migrate_loaded_snarl() in canvas/mod.rs |
| Performance optimization | ENGINE_INTERNALS.md + UI_ARCHITECTURE.md | puffin profiling section |

### "Something is broken"

| Symptom | Likely Document | Key Sections |
|---------|-----------------|--------------|
| Signals not flowing through graph | ENGINE_INTERNALS.md | Post-passes, collector_sigs injection |
| Cross-device mapping wrong | AUTOMAP_SYSTEM.md | resolve_mapping three-pass algorithm |
| Sub-patch evaluation incorrect | ENGINE_INTERNALS.md | eval_subgraph, namespaced UIDs |
| Device not appearing in UI | DEVICES_REFERENCE.md | Backend polling, device identification |
| Network latency or packet loss | NETWORK_REFERENCE.md | Staleness detection, reconnection logic |
| Patch won't load after update | PATCH_FORMATS.md | Migration functions, version field |
| GPU crash or black screen | UI_ARCHITECTURE.md | GPU Loss Recovery section |

---

## Architecture at a Glance

```
┌─────────────────────────────────────────────────────────────────┐
│                        USER INTERFACE                           │
│  ┌─────────────┐  ┌──────────────┐  ┌─────────────────────────┐ │
│  │   Canvas    │  │  Overlays    │  │  Device Panels          │ │
│  │  (Snarl)    │  │  Info/Menu/  │  │  Virtual + Physical     │ │
│  │             │  │  Config      │  │                         │ │
│  └──────┬──────┘  └──────────────┘  └────────────┬────────────┘ │
│         │                                        │              │
│         ▼                                        ▼              │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │                   FlexInputApp                          │   │
│  │  • Tab management (multi-patch)                         │   │
│  │  • Settings persistence                                 │   │
│  │  • Gamepad navigation                                   │   │
│  └──────────────────────────┬──────────────────────────────┘   │
│                             │                                   │
└─────────────────────────────┼───────────────────────────────────┘
                              │ ArcSwap (lock-free publish)
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                     PROCESSING THREAD                           │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │              eval_graph_tick()                            │  │
│  │  Phase 1: Preprocess (deadzone, gyro, source-block)       │  │
│  │  Phase 2: Main loop (compute_node, sub-patch recursion)   │  │
│  │  Phase 3: Post-passes (self-sink, feedback, network)      │  │
│  └──────────────────────────┬───────────────────────────────┘  │
│                             │                                   │
│                             ▼                                   │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │              Module Evaluation                            │  │
│  │  • Pure modules (eval_pure)                               │  │
│  │  • Stateful modules (NodeState)                           │  │
│  │  • AutoMap nodes (split, collect, remapper, etc.)         │  │
│  └──────────────────────────┬───────────────────────────────┘  │
└─────────────────────────────┼───────────────────────────────────┘
                              │ SinkBus (RwLock)
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                         I/O THREAD                              │
│  ┌─────────────────┐  ┌─────────────────┐  ┌───────────────┐  │
│  │ Physical Devices│  │ Virtual Devices │  │ Network Send/ │  │
│  │ (gilrs, SDL3,   │  │ (HIDMaestro,    │  │ Receive       │  │
│  │  MIDI)           │  │  KeyMouse)      │  │               │  │
│  └────────┬────────┘  └────────┬────────┘  └───────┬───────┘  │
│           │                    │                    │          │
│           ▼                    ▼                    ▼          │
│  ┌─────────────────────────────────────────────────────────┐  │
│  │              Device Polling Loop                        │  │
│  │  • Read hardware → dev_sigs map                         │  │
│  │  • Write computed signals → virtual devices             │  │
│  │  • Apply bypass/suppress modes                          │  │
│  └─────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

---

## Key Design Decisions (Summary)

### Why Three Threads?

| Thread | Frequency | Responsibility |
|--------|-----------|----------------|
| UI | 60-144 Hz | egui rendering, user input, graph building |
| Processing | 500-2000 Hz | Signal graph evaluation, module computation |
| I/O | 500-4000 Hz | Device polling, output dispatch, network send/recv |

**Rationale:** UI cannot be blocked by device I/O latency. Processing must run at fixed rate independent of frame timing. I/O operates at hardware polling rates.

### Why ArcSwap for Graph Publishing?

- Processing thread reads every tick (500-2000 Hz)
- UI thread writes every frame (~60-144 Hz)
- ArcSwap provides lock-free reads with only a refcount bump
- RwLock would cause contention at high processing rates

### Why Namespaced UIDs for Sub-patches?

- Multiple sub-patches can share inner node indices (e.g., both have node 0)
- Without namespacing, state maps would collide across nesting levels
- Splitmix64-style finalizer ensures collision-free mapping

### Why Three-Pass AutoMap Resolution?

1. **Direct ID match** handles the common case (positional naming works for most buttons)
2. **Semantic group fan-out** bridges truly unique cross-device aliases (capture↔mute)
3. **Digital-analog trigger bridge** ensures digital-only pads can drive analog destinations

### Why Post-Pass Architecture?

- Self-sink routing requires computed[] to be filled first
- Feedback injection drain needs all injectors to have published
- Network Receive aggregation must run after all feedback sources are resolved
- Macro namespace snapshot preserves one-tick-stale state for readers

---

## File Map by Concern

### Core Types (crates/core)
```
signal.rs      → Signal, SignalType enums + coercion rules
module.rs      → Module trait, ModuleDescriptor, PinDescriptor
patch.rs       → Patch, NodeInstance, Wire, SubPatch structs
automap.rs     → ALL_PINS, FEEDBACK_PAIRS, resolve_mapping()
macros.rs      → Macro namespace constants and helpers
menu.rs        → Menu pin parsing (Pin::Open, Pin::Hover, etc.)
touchzones.rs  → Touch zone pin parsing (Pin::Zone, Pin::Click)
```

### Engine (crates/engine)
```
lib.rs         → Engine struct, re-exports
graph.rs       → ProcessingGraph, NodeSnap, SinkTarget definitions
state.rs       → NodeState structure
thread.rs      → spawn_processing_thread, ArcSwap types
eval.rs        → eval_graph_tick main function (~1200 lines)
eval/compute.rs    → compute_node dispatch, eval_pure (~1300 lines)
eval/config.rs     → Curve configuration helpers
eval/curves.rs     → apply_curve, sample_curve, bias functions
eval/device_cal.rs → Deadzone/gyro calibration application
eval/publish.rs    → Module publish hooks (ASTH, network)
eval/registry.rs   → eval_hooks() registry for Phase C modules
eval/activation.rs → Switch module state machine
eval/modules/*.rs  → Specialized evaluators (lean, map_action, remapper, etc.)
```

### Modules (crates/modules)
```
lib.rs              → all_modules() registration aggregator
math.rs             → Add, Subtract, Multiply, Divide, Clamp, Abs, Negate, MapRange
logic.rs            → AND, OR, NOT, XOR, Equal, NotEqual, GreaterThan, LessThan, HasChanged, LogicDelay, Counter
processing.rs       → Delay, Average, DCFilter, ResponseCurve, VecResponseCurve, VecReshape, TwoWayResponseCurve, Gyro3DOF
display.rs          → Readout, Oscilloscope, TriggerScope, Vectorscope, Controller3D
generator.rs        → Oscillator, Envelope
network.rs          → NetworkSend, NetworkReceive module definitions
touch.rs            → Touch Zones module (ports + mapping modes)
menu.rs             → Virtual Menu module
macro_module.rs     → Macro Output module
subpatch.rs         → Inlet, Outlet module definitions
input_viewer.rs     → Input Viewer display module
util.rs             → Constant, Switch, Knob, Selector, Dropdown, Text, SVG, Split modules
```

### UI (crates/ui)
```
lib.rs              → log_crash(), relaunch_self_and_exit() helpers
app.rs              → FlexInputApp struct + eframe::App impl (~4600 lines)
app/bind_window.rs      → Bind-to-process picker window
app/chrome.rs           → Custom title bar rendering
app/devices_pool.rs     → SharedDevicePool reconciliation
app/graph.rs            → build_processing_graph(), apply_display_state()
app/hidhide_ui.rs       → HidHide configuration window
app/nav/*.rs              → Navigation panel components (config, curves, fields, etc.)
app/persistence.rs        → Workspace save/load
app/settings_window.rs    → Settings dialog
app/subpatch.rs           → Sub-patch editor window management
app/threads.rs            → spawn_io_thread(), spawn_processing_thread() wrappers
canvas/mod.rs         → Canvas struct + operations (~3200 lines)
canvas/node.rs        → NodeData, UiSubPatch definitions
canvas/viewer/*.rs    → Per-module-type node viewers
easy/*.rs             → Easy mode layout and wiring
panels/*.rs           → Device panel renderers
settings.rs           → AppSettings struct + persistence
overlay.rs            → Info overlay viewport management
menu_overlay.rs       → Virtual menu overlay viewport
config_overlay.rs     → Config overlay (M3) viewport
gamepad_nav.rs        → Game controller UI navigation (~800 lines)
guide_watcher.rs      → Guide button monitoring thread
process_list.rs       → Foreground detection, HWND management
device_ops.rs         → Background device lifecycle worker
panic_hotkey.rs       → Global panic shortcut listener
pin_hotkey.rs         → Pin/always-on-top hotkey listener
model/*.rs            → 3D controller model loading and rendering
widgets/mod.rs        → Reusable UI widgets
```

### Devices (crates/devices)
```
lib.rs              → DeviceBackend trait, init_backends()
gilrs_backend.rs    → gilrs gamepad polling implementation
sdl_backend.rs      → SDL3 motion sensor access
midi.rs             → MIDI input/output handling
layouts.rs          → Pin definitions per controller kind (~500 lines)
hidhide.rs          → HidHide client wrapper
identification.rs   → Device identification and matching
gyro.rs             → Gyroscope data processing
gamepad.rs          → Gamepad-specific utilities
dualsense_haptic.rs → DualSense haptic feedback handling
haptic_pcm.rs       → PCM haptic sample playback
loopback_haptic.rs  → Loopback haptic routing
loopback_manager.rs → Loopback device lifecycle
spectrum.rs         → Audio spectrum analysis (for ASTH)
```

### Virtual Devices (crates/virtual)
```
lib.rs              → VirtualDevice trait definition
hidmaestro_device.rs    → XInput/DS4/DualSense implementations (~600 lines)
keymouse_hm.rs        → Keyboard/mouse emulation via HIDMaestro
layouts.rs            → Virtual device pin layouts
driver_availability.rs  → Check if HIDMaestro driver is installed
windows.rs            → Windows-specific utilities
```

### HIDMaestro (crates/hidmaestro)
```
lib.rs              → HIDMaestro client entry point
helper_ipc.rs       → Helper process communication protocol
deploy.rs           → Driver installation logic
descriptor.rs       → HID descriptor generation
encode.rs           → Report encoding for virtual devices
helper.rs           → Helper binary management
install.rs          → Driver installation routines
orchestrator.rs     → Multi-device lifecycle coordination
profile.rs          → Device profile management
server.rs           → IPC server implementation
shm.rs              → Shared memory communication channel
hidhide.rs          → HidHide integration
```

### Network (crates/net)
```
lib.rs              → Network module re-exports
frame.rs            → Frame serialization/deserialization
protocol.rs         → Protocol versioning and compatibility
manager.rs          → Connection management state machine
crypto.rs           → ChaCha20-Poly1305 encryption helpers
transport/mod.rs    → Transport trait definitions
transport/udp.rs    → LAN transport implementation
transport/p2p.rs    → P2P (iroh) transport implementation
```

### App Binary (app/)
```
src/main.rs         → Entry point, GPU recovery logic, panic hooks
build.rs            → Build-time script (embed assets)
assets/             → Static resources (models, icons, SVGs)
```

---

## Line Count Summary

| Crate | Approximate Lines | Notes |
|-------|------------------|-------|
| `crates/core` | ~2,500 | Signal types, module trait, patch format, AutoMap definitions |
| `crates/engine` | ~6,500 | Graph evaluation, compute dispatch, curve math, publish hooks |
| `crates/modules` | ~4,000 | All 10 module categories with registration |
| `crates/ui` | ~25,000 | Largest crate; app.rs alone is ~4,600 lines |
| `crates/devices` | ~3,500 | Backend implementations and layout definitions |
| `crates/virtual` | ~1,800 | Virtual device traits and HIDMaestro integration |
| `crates/hidmaestro` | ~2,500 | Driver client, helper IPC, report encoding |
| `crates/net` | ~1,500 | Transport implementations and frame serialization |
| **Total** | **~47,300** | Excluding vendor/ and target/ directories |

**Largest files:**
- `crates/ui/src/app.rs` — ~4,600 lines (main application state + eframe::App impl)
- `crates/engine/src/eval.rs` — ~1,200 lines (graph evaluation pipeline)
- `crates/engine/src/eval/compute.rs` — ~1,300 lines (node dispatch + pure eval)
- `crates/ui/src/canvas/mod.rs` — ~3,200 lines (canvas operations)
- `crates/devices/src/layouts.rs` — ~500 lines (pin definitions per controller)

---

## How to Use This Documentation

### For Human Developers

1. **Start with ARCHITECTURE_BLUEPRINT.md** for the high-level picture
2. **Jump to the relevant subsystem document** when working on a specific area
3. **Use MODULES_REFERENCE.md** when adding or modifying signal processing modules
4. **Consult DEVELOPMENT_GUIDELINES.md** for coding conventions and best practices
5. **Reference PATCH_FORMATS.md** when dealing with serialization or migration

### For LLM-Assisted Development

When asked to fix a bug or add a feature:

1. **Read the relevant document first** to understand the system context
2. **Check the File Map by Concern section** to locate the exact source files
3. **Review the Key Design Decisions summary** to avoid repeating known patterns
4. **Consult Common Pitfalls & Solutions** in DEVELOPMENT_GUIDELINES.md before implementing
5. **Verify against the Quick Reference Guides** table for your specific task

### For New Contributors

1. Start with ARCHITECTURE_BLUEPRINT.md → ENGINE_INTERNALS.md → MODULES_REFERENCE.md
2. Read the corresponding source files mentioned in each document
3. Run `cargo test` to verify the test suite passes
4. Add tests for any new functionality before submitting PRs
5. Follow the Code Review Checklist in DEVELOPMENT_GUIDELINES.md

---

## Maintenance Notes

### When Updating Documentation

1. **Keep code examples in sync** with actual source (run `cargo doc` to verify)
2. **Update line counts** when files grow significantly (>10% change)
3. **Add new sections** for any new subsystems or major feature additions
4. **Remove outdated information** — mark deprecated patterns clearly

### When Adding New Modules

1. Update MODULES_REFERENCE.md with the new module's specification
2. Add to the appropriate category in crates/modules/src/
3. Register in all_modules() via the category's registrations() function
4. If using Phase C publish hook, document in ENGINE_INTERNALS.md → Module Registry Seam

### When Modifying AutoMap System

1. Update AUTOMAP_SYSTEM.md with any changes to resolve_mapping or FEEDBACK_PAIRS
2. Ensure ALL_PINS and FEEDBACK_INLET_PINS stay in sync (guarded by tests)
3. Document any new semantic groups or family-specific label mappings

### When Changing Patch Formats

1. Update PATCH_FORMATS.md with new schema details
2. Add migration logic in migrate_loaded_snarl() if breaking changes are introduced
3. Increment PATCH_VERSION constant and document the change in CHANGELOG.md

---

## References & Links

- **Source code:** [GitHub Repository](https://github.com/x-iso/FlexInput)
- **Issue tracker:** [GitHub Issues](https://github.com/x-iso/FlexInput/issues)
- **Discord community:** [FlexInput Discord](https://discord.gg/flexinput) (if available)
- **Vendor libraries:** `vendor/egui-snarl/`, `vendor/egui-wgpu/`, `vendor/egui-winit/`

---

*Documentation last updated: 2026-07-25*  
*Blueprint version: 1.0*  
*Maintained by: FlexInput development team*
