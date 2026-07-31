# FlexInput Patch Formats Reference

## Overview

FlexInput persists to JSON via serde. The on-disk formats are:

| File | Extension / name | Root type | Where defined |
|------|------------------|-----------|---------------|
| Full patch | `.fxp` | `UiPatch` | `crates/ui/src/canvas/mod.rs` |
| Sub-patch preset | `.fxsp` | `SubPatchFile` | `crates/ui/src/app/subpatch.rs` |
| Response curve | `.fxc` | `CurveFile` | `crates/ui/src/canvas/viewer/curve_support.rs` |
| Workspace autosave | `workspace.json` | `PersistedWorkspace` | `crates/ui/src/settings.rs` |
| Crash recovery | `recovery.json` | `PersistedWorkspace` | `crates/ui/src/settings.rs` |
| Settings | `settings.json` | `AppSettings` | `crates/ui/src/settings.rs` |

> **The graph is a serialized egui-snarl `Snarl<NodeData>`, NOT `core::Patch`.**
> This is the single most common misconception. `crates/core/src/patch.rs` DOES
> define `Patch` / `NodeInstance` (with a `Uuid` id) / `Wire` (with `from_pin`/`to_pin`
> names) and `PATCH_VERSION`, but those types are **legacy** and are *not* used by the
> file formats above (the vestigial `flexinput_engine::Engine` struct still carries a
> `core::Patch` field, but the live processing path builds a `ProcessingGraph` from the
> snarl instead — see ENGINE_INTERNALS.md). Do not hand-author graph JSON against the
> `core::Patch` schema; it will not load.

All graph files store the graph as an egui-snarl `Snarl<NodeData>`
(`vendor/egui-snarl/src/lib.rs`): `{ nodes: Slab<Node<NodeData>>, wires: <set of
Wire> }`. Nodes are keyed by an **integer slab index** (`NodeId(usize)`), and a wire is
a pair of `(out_node_index, out_pin_index)` → `(in_node_index, in_pin_index)` — there
are **no UUIDs and no pin-name wires**. FlexInput never writes this JSON by hand; it is
produced by snarl's own `Serialize`/`Deserialize`.

---

## Full Patch Format (`.fxp`)

### Root type — `UiPatch`

```rust
// crates/ui/src/canvas/mod.rs
pub struct UiPatch {
    pub version: u32,                 // currently 1
    pub snarl: Snarl<NodeData>,       // the whole graph (egui-snarl serialization)
    pub virtual_device_ids: Vec<String>,   // e.g. ["virtual.xinput.0"]
    #[serde(default)]
    pub bound_exes: Vec<String>,      // auto-switch exe filenames, e.g. ["game.exe"]
    #[serde(default)]
    pub auto_bypass: bool,            // bypass output when bound process unfocused
    #[serde(default)]
    pub easy_preset_path: Option<PathBuf>,  // re-link the Easy-mode preset on reopen
    #[serde(default, skip_serializing_if = "OverlayLayout::is_empty")]
    pub overlay: OverlayLayout,       // info-overlay pinned elements
    #[serde(default, skip_serializing_if = "OverlayLayout::is_empty")]
    pub config: OverlayLayout,        // config-overlay tweak-pins
}
```

Written by `Canvas::save_patch(..)`, read by `Canvas::load_patch()` (which also accepts
`.fxsp`, see below). On load the snarl is passed through `migrate_loaded_snarl` +
`migrate_ds4_pin_ids`. On save it is passed through `sanitize_snarl_for_save` (strips
transient/secret fields such as network passphrases — see `sanitize_node_for_save`).

### Node schema — `NodeData`

Each snarl node's value is a `NodeData` (`crates/ui/src/canvas/node.rs`):

```rust
pub struct NodeData {
    pub module_id: String,            // matches ModuleDescriptor::id, or "subpatch"
    pub display_name: String,
    pub category: String,
    pub inputs: Vec<PinDescriptor>,   // {name, signal_type, optional}
    pub outputs: Vec<PinDescriptor>,
    pub params: HashMap<String, serde_json::Value>,  // all module config lives here
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subpatch: Option<Box<UiSubPatch>>,  // present only when module_id == "subpatch"
    #[serde(skip)]
    pub extra: NodeExtra,             // LIVE-ONLY (last_signals, scope history, …) — never persisted
}
```

Notes that matter:
- **`inputs`/`outputs` are serialized on the node**, so a patch remembers a node's pin
  set even if the module descriptor later changes — this is why `migrate_loaded_snarl`
  rewrites pin lists for modules that gained pins (Map Action's Analog output, ASTH's
  raw-analysis outputs).
- **`extra` is `#[serde(skip)]`** — live signal values, scope history, status dots and
  AutoMap glow are runtime state and are never written to disk.
- All tunable module configuration is untyped `params` (JSON values), keyed by string.
  See MODULES_REFERENCE.md for per-module keys.

### `device.source` / `device.sink`

These are ordinary `NodeData` nodes distinguished by `module_id`; their config lives in
`params`:
- `device.source`: `device_id` (`"{backend}:{family}:{instance}"`, e.g.
  `"gilrs:dualsense:0"`), plus optional per-device calibration keys (deadzone, gyro,
  invert, …).
- `device.sink`: `device_id` (`"virtual.{kind}.{n}"`, e.g. `"virtual.xinput.0"`), plus
  output-shaping keys (rumble floor/max/exp, mouse sensitivity for KB/M sinks, …).

### Migration — `migrate_loaded_snarl`

`crates/ui/src/canvas/mod.rs`. Runs on every load, recursing into sub-patch snarls:

```rust
pub fn migrate_loaded_snarl(snarl: &mut Snarl<NodeData>) {
    for node in snarl.nodes_mut() {
        // e.g. backfill outputs for modules that gained pins, rewrite legacy
        // ViGEm device ids → HIDMaestro, backfill rumble defaults, …
        if let Some(sp) = node.subpatch.as_mut() {
            migrate_loaded_snarl(&mut sp.snarl);   // recurse
        }
    }
}
```

`migrate_ds4_pin_ids` similarly rewrites older DualShock4 pin ids (on `device.sink`
`input_pin_ids`), and `migrate_generic_button_pin` rewrites the legacy Generic-pad
stick-click / menu ids on `device.source` `output_pin_ids` (`btn_lstick` → `btn_ls`,
`btn_rstick` → `btn_rs`, `btn_select` → `btn_back`, `btn_mode` → `btn_guide`). Add
migrations here (never a version bump alone) when a param key or pin set changes.

Note that `migrate_ds4_pin_ids` runs only on the `.fxp` load path, while
`migrate_loaded_snarl` also runs on workspace restore — put anything that must survive
an app restart in the latter.

---

## Sub-Patch Preset Format (`.fxsp`)

### Root type — `SubPatchFile`

A `.fxsp` is **not** a `UiPatch`. It carries one `UiSubPatch` directly:

```rust
// crates/ui/src/app/subpatch.rs
pub(crate) struct SubPatchFile {
    pub(crate) version: u32,
    pub(crate) sub_patch: UiSubPatch,
}
```

```rust
// crates/ui/src/canvas/node.rs
pub struct UiSubPatch {
    pub display_name: String,
    pub pins_in: Vec<SubPatchPin>,    // {name, signal_type}
    pub pins_out: Vec<SubPatchPin>,
    pub snarl: Box<Snarl<NodeData>>,  // the inner graph
    pub items: Vec<LayoutItem>,       // pinned widgets + decorations on the body
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlay_items: Vec<LayoutItem>,  // info-overlay pins that travel with the preset
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config_items: Vec<LayoutItem>,   // config-overlay tweak-pins that travel with it
}
```

`LayoutItem` is the unified Z-ordered body/overlay element list:

```rust
pub enum LayoutItem {
    Module(ExposedModule),   // { inner_node_id: usize, element_id: String, pos, size, … }
    Deco(LayoutDecoration),  // text / svg / image / shape decorations
}
```

`overlay_items`/`config_items` are how a preset ships built-in overlays: on save,
tab-level overlay pins sourced from a sub-patch are *attributed* into that sub-patch
(`attribute_overlays_into_subpatches`); on load they are *materialized* back onto the tab
(`materialize_subpatch_overlays`). First-level sub-patches only.

### Loading a `.fxsp`

`Canvas::load_patch()` accepts both `.fxp` and `.fxsp`. For a `.fxsp` it builds an empty
canvas and inserts a single `subpatch` node whose `UiSubPatch` is the loaded preset,
with the outer node's pin descriptors mirrored from `pins_in`/`pins_out`. If the result
is Easy-mode-compatible (`is_easy_compatible_canvas` in `app.rs` — exactly one
`subpatch` node plus only `device.source`/`device.sink`, an AutoMap inlet AND outlet,
and a non-empty `items` layout) the app switches to Easy mode.

---

## Response Curve Format (`.fxc`)

### Root type — `CurveFile`

```rust
// crates/ui/src/canvas/viewer/curve_support.rs — every field #[serde(default …)]
pub(crate) struct CurveFile {
    pub points: Vec<[f64; 2]>,   // control points in [-1,1]² (or in-range) space
    pub biases: Vec<f64>,        // one per segment (points.len()-1); segment curvature
    pub absolute: bool,          // default true; mirror the curve about the origin
    pub in_min: f64, pub in_max: f64,    // input range (defaults -1 / 1)
    pub out_min: f64, pub out_max: f64,  // output range (defaults -1 / 1)
    pub grid_x: i64, pub grid_y: i64,    // editor grid (defaults 4)
    pub snap: bool,
    pub scale_t: f64,            // log/exp pre-warp (-1..1)
    pub trail_ms: i64,           // live-dot trail length (default 300)
    pub show_scaled_grid: bool,
    pub show_grid_labels: bool,
}
```

One `.fxc` is cross-compatible across the scalar / vec / two-way curve modules and the
envelope generator — each reads the fields it uses and ignores the rest. Curve data on a
node lives in that node's `params` (points/biases/etc.), not as a file path; `.fxc` is an
import/export convenience, loaded/saved from the curve editor's context menu.

### Evaluation

`crates/engine/src/eval/curves.rs`:

```rust
pub fn apply_curve(
    x: f32, pts: &[[f32; 2]], biases: &[f32],
    absolute: bool, in_min: f32, in_max: f32, out_min: f32, out_max: f32, scale_t: f32,
) -> f32;
```

In `absolute` mode it folds the input about the origin, applies the `scale_t` warp,
samples the point list (`sample_curve`) with per-segment `biases`, inverts the warp, and
rescales to the output range preserving sign. See the source for the exact math.

---

## Workspace / Recovery Format

`workspace.json` (opt-in tab persistence) and `recovery.json` (always-on crash-recovery
autosave) share one root type:

```rust
// crates/ui/src/settings.rs
pub struct PersistedWorkspace {
    pub version: u32,                 // currently 1
    pub active_tab: usize,
    pub tabs: Vec<PersistedTab>,
}

pub struct PersistedTab {
    pub title: String,
    #[serde(default)] pub file_path: Option<PathBuf>,
    #[serde(default)] pub bound_exes: Vec<String>,
    #[serde(default)] pub auto_bypass: bool,
    pub snarl: Snarl<NodeData>,       // same graph serialization as .fxp
    #[serde(default)] pub easy_preset_path: Option<PathBuf>,
    #[serde(default)] pub view_salt: u64,      // stable pan/zoom key for this tab
    #[serde(default, skip_serializing_if = "OverlayLayout::is_empty")] pub overlay: OverlayLayout,
    #[serde(default, skip_serializing_if = "OverlayLayout::is_empty")] pub config: OverlayLayout,
}
```

Both are produced by `FlexInputApp::build_persisted_workspace()`
(`crates/ui/src/app/persistence.rs`), which sanitizes each snarl and attributes
sub-patch overlay pins before serializing — it never mutates the live state.

**Save triggers:**
- `workspace.json`: `save_workspace_now()` — only when the `keep_workspace` setting is on.
- `recovery.json`: `maybe_write_recovery_snapshot()` — once per frame, but only when a
  settled edit changed `total_mutation_gen()` since the last write (debounced), and
  independent of `keep_workspace` so a GPU-loss relaunch never loses work. The write is
  atomic (temp + rename).

**Recovery flow:** GPU loss / relaunch → `load_workspace`/recovery restores the tabs →
recovery snapshot is cleared once the restore succeeds.

---

## Troubleshooting

### Patch won't load / "invalid" after an update
A module's pin set or param keys changed without a matching migration. Add the fixup to
`migrate_loaded_snarl` (rewrite pin lists / rename params / backfill defaults) — recall it
recurses into sub-patch snarls. A bare `version` bump does **not** migrate anything.

### Wires lost after reload
Snarl wires are index pairs, not pin names — they break only if a node's pin **order**
changed. Fix by preserving pin order in the migration (append new pins; never reorder),
or rebuild the pin list in `migrate_loaded_snarl` so old indices still resolve.

### Module params reset to defaults
The param key was renamed. Migrate it (`params.remove(old)` → `params.insert(new, val)`)
in `migrate_loaded_snarl`; unknown keys are otherwise dropped on the next save.

### Corrupted `recovery.json` crashes launch
Delete `%APPDATA%\FlexInput\recovery.json` and relaunch. (The atomic write makes a
half-written file impossible, but a schema mismatch from a downgrade can still fail to
deserialize.)

### `.fxp` files unexpectedly large
Expected: the snarl serializes every node's full `inputs`/`outputs`/`params`. Live state
(`NodeExtra`) is skipped, so size scales with graph size, not runtime.

---

## References

- `.fxp` / `UiPatch` / migration / sanitize: `crates/ui/src/canvas/mod.rs`
- `.fxsp` / `SubPatchFile`: `crates/ui/src/app/subpatch.rs`
- `NodeData` / `UiSubPatch` / `LayoutItem` / `OverlayLayout` / overlay attribute+materialize:
  `crates/ui/src/canvas/node.rs`
- `.fxc` / `CurveFile`: `crates/ui/src/canvas/viewer/curve_support.rs`
- `PersistedWorkspace` / `PersistedTab` / save/load: `crates/ui/src/settings.rs`
- Workspace/recovery orchestration: `crates/ui/src/app/persistence.rs`
- Snarl serialization: `vendor/egui-snarl/src/lib.rs`
- Legacy (non-persistence) types: `crates/core/src/patch.rs`

---

*Last updated: 2026-07-30*
