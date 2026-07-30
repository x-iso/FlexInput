# FlexInput Development Guidelines & Conventions

## Overview

This document establishes coding conventions, architectural patterns, and best practices for developing within the FlexInput codebase. Following these guidelines ensures consistency, maintainability, and prevents common pitfalls that have emerged from the project's evolution.

---

## Coding Standards

### Rust Style

**Formatter:** Use `rustfmt` with default settings. Run before commits:
```bash
cargo fmt --all
```

**Linter:** Address all clippy warnings:
```bash
cargo clippy -- -D warnings
```

**Naming Conventions:**
- **Types/traits/enums:** `PascalCase` (e.g., `ProcessingGraph`, `Module`)
- **Functions/methods:** `snake_case` (e.g., `eval_graph_tick`, `compute_node`)
- **Constants:** `SCREAMING_SNAKE_CASE` (e.g., `PATCH_VERSION`, `DEFAULT_STALENESS_WINDOW`)
- **Variables:** `snake_case` (e.g., `device_id`, `node_state`)
- **Module files:** `snake_case.rs` (e.g., `signal.rs`, `compute.rs`)

### File Organization

**One public type per file when possible.** Exceptions:
- Small related types (< 50 lines) can share a file
- Private helper functions stay in the same file as their public API

**Module structure:**
```rust
// crates/engine/src/eval/compute.rs

//! Per-node computation dispatch and pure module evaluation.

use super::*;  // Import from parent eval module

// ── Public API ────────────────────────────────────────────────────────────────
pub fn compute_node(...) -> Vec<Option<Signal>> { ... }
pub fn eval_pure(...) -> Option<Signal> { ... }

// ── Private helpers ───────────────────────────────────────────────────────────
fn resolve_input_signal(...) -> Option<Signal> { ... }
```

### Documentation Comments

**Public API:** Use `///` doc comments with examples:
```rust
/// Evaluates one tick of the processing graph.
///
/// # Arguments
/// * `graph` - Topologically-sorted node graph snapshot
/// * `state` - Per-node mutable state (grows lazily); source-block suppression is
///             read from state[MACRO_CARRY_UID].source_block (not a param)
/// * `dev_sigs` - Raw device signals from I/O thread
/// * `dt` - Delta time in seconds since last tick
///
/// # Example
/// ```ignore
/// let mut out = TickOutput::default();
/// eval_graph_tick(&graph, &mut state, &dev_sigs, dt, &mut out);  // 5 args
/// ```
pub fn eval_graph_tick(...) { ... }
```

**Private/internal:** Use `//!` for module-level docs, `//` for inline comments:
```rust
// Post-pass 3: Feedback Control injection drain.
// Runs AFTER the main loop so every injector node has run first.
fn drain_feedback_injections(...) { ... }
```

### Error Handling

**Prefer `Result<T, E>` over panics.** Use thiserror for custom error types:
```rust
#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("HIDMaestro helper not running: {0}")]
    HelperNotRunning(String),
    
    #[error("Device creation failed: {0}")]
    CreationFailed(String),
}
```

**Use `?` operator for propagation:**
```rust
fn create_virtual_device(id: &str) -> Result<Box<dyn VirtualDevice>> {
    let helper = HelperClient::new()?;  // Propagate connection error
    let response = helper.send(Request::Create { device_id: id.into(), .. })?;  // Propagate IPC error
    
    if !response.success {
        return Err(DeviceError::CreationFailed(response.message));
    }
    
    Ok(Box::new(XInputDevice::new(id)))
}
```

**Avoid unwrap() in production code.** Use `expect()` with descriptive messages:
```rust
// Bad: silent panic with no context
let value = map.get("key").unwrap();

// Good: panic message explains what went wrong
let value = map.get("key").expect("Expected 'key' to be present in params");
```

---

## Architecture Patterns

### Thread Communication

**Rule:** Never hold locks across thread boundaries. Use message passing or atomic operations.

**Pattern 1: ArcSwap for read-heavy shared state:**
```rust
// UI thread publishes graph every frame
proc_graph.store(Arc::new(graph_snap));

// Processing thread reads lock-free every tick
let graph = proc_graph.load();
```

**Pattern 2: Mutex for complex invariants:**
```rust
// I/O thread writes device signals
{
    let mut bus = sink_bus.write().unwrap();
    bus.insert((device_id, pin), signal);
}

// Processing thread reads snapshot
let outputs = proc_outputs.lock().unwrap();
for (&(uid, pin), &sig) in &outputs.node_outputs { ... }
```

**Pattern 3: AtomicBool for simple flags:**
```rust
// UI thread sets bypass flag
io_bypass.store(true, Ordering::Relaxed);

// I/O thread checks flag each tick
if io_bypass.load(Ordering::Relaxed) {
    dev.reset_outputs();
}
```

### State Management

**Rule:** Keep state close to where it's used. Avoid global mutable state.

**Pattern: NodeState for per-node memory:**
```rust
pub struct NodeState {
    aux_f32: Vec<f32>,              // Module-specific floats
    prev_signals: Vec<Option<Signal>>,  // Previous frame inputs
    delay_bufs: Vec<VecDeque<(Instant, f32)>>,  // Delay line buffers
}

// Lazy growth to avoid per-tick allocation
while state.delay_bufs.len() < inputs.len() {
    state.delay_bufs.push(VecDeque::new());
}
```

**Pattern: HashMap keyed by namespaced UID for sub-patch isolation:**
```rust
let ns_uid = namespaced_uid(outer_uid, inner_node_idx);
let node_state = state.entry(ns_uid).or_insert_with(NodeState::default);
```

### Module Registration

**Rule:** Register modules in their category file. Keep `all_modules()` as a simple extension point.

```rust
// crates/modules/src/math.rs
pub fn registrations() -> Vec<ModuleRegistration> {
    vec![
        ModuleRegistration {
            descriptor: ModuleDescriptor {
                id: "math.add",
                display_name: "Add",
                category: "Math",
                inputs: vec![PinDescriptor::new("A", SignalType::Any)],
                outputs: vec![PinDescriptor::new("Out", SignalType::Float)],
            },
            factory: || Box::<AddModule>::default(),
        },
    ]
}

// crates/modules/src/lib.rs
pub fn all_modules() -> Vec<ModuleRegistration> {
    let mut modules = Vec::new();
    modules.extend(math::registrations());
    modules.extend(logic::registrations());
    // ... etc
    modules
}
```

### Phase C: Module Registry Seam

For modules needing custom publish behavior (e.g., ASTH, Network Send/Receive):

```rust
// crates/engine/src/eval/registry.rs
pub struct ModuleHook {
    pub publish: Option<fn(&NodeSnap, usize, &HashMap<...>, &mut HashMap<...>) -> Vec<Option<Signal>>>,
}

pub fn eval_hooks(module_id: &str) -> Option<ModuleHook> {
    match module_id {
        "module.audio_stream_haptics" => Some(ModuleHook {
            publish: Some(audio_stream_haptics_publish),
        }),
        _ => None,
    }
}

// In eval_graph_tick main loop:
if let Some(publish) = eval_hooks(&snap.module_id).and_then(|h| h.publish) {
    let out = publish(snap, idx, dev_sigs, &mut collector_sigs);
    last_outputs.insert(idx, out.clone());
    computed[idx] = out;
    continue;
}
```

---

## Performance Guidelines

### Real-Time Constraints

**Processing thread:** 500 µs budget at 2 kHz
- Avoid allocations in hot path (use `clear()` instead of drop/recreate)
- Use `SmallVec` for small fixed-size arrays
- Profile with puffin to identify bottlenecks

**I/O thread:** Up to 4 kHz device polling
- Batch signal writes to ArcSwap (don't write per-pin)
- Use `try_lock()` on shared mutexes to avoid blocking

### Memory Management

**Rule:** Prefer stack allocation for small, short-lived data.

```rust
// Good: SmallVec on stack
let outputs: SmallVec<[Signal; 4]> = vec![Some(sig1), Some(sig2)];

// Bad: Heap allocation for tiny vector
let outputs: Vec<Signal> = vec![Some(sig1), Some(sig2)];
```

**Rule:** Reuse allocations via `clear()` instead of drop/recreate.

```rust
// Good: Retain capacity
out.clear();  // HashMap/Vec clear but keep allocated memory
eval_graph_tick(..., &mut out);

// Bad: Allocate every tick
let mut out = TickOutput::default();  // Fresh allocation each iteration
```

### Thread Safety Checks

**Rule:** Use `#[cfg(test)]` for thread-safety assertions in tests.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_arc_swap_send_sync() {
        // Compile-time assertion that ArcSwap is Send + Sync
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ArcSwap<ProcessingGraph>>();
    }
}
```

---

## Testing Strategy

### Unit Tests

**Location:** `#[cfg(test)] mod tests { ... }` at bottom of source file, or separate `tests/` directory.

**Coverage targets:**
- Module evaluation functions (pure and stateful)
- Signal coercion rules
- Curve interpolation math
- AutoMap mapping resolution

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_signal_coercion() {
        assert_eq!(Signal::Float(0.5).coerce_to(SignalType::Bool), Some(Signal::Bool(true)));
        assert_eq!(Signal::Bool(false).coerce_to(SignalType::Float), Some(Signal::Float(0.0)));
        assert_eq!(Signal::Vec2(glam::Vec2::new(1.0, 2.0)).coerce_to(SignalType::Float), None);
    }
    
    #[test]
    fn test_curve_evaluation() {
        let points = vec![[-1.0, -1.0], [0.0, 0.0], [1.0, 1.0]];
        let biases = vec![0.0, 0.0];
        
        assert!((apply_curve(0.5, &points, &biases, false, -1.0, 1.0, -1.0, 1.0, 1.0) - 0.5).abs() < 0.01);
    }
}
```

### Integration Tests

**Location:** `crates/*/tests/` directory.

**Test scenarios:**
- Full graph evaluation with known inputs/outputs
- Sub-patch recursive evaluation
- Network loopback (send/receive same instance)
- Device polling → processing → output pipeline

```rust
// crates/engine/tests/graph_eval.rs
#[test]
fn test_simple_add_graph() {
    let mut graph = ProcessingGraph::default();
    graph.nodes.push(NodeSnap {
        module_id: "math.add".into(),
        params: HashMap::new(),
        n_outputs: 1,
        input_sources: vec![Some((0, 0)), Some((1, 0))],  // Connect to two constants
        ..Default::default()
    });
    
    let dev_sigs = HashMap::new();
    let mut state = HashMap::new();
    let mut out = TickOutput::default();
    
    eval_graph_tick(&graph, &mut state, &dev_sigs, 0.001, &mut out);  // 5 args
    
    assert_eq!(out.outputs[&(0, 0)], Some(Some(Signal::Float(3.0))));  // outputs is Option<Signal>
}
```

### Property-Based Tests

Use `proptest` for mathematical functions:

```rust
#[cfg(test)]
mod proptests {
    use proptest::prelude::*;
    
    proptest! {
        #[test]
        fn test_curve_monotonic(points in prop::collection::vec((-1.0f32..=1.0f32, -1.0f32..=1.0f32), 2..10)) {
            // Curves should be monotonically non-decreasing if all y values increase
            let sorted: Vec<_> = points.iter().enumerate()
                .filter(|(i, (_, y))| *i > 0 && y.1 >= points[i-1].1)
                .count();
            assert_eq!(sorted, points.len() - 1);
        }
    }
}
```

---

## Debugging Techniques

### Enable Verbose Logging

Set environment variables before running:
```bash
# Engine evaluation logging
FLEXINPUT_ENGINE_DEBUG=1 cargo run

# Network frame dumps
FLEXINPUT_NET_DEBUG=1 cargo run

# Device polling traces
RUST_LOG=debug cargo run
```

### Puffin Profiler

**Setup:**
```bash
cargo install puffin_viewer
```

**Usage:**
1. Enable profiler in Settings → Developer → Profiler
2. Run `puffin_viewer --url 127.0.0.1:8585`
3. View real-time flame graphs in browser

**Key profile scopes to monitor:**
- `main_node_loop` - Overall evaluation time
- `compute_node` - Per-node dispatch overhead
- `eval_subgraph` - Sub-patch recursion cost
- `preprocess_dev_sigs` - Device signal preparation

### GDB/LLDB Debugging

**Break on panic:**
```bash
gdb --args cargo run
(gdb) break rust_panic
(gdb) run
```

**Inspect thread state:**
```bash
(gdb) info threads
(gdb) thread 3  # Switch to processing thread
(gdb) print *state
```

### Visual Studio Debugging (Windows)

1. Build in debug mode: `cargo build`
2. Open project in VS Code with rust-analyzer
3. Set breakpoints in source files
4. Launch with F5 (debugger attaches automatically)

---

## Common Pitfalls & Solutions

### 1. Thread Race Conditions

**Symptom:** Intermittent panics or incorrect signal values.

**Cause:** Multiple threads accessing shared state without proper synchronization.

**Solution:** Use `Arc<RwLock<T>>` for read-heavy, `Arc<Mutex<T>>` for write-heavy:
```rust
// Bad: Shared HashMap without lock
let dev_sigs: Arc<HashMap<...>> = Arc::new(HashMap::new());

// Good: RwLock protects concurrent access
let dev_sigs: Arc<RwLock<HashMap<...>>> = Arc::new(RwLock::new(HashMap::new()));
```

### 2. ArcSwap Misuse

**Symptom:** Processing thread reads stale graph, or UI thread blocks on write.

**Cause:** Using `ArcSwap` for data that needs atomic updates to multiple fields.

**Solution:** Use `Arc<RwLock<T>>` for complex structures:
```rust
// Bad: ArcSwap for multi-field struct
let state: ArcSwap<NodeState> = ArcSwap::from_pointee(NodeState::default());

// Good: RwLock for complex state
let state: Arc<RwLock<NodeState>> = Arc::new(RwLock::new(NodeState::default()));
```

### 3. Sub-patch UID Collisions

**Symptom:** Remapper in nested sub-patch overwrites top-level remapper outputs.

**Cause:** Using raw node indices instead of namespaced UIDs.

**Solution:** Always use `namespaced_uid(outer, inner)` for state keys:
```rust
// Bad: Raw index (collides across sub-patches)
let key = snap.node_uid;

// Good: Namespaced UID (unique across all nesting levels)
let ns_uid = namespaced_uid(outer_uid, snap.node_uid);
let key = ns_uid;
```

### 4. Lock Contention at High Frequencies

**Symptom:** Processing thread stalls when UI updates graph.

**Cause:** Holding `Mutex` or `RwLock` across long computations.

**Solution:** Minimize lock hold time, use `try_lock()` where appropriate:
```rust
// Bad: Hold lock during entire evaluation
let mut out = proc_outputs.lock().unwrap();
eval_graph_tick(...);  // Blocks UI thread if it tries to write

// Good: Lock only for read/write of final result
let mut out = TickOutput::default();
eval_graph_tick(...);
if let Ok(mut locked) = proc_outputs.try_lock() {
    *locked = out;
}
```

### 5. Memory Leaks in State Vectors

**Symptom:** `NodeState` grows unbounded over long sessions.

**Cause:** Vec/HashMap fields never cleared, only grown.

**Solution:** Clear specialized buffers when not needed:
```rust
// In module cleanup or on state reset:
state.delay_bufs.clear();
state.avg_bufs.clear();
state.dc_fast.clear();
```

---

## UI Pitfalls Learned the Hard Way (egui / gamepad-nav / overlays)

These are project-specific traps that have each cost more than one debugging
session. Read this section before touching gamepad-nav, overlays, or any pinned
widget — the same handful of mistakes keep resurfacing in new places.

### 6. Nav highlight invisible in the config overlay (the recurring one)

**Symptom:** A selection glow/ring (curve dot, TZ/menu divider, remapper card,
value-field ring) shows on the main canvas but is INVISIBLE when the same element
is pinned to the config overlay — fixed once per channel, then breaks again for the
next channel.

**Root cause:** `egui::Context::data`/`data_mut` is **shared across all viewports**,
but `ctx.cumulative_pass_nr()` is **per-viewport**. The gamepad-nav driver
(`run_gamepad_nav`) runs in the ROOT viewport and stamps every highlight channel
with the root pass number. Renderers gate `channel_pass == ui.ctx().cumulative_pass_nr()`.
The config overlay renders in its OWN viewport whose pass counter differs, so every
pass-gated selection highlight mismatches there.

**Solution — the classification rule (mechanical):** there are two channel classes;
do NOT blanket-swap them.
- **Selection/focus channels** are *nav-driver-stamped* and read cross-viewport
  (`gp_nav_curve_sel`, `gp_nav_tz`, `gp_nav_tz_zone`, `gp_nav_tz_seam`,
  `gp_nav_remap_card`, `gp_nav_active`, …). Gate these with
  `crate::widgets::nav_pass(ctx)` / `nav_pass_matches(ctx, pass)`, which returns the
  driver's stored pass (`gp_nav_pass`, a shared-data slot) and so matches in every
  viewport.
- **Rect channels** are *renderer-stamped* for same-viewport use (`gp_nav_field_rects`,
  `gp_nav_tz_lines`, `gp_nav_item_rects`, `gp_nav_remap_card_rects`, …). Keep these on
  `cumulative_pass_nr()` — publisher and reader are in the same viewport.

The test is literally *"does the nav driver stamp this channel?"* → `nav_pass`.
*"Does a renderer stamp it for a reader in the same viewport?"* → `cumulative_pass_nr`.
Never re-stamp channels inside `config_overlay.rs` to paper over this — that scattered
re-stamp pattern is what kept regressing; the `nav_pass` gate is the single fix.

### 7. NEVER nest egui `ctx` lock acquisitions (hard freeze)

**Symptom:** The whole app freezes (epaint deadlock), often only under gamepad nav.

**Cause:** `ctx.cumulative_pass_nr()`, `ctx.data(..)` and `ctx.data_mut(..)` each take
the egui context lock. Calling one inside another's closure re-enters the lock and
deadlocks epaint.

**Solution:** Read every ctx value into a plain local FIRST, then operate on locals:
```rust
// BAD — data_mut closure calls cumulative_pass_nr(), which re-locks ctx:
ctx.data_mut(|d| d.insert_temp(id, (ctx.cumulative_pass_nr(), rects)));

// GOOD — read the pass into a local first:
let pass = ctx.cumulative_pass_nr();
ctx.data_mut(|d| d.insert_temp(id, (pass, rects)));
```

### 8. Gamepad field-editor lockstep triad

Any pinned multi-control row that is gamepad-editable is governed by THREE things
that must agree exactly, or the focus ring lands on the wrong control (or the wrong
field edits):

1. `nav_element_fields(outer)` — the ordered `Vec<NavFieldDef>` the unified
   multi-field editor (`nav_drive_fields`) walks. (`crates/ui/src/app/nav/fields.rs`)
2. `publish_nav_field_rects(ui, inner_id, &rects)` — the renderer must push one rect
   per field in the **same order**. (`crates/ui/src/canvas/viewer/scale.rs`)
3. `elem_has_fields(mid, elem)` — a static `matches!` mirror of #1's coverage, used by
   the cursor hit-test which has no selection context.

If a row has **conditional** sub-fields (e.g. the Virtual Menu `options` element only
shows Touch#/Gyro-axes/Gyro-× when the Touch/Gyro checkboxes are on), then #1 and #2
must gate on the **same derived condition** — including any legacy-default fallback the
renderer uses — so the field list and the rect list stay index-aligned however the
toggles are set. The multi-field editor clamps `field_index` to the current length each
frame, so a shrinking list is safe, but a MIS-ORDERED list is not.

Adding a new editable element = add an arm to #1 + the mirror entry in #3 + a
`publish_nav_field_rects` call in its renderer. The MultiField widget kind is picked up
automatically for any element with a non-empty field list (`nav_selected_kind`'s
fallback), and works in both the sub-patch canvas and the config overlay for free.

### 9. Reuse the Easy-mode nav machinery — don't reinvent it

Config-overlay gamepad editing MUST reuse the existing nav machinery (`EditLevel`
state machine, the unified field editor, curve/TZ drivers, cursor, highlight channels).
The overlay drives the *same* editors the sub-patch canvas does via
`config_nav_sel`/`nav_config_override`; it only differs in how the target element is
selected and in the passthrough it applies. Building a parallel editor for the overlay
is how divergence bugs start.

### 10. Transparent overlay windows need `WS_EX_NOREDIRECTIONBITMAP`

`WS_EX_TRANSPARENT` + `WS_EX_LAYERED` alone is NOT enough on this stack: without
`WS_EX_NOREDIRECTIONBITMAP` the DWM composites an opaque white sheet UNDER the
transparent window (observed on Win11). The extended style is applied via the vendored
`egui-winit` patch — if you touch overlay viewport creation, preserve it.

---

## Design Decisions Worth Knowing (recent)

### Touch Zones / Virtual Menu geometry = one BSP tree, both modes

Zone geometry has a single authoritative backend: the **BSP zone tree**
(`flexinput_core::touchzones::ZoneNode`, persisted per field as `zone_tree{N}`).
Both **Ports** and **Mapping** modes, and both **grid** and **radial** menu layouts,
run on this tree. The legacy grid (`col_edges{N}`/`row_edges{N}`) is kept ONLY as a
migration source: `ZoneNode::from_grid` migrates a grid losslessly (leaf id ==
row-major grid index; `zones()` returns leaves in row-major order), so an un-edited
patch yields byte-identical pin ids/order and existing wiring is undisturbed. The
first structural edit persists `zone_tree{N}`, which then becomes authoritative.

Consequences to respect:
- Per-zone divides are per-leaf (partial dividers), never full-width/height cuts.
- Port regeneration after a structural edit must be **wiring-preserving keyed by pin
  id** (`tz_regen_ports_preserving`): snapshot out-pin remotes by id → rebuild →
  reconnect surviving ids. Surviving leaves keep their wiring; a subdivide's new leaf
  gets empty ports; a merged leaf's wiring drops.
- Do NOT reintroduce a `if mapping { tree } else { grid }` split in any editor/renderer
  — that split was the source of the "ports mode only does full cuts" and "radial ports
  is broken" bugs.

### Dynamic gamepad-glyph icons (`gp:<pin>`)

Any icon slot (Macro port, Menu/TZ per-zone override, SVG layout decoration, the shared
`icon_picker_button`) can hold the abstract key **`gp:<pin>`** (e.g. `gp:btn_south`) —
a gamepad control glyph that renders in the CURRENTLY-connected pad's family style and
**restyles live** when the pad changes.
- Resolution funnels through `macro_icons::macro_port_icon_texture` →
  `icon_key_svg_bytes(key, skin)` → `remapper_icons::gp_pin_svg(skin, pin)`. Texture
  cache is keyed by (skin, pin, size) so a pad change re-rasterizes everywhere with zero
  per-site work. Pins the current family lacks fall back to their NATIVE style (a
  DualSense touchpad glyph stays PlayStation-style under a Switch Pro).
- The ambient skin is published once per frame to the ctx slot `fxi_current_gp_skin` and
  read via `current_gp_skin(ctx)`.
- **Derive the skin from the physical pad's VID/PID `ControllerKind`, NOT from the
  device-id string.** A re-enumerated virtual DualSense enumerates as "Wireless
  Controller", which matches no PlayStation keyword and silently fell back to Xbox — the
  cause of the "shows Xbox glyphs while a Switch Pro is connected" bug. Skip
  own-virtual (`is_own_virtual_gilrs_id`) and MIDI devices when choosing the source pad.

### HIDMaestro: changing a virtual device's HID report descriptor requires a PID bump

The helper reclaims persisted virtual devices by `device_id` and never re-applies the
report descriptor to an existing node. So if you change a virtual device's HID report
descriptor (e.g. mouse 6→7 byte report for a scroll field), you MUST also bump the
profile PID — otherwise the stale-node guard keeps the old device and the driver desyncs
from the new report layout (this once killed LMB/RMB). Bumping the PID forces
destroy+recreate.

---

## Code Review Checklist

Before submitting PRs, verify:

- [ ] **Thread safety:** No shared mutable state without locks/ArcSwap
- [ ] **Performance:** Hot path (< 10 µs per tick) doesn't allocate
- [ ] **Error handling:** All `unwrap()` calls have descriptive messages or are in tests
- [ ] **Tests:** New functionality has unit/integration tests
- [ ] **Documentation:** Public API has doc comments with examples
- [ ] **Naming:** Follows Rust conventions (snake_case, PascalCase, etc.)
- [ ] **Formatting:** `cargo fmt --all` passes
- [ ] **Clippy:** `cargo clippy -- -D warnings` has no errors

---

## Git Workflow

### Branch Naming

```bash
# Feature branches
git checkout -b feature/add-new-module

# Bug fixes
git checkout -b fix/subpatch-uid-collision

# Releases
git checkout -b release/v0.14.0
```

### Commit Messages

Follow conventional commits:
```
feat(modules): add vec_reshape module for directional vector shaping
fix(engine): resolve sub-patch UID collision in namespaced_uid
perf(ui): skip snarl clone when no pointer interaction detected
docs(readme): update architecture section with three-thread design
test(automap): add integration test for cross-family button mapping
```

### Pull Request Template

```markdown
## Description
Brief description of changes.

## Type of Change
- [ ] Bug fix (non-breaking change fixing an issue)
- [ ] New feature (non-breaking change adding functionality)
- [ ] Breaking change (fix or feature that would cause existing functionality to not work as expected)
- [ ] Documentation update

## Testing
- [ ] Unit tests pass (`cargo test`)
- [ ] Integration tests pass (`cargo test --tests`)
- [ ] Manual testing completed

## Checklist
- [ ] Code follows style guidelines
- [ ] Self-review completed
- [ ] Comments added for complex logic
- [ ] No new warnings from clippy
```

---

## Build System & Dependencies

### Cargo Workspace Structure

```toml
# Root Cargo.toml
[workspace]
members = [
    "crates/core",
    "crates/engine",
    "crates/devices",
    "crates/virtual",
    "crates/hidmaestro",
    "crates/net",
    "crates/modules",
    "crates/ui",
    "app",
]
default-members = ["app"]
```

### Dependency Management

**Workspace dependencies:** Define in root `[workspace.dependencies]`:
```toml
[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
glam = { version = "0.27", features = ["serde"] }
egui = "0.33"
```

**Crate-level dependencies:** Reference workspace deps in `Cargo.toml`:
```toml
[dependencies]
flexinput-core = { path = "../core" }
serde = { workspace = true }
glam = { workspace = true }
```

### Vendored Dependencies

Three vendored crates in `vendor/` directory:

1. **egui-snarl** - Graph UI library (modified for transparent window support)
2. **egui-wgpu** - WGPU renderer (modified for GPU loss handling)
3. **egui-winit** - Windowing integration (modified for extended styles)

**Why vendored?**
- Need unreleased fixes not in upstream crates
- Custom modifications for FlexInput-specific features
- Avoid dependency version conflicts

**Updating vendored deps:**
```bash
# Copy from crate registry
cargo vendor --version-precise 0.9.0 egui-snarl vendor/egui-snarl

# Apply custom patches
git diff vendor/egui-snarl > patches/snarl-fixes.patch
```

### Feature Flags

Control optional functionality via Cargo features:

```toml
# Root Cargo.toml
[features]
p2p = ["flexinput-net/p2p"]      # Enable iroh P2P transport
probe-bin = []                    # Build hm_shm_probe binary
helper-bin = []                   # Build elevated helper binary
```

**Usage in code:**
```rust
#[cfg(feature = "p2p")]
use flexinput_net::transport::p2p;

fn main() {
    #[cfg(feature = "probe-bin")]
    hm_shm_probe::run();
}
```

---

## Platform-Specific Considerations

### Windows (Primary Target)

**HIDMaestro driver:** Requires Administrator privileges for UMDF2 installation.

**Transparent windows:** Use `WS_EX_TRANSPARENT` + `WS_EX_LAYERED` for see-through effect.

**GPU loss recovery:** Vendored egui-wgpu raises `GPU_LOST` flag instead of panicking.

### Linux (Experimental)

**Backend support:** SDL3 backend works on Linux via udev.

**HIDMaestro:** Not available on Linux (UMDF2 is Windows-only). Use ViGEmBus fallback or skip virtual devices.

**wgpu backends:** Enable Vulkan for best performance:
```toml
[dependencies]
wgpu = { version = "27", features = ["vulkan"] }
```

### macOS (Not Supported)

FlexInput targets Windows only due to HIDMaestro dependency. No plans for macOS support.

---

## References

- Workspace config: `Cargo.toml`
- Module trait: `crates/core/src/module.rs`
- Engine thread spawn: `crates/engine/src/thread.rs`
- UI app structure: `crates/ui/src/app.rs`
- Device backends: `crates/devices/src/lib.rs`
