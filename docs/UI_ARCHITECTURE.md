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

The main application struct holds all persistent state:

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

UI-specific node metadata extending the core `NodeInstance`:

```rust
pub struct NodeData {
    pub module_id: String,
    pub display_name: String,
    pub category: String,
    pub inputs: Vec<PinDescriptor>,
    pub outputs: Vec<PinDescriptor>,
    pub params: HashMap<String, Value>,
    pub subpatch: Option<Box<UiSubPatch>>,  // Inline sub-patch definition
    
    // Live signal data (not persisted)
    pub extra: NodeExtra,
}

pub struct NodeExtra {
    pub last_signals: Vec<Option<Signal>>,      // Last evaluated outputs
    pub history: HashMap<usize, Vec<f32>>,       // Scope sample history
    pub status: NodeStatus,                      // Live/disconnected indicator
    pub automap_glow: Option<AUTOMAP_GLOW>,      // AutoMap bus glow effect
}

pub enum NodeStatus {
    Live,              // Green dot - device connected and active
    Disconnected,      // Red dot - no signal flow
    Error(String),     // Error state with message
}
```

### UiSubPatch Structure

Nested sub-patch definition for the UI:

```rust
pub struct UiSubPatch {
    pub display_name: String,
    pub pins_in: Vec<SubPatchPin>,
    pub pins_out: Vec<SubPatchPin>,
    pub snarl: Box<Snarl<NodeData>>,  // Inner graph
    pub items: Vec<PinnedItem>,        // Pinned widgets on body
}

pub struct PinnedItem {
    pub node_id: usize,                // Index into inner snarl
    pub position: egui::Pos2,          // Position on sub-patch body
    pub size: egui::Vec2,              // Measured widget size
}
```

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
- `WS_EX_TRANSPARENT` + `WS_EX_LAYERED` for see-through effect
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
- Suppresses specific physical input pins based on focused context

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

## Gamepad UI Navigation (`gamepad_nav.rs`)

### NavDevice Structure

Tracks which physical device is driving the UI:

```rust
pub struct NavDevice {
    pub id: String,                    // Device ID (e.g., "gilrs:dualsense:0")
    pub enabled: bool,                 // Nav active for this device
    pub mode: NavMode,                 // Current navigation state
}

pub enum NavMode {
    Idle,                              // No interaction
    Cursor,                            // Moving selection cursor
    EditValue,                         // Editing a numeric value
    SelectCard,                        // Selecting mapping card
    OpenMenu,                          // Navigating menu system
}
```

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
5. Return `(ProcessingGraph, HashSet<usize>)` with dirty node UIDs

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

**`ProcessingOutput` structure:**
```rust
pub struct ProcessingOutput {
    pub node_outputs: HashMap<(usize, usize), Signal>,  // (uid, pin) → signal
    pub last_inputs: HashMap<usize, Vec<Option<Signal>>>,
    pub last_outputs: HashMap<usize, Vec<Option<Signal>>>,
    pub scope_pending: Vec<(usize, Vec<Option<f32>>)>,
}
```

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

## Window Management

### Main Window Properties

```rust
// In build_eframe_options()
eframe::Frame {
    with_decorations: false,      // Custom title bar
    transparent: true,            // See-through support
    resizable: true,
    fullscreen: false,
}
```

### Overlay Viewport Creation

Each overlay uses a separate `egui::Viewport`:

```rust
let viewport = egui::ViewportBuilder::default()
    .with_titlebar_visible(false)
    .with_always_on_top(true)
    .with_transparent(true)
    .with_resizable(false);

egui::Viewports::try_new(ctx, viewport).unwrap();
```

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

*Last updated: 2026-07-25*  
*Blueprint version: 1.0*
