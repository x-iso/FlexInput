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

/// Which point of ONE axis an overlay element is anchored to when the viewport
/// differs from the authored size (see [`resolve_anchored_rect`]). This is the
/// PLACEMENT only — whether the size also scales is the separate `stretch_*`
/// flag on [`Anchor`]. `Auto` derives the point (and the stretch) from where the
/// element sits in the 3×3 zone grid — the default, and the only behaviour
/// legacy layouts had.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AnchorAxis {
    /// Derive point + stretch from the element's span in the 3×3 zone grid.
    #[default]
    Auto,
    /// Fixed margin from the start edge (left / top).
    Start,
    /// Held at the same fraction of the axis (centre).
    Center,
    /// Fixed margin from the end edge (right / bottom).
    End,
}

/// Per-axis anchoring for an overlay element. `Anchor::default()` is all-`Auto`
/// with no stretch, id, or link — derive both axes from placement, i.e. the
/// pre-anchor behaviour — so it round-trips as absent on legacy and un-anchored
/// layouts.
///
/// Placement (`x`/`y`) and stretch (`stretch_x`/`stretch_y`) are INDEPENDENT: an
/// element can be anchored to a specific zone (e.g. bottom-centre) AND stretch
/// on either/both axes so a element spanning zones keeps the same RELATIVE
/// width/height across aspect ratios. Stretch scales the size proportionally to
/// the viewport; the anchor point decides which edge/centre stays put.
///
/// `id`/`to` implement anchor-to-OBJECT: an element with `to == Some(id)` is
/// laid out relative to the RESOLVED rect of the element whose `id` matches
/// (its `x`/`y`/stretch then apply WITHIN that target frame instead of the
/// screen), so framed groups (a box + its label + icons) move and stretch
/// together. Ids are assigned lazily — only an element that is actually the
/// target of a link gets a nonzero `id`. `0` = unassigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Anchor {
    #[serde(default)]
    pub x: AnchorAxis,
    #[serde(default)]
    pub y: AnchorAxis,
    /// Scale width proportionally with the viewport (keep the same relative
    /// width) instead of a fixed physical width.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stretch_x: bool,
    /// Scale height proportionally with the viewport.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stretch_y: bool,
    /// This element's stable id (nonzero only once it is a link target).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub id: u64,
    /// If set, follow the element whose `id` equals this value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<u64>,
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

impl Anchor {
    /// Fully default: both axes derive from placement, no stretch, no id, no
    /// link (the serde/skip default — round-trips as absent).
    pub fn is_auto(&self) -> bool {
        self.x == AnchorAxis::Auto
            && self.y == AnchorAxis::Auto
            && !self.stretch_x
            && !self.stretch_y
            && self.id == 0
            && self.to.is_none()
    }
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
    /// Per-pin Touch Zones / Virtual Menu FIELD pad style + visibility.
    /// Populated via the layout inspector strip. `None` = the module's own
    /// colours, always shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub menu_style_override: Option<MenuStyleOverride>,
    /// Overlay layouts only: when this element stretches across anchor zones
    /// (see [`resolve_anchored_rect`]), scale it uniformly (preserving aspect)
    /// and centre it in the stretched band instead of distorting. Ignored by
    /// sub-patch body layouts (which are px-based, not screen-anchored).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keep_aspect: bool,
    /// Overlay layouts only: explicit per-axis anchoring. `Anchor::default()`
    /// (`{ Auto, Auto }`) derives the anchor from placement in the 3×3 zone grid
    /// — the pre-anchor behaviour — so legacy/un-anchored pins round-trip as
    /// absent. Set via the overlay toolbar's Anchor picker. Ignored by sub-patch
    /// body layouts.
    #[serde(default, skip_serializing_if = "Anchor::is_auto")]
    pub anchor: Anchor,
}

/// Per-pin style for a pinned Touch Zones / Virtual Menu field pad. Colour
/// fields fall back FIELD-BY-FIELD to the module's own `main_color` /
/// `highlight_color` params (unlike [`IvStyleOverride`]'s wholesale replace),
/// so a pin can recolour just the highlight, just the plate, or neither.
/// `visibility` gates when the pad is painted in LIVE views — the layout
/// editor always paints it so it stays selectable.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MenuStyleOverride {
    /// Main colour (plate/borders tint; alpha = pad opacity). `None` = module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main: Option<[u8; 4]>,
    /// Highlight colour (active zone / affordances). `None` = module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hi: Option<[u8; 4]>,
    /// When the pinned pad is painted (live views only).
    #[serde(default)]
    pub visibility: ZoneVisibility,
}

/// When a pinned Touch Zones / Menu pad is painted in live views.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ZoneVisibility {
    /// Always painted (default).
    #[default]
    Always,
    /// Painted only while at least one touch point / zone is active.
    OnTouch,
    /// Only the zone(s) currently touched are painted (whole-pad chrome
    /// hidden). Radial menus treat this like `OnTouch`.
    TouchedZones,
}

/// Per-pin style for a pinned Input Viewer board. Unlike the color-per-field
/// overrides above (which fall back field-by-field), this is a COMPLETE style
/// — `Some` replaces the default board style wholesale, `None` uses defaults.
/// Colors carry alpha so the plate can go fully transparent over a game.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// 3D controller viewer only — this pin's model colour overrides, keyed
    /// like the node's `materials` param (`"body"` → `[r, g, b]`). Groups
    /// missing from the map fall back to the module's shared scheme;
    /// `None` = follow the module's colours entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub c3d_materials: Option<serde_json::Map<String, serde_json::Value>>,
    /// 3D controller viewer only — this pin's model choice. `None` = follow
    /// the module's model; `Some("")` = auto-detect from the connected
    /// device; `Some(name)` = that model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub c3d_model: Option<String>,
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
        #[serde(default, skip_serializing_if = "Anchor::is_auto")]
        anchor: Anchor,
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
        /// Chosen shared-set / `gp:<pin>` icon key ("" = use `svg_data`). When set,
        /// it wins over `svg_data` and resolves dynamically (gp icons restyle to
        /// the current pad) — the unified icon picker writes one or the other.
        #[serde(default)]
        icon_key: String,
        #[serde(default, skip_serializing_if = "Anchor::is_auto")]
        anchor: Anchor,
    },
    Rect {
        pos: [f32; 2],
        size: [f32; 2],
        fill: [u8; 4],
        stroke: [u8; 4],
        stroke_px: f32,
        corner_radius: f32,
        #[serde(default, skip_serializing_if = "Anchor::is_auto")]
        anchor: Anchor,
    },
    Ellipse {
        pos: [f32; 2],
        size: [f32; 2],
        fill: [u8; 4],
        stroke: [u8; 4],
        stroke_px: f32,
        #[serde(default, skip_serializing_if = "Anchor::is_auto")]
        anchor: Anchor,
    },
    Line {
        a: [f32; 2],
        b: [f32; 2],
        stroke: [u8; 4],
        stroke_px: f32,
        #[serde(default, skip_serializing_if = "Anchor::is_auto")]
        anchor: Anchor,
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

    /// This decoration's explicit per-axis anchor (`{ Auto, Auto }` = derive
    /// from placement — the default).
    pub fn anchor(&self) -> Anchor {
        match self {
            LayoutDecoration::Text { anchor, .. }
            | LayoutDecoration::Svg { anchor, .. }
            | LayoutDecoration::Rect { anchor, .. }
            | LayoutDecoration::Ellipse { anchor, .. }
            | LayoutDecoration::Line { anchor, .. } => *anchor,
        }
    }

    /// Set this decoration's explicit per-axis anchor.
    pub fn set_anchor(&mut self, a: Anchor) {
        match self {
            LayoutDecoration::Text { anchor, .. }
            | LayoutDecoration::Svg { anchor, .. }
            | LayoutDecoration::Rect { anchor, .. }
            | LayoutDecoration::Ellipse { anchor, .. }
            | LayoutDecoration::Line { anchor, .. } => *anchor = a,
        }
    }

    /// A copy re-anchored within an arbitrary reference frame: the decoration's
    /// geometry is expressed in the authored frame `[ref_ao, ref_ao+ref_as]` and
    /// remapped to the resolved frame `[ref_co, ref_co+ref_cs]`, applying its own
    /// per-axis anchor within that frame. For screen anchoring the frame is the
    /// whole overlay (`[0,0], authored → [0,0], current`); for anchor-to-object
    /// it is the target's authored → resolved rect. Rect/Ellipse/SVG take the
    /// resolved pos+size; Line maps both endpoints within the resolved bbox; Text
    /// only repositions (its font size is DPI-fixed, so it never scales).
    pub(crate) fn resolved_within(
        &self,
        ref_ao: [f32; 2],
        ref_as: [f32; 2],
        ref_co: [f32; 2],
        ref_cs: [f32; 2],
    ) -> LayoutDecoration {
        let (p, s) = self.bbox();
        let rel = [p[0] - ref_ao[0], p[1] - ref_ao[1]];
        let (rp, rs) = resolve_anchored_rect(rel, s, ref_as, ref_cs, false, self.anchor());
        let np = [ref_co[0] + rp[0], ref_co[1] + rp[1]];
        let ns = rs;
        let sx = if s[0] > 0.5 { ns[0] / s[0] } else { 1.0 };
        let sy = if s[1] > 0.5 { ns[1] / s[1] } else { 1.0 };
        let map = |pt: [f32; 2]| [np[0] + (pt[0] - p[0]) * sx, np[1] + (pt[1] - p[1]) * sy];
        let mut d = self.clone();
        match &mut d {
            LayoutDecoration::Text { pos, .. } => *pos = np,
            LayoutDecoration::Svg { pos, size, .. }
            | LayoutDecoration::Rect { pos, size, .. }
            | LayoutDecoration::Ellipse { pos, size, .. } => {
                *pos = np;
                *size = ns;
            }
            LayoutDecoration::Line { a, b, .. } => {
                *a = map(*a);
                *b = map(*b);
            }
        }
        d
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
    /// Explicit per-axis anchor (`{ Auto, Auto }` = derive from placement).
    pub fn anchor(&self) -> Anchor {
        match self {
            LayoutItem::Module(m) => m.anchor,
            LayoutItem::Deco(d)   => d.anchor(),
        }
    }
    /// Set the explicit per-axis anchor on this item (module or decoration).
    pub fn set_anchor(&mut self, a: Anchor) {
        match self {
            LayoutItem::Module(m) => m.anchor = a,
            LayoutItem::Deco(d)   => d.set_anchor(a),
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
    /// Snap grid as the number of cells per axis (overlays are screen-anchored,
    /// so the grid is a % division, not px): 30 → 3.33%, 18 → 5.56%. Constrained
    /// to multiples of 3 (floor 9) so the grid always nests inside the 3×3 anchor
    /// zones — their edges at 1/3 and 2/3 land on grid lines. Legacy layouts
    /// stored a px value here; reinterpreted as a cell count until reset.
    #[serde(default = "default_overlay_divisions")]
    pub snap_grid_px: u32,
    /// Overlay viewport size (logical px) this layout was last edited at. Element
    /// positions are stored at this size; [`resolve_anchored_rect`] re-anchors
    /// them to the CURRENT viewport so a preset adapts across resolutions and
    /// aspect ratios. `None` on legacy layouts ⇒ treated as the current viewport
    /// (identity, i.e. today's behaviour) until first edited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authored_size: Option<[f32; 2]>,
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
            snap_grid_px: default_overlay_divisions(),
            authored_size: None,
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

/// The anchor point + stretch an `Auto` axis derives for a span `[pos, pos+size)`
/// on an axis of length `authored`: the point comes from which 3×3 zone the
/// span's CENTRE sits in (start / centre / end); stretch is on when the span
/// covers ≥2 zones. Exposed so the anchor picker can show what `Auto` maps to.
pub(crate) fn zone_axis(pos: f32, size: f32, authored: f32) -> (AnchorAxis, bool) {
    if authored <= 1.0 {
        return (AnchorAxis::Start, false);
    }
    let third = authored / 3.0;
    let eps = 1e-3;
    let first = ((pos / third).floor() as i32).clamp(0, 2);
    let last = ((((pos + size) - eps) / third).floor() as i32).clamp(0, 2);
    let stretch = first != last;
    let center_zone = (((pos + size * 0.5) / third).floor() as i32).clamp(0, 2);
    let point = match center_zone {
        0 => AnchorAxis::Start,
        2 => AnchorAxis::End,
        _ => AnchorAxis::Center,
    };
    (point, stretch)
}

/// Re-anchor an overlay element from the size it was authored at to the current
/// overlay viewport. Per axis, the anchor `point` (`Start`/`Center`/`End`) fixes
/// which edge/centre stays put, and `stretch` decides whether the size scales
/// proportionally with the viewport (keeping the same RELATIVE width/height) or
/// stays a fixed physical size. `Auto` derives both from the element's span in
/// the 3×3 zone grid (single zone → that zone, no stretch; spanning ≥2 zones →
/// stretch). With `keep_aspect`, both axes scale by the smaller factor so a
/// stretched element keeps its shape and re-centres.
///
/// Inputs/outputs are overlay-LOCAL logical px (the caller adds the overlay
/// origin). Identity when `authored` is unset/degenerate or equals `current`.
pub(crate) fn resolve_anchored_rect(
    pos: [f32; 2],
    size: [f32; 2],
    authored: [f32; 2],
    current: [f32; 2],
    keep_aspect: bool,
    anchor: Anchor,
) -> ([f32; 2], [f32; 2]) {
    // One axis: returns (new_pos, new_size, stretched).
    fn axis(point: AnchorAxis, stretch: bool, pos: f32, size: f32, authored: f32, current: f32) -> (f32, f32, bool) {
        if authored <= 1.0 || current <= 1.0 {
            return (pos, size, false);
        }
        let hi = pos + size;
        // Auto derives both; an explicit point uses the given stretch flag.
        let (p, st) = match point {
            AnchorAxis::Auto => zone_axis(pos, size, authored),
            pt => (pt, stretch),
        };
        // Stretch keeps the same relative size ⇒ scale the dimension by the
        // viewport ratio. Otherwise the physical size is preserved (DPI).
        let size_new = if st { size * (current / authored) } else { size };
        let new_pos = match p {
            AnchorAxis::Start => pos, // fixed near margin (physical px)
            AnchorAxis::End => current - (authored - hi) - size_new, // fixed far margin
            AnchorAxis::Center => {
                // hold the centre at the same fraction of the axis.
                let center_frac = (pos + size * 0.5) / authored;
                center_frac * current - size_new * 0.5
            }
            AnchorAxis::Auto => unreachable!("resolved above"),
        };
        (new_pos, size_new.max(1.0), st)
    }

    let (nx, nw, sx) = axis(anchor.x, anchor.stretch_x, pos[0], size[0], authored[0], current[0]);
    let (ny, nh, sy) = axis(anchor.y, anchor.stretch_y, pos[1], size[1], authored[1], current[1]);

    if keep_aspect && (sx || sy) {
        let fx = if size[0] > 0.5 { nw / size[0] } else { 1.0 };
        let fy = if size[1] > 0.5 { nh / size[1] } else { 1.0 };
        let f = fx.min(fy);
        let aw = size[0] * f;
        let ah = size[1] * f;
        // Re-centre the uniformly-scaled element within its resolved band.
        let cx = nx + nw * 0.5;
        let cy = ny + nh * 0.5;
        return ([cx - aw * 0.5, cy - ah * 0.5], [aw, ah]);
    }
    ([nx, ny], [nw, nh])
}

/// Cap on anchor-to-object chain depth (guards cycles / runaway nesting).
const ANCHOR_LINK_MAX_DEPTH: u8 = 8;

/// The reference frame item `idx` resolves within, as
/// `(auth_origin, auth_size, cur_origin, cur_size)`. For a normally-anchored
/// element that is the whole overlay (`[0,0], authored → [0,0], current`); for
/// one that follows another (anchor-to), the target's authored bbox → resolved
/// rect. Falls back to the screen frame if the target is missing, is itself, or
/// the chain is too deep (cycle guard).
fn item_ref_frame(
    items: &[LayoutItem],
    idx: usize,
    authored: [f32; 2],
    current: [f32; 2],
    depth: u8,
) -> ([f32; 2], [f32; 2], [f32; 2], [f32; 2]) {
    if depth < ANCHOR_LINK_MAX_DEPTH {
        if let Some(to) = items.get(idx).and_then(|it| it.anchor().to).filter(|&t| t != 0) {
            if let Some(j) = items
                .iter()
                .position(|it| { let a = it.anchor(); a.id != 0 && a.id == to })
            {
                if j != idx {
                    let (t_ap, t_as) = items[j].bbox();
                    let (t_cp, t_cs) = resolve_layout_rect_inner(items, j, authored, current, depth + 1);
                    return (t_ap, t_as, t_cp, t_cs);
                }
            }
        }
    }
    ([0.0, 0.0], authored, [0.0, 0.0], current)
}

fn resolve_layout_rect_inner(
    items: &[LayoutItem],
    idx: usize,
    authored: [f32; 2],
    current: [f32; 2],
    depth: u8,
) -> ([f32; 2], [f32; 2]) {
    let Some(it) = items.get(idx) else { return ([0.0, 0.0], [0.0, 0.0]) };
    let (ao, as_, co, cs) = item_ref_frame(items, idx, authored, current, depth);
    let (p, s) = it.bbox();
    let rel = [p[0] - ao[0], p[1] - ao[1]];
    let keep = matches!(it, LayoutItem::Module(m) if m.keep_aspect);
    let (rp, rs) = resolve_anchored_rect(rel, s, as_, cs, keep, it.anchor());
    ([co[0] + rp[0], co[1] + rp[1]], rs)
}

/// Resolve item `idx`'s rect (overlay-local px) at the current viewport,
/// following any anchor-to-object link. Equivalent to [`resolve_anchored_rect`]
/// for a screen-anchored item, plus target-relative placement for a follower.
pub(crate) fn resolve_layout_rect(
    items: &[LayoutItem],
    idx: usize,
    authored: [f32; 2],
    current: [f32; 2],
) -> ([f32; 2], [f32; 2]) {
    resolve_layout_rect_inner(items, idx, authored, current, 0)
}

/// The fully re-anchored decoration for item `idx` (following any anchor-to
/// link), ready to paint. Returns the decoration unchanged if `idx` is not a
/// decoration.
pub(crate) fn resolve_layout_deco(
    items: &[LayoutItem],
    idx: usize,
    authored: [f32; 2],
    current: [f32; 2],
) -> LayoutDecoration {
    let Some(LayoutItem::Deco(d)) = items.get(idx) else {
        // Caller only invokes this for decorations; return a harmless default.
        return LayoutDecoration::Rect {
            pos: [0.0, 0.0], size: [0.0, 0.0], fill: [0, 0, 0, 0],
            stroke: [0, 0, 0, 0], stroke_px: 0.0, corner_radius: 0.0,
            anchor: Anchor::default(),
        };
    };
    let (ao, as_, co, cs) = item_ref_frame(items, idx, authored, current, 0);
    d.resolved_within(ao, as_, co, cs)
}

/// The smallest unused link id in `items` (max existing + 1, never 0). Used when
/// establishing an anchor-to link so the target gets a stable id.
pub(crate) fn next_anchor_link_id(items: &[LayoutItem]) -> u64 {
    items.iter().map(|it| it.anchor().id).max().unwrap_or(0) + 1
}

/// Does the anchor-to chain starting at `from_idx` reach any index in `banned`
/// (within the depth cap)? Used before creating a link `follower → target` to
/// reject a cycle: linking is unsafe when the target already follows the
/// follower (directly or transitively).
pub(crate) fn anchor_chain_reaches(items: &[LayoutItem], from_idx: usize, banned: &[usize]) -> bool {
    let mut cur = from_idx;
    for _ in 0..ANCHOR_LINK_MAX_DEPTH {
        let Some(to) = items.get(cur).and_then(|it| it.anchor().to).filter(|&t| t != 0) else {
            return false;
        };
        let Some(j) = items.iter().position(|it| { let a = it.anchor(); a.id != 0 && a.id == to }) else {
            return false;
        };
        if banned.contains(&j) {
            return true;
        }
        cur = j;
    }
    true // chain too deep → treat as a cycle
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
    /// Info-overlay pins contributed BY this sub-patch — the pins a tab's info
    /// overlay exposes from elements inside this sub-patch. Stored here (rather
    /// than only on the tab) so they travel with the sub-patch into a `.fxsp`
    /// preset. `source_path` is empty (they reference `inner_node_id` inside this
    /// sub-patch's own `snarl`); the tab re-materializes them with the outer
    /// node's id. See `attribute_overlays_into_subpatches` /
    /// `materialize_subpatch_overlays`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlay_items: Vec<LayoutItem>,
    /// Config-overlay counterpart to `overlay_items` (the M3 tweak-pins).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config_items: Vec<LayoutItem>,
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

// ── Overlay-pin ↔ sub-patch attribution ─────────────────────────────────────
//
// Info/config overlay pins live on the tab (`OverlayLayout`) during a session,
// but pins whose `source_path == [sp]` reference a first-level sub-patch node.
// So overlays travel with a `.fxsp` preset, these two inverse transforms move
// such pins into the sub-patch on SAVE and restore them on LOAD. Tab-canvas pins
// (`source_path == []`) and decorations always stay on the tab.

/// SAVE side: move every overlay/config pin that references a first-level
/// sub-patch node into that node's `overlay_items` / `config_items` (clearing
/// `source_path`). Tab-canvas pins stay. Orphan pins (source_path points at a
/// node that is missing or not a sub-patch) are dropped — they can't render.
/// Operate on a CLONE of the snarl + layouts at serialize time; never the live
/// state.
pub fn attribute_overlays_into_subpatches(
    snarl: &mut Snarl<NodeData>,
    overlay: &mut OverlayLayout,
    config: &mut OverlayLayout,
) {
    // The tab overlays are the live source of truth. A sub-patch node may still
    // carry stale `overlay_items` from when it was loaded; clear them so the
    // rebuild below can't duplicate, and so a pin the user deleted on the tab
    // actually disappears from the preset.
    let ids: Vec<egui_snarl::NodeId> = snarl.nodes_ids_data().map(|(id, _)| id).collect();
    for id in ids {
        if let Some(subp) = snarl.get_node_mut(id).and_then(|n| n.subpatch.as_mut()) {
            subp.overlay_items.clear();
            subp.config_items.clear();
        }
    }
    attribute_layout_into_subpatches(snarl, &mut overlay.items, false);
    attribute_layout_into_subpatches(snarl, &mut config.items, true);
}

fn attribute_layout_into_subpatches(
    snarl: &mut Snarl<NodeData>,
    items: &mut Vec<LayoutItem>,
    into_config: bool,
) {
    for item in std::mem::take(items) {
        // Only single-level module pins are attributable; decorations and
        // tab-canvas pins (empty source_path) stay on the tab.
        let sp = match &item {
            LayoutItem::Module(m) if m.source_path.len() == 1 => Some(m.source_path[0]),
            _ => None,
        };
        let Some(sp) = sp else { items.push(item); continue; };
        let subp = snarl.get_node_mut(egui_snarl::NodeId(sp))
            .and_then(|n| n.subpatch.as_mut());
        match (subp, item) {
            (Some(subp), LayoutItem::Module(mut m)) => {
                m.source_path.clear();
                if into_config { subp.config_items.push(LayoutItem::Module(m)); }
                else { subp.overlay_items.push(LayoutItem::Module(m)); }
            }
            // Node missing or not a sub-patch → orphan, drop.
            _ => {}
        }
    }
}

/// LOAD side: for each first-level sub-patch node, append its stored
/// `overlay_items` / `config_items` onto the tab's `overlay` / `config` with
/// `source_path` set to that node's id. Deduped by
/// `(source_path, inner_node_id, element_id)` so re-materializing (or loading a
/// preset already present) never doubles pins.
pub fn materialize_subpatch_overlays(
    snarl: &Snarl<NodeData>,
    overlay: &mut OverlayLayout,
    config: &mut OverlayLayout,
) {
    for (node_id, n) in snarl.nodes_ids_data() {
        let Some(subp) = n.value.subpatch.as_ref() else { continue };
        materialize_layout(&subp.overlay_items, &mut overlay.items, node_id.0);
        materialize_layout(&subp.config_items, &mut config.items, node_id.0);
    }
}

/// COPY (not move) a tab's overlay/config pins that reference sub-patch node
/// `sp_id` into `target`'s stored item lists (source_path cleared), for baking a
/// `.fxsp` preset without disturbing the live tab overlays. Clear `target`'s
/// lists first if you want the tab overlays to be the sole source of truth.
pub fn collect_overlays_for_subpatch(
    sp_id: usize,
    overlay: &OverlayLayout,
    config: &OverlayLayout,
    target: &mut UiSubPatch,
) {
    collect_layout_for(sp_id, &overlay.items, &mut target.overlay_items);
    collect_layout_for(sp_id, &config.items, &mut target.config_items);
}

fn collect_layout_for(sp_id: usize, src: &[LayoutItem], dst: &mut Vec<LayoutItem>) {
    for item in src {
        if let LayoutItem::Module(m) = item {
            if m.source_path == [sp_id] {
                let mut m = m.clone();
                m.source_path.clear();
                dst.push(LayoutItem::Module(m));
            }
        }
    }
}

fn materialize_layout(src: &[LayoutItem], dst: &mut Vec<LayoutItem>, sp: usize) {
    for item in src {
        let LayoutItem::Module(m) = item else { continue };
        let dup = dst.iter().any(|it| matches!(it, LayoutItem::Module(e)
            if e.source_path == [sp]
            && e.inner_node_id == m.inner_node_id
            && e.element_id == m.element_id));
        if dup { continue; }
        let mut m = m.clone();
        m.source_path = vec![sp];
        dst.push(LayoutItem::Module(m));
    }
}

fn default_snap_grid_px() -> u32 { 8 }
/// Default overlay snap: 60 cells per axis (= 1.67% each) — a fine grid by
/// default (lower % = finer). Always a multiple of 3 so the grid nests inside
/// the 3×3 anchor zones (edges at 1/3 and 2/3 land on grid lines); floor 9.
fn default_overlay_divisions() -> u32 { 60 }

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
            overlay_items: vec![],
            config_items: vec![],
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

    // ── resolve_anchored_rect ──
    fn anchor(pos: [f32; 2], size: [f32; 2], authored: [f32; 2], current: [f32; 2]) -> ([f32; 2], [f32; 2]) {
        resolve_anchored_rect(pos, size, authored, current, false, Anchor::default())
    }

    #[test]
    fn anchor_identity_when_same_size() {
        let a = [900.0, 300.0];
        assert_eq!(anchor([250.0, 40.0], [60.0, 30.0], a, a), ([250.0, 40.0], [60.0, 30.0]));
    }

    #[test]
    fn anchor_left_keeps_margin() {
        // Left third (authored width 900 → thirds at 300): stays put, same size.
        let (p, s) = anchor([20.0, 20.0], [80.0, 40.0], [900.0, 300.0], [1800.0, 600.0]);
        assert!((p[0] - 20.0).abs() < 0.5, "x {}", p[0]);
        assert!((s[0] - 80.0).abs() < 0.5, "w {}", s[0]);
    }

    #[test]
    fn anchor_right_keeps_margin() {
        // Right third: right margin (900-(800+80)=20) preserved on a wider screen.
        let (p, s) = anchor([800.0, 20.0], [80.0, 40.0], [900.0, 300.0], [1800.0, 300.0]);
        assert!((p[0] - (1800.0 - 20.0 - 80.0)).abs() < 0.5, "x {}", p[0]);
        assert!((s[0] - 80.0).abs() < 0.5, "w {}", s[0]);
    }

    #[test]
    fn anchor_center_scales_proportionally() {
        // Centre third, dead-centre element: stays centred, same size.
        let (p, s) = anchor([420.0, 20.0], [60.0, 40.0], [900.0, 300.0], [1800.0, 300.0]);
        assert!((p[0] - (900.0 - 30.0)).abs() < 0.5, "x {} (want centred at 900)", p[0]);
        assert!((s[0] - 60.0).abs() < 0.5, "w {}", s[0]);
    }

    #[test]
    fn anchor_span_auto_stretches_proportionally() {
        // Spans all three columns → Auto stretches: width scales with the
        // viewport (same relative width) and it stays centred.
        let (p, s) = anchor([30.0, 20.0], [840.0, 40.0], [900.0, 300.0], [1800.0, 300.0]);
        // 840/900 relative width kept → 840 * 2 = 1680, centred at 900 → x = 60.
        assert!((s[0] - 1680.0).abs() < 0.5, "w {}", s[0]);
        assert!((p[0] - 60.0).abs() < 0.5, "x {}", p[0]);
    }

    #[test]
    fn anchor_keep_aspect_scales_uniformly() {
        // A full-width span on a screen twice as wide but same height: without
        // keep_aspect it would stretch 2× horizontally only; with it, uniform.
        let (_, s) = resolve_anchored_rect([0.0, 130.0], [900.0, 40.0], [900.0, 300.0], [1800.0, 300.0], true, Anchor::default());
        // horizontal factor ~2, vertical ~1 → min = 1 → size unchanged.
        assert!((s[0] - 900.0).abs() < 1.0 && (s[1] - 40.0).abs() < 1.0, "size {:?}", s);
    }

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
                    menu_style_override: None,
                    keep_aspect: false,
                    anchor: Anchor { x: AnchorAxis::End, y: AnchorAxis::Start, stretch_x: true, ..Default::default() },
                }),
                LayoutItem::Deco(LayoutDecoration::Rect {
                    pos: [5.0, 6.0],
                    size: [100.0, 50.0],
                    fill: [10, 20, 30, 200],
                    stroke: [1, 1, 1, 255],
                    stroke_px: 2.0,
                    corner_radius: 4.0,
                    anchor: Anchor { x: AnchorAxis::Center, y: AnchorAxis::Start, ..Default::default() },
                }),
            ],
            snap_enabled: true,
            snap_grid_px: 16,
            authored_size: Some([1920.0, 1080.0]),
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
                assert_eq!(pin.anchor, Anchor { x: AnchorAxis::End, y: AnchorAxis::Start, stretch_x: true, ..Default::default() });
            }
            other => panic!("expected Module, got {other:?}"),
        }
        match &back.items[1] {
            LayoutItem::Deco(d @ LayoutDecoration::Rect { corner_radius, .. }) => {
                assert_eq!(*corner_radius, 4.0);
                assert_eq!(d.anchor(), Anchor { x: AnchorAxis::Center, y: AnchorAxis::Start, ..Default::default() });
            }
            other => panic!("expected Rect deco, got {other:?}"),
        }
    }

    #[test]
    fn anchor_default_is_auto_and_skips_serializing() {
        // Anchor::default() round-trips as absent (no "anchor" key), so legacy
        // and un-anchored layouts stay byte-identical.
        assert!(Anchor::default().is_auto());
        let json = serde_json::to_string(&overlay_pin(1, vec![])).unwrap();
        assert!(!json.contains("anchor"), "auto anchor must not serialize: {json}");
    }

    #[test]
    fn anchor_explicit_end_overrides_left_placement() {
        // An element sitting in the LEFT third but anchored End tracks the right
        // edge (fixed right margin), not the left — proving explicit beats zone.
        let (p, _) = resolve_anchored_rect(
            [20.0, 20.0], [80.0, 40.0], [900.0, 300.0], [1800.0, 300.0], false,
            Anchor { x: AnchorAxis::End, y: AnchorAxis::Auto, ..Default::default() },
        );
        let right_margin = 900.0 - (20.0 + 80.0); // 800
        assert!((p[0] - (1800.0 - right_margin - 80.0)).abs() < 0.5, "x {}", p[0]);
    }

    #[test]
    fn anchor_point_and_stretch_are_independent() {
        // Left-anchored (fixed near margin) AND stretched: the left margin stays
        // put while the width scales proportionally — the two are orthogonal.
        let (p, s) = resolve_anchored_rect(
            [20.0, 20.0], [80.0, 40.0], [900.0, 300.0], [1800.0, 300.0], false,
            Anchor { x: AnchorAxis::Start, stretch_x: true, y: AnchorAxis::Auto, ..Default::default() },
        );
        assert!((p[0] - 20.0).abs() < 0.5, "x {}", p[0]);      // near margin fixed
        assert!((s[0] - 160.0).abs() < 0.5, "w {}", s[0]);      // 80 * (1800/900) = 160
    }

    #[test]
    fn anchor_bottom_center_stretch_x() {
        // The case the picker must express: anchored bottom-centre, stretched
        // horizontally — centred X with proportional width, bottom margin fixed.
        let (p, s) = resolve_anchored_rect(
            [450.0, 250.0], [300.0, 40.0], [1200.0, 300.0], [2400.0, 300.0], false,
            Anchor { x: AnchorAxis::Center, stretch_x: true, y: AnchorAxis::End, ..Default::default() },
        );
        assert!((s[0] - 600.0).abs() < 0.5, "w {}", s[0]);      // 300 * 2 = 600 (relative width kept)
        // centre fraction (450+150)/1200 = 0.5 → centre at 1200, x = 1200 - 300 = 900.
        assert!((p[0] - 900.0).abs() < 0.5, "x {}", p[0]);
        // bottom margin 300-(250+40)=10 preserved (no vertical resize).
        assert!((s[1] - 40.0).abs() < 0.5 && (p[1] - 250.0).abs() < 0.5, "y {:?} {:?}", p, s);
    }

    // ── anchor-to-object ──
    fn rect_item(pos: [f32; 2], size: [f32; 2], anchor: Anchor) -> LayoutItem {
        LayoutItem::Deco(LayoutDecoration::Rect {
            pos, size, fill: [0, 0, 0, 0], stroke: [0, 0, 0, 0], stroke_px: 0.0,
            corner_radius: 0.0, anchor,
        })
    }

    #[test]
    fn anchor_to_object_follows_target_frame() {
        // Target is a full-width strip → stretches with the screen (id = 1).
        let target = rect_item([0.0, 0.0], [1000.0, 100.0], Anchor { id: 1, ..Default::default() });
        // Follower sits near the target's right edge, End-anchored WITHIN it.
        let follower = rect_item(
            [900.0, 10.0], [50.0, 50.0],
            Anchor { x: AnchorAxis::End, to: Some(1), ..Default::default() },
        );
        let items = vec![target, follower];
        // Screen doubles in width: target right edge 1000→2000, so the follower's
        // 50px right margin inside the frame is preserved → x = 1900.
        let (p, s) = resolve_layout_rect(&items, 1, [1000.0, 1000.0], [2000.0, 1000.0]);
        assert!((p[0] - 1900.0).abs() < 0.5, "x {}", p[0]);
        assert!((p[1] - 10.0).abs() < 0.5, "y {}", p[1]);
        assert!((s[0] - 50.0).abs() < 0.5 && (s[1] - 50.0).abs() < 0.5, "size {:?}", s);
    }

    #[test]
    fn anchor_to_missing_target_falls_back_to_screen() {
        // `to` points at an id that no item carries → resolve as screen-anchored.
        let follower = rect_item(
            [20.0, 20.0], [80.0, 40.0],
            Anchor { x: AnchorAxis::Start, to: Some(999), ..Default::default() },
        );
        let items = vec![follower];
        let (p, _) = resolve_layout_rect(&items, 0, [900.0, 300.0], [1800.0, 300.0]);
        assert!((p[0] - 20.0).abs() < 0.5, "x {}", p[0]); // Start keeps left margin
    }

    #[test]
    fn anchor_chain_reaches_detects_cycle() {
        // item0 has id 1; item1 follows id 1 (→ item0). Linking item0 → item1
        // would cycle, so a chain walk from item1 must reach the banned item0.
        let item0 = rect_item([0.0, 0.0], [10.0, 10.0], Anchor { id: 1, ..Default::default() });
        let item1 = rect_item([0.0, 0.0], [10.0, 10.0], Anchor { to: Some(1), ..Default::default() });
        let items = vec![item0, item1];
        assert!(anchor_chain_reaches(&items, 1, &[0]));
        assert!(!anchor_chain_reaches(&items, 0, &[1])); // item0 follows nothing
    }

    /// Fields absent from older documents (`source_path`, `element_id`,
    /// `size`, snap settings) must default in instead of failing the load —
    /// this is the exact shape of a pre-overlay sub-patch layout pin, so it
    /// also proves existing .fxp/.fxsp files keep loading.
    #[test]
    fn overlay_layout_defaults_when_absent() {
        let json = r#"{"items":[{"Module":{"inner_node_id":2,"pos":[1.0,2.0]}}]}"#;
        let layout: OverlayLayout = serde_json::from_str(json).unwrap();
        assert_eq!(layout.snap_grid_px, default_overlay_divisions());
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

    fn subpatch_node() -> NodeData {
        NodeData {
            module_id: "subpatch".into(),
            display_name: "SP".into(),
            category: "Patch".into(),
            inputs: vec![],
            outputs: vec![],
            params: HashMap::new(),
            subpatch: Some(Box::new(UiSubPatch::default())),
            extra: NodeExtra::default(),
        }
    }

    fn overlay_pin(inner: usize, source_path: Vec<usize>) -> LayoutItem {
        LayoutItem::Module(ExposedModule {
            inner_node_id: inner,
            element_id: "value".into(),
            pos: [0.0, 0.0],
            size: [40.0, 20.0],
            text_override: None,
            switch_override: None,
            graph_override: None,
            source_path,
            iv_style_override: None,
            menu_style_override: None,
            keep_aspect: false,
            anchor: Anchor::default(),
        })
    }

    #[test]
    fn overlay_pins_attribute_and_materialize_round_trip() {
        let mut snarl: Snarl<NodeData> = Snarl::new();
        let sp_id = snarl.insert_node(eframe::egui::pos2(0.0, 0.0), subpatch_node());
        let sp = sp_id.0;

        let mut overlay = OverlayLayout::default();
        let mut config = OverlayLayout::default();
        overlay.items.push(overlay_pin(7, vec![sp])); // sub-patch-sourced
        overlay.items.push(overlay_pin(3, vec![]));   // tab-canvas
        config.items.push(overlay_pin(9, vec![sp]));

        attribute_overlays_into_subpatches(&mut snarl, &mut overlay, &mut config);

        // Sub-patch pin left the tab (with source_path cleared); tab-canvas stays.
        assert_eq!(overlay.items.len(), 1);
        assert!(matches!(&overlay.items[0],
            LayoutItem::Module(m) if m.source_path.is_empty() && m.inner_node_id == 3));
        assert!(config.items.is_empty());
        let subp = snarl.get_node(sp_id).unwrap().subpatch.as_ref().unwrap();
        assert_eq!(subp.overlay_items.len(), 1);
        assert!(matches!(&subp.overlay_items[0],
            LayoutItem::Module(m) if m.source_path.is_empty() && m.inner_node_id == 7));
        assert_eq!(subp.config_items.len(), 1);

        // Materialize onto a fresh tab → the pin reappears with source_path=[sp].
        let mut o2 = OverlayLayout::default();
        let mut c2 = OverlayLayout::default();
        materialize_subpatch_overlays(&snarl, &mut o2, &mut c2);
        assert_eq!(o2.items.len(), 1);
        assert!(matches!(&o2.items[0],
            LayoutItem::Module(m) if m.source_path == [sp] && m.inner_node_id == 7));
        assert_eq!(c2.items.len(), 1);

        // Dedup: a second materialize adds nothing.
        materialize_subpatch_overlays(&snarl, &mut o2, &mut c2);
        assert_eq!(o2.items.len(), 1);
        assert_eq!(c2.items.len(), 1);
    }

    #[test]
    fn attribute_drops_orphan_and_keeps_tab_canvas() {
        let mut snarl: Snarl<NodeData> = Snarl::new();
        let mut overlay = OverlayLayout::default();
        let mut config = OverlayLayout::default();
        overlay.items.push(overlay_pin(1, vec![999])); // orphan (no such node)
        overlay.items.push(overlay_pin(2, vec![]));    // tab-canvas
        attribute_overlays_into_subpatches(&mut snarl, &mut overlay, &mut config);
        assert_eq!(overlay.items.len(), 1);
        assert!(matches!(&overlay.items[0],
            LayoutItem::Module(m) if m.inner_node_id == 2));
    }
}
