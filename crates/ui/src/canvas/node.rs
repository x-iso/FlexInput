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

/// Text horizontal alignment for layout-decoration Text items.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum TextAlign { #[default] Left, Center, Right }

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

/// Inner graph + declared I/O for a sub-patch (meta-module) node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiSubPatch {
    pub display_name: String,
    pub pins_in: Vec<SubPatchPin>,
    pub pins_out: Vec<SubPatchPin>,
    #[serde(default = "default_inner_snarl")]
    pub snarl: Box<Snarl<NodeData>>,
    /// Inner modules pinned onto the sub-patch body for direct interaction.
    #[serde(default)]
    pub exposed_modules: Vec<ExposedModule>,
    /// Grid snap on layout-mode drag/resize.
    #[serde(default)]
    pub snap_enabled: bool,
    /// Grid step in logical pixels. Stepped in increments of 2 to keep things tidy.
    #[serde(default = "default_snap_grid_px")]
    pub snap_grid_px: u32,
    /// Static layout decorations (text labels, SVGs, shapes) painted under
    /// `exposed_modules`. Vec order = paint order (first = bottom).
    #[serde(default)]
    pub decorations: Vec<LayoutDecoration>,
    /// Runtime-only: currently selected decoration index for the inspector
    /// strip. Cleared on layout-mode exit.
    #[serde(skip)]
    pub selected_deco: Option<usize>,
    /// Runtime-only: currently selected exposed-module pin index, used by
    /// the inspector strip to surface per-pin overrides (e.g. Text color).
    #[serde(skip)]
    pub selected_pin: Option<usize>,
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
            exposed_modules: vec![],
            snap_enabled: false,
            snap_grid_px: default_snap_grid_px(),
            decorations: vec![],
            selected_deco: None,
            selected_pin: None,
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
            ("mode",         "Mode (Local/Player/World/Laser)"),
            ("gyro_invert",  "Gyro invert row (yaw/pitch/roll)"),
            ("accel_invert", "Accel invert row (X/Y/+Z)"),
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
        "display.oscilloscope" => &[
            ("display",  "Scope display"),
            ("controls", "Win / Scale / Auto / Bi-Uni"),
        ],
        "display.vectorscope"  => &[("display", "Vectorscope display")],
        "module.remapper"      => &[("whole_module", "Whole module")],
        "module.map_action"    => &[("whole_module", "Whole module")],
        _ => &[],
    }
}
