# FlexInput UI Architecture

## Overview

The UI crate (`crates/ui`) implements the entire user interface using egui (embedded GUI) with wgpu rendering. It manages multiple canvas instances, overlay windows, device panels, and handles the complex interaction between user input and the processing engine.

**Key Characteristics:**
- Multi-tab patch management
- Sub-patch editor windows with nested canvases
- Three transparent overlay viewports (info, menu, config)
- Easy mode vs Advanced mode layouts
- Gamepad UI navigation system
- Real-time device status visualization

---

## Main Application Structure (`app.rs`)

### FlexInputApp State (~4600 lines)

> The struct listings in this document are **representative, not exhaustive or
> field-exact** — `FlexInputApp`, `Canvas`, and `AppSettings` each carry far more fields
> than shown, and field names drift. Treat the source files as authoritative; the
> listings here are for orientation. (Sections that give exact serialized shapes — e.g.
> `NodeData`, `ProcessingOutput`, the persistence types — ARE kept field-accurate.)

The main application struct holds the live UI state (a subset):

```rust
pub struct FlexInputApp {
    // Core engine communication
    engine: Engine,
    proc_graph: ArcGraph,              // UI→Processing graph publishing
    proc_device_signals: ArcSignals,   // I/O→UI device signal reading
    proc_outputs: Arc<Mutex<ProcessingOutput>>,  // Processing→UI output reading
    
    // Tab management
    tabs: Vec<PatchTab>,
    active_tab: usize,
    
    // Device pools
    shared_virtual_devices: SharedDevicePool,
    devices: Vec<PhysicalDevice>,      // UI snapshot of physical devices
    
    // Settings & persistence
    settings: AppSettings,
    
    // Overlay windows
    overlay_visible: bool,
    config_overlay_visible: bool,
    
    // Gamepad navigation
    gamepad_nav: GamepadNav,
    
    // Hotkey systems
    panic_shortcut: PanicShortcut,
    pin_toggle_requested: Arc<AtomicBool>,
    overlay_toggle_requested: Arc<AtomicBool>,
    
    // GPU recovery state
    gpu_stalled: bool,
}
```

### PatchTab Structure

Each tab represents an independent patch with its own canvas and settings:

```rust
pub struct PatchTab {
    pub title: String,
    pub file_path: Option<PathBuf>,
    pub bound_exes: Vec<String>,           // Auto-switch processes
    pub canvas: Canvas,                     // Main graph editor
    pub virtual_panel: VirtualDevicePanel,  // Top device panel
    pub bypassed: bool,                     // Manual bypass flag
    pub auto_bypass: bool,                  // Auto-bypass when process not focused
    pub easy_state: EasyState,              // Transient Easy mode state
    pub view_salt: u64,                     // Pan/zoom persistence key
    pub overlay: OverlayLayout,             // Info overlay pinned elements
    pub config: OverlayLayout,              // Config overlay pinned elements
}
```

### SubPatchEditor Structure

Nested sub-patch editor windows:

```rust
struct SubPatchEditor {
    tab_idx: usize,                    // Parent tab index
    node_id: NodeId,                   // Parent subpatch node ID
    parent_editor_idx: Option<usize>,  // Grandparent editor (for nested)
    canvas: Canvas,                     // Inner graph editor
    open: bool,
    last_clipboard_gen: u64,           // Detect cross-boundary paste
    last_synced_parent_gen: Option<u64>,  // Skip redundant syncs
    last_inner_gen: Option<u64>,       // Skip write-back if unchanged
}
```

---

## Canvas System (`canvas/mod.rs`)

### Canvas Structure

The `Canvas` struct wraps egui-snarl for visual graph editing:

```rust
pub struct Canvas {
    pub snarl: Snarl<NodeData>,        // Core graph data structure
    
    // Undo/redo management
    undo_stack: Vec<Snarl<NodeData>>,  // Max 50 levels
    redo_stack: Vec<Snarl<NodeData>>,
    committed_fingerprint: u64,        // FNV-1a hash for change detection
    pending_value_baseline: Option<Snarl<NodeData>>,  // Commit-on-settle capture
    
    // Clipboard operations
    clipboard: Option<ClipboardData>,
    clipboard_gen: u64,                // Detect genuine user copies
    
    // Mutation tracking
    mutation_gen: u64,                 // Incremented on any snarl change
    
    // Sub-patch editor state
    pending_edit_subpatch: Option<NodeId>,
    pending_expose_module: Option<(NodeId, String, [f32; 2])>,
    is_inner: bool,                    // True if this is a sub-patch inner canvas
    pinned_inner_ids: HashSet<usize>,  // Nodes pinned to outer body
    
    // View management
    last_view_center_canvas: Option<egui::Pos2>,
    pending_view_action: Option<PendingViewAction>,
    spawn_glow: HashMap<NodeId, Instant>,  // One-shot glow effects
    view_salt: u64,                    // Independent pan/zoom key
}
```

### NodeData Structure

The snarl node payload — the UI's node type (independent of the legacy `core::NodeInstance`):

```rust
pub struct NodeData {
    pub module_id: String,
    pub display_name: String,
    pub category: String,
    pub inputs: Vec<PinDescriptor>,
    pub outputs: Vec<PinDescriptor>,
    pub params: HashMap<String, serde_json::Value>,  // ALL module config
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subpatch: Option<Box<UiSubPatch>>,  // present only when module_id == "subpatch"
    #[serde(skip)]
    pub extra: NodeExtra,                    // live-only, NEVER persisted
}
```

`NodeExtra` is transient per-frame state fed from the processing thread's outputs — it
is `#[serde(skip)]`, so none of it lands in a `.fxp`. Representative fields (see
`canvas/node.rs` for the full set):

```rust
pub struct NodeExtra {
    pub last_signals: Vec<Option<Signal>>,          // latest input signals (readout/body)
    pub last_out: Vec<Option<Signal>>,              // latest captured outputs (e.g. two-way curve)
    pub history: VecDeque<Vec<Option<f32>>>,        // scope / vectorscope rolling samples
    pub layout_unlocked: bool,                      // body is in layout-edit mode
    pub trig_capture: Option<Vec<Vec<Option<f32>>>>,// frozen trigger-scope waveform
    // … aux_f32 (counter reset), trig_* accumulation, repaint-gate hashes, …
}
```

> There is no `NodeStatus` enum or `automap_glow` field on `NodeData` — the live/
> disconnected dot and AutoMap glow are derived at render time, not stored here.

### UiSubPatch Structure

Nested sub-patch definition for the UI:

```rust
pub struct UiSubPatch {
    pub display_name: String,
    pub pins_in: Vec<SubPatchPin>,
    pub pins_out: Vec<SubPatchPin>,
    pub snarl: Box<Snarl<NodeData>>,  // Inner graph
    pub items: Vec<LayoutItem>,        // pinned widgets + decorations (paint order)
    pub overlay_items: Vec<LayoutItem>,// info-overlay pins that travel with the preset
    pub config_items: Vec<LayoutItem>, // config-overlay tweak-pins that travel with it
}

// One Z-ordered list mixes exposed module widgets and decorations:
pub enum LayoutItem {
    Module(ExposedModule),  // { inner_node_id: usize, element_id: String, pos, size, … }
    Deco(LayoutDecoration), // text / svg / image / shape
}
```

> There is no `PinnedItem` type. A pinned widget is a `LayoutItem::Module(ExposedModule)`,
> where `element_id` selects WHICH part of the module body to expose (`"default"` = the
> whole body). The same `LayoutItem`/`ExposedModule` shape is reused for the info and
> config overlays (`OverlayLayout.items`).

### Canvas Operations

**Undo/Redo:**
- `push_undo()` - Clone current snarl, clear redo stack
- `undo()` / `redo()` - Swap between stacks
- `track_value_edits()` - Commit-on-settle for param edits (sliders, text fields)
- Fingerprint-based change detection using FNV-1a hash

**Clipboard:**
- `copy_selected()` - Copy nodes + internal wires (excludes device.source/sink)
- `paste()` - Insert with offset, validate wire indices
- Cross-boundary paste via app-level clipboard shared across canvases

**Sub-patch Editing:**
- `pending_edit_subpatch` - Request to open editor window
- Sync from outer→inner on open, inner→outer on close
- Mutation gen tracking avoids redundant clones

---

## Overlay Windows

### Info Overlay (`overlay.rs`)

Transparent always-on-top viewport showing:
- Device status indicators
- Signal readouts for selected nodes
- Scope visualizations (oscilloscope, vectorscope)
- Pinned module widgets from `OverlayLayout`

**Window Properties:**
- `WS_EX_TRANSPARENT` + `WS_EX_LAYERED` for see-through effect, PLUS
  `WS_EX_NOREDIRECTIONBITMAP` — without the latter the DWM composites an opaque white
  sheet under the window (applied via the vendored egui-winit patch; see
  DEVELOPMENT_GUIDELINES.md pitfall #10)
- Click-through input handling
- Independent repaint rate (10-60 Hz configurable)

### Virtual Menu Overlay (`menu_overlay.rs`)

Second transparent viewport for gamepad navigation:
- Summoned by open menus in the graph
- Fully independent of info overlay visibility
- Handles menu state machine and card rendering

### Config Overlay M3 (`config_overlay.rs`)

Third transparent viewport for live parameter tweaking:
- Summoned by keyboard chord or Guide button
- Interactive over its panel, click-through elsewhere
- Renders the SAME pinned elements the sub-patch body does (`render_pinned_element`)
  and drives them through the SAME nav editors (see the reuse rule in
  DEVELOPMENT_GUIDELINES.md pitfall #9); any element with a non-empty
  `nav_element_fields` list is editable here for free.

**Live-topology passthrough (`app/config_route.rs`).** The overlay's defining trick:
while you tweak a param over a running game, the inputs used to NAVIGATE the overlay
are suppressed, but the input the tweaked param actually affects PASSES THROUGH so you
feel the change live. For source-like params (a Knob/Constant with no physical
upstream) this can't be derived by tracing upstream, so the resolver traces DOWNSTREAM
from the tweak node to the virtual sink pin(s) it modulates — honoring **live
selection** at gate nodes (Selector/Switch/Dropdown/fork, read from
`node.extra.last_signals`) — then collects the physical inputs feeding those sinks.
A physical stick that doesn't currently resolve to any virtual stick is "free" to be
the tweak control; if both sticks are routed, the D-pad drives the editor
(`ControlInput` / `control_input_from_pins` in `config_route.rs`). The passthrough
channel is a `HashSet<(device, pin)>`, not a single device.

**Overlay pins travel with the sub-patch preset.** Config/Info overlay pins are stored
per-tab (`PatchTab.overlay`/`config`), but a pin exposed from inside a sub-patch is
attributed into that sub-patch's `UiSubPatch.overlay_items`/`config_items` on save and
materialized back onto the tab on load (`attribute_overlays_into_subpatches` /
`materialize_subpatch_overlays` in `canvas/node.rs`). So a factory `.fxsp` preset can
ship built-in overlays. First-level sub-patches only.

---

## Device Panels

### VirtualDevicePanel (`panels/virtual_devices.rs`)

Top panel showing virtual output devices:
- List of active virtual pads (XInput, DS4, DualSense, KeyMouse)
- Per-device status indicators (connected, bypassed)
- Quick actions (ping rumble, calibrate, settings)
- Device creation/destruction via shared pool

### PhysicalDevicePanel (`panels/physical_devices.rs`)

Bottom panel showing connected physical devices:
- Device list with family icons (Xbox, PS4, PS5, Switch Pro)
- Live signal visualization (stick deflection, button state)
- Calibration access for analog sticks/triggers
- HIDHide masking controls

### Floating Heading Tabs

Both panels use floating heading tabs that can be collapsed:
- Animated collapse/expand with `ctx.animate_bool_with_time()`
- Anchored to canvas edges with 1px offset
- Visual grouping via darker background fill

---

## Easy Mode vs Advanced Mode

### Easy Mode Layout

Simplified preset-driven workflow:

```
┌─────────────────────────────────────────┐
│  [Tab Bar]                              │
├──────────┬──────────────────────────────┤
│ Devices  │  Preset Dropdown             │
│ (slide)  ├──────────────────────────────┤
│          │  Sub-patch Body              │
│          │  (center panel)              │
│          ├──────────────────────────────┤
│          │  Pinned Widgets Row          │
└──────────┴──────────────────────────────┘
```

**Characteristics:**
- Left panel slides in/out with collapse animation
- No direct graph editing
- Preset-based workflow (`.fxsp` files)
- Pinned widgets for parameter access
- Gamepad navigation optimized

### Advanced Mode Layout

Full node-based graph editor:

```
┌─────────────────────────────────────────┐
│  [Tab Bar]                              │
├─────────────────────────────────────────┤
│  Virtual Devices Panel (collapsible)    │
├─────────────────────────────────────────┤
│                                         │
│           CANVAS (snarl editor)         │
│                                         │
│                                         │
├─────────────────────────────────────────┤
│  Physical Devices Panel (collapsible)   │
└─────────────────────────────────────────┘
```

**Characteristics:**
- Side panels for device management
- Full graph editing capabilities
- Sub-patch editor windows
- All module types available
- Keyboard shortcuts enabled

---

## Gamepad UI Navigation (`gamepad_nav.rs` + `app/nav/*.rs`)

### GamepadNav state + the EditLevel machine

All nav state lives in one runtime-only (never serialized) struct, `GamepadNav`, on
`FlexInputApp`. The core is an **`EditLevel` state machine** — the current level
decides what dpad/sticks/buttons do:

```rust
pub enum EditLevel {
    Widget,       // moving selection between sub-patch widgets
    Editing,      // editing a scalar/dropdown/multi-field widget
    CurveDots,    // inside a response curve: highlight a dot, RT/LT add/remove
    CurveDot,     // moving the highlighted dot in X/Y; hold-North edits curvature
    RemapScroll,  // inside a remapper-family widget: move/reset/delete/Learn cards
    RemapCard,    // inside one entered card: left/right fields, up/down edits
    TzLines,      // inside a Touch Zones / Virtual Menu FIELD (see TzFocus below)
    TzGrab,       // a focused divider is grabbed and being nudged
    TzCards,      // inside the TZ/menu mapping CARDS widget (zone tab + Learn flow)
}
```

Key runtime fields: `mode: HashMap<String,bool>` (per-device nav-enabled),
`field_index` (focused field in the multi-field editor), `tz_focus`, `cursor_pos`/
`cursor_visible` (RS/gyro pointer), `config_nav_sel` (the config-overlay's selected
`(outer, inner, element)`).

> The older `NavDevice { id, enabled, mode }` / `NavMode { Idle, Cursor, … }` shape
> described in earlier drafts of this doc does not exist — `EditLevel` + `GamepadNav`
> is the real model.

### Touch Zones / Virtual Menu field nav — spatial walk

Inside a TZ/menu field (`EditLevel::TzLines`), focus is a `TzFocus`:

```rust
pub enum TzFocus { Border, Zone(usize), Seam /* radial origin */ }
```

The walk is **spatial and geometry-based** (no grid/tree special-casing): each frame
the renderer publishes `gp_nav_tz_targets` = (pass, `Vec<(kind, a, b, center)>`) in
global screen space (kind 0=zone, 1=border, 2=seam), stamped with `nav_pass`. A dpad/LS
press moves to the nearest target of the OPPOSITE type in that screen direction, so
focus alternates zone↔border in any layout (radial angular wrap is free in screen
space). Actions by focus: a **Border** grabs/recenters/removes a divider; a **Zone**
sets `sel_zone` (retargeting the cards widget) and RT/LT divide it along each axis; the
**Seam** rotates the radial origin. The shared radial mouse editor is
`menu_body::radial_border_editor` (used by both the pinned body and the config overlay,
so radial fields are mouse-editable everywhere).

### The unified multi-field editor

Most editable rows are NOT bespoke — they route through one generic editor,
`nav_drive_fields`, driven by a per-element field table `nav_element_fields`
(`app/nav/fields.rs`). Left/right walk fields; up/down (or South/RT) edit the focused
one (`Value` nudge, `Enum`/`EnumPair` cycle, `Toggle` flip); West=fine, North=reset.
See DEVELOPMENT_GUIDELINES.md → *"Gamepad field-editor lockstep triad"* for the
invariant every editable row must satisfy (`nav_element_fields` ↔
`publish_nav_field_rects` ↔ `elem_has_fields`, including conditional sub-fields).

### Viewport-agnostic highlight pass (`nav_pass`)

Selection highlights are published by the nav driver in the ROOT viewport but must
also render in the config-overlay viewport. Because `ctx.data` is shared across
viewports while `cumulative_pass_nr()` is per-viewport, selection channels gate on
`crate::widgets::nav_pass(ctx)` (the driver's stored pass), while renderer-published
rect channels stay on `cumulative_pass_nr()`. This is the single most-regressed bug
class in the UI — see DEVELOPMENT_GUIDELINES.md → pitfall #6 for the classification
rule before adding any new highlight.

### Navigation Controls

**Gamepad Mapping:**
- Left stick / D-pad: Move cursor
- South (A/Cross): Confirm selection
- East (B/Circle): Back / Cancel
- Start: Open main menu
- Guide button: Summon config overlay (M3)

**Chord Detection:**
- Multi-button combinations for special actions
- Configurable in Settings → Gamepad Navigation
- Global shortcuts independent of nav device

### Nav Widget Types

```rust
pub enum NavWidgetKind {
    None,              // Not interactable (label, graph)
    Value,             // Knob / constant - enter to edit
    Dropdown,          // Select from list
    Toggle,            // Switch on/off
    Curve,             // Response curve editor
    Remapper,          // AutoMap remapper body
    MultiField,        // Row of editable fields
    TouchZones,        // Zone configuration
    TouchZoneCards,    // Mapping card editor
}
```

### Nav Cursor Rendering

Drawn as a highlighted rectangle around the focused widget:
- Animated appearance/disappearance
- Color-coded by widget type
- Positioned using measured node rects from canvas viewer

---

## Settings Window (`settings.rs`)

### AppSettings Structure

Persisted configuration (~200 fields):

```rust
pub struct AppSettings {
    // Thread rates
    pub sample_rate_hz: u32,         // Processing thread rate (500-2000)
    pub polling_hz: u32,             // I/O thread device poll rate (500-4000)
    
    // Visual
    pub theme: Theme,                // Dark / Light / System
    pub contrast: Contrast,          // UI contrast level
    pub see_through_active: bool,    // Transparent canvas background
    pub see_through_alpha: f32,      // Alpha for see-through (0.0-1.0)
    
    // Device defaults
    pub default_stick_deadzone: f32,
    pub default_gyro_mult: f32,
    pub default_mouse_sensitivity: f32,
    pub default_rumble_floor: f32,
    pub default_rumble_max: f32,
    pub default_rumble_exp: f32,
    
    // Hotkeys
    pub panic_shortcut: PanicShortcut,
    pub pin_shortcut: PinShortcut,
    pub overlay_shortcut: PinShortcut,
    pub config_overlay_shortcut: PinShortcut,
    
    // Behavior
    pub ui_mode: UiMode,             // Easy / Advanced
    pub auto_switch: bool,           // Switch tab on foreground process
    pub persist_virtual_devices: bool,  // Keep devices across restarts
    pub keep_workspace: bool,        // Restore all tabs on launch
    
    // Gamepad nav
    pub gamepad_ui_nav_default: bool,
    pub gamepad_chords_nav_only: bool,
    
    // Repaint rates
    pub bg_repaint_hz: u32,          // Background repaint rate (1-30 Hz)
}
```

### Settings Persistence

**Storage Location:** `%APPDATA%\FlexInput\settings.json`

**Save Triggers:**
- On every change (debounced to avoid excessive I/O)
- On application exit (`on_exit()`)
- During GPU recovery relaunch

**Migration:**
- Versioned settings schema
- Automatic migration on load for older formats
- Backward compatibility maintained across releases

---

## Thread Communication

### UI → Processing (Graph Publishing)

```rust
// In FlexInputApp::update()
let (graph_snap, dirty_uids) = build_processing_graph(&snarl, defaults);
self.proc_graph.store(Arc::new(graph_snap));  // ArcSwap publish
```

**`build_processing_graph()` converts Snarl→ProcessingGraph:**
1. Walk all nodes in snarl
2. Resolve wire connections to `input_sources` indices
3. Extract device source/sink metadata
4. Recursively build inline subgraphs
5. Return `(ProcessingGraph, Vec<usize>)` — the graph plus the list of dirty node UIDs

### Processing → UI (Output Reading)

```rust
// In FlexInputApp::update()
match self.proc_outputs.try_lock() {  // Non-blocking read
    Ok(mut out) => {
        for (&(uid, pin), &sig) in &out.node_outputs {
            self.eval_cache.insert((NodeId(uid), pin), sig);
        }
        // Apply display state (last_inputs, last_outputs, scope_samples)
        apply_display_state(&mut snarl, None, &last_inputs, &last_outputs, &scope_lookup);
    }
    Err(_) => {}  // Skip if processing thread is writing
}
```

**`ProcessingOutput` structure** (`crates/engine/src/thread.rs`):
```rust
pub struct ProcessingOutput {
    pub node_outputs: HashMap<(usize, usize), Option<Signal>>,  // (node_uid, pin) → signal
    pub last_inputs: HashMap<usize, Vec<Option<Signal>>>,
    pub last_outputs: HashMap<usize, Vec<Option<Signal>>>,      // captured outputs (two-way curve)
    pub scope_pending: Vec<(usize, Vec<Option<f32>>)>,
}
```

Sink routing to the I/O thread goes through a SEPARATE lock (`SinkBus`), not
`ProcessingOutput`, so the I/O thread never contends on the UI/processing mutex.

### I/O → UI (Device Signals)

```rust
// In FlexInputApp::update()
let snap = self.proc_device_signals.load_full();  // ArcSwap load
self.last_signals = (*snap).clone();
```

**`ArcSignals` wraps:**
```rust
pub struct ArcSignals {
    signals: ArcSwap<HashMap<(String, String), Signal>>,
}
```

Used for live signal display in canvas viewer (oscilloscopes, readouts).

---

## Canvas Viewer (`canvas/viewer.rs`)

### FlexViewer Structure

Stateless renderer for snarl nodes:

```rust
pub struct FlexViewer<'a> {
    pub descriptors: &'a [ModuleDescriptor],
    pub ctx: egui::Context,
    pub live_device_ids: &'a HashSet<String>,
    pub live_signals: &'a HashMap<(String, String), Signal>,
    pub device_rates: &'a HashMap<String, u32>,
    pub panic_shortcut: &'a PanicShortcut,
    pub physical_devices: &'a [PhysicalDevice],
    
    // Request state (consumed after rendering)
    pub pending_wire_menu: Option<WireMenuRequest>,
    pub rename_request: Option<RenameRequest>,
    pub edit_subpatch_request: Option<NodeId>,
}
```

### Node Rendering Pipeline

1. **Header** - Module name, category badge, status dot
2. **Body** - Module-specific widget (curve editor, knob, etc.)
3. **Inputs/Outputs** - Pin circles with signal type colors
4. **Wires** - Colored by SignalType (Float=blue, Bool=yellow, Vec2=green)

### Specialized Viewers

Per-module-type renderers in `canvas/viewer/` subdirectory:
- `automap_*.rs` - AutoMap node viewers
- `curve_*.rs` - Response curve editors
- `remapper_*.rs` - Remapper card editors
- `touch_zones.rs` - Zone configuration UI
- `scopes.rs` - Oscilloscope/vector scope rendering

---

## Icon System (`macro_icons.rs`, `canvas/remapper_icons.rs`, `menu_body.rs`)

Every icon site — Macro ports, the shared `icon_picker_button` (Menu header + per-zone
overrides, Touch Zones per-zone overrides, Macro-output body, SVG layout decorations) —
funnels through ONE resolver: `macro_icons::macro_port_icon_texture(ctx, icon_key,
icon_svg, size)`. An `icon_key` is one of:
- a **build-time SVG file stem** (`ALL_ICONS`, grouped by sub-folder into
  `ICON_CATEGORIES`), or
- an empty key with `icon_svg` carrying a user-loaded custom SVG, or
- the dynamic **`gp:<pin>`** gamepad-glyph scheme (see below).

### Dynamic gamepad glyphs (`gp:<pin>`)

A `gp:<pin>` key (e.g. `gp:btn_south`, `gp:touchpad_touch`) renders in the currently
connected pad's family style and restyles live when the pad changes. The picker's
synthetic **"Gamepad inputs"** category lists `remapper_icons::GAMEPAD_INPUT_PINS`
(faces, dpad, bumpers/triggers, sticks+clicks, menu/system, touchpad click/touch/swipe,
paddles). Resolution: `macro_port_icon_texture`'s `gp:` branch →
`icon_key_svg_bytes(key, current_gp_skin(ctx))` → `remapper_icons::gp_pin_svg(skin,
pin)`, with the texture cached by (skin, pin, size). See DEVELOPMENT_GUIDELINES.md →
*"Dynamic gamepad-glyph icons"* for the native-fallback rule and the VID/PID-vs-id-string
skin-derivation gotcha.

---

## Window Management

### Main Window Properties

Configured in `app/src/main.rs` as an `eframe::NativeOptions` with a `ViewportBuilder`:

```rust
let native_options = eframe::NativeOptions {
    viewport: egui::ViewportBuilder::default()
        .with_title("FlexInput")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([800.0, 500.0])
        .with_decorations(false)   // custom title bar
        .with_resizable(true)
        .with_transparent(true)    // see-through canvas support
        .with_icon(icon),
    depth_buffer: 32,              // 3D Controller viewer depth testing
    wgpu_options,
    ..Default::default()
};
```

### Overlay Viewport Creation

Each overlay is a deferred/immediate egui viewport keyed by a stable id
(`overlay.rs`, `menu_overlay.rs`, `config_overlay.rs`):

```rust
let viewport_id = egui::ViewportId::from_hash_of("fxi_overlay");
let builder = egui::ViewportBuilder::default()
    .with_title_shown(false)
    .with_always_on_top()
    .with_transparent(true)
    .with_taskbar(false)
    // + WS_EX_NOREDIRECTIONBITMAP / click-through via the vendored egui-winit patch
    ;
ctx.show_viewport_immediate(viewport_id, builder, |vctx, _class| {
    // render overlay contents into `vctx`
});
```

(There is no `egui::Viewports::try_new` API — the real entry point is
`ctx.show_viewport_immediate` / `show_viewport_deferred`.)

### GPU Loss Recovery

**Detection:** Vendored egui-wgpu raises `GPU_LOST` atomic flag

**Recovery Flow:**
1. Save crash-recovery snapshot (`recovery.json`)
2. Set HIDMaestro helper to persist=ON (keep devices alive)
3. Relaunch self via `std::process::Command::new(&exe)`
4. New instance detects `FLEXINPUT_GPU_RECOVERY` env var
5. Reclaim persisted virtual devices from helper
6. Restore patch from recovery snapshot

**Stall Mode:** If GPU lost while game is foreground:
- Don't relaunch immediately (would fight game for device)
- Stall GUI thread, keep input/engine threads running
- Relaunch when FlexInput returns to foreground

---

## Performance Optimizations

### Repaint Throttling

```rust
// In app.rs update()
let bg_throttle = vp_minimized || !vp_focused;
REPAINT_SUPPRESSED.store(bg_throttle, Ordering::Relaxed);

// Consumers use request_repaint_throttled() instead of ctx.request_repaint()
pub fn request_repaint_throttled(ctx: &egui::Context) {
    if !REPAINT_SUPPRESSED.load(Ordering::Relaxed) {
        ctx.request_repaint();
    }
}

### Snarl Clone Optimization

```rust
// Only clone snarl when pointer/keyboard interaction is active
let needs_snapshot = ui.ctx().input(|i| {
    i.pointer.any_down()
        || i.pointer.any_released()
        || i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. }))
});
let pre_snapshot = if needs_snapshot { Some(self.snarl.clone()) } else { None };
```

Prevents wasteful 50+ node graph clones on idle frames (dominates frame time at 60 Hz).

### Recovery Snapshot Debouncing

```rust
// Only write when mutation_gen changes AND no value gesture in progress
if self.total_mutation_gen() != self.last_recovery_mutation_gen {
    // Check for ongoing user interaction
    if !ctx.input(|i| i.pointer.any_down()) && !ctx.wants_keyboard_input() {
        maybe_write_recovery_snapshot();
    }
}
```

Prevents thrashing on every frame while keeping crash recovery functional.

### Theme Application Caching

```rust
// Skip egui style walk when theme/contrast hasn't changed
if self.theme_applied_for != Some(key) {
    crate::settings::apply_theme_and_contrast(ctx, &self.settings);
    self.theme_applied_for = Some(key);
}
```

`apply_theme_and_contrast()` walks the entire egui Visuals tree — caching avoids O(n) work every frame.

---

## Sub-patch Editor Sync Protocol

### Outer→Inner Sync (on editor open)

1. Clone outer canvas snarl into `UiSubPatch.snarl`
2. Create new `Canvas` with cloned snarl
3. Set `canvas.is_inner = true` to suppress inlet/outlet context menu
4. Derive nested view salt from parent salt + node ID

### Inner→Outer Sync (on editor close)

1. Compare `inner_canvas.mutation_gen` against `last_synced_parent_gen`
2. If unchanged, skip clone entirely (common case: user didn't edit)
3. If changed, clone inner snarl back into `UiSubPatch.snarl`
4. Bump outer canvas `mutation_gen` to trigger parent editor sync

### Cross-Boundary Clipboard

```rust
// App-level clipboard shared across all canvases
app_clipboard: Option<ClipboardData>,
app_clipboard_from_inner: bool,  // Track paste source for priority

// On paste: prefer inner canvas clipboard if user just copied inside editor
if self.app_clipboard_from_inner || canvas.clipboard().is_none() {
    if let Some(ref cb) = self.app_clipboard {
        canvas.set_clipboard(cb.clone());
    }
}
```

---

## Device Pool Management (`devices_pool.rs`)

### SharedDevicePool Structure

```rust
pub type SharedDevicePool = Arc<Mutex<Vec<Box<dyn VirtualDevice>>>>;
```

Same device instance reused across all tabs. Membership reconciled on:
- Workspace restore (startup)
- Patch load
- Tab close (prune orphaned devices)
- Every frame (catch new sink nodes added via drag-drop)

### Device Lifecycle

```rust
// Create path
device_ops.tx.send(DeviceOp::Create { device_id })
    → worker thread → HIDMaestro helper IPC → device created

// Destroy path  
device_ops.tx.send(DeviceOp::Destroy { device_id })
    → worker thread → HIDMaestro helper IPC → device destroyed

// Reinstall driver
device_ops.tx.send(DeviceOp::ReinstallDriver)
    → worker thread → elevated helper binary → driver reinstall
```

### Failed Device Tracking

```rust
failed_device_ids: HashSet<String>,  // Suppress per-frame retry spam

// Clear failed set when node is removed from canvas
self.failed_device_ids.retain(|id| needed.iter().any(|n| n == id));
```

---

## HIDHide Integration (`hidhide_ui.rs`, `device_ops.rs`)

### Reconcile Flow

```rust
fn reconcile_hidhide(&mut self) {
    // 1. Compute current remapped-physical device set from canvas
    let targets = snarl_virtual_device_ids(&snarl)
        .into_iter()
        .filter_map(|id| resolve_vid_pid(&id))
        .collect::<HashSet<_>>();
    
    // 2. Skip if unchanged (hash-based detection)
    let sig = compute_device_sig(&targets);
    if sig == self.hidhide_last_device_sig && !self.hidhide_dirty {
        return;
    }
    
    // 3. Open transient HidHideClient handle
    let hh = HidHideClient::try_open()?;
    
    // 4. Apply whitelist (processes allowed to see remapped devices)
    hh.set_whitelist(&self.hidhide_proc_list);
    
    // 5. Hide targets from all processes except whitelist
    hh.hide_devices(&targets);
}
```

**Key constraint:** HidHideClient handle is transient — persistently holding it blocks the elevated helper from opening the exclusive control device.

---

## Process List & Auto-Switch (`process_list.rs`)

### Foreground Detection

```rust
pub fn foreground_exe() -> Option<String> {
    // GetForegroundWindow → GetWindowText → FindExecutable → basename
}

pub fn foreground_hwnd() -> Option<isize> {
    // Same as above but returns HWND for focus flip-flop
}
```

### Auto-Switch Logic

```rust
// In update(), when auto_switch is enabled:
if let Some(fg_exe) = foreground_exe() {
    if fg_exe != self.last_fg_exe {
        self.last_fg_exe = fg_exe.clone();
        if let Some(idx) = tabs.iter().position(|t| 
            t.bound_exes.iter().any(|b| b.eq_ignore_ascii_case(&fg_exe))
        ) {
            set_active_tab(idx);
        }
    }
}
```

### Focus Flip-Flop (Pin Feature)

When user toggles always-on-top pin:
1. Capture current foreground HWND before activating FlexInput
2. After activation, defer foreground handoff by a few frames
3. Use synthetic ALT keystroke to defeat focus-stealing prevention
4. Restore captured HWND as foreground after FlexInput level changes settle

---

## Guide Button Watcher (`guide_watcher.rs`)

Monitors connected gamepads for Guide/PS/Home button presses:
- Single press → summon config overlay (M3)
- Double-tap → learn chord for pin shortcut
- Configured via `GuideWatchConfig`:
  ```rust
  pub struct GuideWatchConfig {
      pub enabled: bool,
      pub require_double_tap: bool,
      pub chord_signal: Option<String>,  // e.g., "btn_guide"
  }
  ```

---

## Panic Hotkey (`panic_hotkey.rs`)

Global keyboard shortcut to disable all virtual output:
- Default: `Ctrl+Backtick` (unlikely to conflict with games)
- Listener thread polls for chord, raises `panic_toggle_requested` flag
- UI consumes flag each frame and toggles `panic_active`
- Panic mode forces bypass on active tab regardless of normal state

---

## Pin Hotkey (`pin_hotkey.rs`)

Global keyboard shortcut for always-on-top toggle:
- Configurable chord in Settings
- Same listener pattern as panic hotkey
- Shares `pin_toggle_requested` flag with guide button watcher
- Triggers Win32 `SetWindowPos` for HWND_TOPMOST

---

## Build Script (`build.rs`)

Embeds build-time constants into the UI crate:
```rust
// Embed app icon PNG bytes for title bar rendering
const APP_ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");
```

Also handles platform-specific resource compilation.

---

## Key Files Summary

| File | Purpose | Lines (approx) |
|------|---------|----------------|
| `src/app.rs` | Main application state, eframe::App impl | ~4,600 |
| `src/canvas/mod.rs` | Canvas wrapper, undo/redo, clipboard | ~3,200 |
| `src/canvas/node.rs` | NodeData, UiSubPatch definitions | ~400 |
| `src/canvas/viewer/*.rs` | Per-module-type viewers | ~50 each |
| `src/easy/mod.rs` | Easy mode layout coordination | ~200 |
| `src/easy/io_panel.rs` | Device picker in easy mode | ~300 |
| `src/easy/center_panel.rs` | Sub-patch body + pinned widgets | ~400 |
| `src/easy/wiring.rs` | Auto-wire device.source→subpatch→sinks | ~200 |
| `src/gamepad_nav.rs` | Game controller UI navigation | ~800 |
| `src/panels/canvas.rs` | Canvas panel rendering in main window | ~300 |
| `src/panels/virtual_devices.rs` | Virtual device list panel | ~400 |
| `src/panels/physical_devices.rs` | Physical device list panel | ~500 |
| `src/settings.rs` | Settings struct, persistence, theme | ~600 |
| `src/overlay.rs` | Info overlay viewport management | ~300 |
| `src/menu_overlay.rs` | Virtual menu overlay viewport | ~200 |
| `src/config_overlay.rs` | Config overlay (M3) viewport | ~250 |
| `src/guide_watcher.rs` | Guide button monitoring thread | ~150 |
| `src/process_list.rs` | Foreground detection, HWND management | ~200 |
| `src/device_ops.rs` | Background device lifecycle worker | ~300 |

---

## References

- Main app entry: `app/src/main.rs`
- eframe App trait impl: `crates/ui/src/app.rs` (FlexInputApp::update)
- Canvas rendering: `crates/ui/src/canvas/mod.rs` (Canvas::show)
- Snarl library: `vendor/egui-snarl/src/lib.rs`
- GPU loss handling: `vendor/egui-wgpu/src/renderer.rs`

---

*Last updated: 2026-07-30*  
*Blueprint version: 1.0*
