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

/// Per-module list of exposable UI elements for the "Pin element ▶" submenu.
/// Each tuple is (element_id, human-readable label). The element_id `"default"`
/// is reserved — it always means "the whole body" and is added implicitly.
///
/// Elements that mutate the inlet/outlet structure of the inner module
/// (add/remove pin buttons, Learn, Clear unused) are intentionally NOT listed
/// — exposing them on the outer body would let users break the parent patch's
/// wiring without realising it.
pub fn exposable_elements(module_id: &str) -> &'static [(&'static str, &'static str)] {
    match module_id {
        "module.knob"               => &[("value", "Knob slider")],
        "module.constant"           => &[("value", "Number value")],
        "module.switch"             => &[("toggle", "Toggle button")],
        "module.label"              => &[("text",  "Text")],
        "module.svg"                => &[("image", "SVG image")],
        "module.response_curve"     => &[
            ("curve",     "Curve graph"),
            ("scale_row", "Log/Exp + Abs + Snap"),
            ("range_row", "In/Out range"),
            ("grid_row",  "Grid + Trail"),
        ],
        "module.vec_response_curve" => &[
            ("curve",     "Curve graph"),
            ("scale_row", "Log/Exp + Snap"),
            ("range_row", "In max + Out max"),
            ("grid_row",  "Grid + Trail"),
        ],
        "display.readout"           => &[("value", "Live value display")],
        "processing.gyro_3dof"      => &[
            ("pointer_mode",   "Pointer mode (Pitch+Yaw/Pitch+Roll/Player/World)"),
            ("steering_mode",  "Steering mode (Pitch+Yaw/Pitch+Roll/Player/World)"),
            ("steering_opts",  "Steering options (excl.Y / re-center / ease)"),
            ("lean_threshold", "Lean threshold"),
            ("gyro_invert",    "Gyro invert row (yaw/pitch/roll)"),
            ("accel_invert",   "Accel invert row (X/Y/+Z)"),
            ("lean_left",      "Lean Left section (Learn + mapping cards)"),
            ("lean_right",     "Lean Right section (Learn + mapping cards)"),
        ],
        "module.average"    => &[
            ("samples",   "Sample count"),
            ("spike_mad", "Spike MAD threshold"),
        ],
        "module.delay"      => &[("ms", "Delay (ms)")],
        "module.dc_filter"  => &[
            ("window_ms", "Window (ms)"),
            ("decay_ms",  "Decay (ms)"),
        ],
        "logic.counter"     => &[
            ("mode",       "Mode (Loop/Limit/Bounce/Unlimited)"),
            ("range_mode", "Range (Raw / 0..1) + Reset"),
            ("step",       "Step value"),
            ("min_max",    "Min / Max"),
        ],
        "logic.delay"       => &[
            ("mode", "Mode (Delay ON / Delay OFF)"),
            ("time", "Delay time + unit"),
        ],
        "generator.oscillator" => &[
            ("shape",    "Shape selector (Sine/Tri/Saw/Sqr)"),
            ("freq",     "Frequency unit + value"),
            ("phase",    "Phase + Bi/Uni"),
            ("preview",  "Waveform preview"),
        ],
        "generator.envelope" => &[
            ("curve",            "Envelope graph"),
            ("time_row",         "Time multiplier + unit"),
            ("mode_row",         "Behavior (Hold / Bounce / Loop)"),
            ("sustain_row",      "Sustain point"),
            ("grid_row",         "Grid divisions + Snap"),
            ("grid_options_row", "Show grid / labels"),
        ],
        "display.oscilloscope" => &[
            ("display",  "Scope display"),
            ("controls", "Win / Scale / Auto / Bi-Uni"),
        ],
        "display.trigscope" => &[
            ("display",  "Trigger scope display"),
            ("controls", "Win / Scale / Auto / Bi-Uni"),
        ],
        "display.vectorscope"  => &[("display", "Vectorscope display")],
        "module.remapper"      => &[("whole_module", "Whole module")],
        "module.map_action"    => &[("whole_module", "Whole module")],
        "module.automap_combiner" => &[("whole_module", "Whole module")],
        _ => &[],
    }
}
