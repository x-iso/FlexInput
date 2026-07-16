use std::collections::{HashMap, VecDeque};

use egui_snarl::Snarl;
use flexinput_core::{ModuleDescriptor, PinDescriptor, Signal, SubPatchPin};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Runtime-only per-node UI state (not serialized).
/// Computation state has moved to `NodeState` in the engine crate.
#[derive(Debug, Clone, Default)]
pub struct NodeExtra {
    /// Rolling signal history for oscilloscope / vectorscope nodes.
    /// Populated each frame by draining the processing thread's scope_pending buffer.
    pub history: VecDeque<Vec<Option<f32>>>,
    /// Most recent evaluated signal per input (for readout / body display).
    /// Populated each frame from the processing thread's last_inputs map.
    pub last_signals: Vec<Option<Signal>>,
    /// Most recent evaluated output per channel for nodes that capture outputs
    /// (e.g. twoway_response_curve blended output). Populated from last_outputs map.
    pub last_out: Vec<Option<Signal>>,
    /// UI-side aux scratch used by the counter reset button.
    /// Set by the viewer; read once during graph snapshot building then cleared.
    pub aux_f32: Vec<f32>,
    /// True when the counter reset button was clicked; cleared after snapshot build.
    pub aux_f32_dirty: bool,
    /// True while the sub-patch body is in drag-to-reposition layout edit mode.
    pub layout_unlocked: bool,
    /// Frozen waveform capture for trigger-scope nodes. Updated only on a
    /// rising edge of the trigger input; `None` until the first trigger fires.
    /// Each entry is one sample: `[trig_val, ch1, ch2, …]`.
    pub trig_capture: Option<Vec<Vec<Option<f32>>>>,
    /// Previous trigger-pin value used for rising-edge detection.
    pub trig_prev: f32,
    /// Accumulation buffer filled while capture is in progress.
    pub trig_acc: Vec<Vec<Option<f32>>>,
    /// True when a capture is currently being accumulated.
    pub trig_armed: bool,
    /// Hash of the input signal(s) the last time this node's renderer asked
    /// "did my input change since last frame?". Used by oscilloscope /
    /// vectorscope / trigscope to gate their `request_repaint()` call so
    /// they only force vsync while a signal is actually animating —
    /// without this, three idle scopes lock the whole window at vsync
    /// the same way the response curves did.
    pub prev_input_hash: u64,
    /// Frame counter for the conditional-repaint gate. While the input
    /// looks unchanged, we still want to repaint occasionally so any
    /// pending visual decay (vectorscope trail fade, scope sweep
    /// completing) catches up. Reset to 0 whenever the input changes;
    /// incremented every frame the input looks stable.
    pub idle_frames_since_change: u32,
}

/// An inner module's exposed UI element pinned to the sub-patch body, rendered
/// at a free position with an explicit width/height. Layout mode supports both
/// moving (drag) and resizing (corner handle); Shift+resize maintains aspect ratio.
///
/// `element_id` selects WHICH UI element of the inner module to render — e.g.
/// `"value"` for a Knob's slider, `"curve"` for a Response Curve's graph,
/// `"text"` for a Label. The sentinel `"default"` exposes the whole module body
/// (legacy behaviour, kept for backward compatibility on older patches).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExposedModule {
    /// `NodeId.0` of the inner node inside `UiSubPatch::snarl`.
    pub inner_node_id: usize,
    /// Stable identifier of the exposed UI element within the inner module.
    /// `"default"` means "expose the entire module body".
    #[serde(default = "default_element_id")]
    pub element_id: String,
    /// Top-left position within the sub-patch body in logical pixels.
    pub pos: [f32; 2],
    /// Render size in logical pixels. Body widgets are clamped to this width;
    /// modules without inherent height limits (e.g. Text) use the full size.
    #[serde(default = "default_exposed_size")]
    pub size: [f32; 2],
    /// Per-pin Text color override. Populated only for Text-module pins via
    /// the layout inspector strip; `None` fields fall back to module values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_override: Option<PinTextOverride>,
    /// Per-pin Switch color override. Populated only for Switch-module pins
    /// via the layout inspector strip. Each `None` field falls back to the
    /// default visuals derived from the active state. Fill and outline can be
    /// overridden independently for ON and OFF states.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch_override: Option<PinSwitchOverride>,
    /// Per-pin graph color override. Populated only for graph-module pins
    /// (Response Curve, Oscilloscope, Vectorscope) via the layout inspector
    /// strip. `None` fields fall back to the module's default rendering
    /// (15% transparent background, theme grid, MULTI_COLORS channel palette).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_override: Option<PinGraphOverride>,
    /// Screen-overlay pins only: path from the tab canvas to the snarl that
    /// contains `inner_node_id`. Empty = the layout's implicit snarl (for
    /// sub-patch layout pins that's the owning sub-patch — always empty
    /// there; for overlay pins it means the node sits directly on the tab
    /// canvas). `[sp]` = inside the first-level sub-patch node with
    /// `NodeId.0 == sp`. Deeper nesting is reserved (schema carries it;
    /// milestone 1 resolves at most one level).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_path: Vec<usize>,
    /// Per-pin Input Viewer board style. Populated only for
    /// `module.input_viewer` pins via the layout inspector strip. `None` =
    /// the default board style (dark plate, amber highlight, white tint,
    /// thin outline). Each pinned instance styles independently — the same
    /// board can be an opaque widget in a sub-patch layout and a
    /// see-through overlay board at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iv_style_override: Option<IvStyleOverride>,
}

/// Per-pin style for a pinned Input Viewer board. Unlike the color-per-field
/// overrides above (which fall back field-by-field), this is a COMPLETE style
/// — `Some` replaces the default board style wholesale, `None` uses defaults.
/// Colors carry alpha so the plate can go fully transparent over a game.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct IvStyleOverride {
    /// Board plate fill (incl. transparency).
    pub bg: [u8; 4],
    /// Highlight for pressed elements (halos, trigger fill, dots, rings).
    pub accent: [u8; 4],
    /// Element tint: multiplies the glyph art; brightness ramps with glow.
    pub tint: [u8; 4],
    /// Board outline stroke color.
    pub outline: [u8; 4],
    /// Board outline stroke width (0 = no outline).
    pub outline_px: f32,
    /// 3D controller viewer only — per-pin camera elevation in degrees.
    /// `None` = use the module's own `cam_pitch` param. Ignored by 2D boards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub c3d_pitch: Option<f32>,
    /// 3D controller viewer only — per-pin model opacity (0..1). `None` = use
    /// the module's `overlay_alpha` param.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub c3d_alpha: Option<f32>,
    /// 3D controller viewer only — per-pin highlight fade time in seconds.
    /// `None` = use the module's `highlight_tailoff` param.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub c3d_fade: Option<f32>,
    /// 3D controller viewer only — per-pin widget composite alpha (0..1):
    /// fades the whole rendered controller as a 2D image, independent of the
    /// model-opacity see-through. `None` = fully opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub c3d_composite: Option<f32>,
}

fn default_exposed_size() -> [f32; 2] { [220.0, 100.0] }
fn default_element_id() -> String { "default".to_string() }

/// Per-pin color override for pinned Text modules. `None` on a field means
/// "use the source module's value". Only meaningful when `inner_node_id`
/// references a `module.label` (Text) node.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PinTextOverride {
    #[serde(default)] pub fill: Option<[u8; 4]>,
    #[serde(default)] pub outline: Option<[u8; 4]>,
    #[serde(default)] pub outline_px: Option<f32>,
}

/// Per-pin color override for pinned Switch modules. Allows the layout
/// designer to recolor the button independently of theme visuals — each
/// state (ON / OFF) can override fill, outline, and caption color. `None`
/// fields fall back to the default theme-derived visuals.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PinSwitchOverride {
    #[serde(default)] pub fill_on:     Option<[u8; 4]>,
    #[serde(default)] pub fill_off:    Option<[u8; 4]>,
    #[serde(default)] pub outline_on:  Option<[u8; 4]>,
    #[serde(default)] pub outline_off: Option<[u8; 4]>,
    #[serde(default)] pub text_on:     Option<[u8; 4]>,
    #[serde(default)] pub text_off:    Option<[u8; 4]>,
    #[serde(default)] pub outline_px:  Option<f32>,
}

/// Per-pin color override for pinned graph modules (Response Curve,
/// Oscilloscope, Vectorscope). `None` on a field means "use the module's
/// default rendering". `background` overrides the graph fill (which defaults
/// to a 15%-alpha black); `outline` + `outline_px` draw a frame around the
/// graph rect; `channel_colors[ch]` overrides the line/dot color for input
/// channel `ch` (falling back to the built-in MULTI_COLORS palette when the
/// slot is absent or `None`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PinGraphOverride {
    #[serde(default)] pub background:  Option<[u8; 4]>,
    #[serde(default)] pub outline:     Option<[u8; 4]>,
    #[serde(default)] pub outline_px:  Option<f32>,
    /// Gridline / axis color. `None` falls back to the default brighter grid
    /// (same neutral hue as the graph axis labels).
    #[serde(default)] pub gridline:    Option<[u8; 4]>,
    /// Per-channel line/dot color, indexed by channel. A `None` (or missing
    /// trailing) entry falls back to the default palette for that channel.
    #[serde(default)] pub channel_colors: Vec<Option<[u8; 4]>>,
}

/// Text horizontal alignment for layout-decoration Text items.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum TextAlign { #[default] Left, Center, Right }

/// Text vertical alignment for layout-decoration Text items, within the item's
/// bounding box.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum TextVAlign { #[default] Top, Center, Bottom }

/// Layout-only decorations placed on a sub-patch body. Distinct from
/// `ExposedModule` (which mirrors an inner module's UI); decorations don't
/// reference an inner node. Vec order = paint order (first = bottom).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LayoutDecoration {
    Text {
        pos: [f32; 2],
        size: [f32; 2],
        text: String,
        font_size: f32,
        fill: [u8; 4],
        outline: [u8; 4],
        outline_px: f32,
        align: TextAlign,
        #[serde(default)]
        valign: TextVAlign,
    },
    Svg {
        pos: [f32; 2],
        size: [f32; 2],
        svg_data: String,
        rev: u64,
        tint: [u8; 4],
        tint_mode: String,
        stroke: [u8; 4],
        stroke_px: f32,
    },
    Rect {
        pos: [f32; 2],
        size: [f32; 2],
        fill: [u8; 4],
        stroke: [u8; 4],
        stroke_px: f32,
        corner_radius: f32,
    },
    Ellipse {
        pos: [f32; 2],
        size: [f32; 2],
        fill: [u8; 4],
        stroke: [u8; 4],
        stroke_px: f32,
    },
    Line {
        a: [f32; 2],
        b: [f32; 2],
        stroke: [u8; 4],
        stroke_px: f32,
    },
}

impl LayoutDecoration {
    pub fn type_label(&self) -> &'static str {
        match self {
            LayoutDecoration::Text { .. }    => "Text",
            LayoutDecoration::Svg { .. }     => "SVG",
            LayoutDecoration::Rect { .. }    => "Rectangle",
            LayoutDecoration::Ellipse { .. } => "Ellipse",
            LayoutDecoration::Line { .. }    => "Line",
        }
    }
    /// Bounding rect in body-local coordinates. For Line, the bbox spans
    /// between the two endpoints (with a small inflation for hit testing
    /// applied at the call site).
    pub fn bbox(&self) -> ([f32; 2], [f32; 2]) {
        match self {
            LayoutDecoration::Text { pos, size, .. }
            | LayoutDecoration::Svg { pos, size, .. }
            | LayoutDecoration::Rect { pos, size, .. }
            | LayoutDecoration::Ellipse { pos, size, .. } => (*pos, *size),
            LayoutDecoration::Line { a, b, .. } => {
                let min = [a[0].min(b[0]), a[1].min(b[1])];
                let max = [a[0].max(b[0]), a[1].max(b[1])];
                (min, [max[0] - min[0], max[1] - min[1]])
            }
        }
    }
}

/// Unified layout item: either a module pin (exposed inner-module widget) or
/// a static decoration. The `items` Vec on `UiSubPatch` is in paint order
/// (first = bottom, last = top), giving a single Z-order across both kinds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LayoutItem {
    Module(ExposedModule),
    Deco(LayoutDecoration),
}

impl LayoutItem {
    pub fn bbox(&self) -> ([f32; 2], [f32; 2]) {
        match self {
            LayoutItem::Module(m) => (m.pos, m.size),
            LayoutItem::Deco(d)   => d.bbox(),
        }
    }
    /// Whether the given point (in body-local coords) hits this item. Lines
    /// use a distance-to-segment test; others use the bbox.
    pub fn hit_test(&self, p: [f32; 2]) -> bool {
        if let LayoutItem::Deco(LayoutDecoration::Line { a, b, stroke_px, .. }) = self {
            let tol = (stroke_px + 4.0).max(6.0);
            point_line_dist(p, *a, *b) <= tol
        } else {
            let (lp, ls) = self.bbox();
            p[0] >= lp[0] && p[1] >= lp[1] &&
            p[0] <= lp[0] + ls[0].max(1.0) && p[1] <= lp[1] + ls[1].max(1.0)
        }
    }
}

/// A screen-overlay layout: module elements + decorations pinned onto the
/// transparent info overlay (see `crate::overlay`). One per patch tab,
/// persisted with the tab (workspace + .fxp). Mirrors `UiSubPatch`'s layout
/// fields — items in paint order, snap grid, runtime-only selection — and
/// shares the `LayoutItem` type so the layout-edit machinery (inspector
/// strips, z-order, decorations) is reused verbatim. Overlay module pins
/// reference nodes anywhere in the tab via `ExposedModule::source_path`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayLayout {
    /// Paint order (first = bottom, last = top), same convention as
    /// `UiSubPatch::items`. Positions/sizes are overlay-local logical px.
    #[serde(default)]
    pub items: Vec<LayoutItem>,
    /// Grid snap on overlay-edit drag/resize.
    #[serde(default)]
    pub snap_enabled: bool,
    /// Grid step in logical pixels.
    #[serde(default = "default_snap_grid_px")]
    pub snap_grid_px: u32,
    /// Runtime-only: PRIMARY selected item index (into `items`). Cleared on
    /// overlay-edit exit. Mirrors `UiSubPatch::selected_item`.
    #[serde(skip)]
    pub selected_item: Option<usize>,
    /// Runtime-only: full multi-selection (contains the primary).
    #[serde(skip)]
    pub selected_items: Vec<usize>,
    /// Runtime-only: click-cycle anchor (see `UiSubPatch::cycle_pos`).
    #[serde(skip)]
    pub cycle_pos: Option<[f32; 2]>,
}

impl Default for OverlayLayout {
    fn default() -> Self {
        Self {
            items: vec![],
            snap_enabled: false,
            snap_grid_px: default_snap_grid_px(),
            selected_item: None,
            selected_items: Vec::new(),
            cycle_pos: None,
        }
    }
}

impl OverlayLayout {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

fn point_line_dist(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let abx = b[0] - a[0];
    let aby = b[1] - a[1];
    let len2 = abx * abx + aby * aby;
    if len2 < 1e-6 {
        let dx = p[0] - a[0]; let dy = p[1] - a[1];
        return (dx * dx + dy * dy).sqrt();
    }
    let t = (((p[0] - a[0]) * abx + (p[1] - a[1]) * aby) / len2).clamp(0.0, 1.0);
    let qx = a[0] + abx * t;
    let qy = a[1] + aby * t;
    let dx = p[0] - qx; let dy = p[1] - qy;
    (dx * dx + dy * dy).sqrt()
}

/// Inner graph + declared I/O for a sub-patch (meta-module) node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiSubPatch {
    pub display_name: String,
    pub pins_in: Vec<SubPatchPin>,
    pub pins_out: Vec<SubPatchPin>,
    #[serde(default = "default_inner_snarl")]
    pub snarl: Box<Snarl<NodeData>>,
    /// Unified layout items in paint order (first = bottom). Modules and
    /// decorations share one Z-order list.
    #[serde(default)]
    pub items: Vec<LayoutItem>,
    /// Legacy fields, read only — drained into `items` on first frame, then
    /// never written back (skip_serializing_if).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exposed_modules: Vec<ExposedModule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decorations: Vec<LayoutDecoration>,
    /// Grid snap on layout-mode drag/resize.
    #[serde(default)]
    pub snap_enabled: bool,
    /// Grid step in logical pixels. Stepped in increments of 2 to keep things tidy.
    #[serde(default = "default_snap_grid_px")]
    pub snap_grid_px: u32,
    /// Runtime-only: PRIMARY selected item index (into `items`). Drives the
    /// inspector strip and resize handle. Cleared on layout-mode exit. When a
    /// multi-selection is active, this is the last item the user clicked (it is
    /// always also present in `selected_items`).
    #[serde(skip)]
    pub selected_item: Option<usize>,
    /// Runtime-only: full multi-selection set (indices into `items`). Source of
    /// truth for "what is selected". Empty ⇒ nothing selected. When non-empty
    /// it always contains `selected_item`. Multi-drag moves every member; bulk
    /// style edits apply to every member where the field exists. Cleared on
    /// layout-mode exit.
    #[serde(skip)]
    pub selected_items: Vec<usize>,
    /// Runtime-only: tracks the last hit position + cursor pos for click-cycle
    /// behavior (so repeated clicks at the same spot cycle through overlapping
    /// items rather than always selecting the topmost).
    #[serde(skip)]
    pub cycle_pos: Option<[f32; 2]>,
}

impl UiSubPatch {
    /// Drain any legacy `exposed_modules` / `decorations` into the unified
    /// `items` Vec. Decorations go to the bottom (preserving their internal
    /// order), modules on top.
    pub fn migrate_into_items(&mut self) {
        if self.exposed_modules.is_empty() && self.decorations.is_empty() {
            return;
        }
        let decos = std::mem::take(&mut self.decorations);
        let mods  = std::mem::take(&mut self.exposed_modules);
        let mut migrated: Vec<LayoutItem> = decos.into_iter().map(LayoutItem::Deco).collect();
        migrated.extend(mods.into_iter().map(LayoutItem::Module));
        // Prepend migrated items so previously-saved items (rare, since new
        // schema is the writer) end up on top.
        migrated.extend(std::mem::take(&mut self.items));
        self.items = migrated;
    }

    /// True when no module is pinned and no decoration exists. Used to decide
    /// whether the sub-patch body should be rendered at all.
    pub fn is_layout_empty(&self) -> bool {
        self.items.is_empty()
            && self.exposed_modules.is_empty()
            && self.decorations.is_empty()
    }

    /// Iterate over all module pins in `items`. Returns (item_idx, &ExposedModule).
    pub fn iter_module_pins(&self) -> impl Iterator<Item = (usize, &ExposedModule)> {
        self.items.iter().enumerate().filter_map(|(i, it)| match it {
            LayoutItem::Module(m) => Some((i, m)),
            _ => None,
        })
    }

    /// Append a module pin to the top of the Z-order. Returns the new index.
    pub fn push_module_pin(&mut self, m: ExposedModule) -> usize {
        self.items.push(LayoutItem::Module(m));
        self.items.len() - 1
    }

    /// True when any module pin references `inner_node_id`.
    pub fn has_module_pin_for(&self, inner_node_id: usize) -> bool {
        self.iter_module_pins().any(|(_, m)| m.inner_node_id == inner_node_id)
    }

    /// Remove all module pins referencing `inner_node_id`. Adjusts
    /// `selected_item` to stay valid.
    pub fn remove_module_pins_for(&mut self, inner_node_id: usize) {
        let mut to_remove: Vec<usize> = self.items.iter().enumerate().filter_map(|(i, it)| {
            matches!(it, LayoutItem::Module(m) if m.inner_node_id == inner_node_id).then_some(i)
        }).collect();
        to_remove.sort_unstable_by(|a, b| b.cmp(a));
        for i in to_remove {
            self.items.remove(i);
            if let Some(sel) = self.selected_item {
                if sel == i { self.selected_item = None; }
                else if sel > i { self.selected_item = Some(sel - 1); }
            }
        }
    }

    /// Largest Y reached by any module pin (used by the "next pin Y" cascade
    /// when adding a new pin via the editor).
    pub fn module_pins_bottom_y(&self) -> f32 {
        self.iter_module_pins()
            .map(|(_, m)| m.pos[1] + m.size[1])
            .fold(0.0f32, f32::max)
    }
}

fn default_snap_grid_px() -> u32 { 8 }

fn default_inner_snarl() -> Box<Snarl<NodeData>> {
    Box::new(Snarl::new())
}

impl Default for UiSubPatch {
    fn default() -> Self {
        UiSubPatch {
            display_name: "Sub-patch".to_string(),
            pins_in: vec![],
            pins_out: vec![],
            snarl: default_inner_snarl(),
            items: vec![],
            exposed_modules: vec![],
            decorations: vec![],
            snap_enabled: false,
            snap_grid_px: default_snap_grid_px(),
            selected_item: None,
            selected_items: Vec::new(),
            cycle_pos: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeData {
    pub module_id: String,
    pub display_name: String,
    pub category: String,
    pub inputs: Vec<PinDescriptor>,
    pub outputs: Vec<PinDescriptor>,
    pub params: HashMap<String, Value>,
    /// Present only when module_id == "subpatch".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subpatch: Option<Box<UiSubPatch>>,
    #[serde(skip)]
    pub extra: NodeExtra,
}

impl From<&ModuleDescriptor> for NodeData {
    fn from(d: &ModuleDescriptor) -> Self {
        NodeData {
            module_id: d.id.to_string(),
            display_name: d.display_name.to_string(),
            category: d.category.to_string(),
            inputs: d.inputs.clone(),
            outputs: d.outputs.clone(),
            params: HashMap::new(),
            subpatch: if d.id == "subpatch" { Some(Box::new(UiSubPatch::default())) } else { None },
            extra: NodeExtra::default(),
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_layout_serde_roundtrip() {
        let layout = OverlayLayout {
            items: vec![
                LayoutItem::Module(ExposedModule {
                    inner_node_id: 7,
                    element_id: "curve".into(),
                    pos: [40.0, 60.0],
                    size: [220.0, 120.0],
                    text_override: None,
                    switch_override: None,
                    graph_override: Some(PinGraphOverride {
                        background: Some([1, 2, 3, 4]),
                        ..Default::default()
                    }),
                    source_path: vec![3],
                    iv_style_override: None,
                }),
                LayoutItem::Deco(LayoutDecoration::Rect {
                    pos: [5.0, 6.0],
                    size: [100.0, 50.0],
                    fill: [10, 20, 30, 200],
                    stroke: [1, 1, 1, 255],
                    stroke_px: 2.0,
                    corner_radius: 4.0,
                }),
            ],
            snap_enabled: true,
            snap_grid_px: 16,
            selected_item: Some(1), // runtime-only: must NOT survive
            selected_items: vec![1],
            cycle_pos: Some([9.0, 9.0]),
        };
        let json = serde_json::to_string(&layout).unwrap();
        let back: OverlayLayout = serde_json::from_str(&json).unwrap();
        assert_eq!(back.items.len(), 2);
        assert!(back.snap_enabled);
        assert_eq!(back.snap_grid_px, 16);
        // Runtime-only selection state is serde(skip).
        assert_eq!(back.selected_item, None);
        assert!(back.selected_items.is_empty());
        assert_eq!(back.cycle_pos, None);
        match &back.items[0] {
            LayoutItem::Module(pin) => {
                assert_eq!(pin.source_path, vec![3]);
                assert_eq!(pin.inner_node_id, 7);
                assert_eq!(pin.element_id, "curve");
                assert_eq!(
                    pin.graph_override.as_ref().unwrap().background,
                    Some([1, 2, 3, 4]),
                );
            }
            other => panic!("expected Module, got {other:?}"),
        }
        match &back.items[1] {
            LayoutItem::Deco(LayoutDecoration::Rect { corner_radius, .. }) => {
                assert_eq!(*corner_radius, 4.0);
            }
            other => panic!("expected Rect deco, got {other:?}"),
        }
    }

    /// Fields absent from older documents (`source_path`, `element_id`,
    /// `size`, snap settings) must default in instead of failing the load —
    /// this is the exact shape of a pre-overlay sub-patch layout pin, so it
    /// also proves existing .fxp/.fxsp files keep loading.
    #[test]
    fn overlay_layout_defaults_when_absent() {
        let json = r#"{"items":[{"Module":{"inner_node_id":2,"pos":[1.0,2.0]}}]}"#;
        let layout: OverlayLayout = serde_json::from_str(json).unwrap();
        assert_eq!(layout.snap_grid_px, default_snap_grid_px());
        assert!(!layout.snap_enabled);
        match &layout.items[0] {
            LayoutItem::Module(pin) => {
                assert!(pin.source_path.is_empty());
                assert_eq!(pin.inner_node_id, 2);
                assert_eq!(pin.element_id, "default");
                assert_eq!(pin.size, default_exposed_size());
            }
            other => panic!("expected Module, got {other:?}"),
        }
    }
}
