# FlexInput Architecture Blueprint

## Overview

FlexInput is a node-based HID/MIDI input routing and mapping application for Windows. It connects physical controllers (gamepads, MIDI devices) to virtual outputs (Xbox 360/XInput, DualShock, keyboard/mouse) through a visual signal graph with real-time processing capabilities.

**Key Characteristics:**
- Three-thread architecture: UI, Processing, I/O
- Node-based signal graph using egui-snarl
- Real-time device polling at up to 4 kHz
- Multi-platform virtual output support via HIDMaestro driver
- Network input routing (LAN/PSK/P2P)
- Sub-patch system for reusable complex blocks

---

## Workspace Structure

```
FlexInput/
├── app/                          # Main application binary
│   ├── src/main.rs               # Entry point, GPU recovery logic
│   └── assets/                   # Static resources (models, icons)
├── crates/
│   ├── core/                     # Core types: Signal, Module, Patch
│   ├── engine/                   # Processing thread, graph evaluation
│   ├── devices/                  # Physical device backends (gilrs, SDL3, MIDI)
│   ├── virtual/                  # Virtual output device abstractions
│   ├── hidmaestro/               # HIDMaestro driver integration
│   ├── net/                      # Network transport layer
│   ├── modules/                  # Signal processing modules
│   └── ui/                       # egui-based user interface
└── vendor/                       # Vendored dependencies (egui-snarl, egui-wgpu)
```

---

## Core Data Types (`crates/core`)

### Signal System

**`Signal` enum** - The fundamental data type flowing through the graph:
```rust
pub enum Signal {
    Float(f32),   // Analog values (-1.0 to 1.0 typically)
    Bool(bool),   // Digital on/off
    Vec2(Vec2),   // 2D vectors (sticks, touchpad)
    Vec4(Vec4),   // Quaternions (gyro orientation)
    Int(i32),     // Integer values
}
```

**`SignalType` enum** - Type system for pins and wires:
- `Float`, `Bool`, `Vec2`, `Int`, `Vec4`, `Any`, `AutoMap`
- Type coercion rules defined in `Signal::coerce_to()`
- Wire validation via `SignalType::accepts(incoming)`

### Module System

**`ModuleDescriptor`** - Static module metadata:
```rust
pub struct ModuleDescriptor {
    pub id: &'static str,           // Stable dot-namespaced ID (e.g., "math.multiply")
    pub display_name: &'static str,
    pub category: &'static str,     // "Math", "Logic", "Processing", etc.
    pub inputs: Vec<PinDescriptor>,
    pub outputs: Vec<PinDescriptor>,
}
```

**`Module` trait** - Runtime module interface:
```rust
pub trait Module: Send + 'static {
    fn descriptor() -> ModuleDescriptor;
    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]>;
    fn has_overlay_widget(&self) -> bool { false }
}
```

**`ModuleFactory`** - Type-erased constructor: `fn() -> Box<dyn Module>`

### Patch System

**`Patch`** - Serialized graph document (`.fxp` files):
```rust
pub struct Patch {
    pub version: u32,               // PATCH_VERSION = 1
    pub nodes: Vec<NodeInstance>,
    pub wires: Vec<Wire>,
}
```

**`NodeInstance`** - Runtime node with parameters:
- `id: Uuid` - Unique identifier
- `module_id: String` - Matches `ModuleDescriptor::id` or special types ("subpatch", "device.source", "device.sink")
- `position: [f32; 2]` - Canvas coordinates
- `params: HashMap<String, serde_json::Value>` - Configurable parameters
- `subpatch: Option<Box<SubPatch>>` - Inline sub-patch definition

**`Wire`** - Connection between nodes:
```rust
pub struct Wire {
    pub from_node: Uuid,
    pub from_pin: String,
    pub to_node: Uuid,
    pub to_pin: String,
}
```

---

## Engine Architecture (`crates/engine`)

### Three-Thread Design

| Thread | Role | Frequency |
|--------|------|-----------|
| **UI** | egui event loop, canvas rendering | Monitor refresh rate (60-144 Hz) |
| **Processing** | Signal graph evaluation | Configurable (500 Hz - 2 kHz) |
| **I/O** | Device polling, output dispatch | Configurable (500 Hz - 4 kHz) |

### Processing Graph (`ProcessingGraph`)

Topologically-sorted snapshot rebuilt every frame by the UI thread:
```rust
pub struct ProcessingGraph {
    pub nodes: Vec<NodeSnap>,
}

pub struct NodeSnap {
    pub node_uid: usize,              // egui_snarl::NodeId.0
    pub module_id: String,
    pub params: HashMap<String, Value>,
    pub n_outputs: usize,
    pub input_sources: Vec<Option<(usize, usize)>>,  // (source_node_idx, output_pin)
    pub device_id: Option<String>,
    pub output_pin_ids: Vec<String>,
    pub sink_target: Option<SinkTarget>,
    pub inline_subgraph: Option<Box<InlineSubgraph>>,
}

pub struct SinkTarget {
    pub device_id: String,
    pub pin_ids: Vec<String>,
    pub multi_sources: Vec<Vec<(usize, usize)>>,
    pub automap_source: Option<(String, Vec<String>)>,
    pub feedback_sources: Vec<FeedbackSource>,
    pub is_self_sink: bool,
    pub digital_trigger_bridge: bool,
}
```

### Graph Evaluation (`eval_graph_tick`)

**Main evaluation loop:**
1. **Preprocess device signals** - Apply deadzone + gyro multiplier to source nodes
2. **Apply source-block suppression** - Zero pointer pins when Virtual Menu is open
3. **Iterate through all nodes** in topological order:
   - Handle special node types (AutoMap Collector, Remapper, Touch Zones, Menu)
   - Evaluate inline sub-patches recursively
   - Dispatch to `compute_node()` for standard modules
4. **Post-pass operations:**
   - Self-sink feedback routing
   - Reverse-feedback through AutoMap Selectors
   - Feedback Control injection drain
   - Network Receive feedback aggregation

**Node evaluation dispatch (`compute_node`):**
- Pattern match on `module_id`
- Special handling for device.source, automap_split, menu, touch_zones, macro
- Pure function evaluation via `eval_pure()` for math/logic modules
- Stateful computation for generators (oscillator, envelope), filters (delay, average, DC filter)

### Module Registry (`eval_hooks`)

Phase C architecture: modules can register custom publish hooks that override default behavior. Migrated modules include Audio Stream Haptics and Network Send/Receive.

---

## Module System (`crates/modules`)

### Module Categories

| Category | Modules |
|----------|---------|
| **Utility** | Constant, Switch, Knob, Selector, Dropdown, Text, SVG, Split, Sub-patch |
| **Math** | Add, Subtract, Multiply, Divide, Clamp, Abs, Negate, Map Range |
| **Logic** | AND, OR, NOT, XOR, Equal, NotEqual, GreaterThan, LessThan, Has Changed, Logic Delay, Counter |
| **Processing** | Delay, Average, DC Filter, Response Curve, Vec Response Curve, Vec Reshaper, Two-way Response Curve, Gyro 3DOF |
| **Display** | Readout, Oscilloscope, Trigger Scope, Vectorscope |
| **Generator** | Oscillator, Envelope |
| **AutoMap** | AutoMap Splitter, Collector, Fork, Selector, Combiner, Touch Zones, Remapper, Map Action, Feedback Control, Audio Stream Haptics |
| **SubPatch** | Inlet, Outlet |
| **Network** | Network Send, Network Receive |

### Module Registration Pattern

```rust
// In crates/modules/src/math.rs
pub fn registrations() -> Vec<ModuleRegistration> {
    vec![
        ModuleRegistration {
            descriptor: ModuleDescriptor {
                id: "math.add",
                display_name: "Add",
                category: "Math",
                inputs: vec![PinDescriptor::new("A", SignalType::Any), PinDescriptor::new("B", SignalType::Any)],
                outputs: vec![PinDescriptor::new("Out", SignalType::Float)],
            },
            factory: || Box::<AddModule>::default(),
        },
        // ... more modules
    ]
}
```

### Stateful Modules

Modules maintain state in `NodeState`:
- `aux_f32: Vec<f32>` - Auxiliary floating-point state (used for counters, phase, etc.)
- `prev_signals: Vec<Option<Signal>>` - Previous frame inputs (change detection)
- `delay_bufs: Vec<VecDeque<(Instant, f32)>>` - Delay line buffers
- `avg_bufs: Vec<VecDeque<f32>>` - Averaging buffers

---

## UI Architecture (`crates/ui`)

### Main Application Structure

**`FlexInputApp`** - Central state holder (~4600 lines in app.rs):
- Engine instance and processing thread communication
- Tab management (multi-patch support)
- Device pools (physical + virtual)
- Settings and persistence
- Overlay windows (info, menu, config)

### Canvas System (`canvas/mod.rs`)

**`Canvas`** - egui-snarl wrapper:
- Manages `Snarl<NodeData>` for visual graph editing
- Undo/redo stack (50 levels max)
- Clipboard operations with cross-boundary paste support
- Mutation tracking via `mutation_gen` counter
- View salt for independent pan/zoom per canvas

**`NodeData`** - UI-specific node metadata:
```rust
pub struct NodeData {
    pub module_id: String,
    pub display_name: String,
    pub category: String,
    pub inputs: Vec<PinDescriptor>,
    pub outputs: Vec<PinDescriptor>,
    pub params: HashMap<String, Value>,
    pub subpatch: Option<Box<UiSubPatch>>,
    pub extra: NodeExtra,  // Live signal data, scope samples, etc.
}
```

### Sub-patch Editors

Nested graph editing with recursive evaluation:
- Each sub-patch has its own `Canvas` instance
- Inline subgraphs evaluated via `eval_subgraph()`
- Namespaced UIDs prevent collisions in shared state map
- Outlet/Inlet nodes bridge outer and inner graphs

### Overlay Windows

Three transparent viewport layers:
1. **Info Overlay** - Device status, signal readouts (always-on-top)
2. **Virtual Menu Overlay** - Gamepad navigation UI for menus
3. **Config Overlay (M3)** - Live parameter tweaking overlay

All use `WS_EX_TRANSPARENT` + `WS_EX_LAYERED` for see-through effect on Windows.

### Easy Mode vs Advanced Mode

**Easy Mode:**
- Simplified preset-driven workflow
- Left panel: device picker, right panel: sub-patch body
- No direct graph editing
- Pinned widgets for parameter access

**Advanced Mode:**
- Full node-based graph editor
- Side panels for virtual/physical devices
- Sub-patch editor windows
- All features available

---

## Device Handling (`crates/devices`, `crates/virtual`)

### Physical Devices

**Backend abstraction:**
```rust
pub trait DeviceBackend: Send {
    fn poll(&mut self) -> Vec<PhysicalDevice>;
    fn name(&self) -> &str;
}
```

**Supported backends:**
- **gilrs** - XInput, DualShock 4, DualSense, Switch Pro (primary)
- **SDL3** - Third-party controllers with special features (gyro, extra buttons)
- **MIDI** - Per-CC output pins with CC Learn

**Device identification:**
- Format: `{backend}:{family}:{instance}` (e.g., `gilrs:dualsense:0`)
- VID/PID pairs for cross-platform matching
- Semantic groups for cross-family button mapping

### Virtual Devices

**`VirtualDevice` trait:**
```rust
pub trait VirtualDevice: Send {
    fn id(&self) -> &str;
    fn kind(&self) -> DeviceKind;
    fn is_connected(&self) -> bool;
    fn send(&mut self, signals: &HashMap<String, Signal>);
    fn reset_outputs(&mut self);
}
```

**Device kinds:**
- `XInput` - Virtual Xbox 360 controller via HIDMaestro
- `DS4` - Virtual DualShock 4
- `DualSense` - Virtual DualSense
- `KeyMouse` - Virtual keyboard/mouse

### HIDMaestro Integration (`crates/hidmaestro`)

Pure-Rust UMDF2 client for virtual device creation:
- Helper binary runs elevated (driver installation)
- Shared memory communication channel
- Device lifecycle management (create/destroy/persist)
- Rumble feedback shaping (floor/max/exp parameters)

---

## Network Subsystem (`crates/net`)

### Transport Tiers

| Tier | Protocol | Encryption | Use Case |
|------|----------|------------|----------|
| **LAN** | UDP | None | Same network, trusted |
| **PSK** | UDP | ChaCha20-Poly1305 | Internet, shared passphrase |
| **P2P** | iroh | Cryptographic keypair | NAT traversal |

### Frame Format

```rust
pub struct NetworkFrame {
    pub timestamp: f64,           // Server time in seconds
    pub signals: HashMap<String, Signal>,  // All AutoMap pins
    pub haptics: HapticFeedback,  // Bidirectional rumble/lightbar
}
```

### AutoMap Bus

Carries complete gamepad state between instances:
- All canonical pins (sticks, buttons, triggers, gyro)
- Virtual KB/M keys
- Haptic feedback signals (rumble, lightbar) flow backward along same wire

---

## AutoMap System (`crates/core/src/automap.rs`)

### Canonical Pin Definitions

**`ALL_PINS`** - Complete list of auto-mappable signals:
- Gamepad sticks (Vec2 + individual axes)
- Triggers (analog + digital)
- Buttons (positional: south/east/west/north, LB/RB, LT/RT dig)
- D-Pad (Vec2 + individual axes + boolean directions)
- Gyro/Accelerometer (3DOF each)
- Touchpad coordinates and active state
- Extra buttons (paddles L1-L4, misc M1-M6)
- Virtual KB/M keys and mouse

**`FEEDBACK_INLET_PINS`** - Haptic input targets:
- Classic rumble (strong/weak)
- Light bar (R/G/B)
- HD rumble carrier 1 & 2 (L/R amplitude/frequency)
- DualSense HD haptics
- Adaptive triggers (mode/start/end/strength/freq)

### Cross-Family Mapping

**`resolve_mapping(src_pins, dst_pins)`** - Three-pass algorithm:
1. **Direct ID match** - Same pin name on both devices
2. **Semantic group fan-out** - Bridge unique buttons (capture↔mute)
3. **Digital-analog trigger bridge** - Digital triggers drive analog when no real analog exists

**`FEEDBACK_PAIRS`** - Reverse-flow mapping for haptics:
- Maps virtual output pins to physical input pins
- Enables bidirectional rumble/lightbar without explicit wiring

---

## Signal Processing Pipeline

### Evaluation Flow

```
Device Polling (I/O Thread)
    ↓
dev_sigs: HashMap<(device_id, pin_id), Signal>
    ↓
eval_graph_tick() (Processing Thread)
    ├── preprocess_dev_sigs() - Deadzone, gyro mult
    ├── Source-block suppression (Virtual Menu)
    ├── Main node loop
    │   ├── Device source nodes → read dev_sigs
    │   ├── AutoMap nodes → inject/collect signals
    │   ├── Module evaluation → compute_node()
    │   └── Sub-patch recursion
    ├── Post-pass: self-sink feedback
    ├── Post-pass: reverse-feedback routing
    ├── Post-pass: Feedback Control injection
    └── Post-pass: Network Receive aggregation
    ↓
sink_outputs: HashMap<(device_id, pin_id), Signal>
    ↓
Virtual Device Send (I/O Thread)
    ↓
Hardware Output
```

### Type Coercion Rules

- `Bool` ↔ `Float` (0/1 mapping)
- `Int` → `Float` (numeric cast)
- `Vec4` → `Float` (x component only)
- `Any` accepts all types
- `AutoMap` only connects to `AutoMap`

### Signal Combination

Multi-source sinks combine signals additively:
```rust
fn combine_signals(a: Signal, b: Signal) -> Signal {
    match (a, b) {
        (Signal::Float(x), Signal::Float(y)) => Signal::Float(x + y),
        (Signal::Vec2(x), Signal::Vec2(y)) => Signal::Vec2(x + y),
        _ => a,  // Fallback: first wins
    }
}
```

---

## Configuration & Persistence

### Settings (`AppSettings`)

Persisted to `%APPDATA%\FlexInput\settings.json`:
- Polling rate, sample rate
- Theme, contrast, see-through settings
- Device defaults (deadzone, gyro multiplier)
- Rumble shaping parameters
- Hotkey bindings (panic, pin, overlay, config overlay)
- UI mode (Easy/Advanced)
- Auto-switch, bypass preferences

### Patch Files

**`.fxp`** - Full patch with graph, positions, params:
```json
{
  "version": 1,
  "nodes": [...],
  "wires": [...]
}
```

**`.fxsp`** - Sub-patch preset (Easy mode):
- Single sub-patch node with AutoMap inlet/outlet
- Factory presets in `app/assets/sub-patches/`

**`.fxc`** - Response curve files (reusable across patches)

### Crash Recovery

Automatic snapshot on GPU loss or crash:
- Saved to `%APPDATA%\FlexInput\recovery.json`
- Restored on next launch if present
- Deleted after successful recovery

---

## Performance Considerations

### Real-Time Constraints

**Processing thread:**
- 500 µs budget at 2 kHz
- Catchup ticks recover from UI frame hiccups (max 16 ticks)
- `arc-swap` for lock-free graph publishing

**I/O thread:**
- Up to 4 kHz device polling
- `try_lock()` on processing output mutex to avoid blocking

### Optimization Patterns

1. **Profile scopes** - `puffin::profile_scope!()` throughout for CPU profiling
2. **Early-outs** - Skip expensive operations when no relevant nodes exist
3. **State reuse** - `TickOutput` cleared but not dropped between ticks
4. **ArcSwap** - Refcount bump instead of cloning large structures
5. **Debouncing** - HidHide reconcile, recovery snapshot writes

### Debug Build Performance

Compromise in `Cargo.toml`:
```toml
[profile.dev]
opt-level = 1

[profile.dev.package."*"]
opt-level = 3
```
- Own crates: opt-level 1 (fast enough for real-time)
- Dependencies: opt-level 3 (egui/wgpu/glam never profiled)

---

## Key Design Decisions

### Why HIDMaestro Over ViGEm?

- Pure-Rust UMDF2 client (no C++ dependency)
- Better performance and reliability
- Supports advanced features (HD rumble, adaptive triggers)
- Self-installing driver via elevated helper binary

### Why Three Threads?

- **UI thread** - egui event loop, cannot be blocked by device I/O
- **Processing thread** - Real-time graph evaluation at fixed rate
- **I/O thread** - Device polling and output dispatch, independent timing

### Why ArcSwap for Graph Publishing?

- UI thread publishes fresh graph every frame
- Processing thread reads via lock-free `load()` (refcount bump only)
- Avoids RwLock contention at high frequencies

### Sub-Patch Evaluation Strategy

- Recursive `eval_subgraph()` with namespaced UIDs
- Inlet/Outlet nodes bridge outer and inner graphs
- AutoMap collectors/remappers inside sub-patches use namespaced keys
- Prevents collisions when multiple sub-patches share inner node indices

### GPU Loss Recovery

- Vendored egui-wgpu raises `GPU_LOST` flag instead of panicking
- App saves recovery snapshot and relaunches itself
- Input/engine threads continue running (don't need GPU)
- Virtual devices persist via HIDMaestro helper across relaunch

---

## Testing Strategy

### Unit Tests

- `crates/core/src/automap.rs` - Cross-family mapping logic
- `crates/engine/src/eval/modules/lean.rs` - 3DOF lean mappings
- `crates/ui/tests/` - Patch compatibility, output routing

### Integration Tests

- Network loopback (`crates/net/tests/loopback.rs`)
- HIDMaestro live device tests (`crates/virtual/tests/hidmaestro_live.rs`)

---

## Common Patterns & Conventions

### Module ID Naming

- **Categories**: `math.*`, `logic.*`, `processing.*`, `display.*`, `generator.*`
- **Device nodes**: `device.source`, `device.sink`
- **AutoMap**: `module.automap_*` (split, collect, fork, selector, combiner)
- **Sub-patch**: `subpatch.inlet`, `subpatch.outlet`

### Parameter Naming

- Snake_case for JSON keys
- Descriptive names: `rumble_floor`, `deadzone`, `mouse_sensitivity`
- Optional params use `#[serde(default)]`

### Signal Flow

- Inputs: `[Option<Signal>]` (None = unwired)
- Outputs: `SmallVec<[Signal; 4]>` (bounded for stack allocation)
- State: `NodeState` with `aux_f32`, `prev_signals`, specialized buffers

### Error Handling

- Best-effort operations in panic hooks (crash logging must not panic)
- Graceful degradation (missing devices, stale signals)
- Explicit error propagation for critical paths (device creation, network send)

---

## Future Architecture Notes

### Phase C: Module Registry Seam

Modules can register custom publish hooks for specialized evaluation (e.g., Audio Stream Haptics, Network Send/Receive). This allows migrating complex modules out of the hardcoded dispatch without breaking changes.

### Virtual Menu System

Open menus suppress physical input to prevent game interference while allowing menu navigation. Uses `UiSourceBlock` channel from UI to processing thread.

### Config Overlay (M3)

Live parameter tweaking overlay summoned by keyboard chord or Guide button. Suppresses specific pins based on focused tweak-pin context.

---

## References

- [README.md](../README.md) - User-facing documentation
- [CHANGELOG.md](../CHANGELOG.md) - Version history
- `vendor/egui-snarl/src/lib.rs` - Graph UI implementation
- `vendor/egui-wgpu/src/renderer.rs` - GPU loss handling

---

*Last updated: 2026-07-25*  
*Blueprint version: 1.0*
