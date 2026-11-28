//! This module provides functionality for showing [`Snarl`] graph in [`Ui`].

use std::{collections::HashMap, hash::Hash};

use egui::{
    Align, Color32, CornerRadius, Frame, Id, LayerId, Layout, Margin, Modifiers,
    PointerButton, Popup, PopupAnchor, PopupKind, Pos2, Rect, Scene, Sense, Shape, Stroke,
    StrokeKind, Style, Ui, UiBuilder, UiKind, UiStackInfo, Vec2,
    collapsing_header::paint_default_icon,
    emath::{GuiRounding, TSTransform},
    epaint::Shadow,
    pos2,
    response::Flags,
    vec2,
};
use egui_scale::EguiScale;
use smallvec::SmallVec;

use crate::{InPin, InPinId, Node, NodeId, OutPin, OutPinId, Snarl, ui::wire::WireId};

mod background_pattern;
mod grid;
pub mod route;
mod pin;
mod scale;
mod state;
pub mod touch;
mod viewer;
mod wire;

use self::{
    grid::{GridDrag, GridMemory},
    state::{HoldOn, NewWires, RowHeights, WireHold, WireHoldStep},
    wire::{draw_wire, hit_wire, pick_wire_style},
};

pub use self::{
    background_pattern::{BackgroundPattern, Grid},
    grid::{GridLayout, GridStyle, RouteSet, WireRouting, wire_route},
    pin::{AnyPin, AnyPins, PinInfo, PinShape, PinWireInfo, SnarlPin},
    state::{get_selected_nodes, NodeState, SnarlState},
    viewer::SnarlViewer,
    wire::{WireLayer, WireStyle},
};

/// Controls how header, pins, body and footer are placed in the node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "egui-probe", derive(egui_probe::EguiProbe))]
pub enum NodeLayoutKind {
    /// Input pins, body and output pins are placed horizontally.
    /// With header on top and footer on bottom.
    ///
    /// +---------------------+
    /// |       Header        |
    /// +----+-----------+----+
    /// | In |           | Out|
    /// | In |   Body    | Out|
    /// | In |           | Out|
    /// | In |           |    |
    /// +----+-----------+----+
    /// |       Footer        |
    /// +---------------------+
    ///
    #[default]
    Coil,

    /// All elements are placed in vertical stack.
    /// Header is on top, then input pins, body, output pins and footer.
    ///
    /// +---------------------+
    /// |       Header        |
    /// +---------------------+
    /// | In                  |
    /// | In                  |
    /// | In                  |
    /// | In                  |
    /// +---------------------+
    /// |       Body          |
    /// +---------------------+
    /// |                 Out |
    /// |                 Out |
    /// |                 Out |
    /// +---------------------+
    /// |       Footer        |
    /// +---------------------+
    Sandwich,

    /// All elements are placed in vertical stack.
    /// Header is on top, then output pins, body, input pins and footer.
    ///
    /// +---------------------+
    /// |       Header        |
    /// +---------------------+
    /// |                 Out |
    /// |                 Out |
    /// |                 Out |
    /// +---------------------+
    /// |       Body          |
    /// +---------------------+
    /// | In                  |
    /// | In                  |
    /// | In                  |
    /// | In                  |
    /// +---------------------+
    /// |       Footer        |
    /// +---------------------+
    FlippedSandwich,
    // TODO: Add vertical layouts.
}

/// Controls how node elements are laid out.
///
///
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "egui-probe", derive(egui_probe::EguiProbe))]
pub struct NodeLayout {
    /// Controls method of laying out node elements.
    pub kind: NodeLayoutKind,

    /// Controls minimal height of pin rows.
    pub min_pin_row_height: f32,

    /// Controls how pin rows heights are set.
    /// If true, all pin rows will have the same height, matching the largest content.
    /// False by default.
    pub equal_pin_row_heights: bool,
}

impl NodeLayout {
    /// Creates new [`NodeLayout`] with `Coil` kind and flexible pin heights.
    #[must_use]
    #[inline]
    pub const fn coil() -> Self {
        NodeLayout {
            kind: NodeLayoutKind::Coil,
            min_pin_row_height: 0.0,
            equal_pin_row_heights: false,
        }
    }

    /// Creates new [`NodeLayout`] with `Sandwich` kind and flexible pin heights.
    #[must_use]
    #[inline]
    pub const fn sandwich() -> Self {
        NodeLayout {
            kind: NodeLayoutKind::Sandwich,
            min_pin_row_height: 0.0,
            equal_pin_row_heights: false,
        }
    }

    /// Creates new [`NodeLayout`] with `FlippedSandwich` kind and flexible pin heights.
    #[must_use]
    #[inline]
    pub const fn flipped_sandwich() -> Self {
        NodeLayout {
            kind: NodeLayoutKind::FlippedSandwich,
            min_pin_row_height: 0.0,
            equal_pin_row_heights: false,
        }
    }

    /// Returns new [`NodeLayout`] with same `kind` and specified pin heights.
    #[must_use]
    #[inline]
    pub const fn with_equal_pin_rows(self) -> Self {
        NodeLayout {
            kind: self.kind,
            min_pin_row_height: self.min_pin_row_height,
            equal_pin_row_heights: true,
        }
    }

    /// Returns new [`NodeLayout`] with same `kind` and specified minimum pin row height.
    #[must_use]
    #[inline]
    pub const fn with_min_pin_row_height(self, min_pin_row_height: f32) -> Self {
        NodeLayout {
            kind: self.kind,
            min_pin_row_height,
            equal_pin_row_heights: self.equal_pin_row_heights,
        }
    }
}

impl From<NodeLayoutKind> for NodeLayout {
    #[inline]
    fn from(kind: NodeLayoutKind) -> Self {
        NodeLayout {
            kind,
            min_pin_row_height: 0.0,
            equal_pin_row_heights: false,
        }
    }
}

impl Default for NodeLayout {
    #[inline]
    fn default() -> Self {
        NodeLayout::coil()
    }
}

#[derive(Clone, Copy, Debug)]
enum OuterHeights<'a> {
    Flexible { rows: &'a [f32] },
    Matching { max: f32 },
    Tight,
}

#[derive(Clone, Copy, Debug)]
struct Heights<'a> {
    rows: &'a [f32],
    outer: OuterHeights<'a>,
    min_outer: f32,
}

impl Heights<'_> {
    fn get(&self, idx: usize) -> (f32, f32) {
        let inner = match self.rows.get(idx) {
            Some(&value) => value,
            None => 0.0,
        };

        let outer = match &self.outer {
            OuterHeights::Flexible { rows } => match rows.get(idx) {
                Some(&outer) => outer.max(inner),
                None => inner,
            },
            OuterHeights::Matching { max } => max.max(inner),
            OuterHeights::Tight => inner,
        };

        (inner, outer.max(self.min_outer))
    }
}

impl NodeLayout {
    fn input_heights(self, state: &NodeState) -> Heights<'_> {
        let rows = state.input_heights().as_slice();

        let outer = match (self.kind, self.equal_pin_row_heights) {
            (NodeLayoutKind::Coil, false) => OuterHeights::Flexible {
                rows: state.output_heights().as_slice(),
            },
            (_, true) => {
                let mut max_height = 0.0f32;
                for &h in state.input_heights() {
                    max_height = max_height.max(h);
                }
                for &h in state.output_heights() {
                    max_height = max_height.max(h);
                }
                OuterHeights::Matching { max: max_height }
            }
            (_, false) => OuterHeights::Tight,
        };

        Heights {
            rows,
            outer,
            min_outer: self.min_pin_row_height,
        }
    }

    fn output_heights(self, state: &'_ NodeState) -> Heights<'_> {
        let rows = state.output_heights().as_slice();

        let outer = match (self.kind, self.equal_pin_row_heights) {
            (NodeLayoutKind::Coil, false) => OuterHeights::Flexible {
                rows: state.input_heights().as_slice(),
            },
            (_, true) => {
                let mut max_height = 0.0f32;
                for &h in state.input_heights() {
                    max_height = max_height.max(h);
                }
                for &h in state.output_heights() {
                    max_height = max_height.max(h);
                }
                OuterHeights::Matching { max: max_height }
            }
            (_, false) => OuterHeights::Tight,
        };

        Heights {
            rows,
            outer,
            min_outer: self.min_pin_row_height,
        }
    }
}

/// Controls style of node selection rect.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "egui-probe", derive(egui_probe::EguiProbe))]
pub struct SelectionStyle {
    /// Margin between selection rect and node frame.
    pub margin: Margin,

    /// Rounding of selection rect.
    pub rounding: CornerRadius,

    /// Fill color of selection rect.
    pub fill: Color32,

    /// Stroke of selection rect.
    pub stroke: Stroke,
}

/// Controls how pins are placed in the node.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "egui-probe", derive(egui_probe::EguiProbe))]
pub enum PinPlacement {
    /// Pins are placed inside the node frame.
    #[default]
    Inside,

    /// Pins are placed on the edge of the node frame.
    Edge,

    /// Pins are placed outside the node frame.
    Outside {
        /// Margin between node frame and pins.
        margin: f32,
    },
}

/// Style for rendering Snarl.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "egui-probe", derive(egui_probe::EguiProbe))]
pub struct SnarlStyle {
    /// Controls how nodes are laid out.
    /// Defaults to [`NodeLayoutKind::Coil`].
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub node_layout: Option<NodeLayout>,

    /// Frame used to draw nodes.
    /// Defaults to [`Frame::window`] constructed from current ui's style.
    #[cfg_attr(
        feature = "serde",
        serde(
            skip_serializing_if = "Option::is_none",
            default,
            with = "serde_frame_option"
        )
    )]
    pub node_frame: Option<Frame>,

    /// Frame used to draw node headers.
    /// Defaults to [`node_frame`] without shadow and transparent fill.
    ///
    /// If set, it should not have shadow and fill should be either opaque of fully transparent
    /// unless layering of header fill color with node fill color is desired.
    #[cfg_attr(
        feature = "serde",
        serde(
            skip_serializing_if = "Option::is_none",
            default,
            with = "serde_frame_option"
        )
    )]
    pub header_frame: Option<Frame>,

    /// Blank space for dragging node by its header.
    /// Elements in the header are placed after this space.
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub header_drag_space: Option<Vec2>,

    /// Whether nodes can be collapsed.
    /// If true, headers will have collapsing button.
    /// When collapsed, node will not show its pins, body and footer.
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub collapsible: Option<bool>,

    /// Size of pins.
    #[cfg_attr(feature = "egui-probe", egui_probe(range = 0.0..))]
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub pin_size: Option<f32>,

    /// Default fill color for pins.
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub pin_fill: Option<Color32>,

    /// Default stroke for pins.
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub pin_stroke: Option<Stroke>,

    /// Shape of pins.
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub pin_shape: Option<PinShape>,

    /// Placement of pins.
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub pin_placement: Option<PinPlacement>,

    /// Width of wires.
    #[cfg_attr(feature = "egui-probe", egui_probe(range = 0.0..))]
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub wire_width: Option<f32>,

    /// Size of wire frame which controls curvature of wires.
    #[cfg_attr(feature = "egui-probe", egui_probe(range = 0.0..))]
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub wire_frame_size: Option<f32>,

    /// Whether to downscale wire frame when nodes are close.
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub downscale_wire_frame: Option<bool>,

    /// Weather to upscale wire frame when nodes are far.
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub upscale_wire_frame: Option<bool>,

    /// Controls default style of wires.
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub wire_style: Option<WireStyle>,

    /// Layer where wires are rendered.
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub wire_layer: Option<WireLayer>,

    /// Frame used to draw background
    #[cfg_attr(
        feature = "serde",
        serde(
            skip_serializing_if = "Option::is_none",
            default,
            with = "serde_frame_option"
        )
    )]
    pub bg_frame: Option<Frame>,

    /// Background pattern.
    /// Defaults to [`BackgroundPattern::Grid`].
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub bg_pattern: Option<BackgroundPattern>,

    /// Stroke for background pattern.
    /// Defaults to `ui.visuals().widgets.noninteractive.bg_stroke`.
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub bg_pattern_stroke: Option<Stroke>,

    /// Minimum viewport scale that can be set.
    #[cfg_attr(feature = "egui-probe", egui_probe(range = 0.0..=1.0))]
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub min_scale: Option<f32>,

    /// Maximum viewport scale that can be set.
    #[cfg_attr(feature = "egui-probe", egui_probe(range = 1.0..))]
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub max_scale: Option<f32>,

    /// Enable centering by double click on background
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub centering: Option<bool>,

    /// Stroke for selection.
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub select_stoke: Option<Stroke>,

    /// Fill for selection.
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub select_fill: Option<Color32>,

    /// Flag to control how rect selection works.
    /// If set to true, only nodes fully contained in selection rect will be selected.
    /// If set to false, nodes intersecting with selection rect will be selected.
    pub select_rect_contained: Option<bool>,

    /// Style for node selection.
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub select_style: Option<SelectionStyle>,

    /// Controls whether to show magnified text in crisp mode.
    /// This zooms UI style to max scale and scales down the scene.
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    pub crisp_magnified_text: Option<bool>,

    /// Controls smoothness of wire curves.
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    #[cfg_attr(
        feature = "egui-probe",
        egui_probe(range = 0.0f32..=10.0f32 by 0.05f32)
    )]
    pub wire_smoothness: Option<f32>,

    /// FLEXINPUT PATCH: how much the wires of the hovered or selected nodes stand out
    /// (0 to 1): they get a little thicker and the rest of the graph dims. Off when
    /// `None` or 0.
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    #[cfg_attr(feature = "egui-probe", egui_probe(skip))]
    pub wire_focus: Option<f32>,

    /// FLEXINPUT PATCH: how opaque connected wires are drawn (0 to 1; `None` is 1). The
    /// wires of a node in focus show whole whatever this is.
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    #[cfg_attr(feature = "egui-probe", egui_probe(skip))]
    pub wire_opacity: Option<f32>,

    /// FLEXINPUT PATCH: a see-through wire shows more of itself the more active its signal
    /// is (`PinWireInfo::activity`).
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    #[cfg_attr(feature = "egui-probe", egui_probe(skip))]
    pub wire_activity_reveals: Option<bool>,

    /// FLEXINPUT PATCH: the layout grid. See [`GridLayout`].
    #[cfg_attr(
        feature = "serde",
        serde(skip_serializing_if = "Option::is_none", default)
    )]
    #[cfg_attr(feature = "egui-probe", egui_probe(skip))]
    pub grid: Option<GridLayout>,

    #[doc(hidden)]
    #[cfg_attr(feature = "egui-probe", egui_probe(skip))]
    #[cfg_attr(feature = "serde", serde(skip_serializing, default))]
    /// Do not access other than with .., here to emulate `#[non_exhaustive(pub)]`
    pub _non_exhaustive: (),
}

impl SnarlStyle {
    fn get_node_layout(&self) -> NodeLayout {
        self.node_layout.unwrap_or_default()
    }

    fn get_pin_size(&self, style: &Style) -> f32 {
        self.pin_size.unwrap_or(style.spacing.interact_size.y * 0.6)
    }

    fn get_pin_fill(&self, style: &Style) -> Color32 {
        self.pin_fill
            .unwrap_or(style.visuals.widgets.active.bg_fill)
    }

    fn get_pin_stroke(&self, style: &Style) -> Stroke {
        self.pin_stroke.unwrap_or_else(|| {
            Stroke::new(
                style.visuals.widgets.active.bg_stroke.width,
                style.visuals.widgets.active.bg_stroke.color,
            )
        })
    }

    fn get_pin_shape(&self) -> PinShape {
        self.pin_shape.unwrap_or(PinShape::Circle)
    }

    fn get_pin_placement(&self) -> PinPlacement {
        self.pin_placement.unwrap_or_default()
    }

    fn get_wire_width(&self, style: &Style) -> f32 {
        self.wire_width
            .unwrap_or_else(|| self.get_pin_size(style) * 0.1)
    }

    fn get_wire_frame_size(&self, style: &Style) -> f32 {
        self.wire_frame_size
            .unwrap_or_else(|| self.get_pin_size(style) * 3.0)
    }

    fn get_downscale_wire_frame(&self) -> bool {
        self.downscale_wire_frame.unwrap_or(true)
    }

    fn get_upscale_wire_frame(&self) -> bool {
        self.upscale_wire_frame.unwrap_or(false)
    }

    fn get_wire_style(&self) -> WireStyle {
        self.wire_style.unwrap_or(WireStyle::Bezier5)
    }

    fn get_wire_layer(&self) -> WireLayer {
        self.wire_layer.unwrap_or(WireLayer::BehindNodes)
    }

    fn get_header_drag_space(&self, style: &Style) -> Vec2 {
        self.header_drag_space
            .unwrap_or_else(|| vec2(style.spacing.icon_width, style.spacing.icon_width))
    }

    fn get_collapsible(&self) -> bool {
        self.collapsible.unwrap_or(true)
    }

    fn get_bg_frame(&self, style: &Style) -> Frame {
        self.bg_frame.unwrap_or_else(|| Frame::canvas(style))
    }

    fn get_bg_pattern_stroke(&self, style: &Style) -> Stroke {
        self.bg_pattern_stroke
            .unwrap_or(style.visuals.widgets.noninteractive.bg_stroke)
    }

    fn get_min_scale(&self) -> f32 {
        self.min_scale.unwrap_or(0.2)
    }

    fn get_max_scale(&self) -> f32 {
        self.max_scale.unwrap_or(2.0)
    }

    fn get_node_frame(&self, style: &Style) -> Frame {
        self.node_frame.unwrap_or_else(|| Frame::window(style))
    }

    fn get_header_frame(&self, style: &Style) -> Frame {
        self.header_frame
            .unwrap_or_else(|| self.get_node_frame(style).shadow(Shadow::NONE))
    }

    fn get_centering(&self) -> bool {
        self.centering.unwrap_or(true)
    }

    fn get_select_stroke(&self, style: &Style) -> Stroke {
        self.select_stoke.unwrap_or_else(|| {
            Stroke::new(
                style.visuals.selection.stroke.width,
                style.visuals.selection.stroke.color.gamma_multiply(0.5),
            )
        })
    }

    fn get_select_fill(&self, style: &Style) -> Color32 {
        self.select_fill
            .unwrap_or_else(|| style.visuals.selection.bg_fill.gamma_multiply(0.3))
    }

    fn get_select_rect_contained(&self) -> bool {
        self.select_rect_contained.unwrap_or(false)
    }

    fn get_select_style(&self, style: &Style) -> SelectionStyle {
        self.select_style.unwrap_or_else(|| SelectionStyle {
            margin: style.spacing.window_margin,
            rounding: style.visuals.window_corner_radius,
            fill: self.get_select_fill(style),
            stroke: self.get_select_stroke(style),
        })
    }

    fn get_crisp_magnified_text(&self) -> bool {
        self.crisp_magnified_text.unwrap_or(false)
    }

    /// FLEXINPUT PATCH: the layout grid, when it's on.
    const fn get_grid(&self) -> Option<GridLayout> {
        self.grid
    }

    fn get_wire_smoothness(&self) -> f32 {
        self.wire_smoothness.unwrap_or(1.0)
    }
}

#[cfg(feature = "serde")]
mod serde_frame_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    pub struct Frame {
        pub inner_margin: egui::Margin,
        pub outer_margin: egui::Margin,
        pub rounding: egui::CornerRadius,
        pub shadow: egui::epaint::Shadow,
        pub fill: egui::Color32,
        pub stroke: egui::Stroke,
    }

    #[allow(clippy::ref_option)]
    pub fn serialize<S>(frame: &Option<egui::Frame>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match frame {
            Some(frame) => Frame {
                inner_margin: frame.inner_margin,
                outer_margin: frame.outer_margin,
                rounding: frame.corner_radius,
                shadow: frame.shadow,
                fill: frame.fill,
                stroke: frame.stroke,
            }
            .serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<egui::Frame>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let frame_opt = Option::<Frame>::deserialize(deserializer)?;
        Ok(frame_opt.map(|frame| egui::Frame {
            inner_margin: frame.inner_margin,
            outer_margin: frame.outer_margin,
            corner_radius: frame.rounding,
            shadow: frame.shadow,
            fill: frame.fill,
            stroke: frame.stroke,
        }))
    }
}

impl SnarlStyle {
    /// Creates new [`SnarlStyle`] filled with default values.
    #[must_use]
    pub const fn new() -> Self {
        SnarlStyle {
            node_layout: None,
            pin_size: None,
            pin_fill: None,
            pin_stroke: None,
            pin_shape: None,
            pin_placement: None,
            wire_width: None,
            wire_frame_size: None,
            downscale_wire_frame: None,
            upscale_wire_frame: None,
            wire_style: None,
            wire_layer: None,
            header_drag_space: None,
            collapsible: None,

            bg_frame: None,
            bg_pattern: None,
            bg_pattern_stroke: None,

            min_scale: None,
            max_scale: None,
            node_frame: None,
            header_frame: None,
            centering: None,
            select_stoke: None,
            select_fill: None,
            select_rect_contained: None,
            select_style: None,
            crisp_magnified_text: None,
            wire_smoothness: None,
            grid: None,
            wire_focus: None,
            wire_opacity: None,
            wire_activity_reveals: None,

            _non_exhaustive: (),
        }
    }
}

impl Default for SnarlStyle {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

struct DrawNodeResponse {
    node_moved: Option<(NodeId, Vec2)>,
    node_to_top: Option<NodeId>,
    drag_released: bool,
    pin_hovered: Option<AnyPin>,
    final_rect: Rect,
    /// FLEXINPUT PATCH: from the frame's top-left corner to the node's position.
    frame_offset: Vec2,
    /// FLEXINPUT PATCH: from where the node is drawn, easing in, to where it is.
    ease_offset: Vec2,
}

struct DrawPinsResponse {
    drag_released: bool,
    pin_hovered: Option<AnyPin>,
    final_rect: Rect,
    new_heights: RowHeights,
}

struct DrawBodyResponse {
    final_rect: Rect,
}

struct PinResponse {
    pos: Pos2,
    wire_color: Color32,
    wire_style: WireStyle,
    wire_width_factor: f32,
    /// FLEXINPUT PATCH: `PinWireInfo::activity`.
    activity: f32,
}

// FLEXINPUT PATCH: each pin's height below its node's `pos`, as last drawn.
const PIN_HEIGHTS_KEY: &str = "flexinput-pin-heights";

// FLEXINPUT PATCH: where the wires being dropped were picked up from.
const MOVED_FROM_KEY: &str = "flexinput-wire-moved-from";

// FLEXINPUT PATCH: the node being drawn, and how far from its `pos` it's drawn.
const DRAWN_OFFSET_KEY: &str = "flexinput-drawn-offset";

// FLEXINPUT PATCH: the node being laid out, and the size of the frame it's laid out in.
const LAID_OUT_KEY: &str = "flexinput-laid-out-frame";
/// FLEXINPUT PATCH: see [`drawn_frame`].
const DRAWN_FRAME_KEY: &str = "flexinput-drawn-frame";

/// FLEXINPUT PATCH: how a node's frame is sized along one axis this frame - see
/// `SnarlViewer::node_frame_now`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum FrameNow {
    /// As snarl sizes it: the size it had last frame, rounded to the grid, easing to a new one.
    #[default]
    Snarl,
    /// At this size, margins included: the viewer is easing it to the size its grip asked
    /// for. Never narrower than what it holds: contents that didn't fit hold the frame at
    /// their size until the viewer asks for at least as much (the grip dragged in past them).
    Eased(f32),
    /// As what it holds takes this frame (heights only): an axis that follows one the viewer
    /// is easing, the height of a module that keeps its shape.
    Contents,
}

/// Inside the `SnarlViewer` calls that draw a node: the size of the frame (margins
/// included) it's laid out in this frame - last frame's, or the one it's easing through
/// (`SnarlViewer::node_frame_now`). A body measuring the room around it measures against
/// this, not against last frame's size.
#[must_use]
pub fn laid_out_frame(ctx: &egui::Context, node: NodeId) -> Option<Vec2> {
    ctx.data(|d| d.get_temp::<(NodeId, Vec2)>(egui::Id::new(LAID_OUT_KEY)))
        .filter(|(n, _)| *n == node)
        .map(|(_, v)| v)
}

/// Inside the `SnarlViewer` calls that draw a node: how far from its `pos` the node is drawn
/// this frame - on its way to `pos` while it eases there, [`Vec2::ZERO`] once it's there. Its
/// top, and anything kept as a distance from it, is `pos` plus this.
#[must_use]
pub fn drawn_offset(ctx: &egui::Context, node: NodeId) -> Vec2 {
    ctx.data(|d| d.get_temp::<(NodeId, Vec2)>(egui::Id::new(DRAWN_OFFSET_KEY)))
        .filter(|(n, _)| *n == node)
        .map_or(Vec2::ZERO, |(_, v)| v)
}

/// The frame a node was last drawn at - on its way to its size while that eases, else what
/// it holds - in graph space. Keyed by node only, so a canvas showing two snarls answers
/// for whichever drew the node last.
#[must_use]
pub fn drawn_frame(ctx: &egui::Context, node: NodeId) -> Option<Rect> {
    ctx.data(|d| d.get_temp::<Rect>(egui::Id::new(DRAWN_FRAME_KEY).with(node.0)))
}

/// Inside `SnarlViewer::connect`: the pin the wires being dropped were picked up from, when
/// they are existing wires being moved rather than a new one being drawn.
#[must_use]
pub fn wire_moved_from(ctx: &egui::Context) -> Option<AnyPin> {
    ctx.data(|d| d.get_temp::<Option<AnyPin>>(egui::Id::new(MOVED_FROM_KEY))).flatten()
}

/// Where each pin of a snarl sat on its node the last time it was drawn: its height below
/// the node's position. For laying a patch out pin to pin.
#[derive(Clone, Default, Debug)]
pub struct PinHeights {
    pub inputs: HashMap<InPinId, f32>,
    pub outputs: HashMap<OutPinId, f32>,
}

/// The pin heights the snarl `snarl_id` recorded the last time it was drawn.
#[must_use]
pub fn pin_heights(ctx: &egui::Context, snarl_id: Id) -> Option<PinHeights> {
    ctx.data(|d| d.get_temp(snarl_id.with(PIN_HEIGHTS_KEY)))
}

/// Widget to display [`Snarl`] graph in [`Ui`].
#[derive(Clone, Copy, Debug)]
pub struct SnarlWidget {
    id_salt: Id,
    id: Option<Id>,
    style: SnarlStyle,
    min_size: Vec2,
    max_size: Vec2,
}

impl Default for SnarlWidget {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl SnarlWidget {
    /// Returns new [`SnarlWidget`] with default parameters.
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        SnarlWidget {
            id_salt: Id::new(":snarl:"),
            id: None,
            style: SnarlStyle::new(),
            min_size: Vec2::ZERO,
            max_size: Vec2::INFINITY,
        }
    }

    /// Assign an explicit and globally unique [`Id`].
    ///
    /// Use this if you want to persist the state of the widget
    /// when it changes position in the widget hierarchy.
    ///
    /// Prefer using [`SnarlWidget::id_salt`] otherwise.
    #[inline]
    #[must_use]
    pub const fn id(mut self, id: Id) -> Self {
        self.id = Some(id);
        self
    }

    /// Assign a source for the unique [`Id`]
    ///
    /// It must be locally unique for the current [`Ui`] hierarchy position.
    ///
    /// Ignored if [`SnarlWidget::id`] was set.
    #[inline]
    #[must_use]
    pub fn id_salt(mut self, id_salt: impl Hash) -> Self {
        self.id_salt = Id::new(id_salt);
        self
    }

    /// Set style parameters for the [`Snarl`] widget.
    #[inline]
    #[must_use]
    pub const fn style(mut self, style: SnarlStyle) -> Self {
        self.style = style;
        self
    }

    /// Set minimum size of the [`Snarl`] widget.
    #[inline]
    #[must_use]
    pub const fn min_size(mut self, min_size: Vec2) -> Self {
        self.min_size = min_size;
        self
    }

    /// Set maximum size of the [`Snarl`] widget.
    #[inline]
    #[must_use]
    pub const fn max_size(mut self, max_size: Vec2) -> Self {
        self.max_size = max_size;
        self
    }

    #[inline]
    fn get_id(&self, ui_id: Id) -> Id {
        self.id.unwrap_or_else(|| ui_id.with(self.id_salt))
    }

    /// Render [`Snarl`] using given viewer and style into the [`Ui`].
    #[inline]
    pub fn show<T, V>(&self, snarl: &mut Snarl<T>, viewer: &mut V, ui: &mut Ui) -> egui::Response
    where
        V: SnarlViewer<T>,
    {
        let snarl_id = self.get_id(ui.id());

        show_snarl(
            snarl_id,
            self.style,
            self.min_size,
            self.max_size,
            snarl,
            viewer,
            ui,
        )
    }
}

#[inline(never)]
fn show_snarl<T, V>(
    snarl_id: Id,
    mut style: SnarlStyle,
    min_size: Vec2,
    max_size: Vec2,
    snarl: &mut Snarl<T>,
    viewer: &mut V,
    ui: &mut Ui,
) -> egui::Response
where
    V: SnarlViewer<T>,
{
    #![allow(clippy::too_many_lines)]

    let (mut latest_pos, modifiers) = ui.ctx().input(|i| (i.pointer.latest_pos(), i.modifiers));

    let bg_frame = style.get_bg_frame(ui.style());

    let outer_size_bounds = ui.available_size_before_wrap().max(min_size).min(max_size);

    let outer_resp = ui.allocate_response(outer_size_bounds, Sense::hover());

    ui.painter().add(bg_frame.paint(outer_resp.rect));

    let mut content_rect = outer_resp.rect - bg_frame.total_margin();

    // Make sure we don't shrink to the negative:
    content_rect.max.x = content_rect.max.x.max(content_rect.min.x);
    content_rect.max.y = content_rect.max.y.max(content_rect.min.y);

    let snarl_layer_id = LayerId::new(ui.layer_id().order, snarl_id);
    // FLEXINPUT PATCH: the layer the graph is shown in. The graph draws in a sublayer
    // of it, which isn't what egui finds under the pointer: this is.
    let host_layer = ui.layer_id();

    ui.ctx().set_sublayer(ui.layer_id(), snarl_layer_id);

    let mut min_scale = style.get_min_scale();
    let mut max_scale = style.get_max_scale();

    let ui_rect = content_rect;

    let mut snarl_state =
        SnarlState::load(ui.ctx(), snarl_id, snarl, ui_rect, min_scale, max_scale);
    let mut to_global = snarl_state.to_global();

    let clip_rect = ui.clip_rect();

    let mut ui = ui.new_child(
        UiBuilder::new()
            .ui_stack_info(UiStackInfo::new(UiKind::Frame).with_frame(bg_frame))
            .layer_id(snarl_layer_id)
            .max_rect(Rect::EVERYTHING)
            .sense(Sense::click_and_drag()),
    );

    if style.get_crisp_magnified_text() {
        style.scale(max_scale);
        ui.style_mut().scale(max_scale);

        min_scale /= max_scale;
        max_scale = 1.0;
    }

    clamp_scale(&mut to_global, min_scale, max_scale, ui_rect);

    let mut snarl_resp = ui.response();
    let view_before = to_global;
    Scene::new()
        .zoom_range(min_scale..=max_scale)
        .register_pan_and_zoom(&ui, &mut snarl_resp, &mut to_global);

    // FLEXINPUT PATCH: what two fingers do depends on what they're on.
    //
    // A second finger makes a pinch of it over the canvas, a node's frame or a pin: those
    // only drew the finger's attention by being in the way, so it stops holding them and
    // moves the view instead. Over a widget in a node's body it doesn't: the finger on
    // that is working it, and the other may be too, as on a curve's bend handles. Only a
    // wire being dragged lets its finger keep steering it: that zooms but doesn't pan.
    let (two_fingers, any_fingers, pointer_delta) =
        ui.input(|i| (i.multi_touch().is_some(), i.any_touches(), i.pointer.delta()));
    if two_fingers {
        snarl_state.set_pinching(true);
        snarl_state.stop_wire_hold();
    } else if !any_fingers {
        snarl_state.set_pinching(false);
    }
    let on_chrome = snarl_state.take_chrome_dragged();
    let on_wire = |hold: WireHold| matches!(hold.on(), HoldOn::Wire(..));
    let on_widget = touch::is_active(ui.ctx())
        && !on_chrome
        && !snarl_state.has_new_wires()
        && ui.ctx().dragged_id().is_some_and(|id| id != snarl_resp.id);
    if snarl_state.wire_hold().is_some_and(on_wire) || touch::pinch_blocked(ui.ctx()) || on_widget {
        // The canvas also stays put under a finger resting on a wire, which wobbles.
        to_global = view_before;
    } else if two_fingers && on_chrome && !snarl_state.has_new_wires() {
        // The Scene pans with its own drag, which this finger's isn't.
        to_global.translation += pointer_delta;
    }

    if snarl_resp.changed() {
        ui.ctx().request_repaint();
    }

    // Inform viewer about current transform.
    viewer.current_transform(&mut to_global, snarl);

    snarl_state.set_to_global(to_global);

    let to_global = to_global;
    let from_global = to_global.inverse();

    // Graph viewport
    let viewport = (from_global * ui_rect).round_ui();
    let viewport_clip = from_global * clip_rect;

    ui.set_clip_rect(viewport.intersect(viewport_clip));
    ui.expand_to_include_rect(viewport);

    // Set transform for snarl layer.
    ui.ctx().set_transform_layer(snarl_layer_id, to_global);

    // Map latest pointer position to graph space.
    latest_pos = latest_pos.map(|pos| from_global * pos);

    viewer.draw_background(
        style.bg_pattern.as_ref(),
        &viewport,
        &style,
        ui.style(),
        ui.painter(),
        snarl,
    );

    // FLEXINPUT PATCH: the layout grid's lines.
    let grid_cfg = style.get_grid();
    if let Some(grid) = grid_cfg.filter(|g| g.show) {
        let color = ui.visuals().text_color();
        grid::draw_grid(&grid, ui.painter(), &viewport, to_global.scaling, color);
    }

    let mut node_moved = None;
    let mut node_to_top = None;

    // Process selection rect.
    let mut rect_selection_ended = None;
    if modifiers.shift || snarl_state.is_rect_selection() {
        let select_resp = ui.interact(snarl_resp.rect, snarl_id.with("select"), Sense::drag());

        if select_resp.dragged_by(PointerButton::Primary)
            && let Some(pos) = select_resp.interact_pointer_pos()
        {
            if snarl_state.is_rect_selection() {
                snarl_state.update_rect_selection(pos);
            } else {
                snarl_state.start_rect_selection(pos);
            }
        }

        if select_resp.drag_stopped_by(PointerButton::Primary) {
            if let Some(select_rect) = snarl_state.rect_selection() {
                rect_selection_ended = Some(select_rect);
            }
            snarl_state.stop_rect_selection();
        }
    }

    let wire_frame_size = style.get_wire_frame_size(ui.style());
    let wire_width = style.get_wire_width(ui.style());
    let wire_threshold = style.get_wire_smoothness();

    let wire_shape_idx = match style.get_wire_layer() {
        WireLayer::BehindNodes => Some(ui.painter().add(Shape::Noop)),
        WireLayer::AboveNodes => None,
    };

    let mut input_info = HashMap::new();
    let mut output_info = HashMap::new();

    let mut pin_hovered = None;

    let draw_order = snarl_state.update_draw_order(snarl);
    let mut drag_released = false;

    let mut nodes_bb = Rect::NOTHING;
    let mut node_rects = Vec::new();
    // FLEXINPUT PATCH: the same rects where the nodes are rather than where they're drawn
    // easing in, for placing and pushing, which move the nodes themselves.
    let mut real_rects: Vec<(NodeId, Rect)> = Vec::new();
    let mut ease_offsets: HashMap<NodeId, Vec2> = HashMap::new();
    // FLEXINPUT PATCH: each node's frame offset, for snapping it while it's dragged.
    let mut frame_offsets: HashMap<NodeId, Vec2> = HashMap::new();

    // FLEXINPUT PATCH: the nodes out of focus are drawn part-way transparent. Which they are
    // was worked out at the end of the last frame (it needs every node's rect), so this is
    // one frame behind the pointer, under a fade that takes five.
    let (lit, focus_amount): (Vec<NodeId>, f32) = ui
        .ctx()
        .data(|d| d.get_temp(snarl_id.with("focus-lit")))
        .unwrap_or_default();

    for node_idx in draw_order {
        if !snarl.nodes.contains(node_idx.0) {
            continue;
        }

        let faded = focus_amount > 0.0 && !lit.contains(&node_idx);
        if faded {
            ui.set_opacity(1.0 - FOCUS_FADE * focus_amount);
        }

        // show_node(node_idx);
        viewer.before_node(node_idx, snarl);
        let response = draw_node(
            snarl,
            &mut ui,
            node_idx,
            viewer,
            &mut snarl_state,
            &style,
            snarl_id,
            &mut input_info,
            modifiers,
            &mut output_info,
        );
        viewer.after_node(node_idx, snarl);

        if faded {
            ui.set_opacity(1.0);
        }

        if let Some(response) = response {
            if let Some(v) = response.node_to_top {
                node_to_top = Some(v);
            }
            if let Some(v) = response.node_moved {
                node_moved = Some(v);
            }
            if let Some(v) = response.pin_hovered {
                pin_hovered = Some(v);
            }
            drag_released |= response.drag_released;

            nodes_bb = nodes_bb.union(response.final_rect);
            // FLEXINPUT PATCH: the grid's routes and pushes need every node's rect.
            node_rects.push((node_idx, response.final_rect));
            real_rects.push((node_idx, response.final_rect.translate(response.ease_offset)));
            ease_offsets.insert(node_idx, response.ease_offset);
            frame_offsets.insert(node_idx, response.frame_offset);
        }
    }

    // FLEXINPUT PATCH: where each pin sits on its node this frame, as a height below the
    // node's `pos`, for laying a patch out pin to pin (FlexInput's auto-arrange). Read back
    // with `pin_heights`. Measured from where the node is drawn, which is `pos` once it has
    // eased there.
    {
        let mut heights = PinHeights::default();
        let below = |node: NodeId, y: f32| {
            let n = snarl.nodes.get(node.0)?;
            let off = ease_offsets.get(&node).map_or(0.0, |v| v.y);
            Some(y - (n.pos.y - off))
        };
        for (pin, r) in &input_info {
            if let Some(h) = below(pin.node, r.pos.y) {
                heights.inputs.insert(*pin, h);
            }
        }
        for (pin, r) in &output_info {
            if let Some(h) = below(pin.node, r.pos.y) {
                heights.outputs.insert(*pin, h);
            }
        }
        ui.ctx().data_mut(|d| d.insert_temp(snarl_id.with(PIN_HEIGHTS_KEY), heights));
    }

    let mut hovered_wire = None;
    let mut hovered_wire_disconnect = false;
    let mut wire_shapes = Vec::new();

    // FLEXINPUT PATCH: connected wires routed along the grid's lines, around the nodes.
    let pin_size = style.get_pin_size(ui.style()).max(0.0);
    let routes = grid_cfg.filter(|g| g.wires != WireRouting::Direct).map(|grid| {
        let pin_reach = match style.get_pin_placement() {
            PinPlacement::Outside { margin } => margin + pin_size + 1.0,
            PinPlacement::Edge => pin_size * 0.5 + 1.0,
            PinPlacement::Inside => 1.0,
        };
        let ends: Vec<grid::WireEnds> = snarl
            .wires
            .iter()
            .filter_map(|w| {
                let from = output_info.get(&w.out_pin)?;
                let to = input_info.get(&w.in_pin)?;
                let factor = from.wire_width_factor.max(to.wire_width_factor);
                Some((w.out_pin, w.in_pin, from.pos, to.pos, wire_width * routed_factor(factor)))
            })
            .collect();
        grid::routes(ui.ctx(), snarl_id, &grid, pin_reach, &node_rects, &ends)
    });
    // FLEXINPUT PATCH: a routed wire that changed route is drawn easing from its old shape
    // to its new one. Only the wires still on the way are here; the rest draw their route.
    let eased_wires = routes
        .as_ref()
        .map(|r| eased_routes(ui.ctx(), snarl_id, &r.drawn, grid_cfg.map_or(0.0, |g| g.ease)))
        .unwrap_or_default();

    // FLEXINPUT PATCH: a lost pin or a lifted finger can leave a hold behind.
    if !ui.input(|i| i.pointer.primary_down()) {
        snarl_state.stop_wire_hold();
    }

    // FLEXINPUT PATCH: a finger coming down on the canvas near a wire might stay for the
    // wire's menu. That's the same menu as a right click on the wire, which touch has no
    // other way to ask for.
    let touch = touch::is_active(ui.ctx());
    if touch
        && !snarl_state.has_new_wires()
        && snarl_state.wire_hold().is_none()
        && snarl_resp.is_pointer_button_down_on()
        && ui.input(|i| i.pointer.primary_pressed())
        && let Some(pos) = latest_pos
    {
        let target = nearest_wire(
            ui.ctx(),
            snarl,
            snarl_id,
            &style,
            pos,
            TOUCH_WIRE_REACH / to_global.scaling,
            wire_frame_size,
            &input_info,
            &output_info,
            routes.as_deref(),
        );
        if let Some((out_pin, in_pin)) = target {
            snarl_state.begin_wire_hold(HoldOn::Wire(out_pin, in_pin), ui.ctx());
        }
    }
    let mut wire_held = None;
    if let Some(hold) = snarl_state.wire_hold()
        && let HoldOn::Wire(out_pin, in_pin) = hold.on()
    {
        match hold.step(ui.ctx()) {
            WireHoldStep::Holding(progress) => {
                wire_held = Some((hold.origin(), progress, out_pin));
                ui.ctx().request_repaint();
            }
            WireHoldStep::Moved | WireHoldStep::Gone => snarl_state.stop_wire_hold(),
            WireHoldStep::Lifted => {
                snarl_state.stop_wire_hold();
                if InPin::new(snarl, in_pin).remotes.contains(&out_pin) {
                    let (from, to) = (OutPin::new(snarl, out_pin), InPin::new(snarl, in_pin));
                    viewer.disconnect(&from, &to, snarl);
                }
            }
        }
    }

    let wire_hold = snarl_state
        .wire_hold()
        .map(|hold| (hold.on(), ui.input(|i| hold.progress(i.time))));
    let moved_from = snarl_state.moved_from();
    let highlighted_wire = viewer.highlighted_wire();

    // FLEXINPUT PATCH: the wires of the hovered or selected nodes stand out.
    let reveal = style.wire_opacity.is_some_and(|o| o < 1.0);
    let focus = wire_focus(&ui, snarl, snarl_id, &snarl_state, &node_rects, style.wire_focus, host_layer, reveal);
    let focused = |node: NodeId| focus.nodes.contains(&node);

    // Draw and interact with wires
    for wire in snarl.wires.iter() {
        let Some(from_r) = output_info.get(&wire.out_pin) else {
            continue;
        };
        let Some(to_r) = input_info.get(&wire.in_pin) else {
            continue;
        };

        if !snarl_state.has_new_wires() && snarl_resp.contains_pointer() && hovered_wire.is_none() {
            // Try to find hovered wire
            // If not dragging new wire
            // And not hovering over item above.

            if let Some(latest_pos) = latest_pos {
                let routed = routes.as_ref().and_then(|r| r.paths.get(&(wire.out_pin, wire.in_pin)));
                let wire_hit = if let Some(path) = routed {
                    let reach = wire_width * routed_factor(from_r.wire_width_factor.max(to_r.wire_width_factor));
                    route::distance_sq(path, latest_pos) < (reach * 0.5 + 1.5).max(3.0).powi(2)
                } else {
                    hit_wire(
                        ui.ctx(),
                        WireId::Connected {
                            snarl_id,
                            out_pin: wire.out_pin,
                            in_pin: wire.in_pin,
                        },
                        wire_frame_size,
                        style.get_upscale_wire_frame(),
                        style.get_downscale_wire_frame(),
                        from_r.pos,
                        to_r.pos,
                        latest_pos,
                        wire_width.max(2.0),
                        pick_wire_style(from_r.wire_style, to_r.wire_style),
                    )
                };

                if wire_hit {
                    hovered_wire = Some(wire);

                    let wire_r =
                        ui.interact(snarl_resp.rect, ui.make_persistent_id(wire), Sense::click());

                    //Remove hovered wire by second click
                    hovered_wire_disconnect |= wire_r.clicked_by(PointerButton::Secondary);
                }
            }
        }

        let mut color = mix_colors(from_r.wire_color, to_r.wire_color);
        let key = (wire.out_pin, wire.in_pin);
        // How opaque the wire is drawn, for its dark underlay to go with it.
        let mut wire_alpha = 1.0_f32;

        let width_factor = from_r.wire_width_factor.max(to_r.wire_width_factor);
        let mut draw_width = wire_width * width_factor;
        if hovered_wire == Some(wire) {
            draw_width *= 1.5;
        }

        // FLEXINPUT PATCH: the wires a hold is about to pick up swell as it goes, under a
        // finger that hides the pin. Once picked up they fade where they were, since
        // they're still connected there until the move lands.
        let on_pin = |pin: AnyPin| match pin {
            AnyPin::Out(pin) => wire.out_pin == pin,
            AnyPin::In(pin) => wire.in_pin == pin,
        };
        match wire_hold {
            Some((HoldOn::Pin(pin), progress)) if on_pin(pin) => {
                draw_width *= 1.0 + progress * 1.5;
            }
            // A held wire shows from the first moment which one the finger found.
            Some((HoldOn::Wire(out_pin, in_pin), progress))
                if (wire.out_pin, wire.in_pin) == (out_pin, in_pin) =>
            {
                draw_width *= 2.0 + progress * 1.5;
            }
            _ => {}
        }
        if highlighted_wire == Some((wire.out_pin, wire.in_pin)) {
            draw_width *= 2.5;
        }
        let in_focus = focused(wire.out_pin.node) || focused(wire.in_pin.node);
        if focus.amount > 0.0 {
            if in_focus {
                draw_width *= 1.0 + FOCUS_THICKEN * focus.amount;
                color = color.lerp_to_gamma(Color32::WHITE, FOCUS_BRIGHTEN * focus.amount);
            } else {
                color = color.gamma_multiply(1.0 - FOCUS_FADE * focus.amount);
            }
        }
        // FLEXINPUT PATCH: see-through wires (the style's opacity), more of one showing the
        // more active its signal is, and all of one in focus, hovered or picked out.
        let opacity = style.wire_opacity.unwrap_or(1.0).clamp(0.0, 1.0);
        if opacity < 1.0 {
            let mut alpha = opacity;
            if style.wire_activity_reveals.unwrap_or(false) {
                let activity = from_r.activity.max(to_r.activity).clamp(0.0, 1.0);
                alpha += (1.0 - alpha) * activity;
            }
            if in_focus {
                alpha += (1.0 - alpha) * focus.fade;
            }
            if hovered_wire == Some(wire) || highlighted_wire == Some(key) {
                alpha = 1.0;
            }
            color = color.gamma_multiply(alpha);
            wire_alpha = alpha;
        }
        if moved_from.is_some_and(on_pin) {
            color = color.gamma_multiply(0.3);
        }

        if let Some(path) = eased_wires.get(&key).or_else(|| routes.as_ref().and_then(|r| r.drawn.get(&key))) {
            // Thicker than a curve: a routed wire runs along grid lines and beside other
            // wires, and has to stand out from both. (A wire that's already thick, an
            // Auto-Map bundle, keeps its own width.)
            let routed = draw_width / width_factor.max(0.01) * routed_factor(width_factor);
            let shade = ui.visuals().extreme_bg_color.gamma_multiply(wire_alpha);
            draw_routed_wire(&ui, &mut wire_shapes, path, Stroke::new(routed, color), shade);
            continue;
        }

        draw_wire(
            &ui,
            WireId::Connected {
                snarl_id,
                out_pin: wire.out_pin,
                in_pin: wire.in_pin,
            },
            &mut wire_shapes,
            wire_frame_size,
            style.get_upscale_wire_frame(),
            style.get_downscale_wire_frame(),
            from_r.pos,
            to_r.pos,
            Stroke::new(draw_width, color),
            wire_threshold,
            pick_wire_style(from_r.wire_style, to_r.wire_style),
        );
    }

    // Remove hovered wire by second click
    if hovered_wire_disconnect && let Some(wire) = hovered_wire {
        let out_pin = OutPin::new(snarl, wire.out_pin);
        let in_pin = InPin::new(snarl, wire.in_pin);
        viewer.disconnect(&out_pin, &in_pin, snarl);
    }

    if let Some(select_rect) = rect_selection_ended {
        let select_nodes = node_rects.iter().copied().filter_map(|(id, rect)| {
            let select = if style.get_select_rect_contained() {
                select_rect.contains_rect(rect)
            } else {
                select_rect.intersects(rect)
            };

            if select { Some(id) } else { None }
        });

        if modifiers.command {
            snarl_state.deselect_many_nodes(select_nodes);
        } else {
            snarl_state.select_many_nodes(!modifiers.shift, select_nodes);
        }
    }

    if let Some(select_rect) = snarl_state.rect_selection() {
        ui.painter().rect(
            select_rect,
            0.0,
            style.get_select_fill(ui.style()),
            style.get_select_stroke(ui.style()),
            StrokeKind::Inside,
        );
    }

    // If right button is clicked while new wire is being dragged, cancel it.
    // This is to provide way to 'not open' the link graph node menu, but just
    // releasing the new wire to empty space.
    //
    // This uses `button_down` directly, instead of `clicked_by` to improve
    // responsiveness of the cancel action.
    if snarl_state.has_new_wires() && ui.input(|x| x.pointer.button_down(PointerButton::Secondary))
    {
        let _ = snarl_state.take_new_wires();
        snarl_resp.flags.remove(Flags::CLICKED);
    }

    // Do centering unless no nodes are present.
    if style.get_centering() && snarl_resp.double_clicked() && nodes_bb.is_finite() {
        let nodes_bb = nodes_bb.expand(100.0);
        snarl_state.look_at(nodes_bb, ui_rect, min_scale, max_scale);
    }

    if modifiers.command && snarl_resp.clicked_by(PointerButton::Primary) {
        snarl_state.deselect_all_nodes();
    }

    // Wire end position will be overridden when link graph menu is opened.
    let mut wire_end_pos = latest_pos.unwrap_or_else(|| snarl_resp.rect.center());

    // FLEXINPUT PATCH: a wire dragged by a finger. The pointer vanishes on the same frame
    // the finger lifts, and a hold outlives egui's own drag, so neither the pin's drag
    // nor the pointer's position can say where it ended: look at the release itself, and
    // at `interact_pos`, which survives that frame. An OS-cancelled touch ends with no
    // release at all, which counts as giving up on the wire.
    if touch && let Some(new_wires) = snarl_state.new_wires().cloned() {
        let (released, finger) = ui.input(|i| {
            let finger = i.pointer.latest_pos().or_else(|| i.pointer.interact_pos());
            (i.pointer.any_released(), finger.map(|pos| from_global * pos))
        });
        if released {
            drag_released = true;
        } else if latest_pos.is_none() {
            let _ = snarl_state.take_new_wires();
        }
        let radius = TOUCH_SNAP_RADIUS / to_global.scaling;
        let target = finger.and_then(|pos| {
            nearest_pin(&new_wires, pos, radius, &input_info, &output_info, viewer, snarl)
        });
        pin_hovered = target.map(|(pin, _)| pin);
        if let Some(end) = target.map(|(_, pos)| pos).or(finger) {
            wire_end_pos = end;
        }
    }

    if drag_released {
        let moved_from = snarl_state.moved_from();
        // FLEXINPUT PATCH: where wires being moved came from, for the viewer's `connect` to
        // read (`wire_moved_from`): a viewer that asks before connecting has to finish the
        // move itself, and mustn't count the moved wire as one already on the pin.
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(MOVED_FROM_KEY), moved_from));
        let new_wires = snarl_state.take_new_wires();
        if new_wires.is_some() {
            ui.ctx().request_repaint();
        }
        match (new_wires, pin_hovered) {
            (Some(NewWires::In(in_pins)), Some(AnyPin::Out(out_pin))) => {
                for in_pin in in_pins {
                    viewer.connect(
                        &OutPin::new(snarl, out_pin),
                        &InPin::new(snarl, in_pin),
                        snarl,
                    );
                    // FLEXINPUT PATCH: a moved wire leaves its old pin, unless the viewer
                    // refused it the new one.
                    if let Some(AnyPin::Out(from)) = moved_from
                        && from != out_pin
                        && InPin::new(snarl, in_pin).remotes.contains(&out_pin)
                    {
                        snarl.disconnect(from, in_pin);
                    }
                }
            }
            (Some(NewWires::Out(out_pins)), Some(AnyPin::In(in_pin))) => {
                for out_pin in out_pins {
                    viewer.connect(
                        &OutPin::new(snarl, out_pin),
                        &InPin::new(snarl, in_pin),
                        snarl,
                    );
                    if let Some(AnyPin::In(from)) = moved_from
                        && from != in_pin
                        && InPin::new(snarl, in_pin).remotes.contains(&out_pin)
                    {
                        snarl.disconnect(out_pin, from);
                    }
                }
            }
            (Some(new_wires), None) if snarl_resp.hovered() => {
                let pins = match &new_wires {
                    NewWires::In(x) => AnyPins::In(x),
                    NewWires::Out(x) => AnyPins::Out(x),
                };

                if viewer.has_dropped_wire_menu(pins, snarl) {
                    // A wire is dropped without connecting to a pin.
                    // Show context menu for the wire drop.
                    snarl_state.set_new_wires_menu(new_wires);

                    // FLEXINPUT PATCH: a popup anchored to the pointer finds no pointer on
                    // the frame a finger lifts, so this one remembers where the drop was.
                    let at = ui.input(|i| i.pointer.latest_pos().or_else(|| i.pointer.interact_pos()));
                    let base = snarl_id.with("dropped_wire_menu");
                    let (menu_id, _) = touch::menu_opening(ui.ctx(), base, true);
                    ui.ctx().data_mut(|d| d.insert_temp(base, at.unwrap_or(ui_rect.center())));
                    Popup::open_id(ui.ctx(), menu_id);
                }
            }
            _ => {}
        }
    }

    // FLEXINPUT PATCH: these menus used to show only while the pointer had a position, so a
    // menu a finger opened went away the moment the finger lifted.
    let moved_from = snarl_state.moved_from();
    if let Some(new_wires) = snarl_state.take_new_wires_menu() {
        let pins = match &new_wires {
            NewWires::In(x) => AnyPins::In(x),
            NewWires::Out(x) => AnyPins::Out(x),
        };

        if viewer.has_dropped_wire_menu(pins, snarl) {
            let base = snarl_id.with("dropped_wire_menu");
            let (menu_id, for_touch) = touch::menu_opening(ui.ctx(), base, false);
            let at: Pos2 = ui.ctx().data(|d| d.get_temp(base)).unwrap_or(ui_rect.center());
            let menu = Popup::new(
                menu_id,
                ui.ctx().clone(),
                PopupAnchor::Position(at),
                ui.layer_id(),
            )
            .kind(PopupKind::Menu)
            .layout(Layout::top_down_justified(Align::Min))
            .style(touch::menu_style_of(for_touch))
            .open_memory(None)
            .show(|ui| {
                let pins = match &new_wires {
                    NewWires::In(x) => AnyPins::In(x),
                    NewWires::Out(x) => AnyPins::Out(x),
                };

                let menu_pos = from_global * ui.cursor().min;

                // Override wire end position when the wire-drop context menu is opened.
                wire_end_pos = menu_pos;

                // The context menu is opened as *link* graph menu.
                touch::menu_scroll(ui, for_touch, |ui| {
                    viewer.show_dropped_wire_menu(menu_pos, ui, pins, moved_from, snarl);
                });
            });

            // Even though menu could be closed in `show_dropped_wire_menu`,
            // we need to revert the new wires here, because menu state is inaccessible.
            // Next frame context menu won't be shown and wires will be removed.
            if menu.is_some() {
                snarl_state.set_new_wires_menu(new_wires);
            }
        }
    } else {
        // (A finger held on a wire is after the wire's menu, though egui calls it a long
        // touch on the canvas before the hold is up.)
        let at = ui.input(|i| i.pointer.interact_pos()).unwrap_or_else(|| ui_rect.center());
        if wire_held.is_none() && viewer.has_graph_menu(at, snarl) {
            touch::context_menu(&snarl_resp, |ui| {
                let menu_pos = from_global * ui.cursor().min;

                viewer.show_graph_menu(menu_pos, ui, snarl);
            });
        }
    }

    match snarl_state.new_wires() {
        None => {}
        Some(NewWires::In(in_pins)) => {
            for &in_pin in in_pins {
                let from_pos = wire_end_pos;
                // FLEXINPUT PATCH: picked-up wires can end on a pin that isn't drawn.
                let Some(to_r) = input_info.get(&in_pin) else {
                    continue;
                };

                draw_wire(
                    &ui,
                    WireId::NewInput { snarl_id, in_pin },
                    &mut wire_shapes,
                    wire_frame_size,
                    style.get_upscale_wire_frame(),
                    style.get_downscale_wire_frame(),
                    from_pos,
                    to_r.pos,
                    Stroke::new(wire_width, to_r.wire_color),
                    wire_threshold,
                    to_r.wire_style,
                );
            }
        }
        Some(NewWires::Out(out_pins)) => {
            for &out_pin in out_pins {
                let Some(from_r) = output_info.get(&out_pin) else {
                    continue;
                };
                let to_pos = wire_end_pos;

                draw_wire(
                    &ui,
                    WireId::NewOutput { snarl_id, out_pin },
                    &mut wire_shapes,
                    wire_frame_size,
                    style.get_upscale_wire_frame(),
                    style.get_downscale_wire_frame(),
                    from_r.pos,
                    to_pos,
                    Stroke::new(wire_width, from_r.wire_color),
                    wire_threshold,
                    from_r.wire_style,
                );
            }
        }
    }

    // FLEXINPUT PATCH: the nodes neither in focus nor wired to it go part-way transparent
    // with the wires - next frame, when they are drawn (see the node loop).
    {
        let lit: Vec<NodeId> = focus.nodes.iter().chain(focus.neighbors.iter()).copied().collect();
        ui.ctx().data_mut(|d| d.insert_temp(snarl_id.with("focus-lit"), (lit, focus.amount)));
    }

    match wire_shape_idx {
        None => {
            ui.painter().add(Shape::Vec(wire_shapes));
        }
        Some(idx) => {
            ui.painter().set(idx, Shape::Vec(wire_shapes));
        }
    }

    if let Some((origin, progress, out_pin)) = wire_held {
        let color = output_info.get(&out_pin).map_or(Color32::WHITE, |r| r.wire_color);
        touch::paint_hold_ring(ui.painter(), from_global * origin, to_global.scaling, progress, color);
    }

    ui.advance_cursor_after_rect(Rect::from_min_size(snarl_resp.rect.min, Vec2::ZERO));

    if let Some(node) = node_to_top
        && snarl.nodes.contains(node.0)
    {
        snarl_state.node_to_top(node);
    }

    // FLEXINPUT PATCH: on the grid, dragged nodes snap as they go (Alt drags freely):
    // each frame's snap starts from where the drag alone would have put them.
    let snap_cfg = grid_cfg.filter(|g| g.snap);

    // FLEXINPUT PATCH: nodes the user just placed settle on the grid where they were put,
    // once they've been drawn at their size (see `Snarl::insert_node_placed`), and push
    // whatever they landed on out of their way.
    let mut placed: Vec<NodeId> = Vec::new();
    if let Some(grid) = snap_cfg {
        let nodes = &snarl.nodes;
        snarl.placing.retain_mut(|(n, wait)| {
            if !nodes.contains(n.0) {
                return false;
            }
            if *wait > 0 {
                *wait -= 1;
                return true;
            }
            placed.push(*n);
            false
        });
        let first = real_rects.iter().find(|(n, _)| placed.contains(n)).map(|&(_, r)| r);
        if let Some(first) = first {
            // The whole group moves alike, so a pasted group keeps its shape.
            let by = grid.snap_offset(first);
            let moves: Vec<(NodeId, Vec2)> = placed.iter().map(|&n| (n, by)).collect();
            apply_pushes(snarl, &moves, ui.ctx());
            if grid.push {
                let moved: Vec<(NodeId, Rect)> = real_rects
                    .iter()
                    .map(|&(n, r)| (n, if placed.contains(&n) { r.translate(by) } else { r }))
                    .collect();
                apply_pushes(snarl, &grid::push_apart(&grid, &moved, &placed), ui.ctx());
            }
        }
    } else {
        snarl.placing.clear();
    }
    let mut grid_dragging = false;
    if let Some((node, delta)) = node_moved
        && snarl.nodes.contains(node.0)
        && !snarl_state.pinching()
    {
        ui.ctx().request_repaint();
        let moving: Vec<NodeId> = if snarl_state.selected_nodes().contains(&node) {
            snarl_state.selected_nodes().to_vec()
        } else {
            vec![node]
        };
        if let Some(grid) = snap_cfg {
            grid_dragging = true;
            let same = |d: &GridDrag| {
                d.origins.len() == moving.len() && d.origins.iter().all(|(n, _)| moving.contains(n))
            };
            let mut drag = GridDrag::load(ui.ctx(), snarl_id).filter(same).unwrap_or_else(|| GridDrag {
                origins: moving
                    .iter()
                    .filter(|n| snarl.nodes.contains(n.0))
                    .map(|&n| (n, snarl.nodes[n.0].pos))
                    .collect(),
                moved: Vec2::ZERO,
            });
            drag.moved += delta;
            for &(n, origin) in &drag.origins {
                if !snarl.nodes.contains(n.0) {
                    continue;
                }
                let free = origin + drag.moved;
                let offset = frame_offsets.get(&n).copied().unwrap_or(Vec2::ZERO);
                snarl.nodes[n.0].pos = if modifiers.alt { free } else { grid.snap_pos(free, offset) };
            }
            drag.store(ui.ctx(), snarl_id);
        } else {
            for node in moving {
                let node = &mut snarl.nodes[node.0];
                node.pos += delta;
            }
        }
    }

    // FLEXINPUT PATCH: a node dropped onto others, or grown into them by the user,
    // pushes them out of its way.
    let mut dropped: Option<Vec<NodeId>> = None;
    if !grid_dragging && let Some(drag) = GridDrag::load(ui.ctx(), snarl_id) {
        GridDrag::clear(ui.ctx(), snarl_id);
        let movers: Vec<NodeId> = drag.origins.iter().map(|&(n, _)| n).collect();
        if let Some(grid) = grid_cfg.filter(|g| g.push) {
            apply_pushes(snarl, &grid::push_apart(&grid, &real_rects, &movers), ui.ctx());
        }
        dropped = Some(movers);
    }
    if let Some(grid) = grid_cfg.filter(|g| g.push) {
        let mut memory = GridMemory::load(ui.ctx(), snarl_id);
        let (pressed, down, now) =
            ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.time));
        if pressed && let Some(pos) = latest_pos {
            memory.note_press(pos, now);
        } else if down {
            memory.note_held(now);
        }
        let grown = memory.grown(&real_rects, now);
        if !grown.is_empty() && !grid_dragging {
            apply_pushes(snarl, &grid::push_apart(&grid, &real_rects, &grown), ui.ctx());
            memory.settle(&grown);
        }
        if let Some(movers) = &dropped {
            memory.settle(movers);
        }
        // A wire or two the user just connected (not a patch's worth arriving at once).
        let wires = snarl.wires.iter().count();
        let before = memory.wire_count();
        if memory.wire_added(wires) && wires - before <= 4 && memory.pressed_lately(now) {
            memory.settle(&[]);
        }

        // FLEXINPUT PATCH: after any of those, wires that ran out of room in a gap get
        // it: the modules beside it move apart, a round at a time as the wires reroute
        // into the wider gap. Not in the frame the change lands, whose routes predate it.
        if !grid_dragging
            && dropped.is_none()
            && grown.is_empty()
            && let Some(routes) = routes.as_ref()
            && let Some(movers) = memory.settle_round()
        {
            let moves = grid::spread_crowded(&grid, &real_rects, &routes.crowding, &movers);
            if moves.is_empty() {
                memory.settled();
            } else {
                apply_pushes(snarl, &moves, ui.ctx());
            }
        }
        memory.store(ui.ctx(), snarl_id);
    }

    snarl_state.store(snarl, ui.ctx());

    snarl_resp
}

// FLEXINPUT PATCH: touch targets, in points on screen whatever the zoom.

/// How far from a pin's center a finger can land on it, sideways.
const TOUCH_PIN_REACH: f32 = 22.0;

/// The same reach's limit in graph space: zoomed far out, a pin mustn't reach over a
/// neighboring node and take its touches.
const TOUCH_PIN_REACH_MAX: f32 = 30.0;

/// How far from a pin a wire dropped by a finger still lands on it.
const TOUCH_SNAP_RADIUS: f32 = 36.0;

/// How far from a wire a finger coming down still finds it.
const TOUCH_WIRE_REACH: f32 = 28.0;

/// Where a finger can land on the pin at `pin` in the row `row`. Most of it lies outward,
/// away from the node (`outward` is -1 for the left, 1 for the right), because a pin takes
/// touches from any widget in the node's body that it overlaps. It stays inside the row,
/// so that neighboring pins never fight over a touch, unless the pin isn't in its row
/// (an Auto-Map pin moved up to the node's header), where it has no neighbors.
fn touch_pin_rect(pin: Rect, row: Rect, scale: f32, outward: f32) -> Rect {
    let reach = (TOUCH_PIN_REACH / scale).min(TOUCH_PIN_REACH_MAX);
    let c = pin.center();
    let (top, bottom) = if row.y_range().contains(c.y) {
        (row.top().min(pin.top()), row.bottom().max(pin.bottom()))
    } else {
        (c.y - reach * 0.5, c.y + reach * 0.5)
    };
    let (out_x, in_x) = (c.x + outward * reach, c.x - outward * reach * 0.4);
    Rect::from_min_max(
        pos2(out_x.min(in_x), (c.y - reach).max(top)),
        pos2(out_x.max(in_x), (c.y + reach).min(bottom)),
    )
}

/// The pin nearest `pos` that would take these wires, and where it is.
fn nearest_pin<T, V>(
    new_wires: &NewWires,
    pos: Pos2,
    radius: f32,
    inputs: &HashMap<InPinId, PinResponse>,
    outputs: &HashMap<OutPinId, PinResponse>,
    viewer: &mut V,
    snarl: &Snarl<T>,
) -> Option<(AnyPin, Pos2)>
where
    V: SnarlViewer<T>,
{
    let mut best: Option<(AnyPin, Pos2)> = None;
    let mut best_dist = radius;
    let mut consider = |pin: AnyPin, at: Pos2, accepts: &mut dyn FnMut(&mut V) -> bool| {
        let dist = at.distance(pos);
        if dist <= best_dist && accepts(viewer) {
            best = Some((pin, at));
            best_dist = dist;
        }
    };
    match new_wires {
        NewWires::Out(out_pins) => {
            for (&id, r) in inputs {
                consider(AnyPin::In(id), r.pos, &mut |viewer| {
                    let to = InPin::new(snarl, id);
                    out_pins
                        .iter()
                        .any(|&out| viewer.can_connect(&OutPin::new(snarl, out), &to, snarl))
                });
            }
        }
        NewWires::In(in_pins) => {
            for (&id, r) in outputs {
                consider(AnyPin::Out(id), r.pos, &mut |viewer| {
                    let from = OutPin::new(snarl, id);
                    in_pins
                        .iter()
                        .any(|&to| viewer.can_connect(&from, &InPin::new(snarl, to), snarl))
                });
            }
        }
    }
    best
}

/// FLEXINPUT PATCH: the grip on a resizable node's bottom-right corner. Dragging it
/// asks the viewer for a frame that size: whole blocks on the grid (Alt for any size),
/// counted from the frame's actual size when the drag began. It sits on the corner of the
/// frame as drawn (`drawn`), which eases to the size the grip asked for.
#[allow(clippy::too_many_arguments)]
fn size_grip<T, V>(
    ui: &mut Ui,
    node: NodeId,
    node_id: Id,
    frame: Rect,
    drawn: Rect,
    narrowest: f32,
    grid: Option<GridLayout>,
    modifiers: Modifiers,
    snarl_state: &mut SnarlState,
    viewer: &mut V,
    snarl: &mut Snarl<T>,
    uniform: bool,
) where
    V: SnarlViewer<T>,
{
    const GRIP: f32 = 12.0;
    let rect = Rect::from_min_max(drawn.max - vec2(GRIP, GRIP), drawn.max);
    let resp = ui
        .interact(rect, node_id.with("size_grip"), Sense::drag())
        .on_hover_cursor(egui::CursorIcon::ResizeNwSe)
        .on_hover_text("Drag to resize, in blocks");
    let key = node_id.with("size_grip_from");
    if resp.drag_started() {
        ui.ctx()
            .data_mut(|d| d.insert_temp(key, (frame.width(), 0.0_f32, frame.height(), 0.0_f32)));
    }
    if resp.dragged() {
        snarl_state.mark_chrome_dragged();
        let (from_w, moved_x, from_h, moved_y) = ui
            .ctx()
            .data(|d| d.get_temp::<(f32, f32, f32, f32)>(key))
            .unwrap_or((frame.width(), 0.0, frame.height(), 0.0));
        let moved_x = moved_x + resp.drag_delta().x;
        let moved_y = moved_y + resp.drag_delta().y;
        ui.ctx()
            .data_mut(|d| d.insert_temp(key, (from_w, moved_x, from_h, moved_y)));
        // FLEXINPUT PATCH: a node that keeps its shape grows by one amount both ways -
        // whichever way the grip has been pulled further since it was picked up.
        let (moved_x, moved_y) = if uniform {
            let d = if moved_y.abs() > moved_x.abs() { moved_y } else { moved_x };
            (d, d)
        } else {
            (moved_x, moved_y)
        };
        let free_w = (from_w + moved_x).max(narrowest).max(ui.spacing().interact_size.x);
        // The height is a floor the viewer may exceed, so the only bound here is one
        // block: content decides what a node actually needs, and a body that runs out of
        // room grows itself another block without the grip having to know.
        let free_h = (from_h + moved_y).max(ui.spacing().interact_size.y);
        let (width, height) = match grid {
            Some(g) if !modifiers.alt => (
                ((free_w / g.block).round() * g.block).max(g.snap_len(narrowest)),
                (free_h / g.block).round() * g.block,
            ),
            _ => (free_w, free_h),
        };
        viewer.resize_node(node, width, height, snarl);
        ui.ctx().request_repaint();
    }
    if resp.drag_stopped() {
        ui.ctx().data_mut(|d| d.remove::<(f32, f32, f32, f32)>(key));
    }

    // Three short diagonal strokes in the corner, brighter under the pointer.
    let visuals = ui.visuals();
    let color = if resp.hovered() || resp.dragged() {
        visuals.widgets.hovered.fg_stroke.color
    } else {
        visuals.weak_text_color().gamma_multiply(0.6)
    };
    let stroke = Stroke::new(1.0_f32, color);
    let corner = drawn.max - vec2(3.0, 3.0);
    for k in 1..=3 {
        let d = k as f32 * 3.0;
        ui.painter().line_segment([corner - vec2(d, 0.0), corner - vec2(0.0, d)], stroke);
    }
}

/// FLEXINPUT PATCH: at full strength, the wires of the nodes in focus are drawn this
/// much thicker than usual, and lightened this far toward white…
const FOCUS_THICKEN: f32 = 0.9;
const FOCUS_BRIGHTEN: f32 = 0.2;
/// …and everything else - the other wires, and the nodes neither in focus nor wired to it -
/// goes this far transparent. Transparent rather than darker: a shaded module reads as
/// switched off, a see-through one as set back. Half at most, so it can all still be read.
const FOCUS_FADE: f32 = 0.5;

/// FLEXINPUT PATCH: which nodes' wires stand out this frame, the nodes at their other
/// ends, and how far along the fade in or out the emphasis is (times its strength).
struct WireFocus {
    nodes: Vec<NodeId>,
    neighbors: Vec<NodeId>,
    amount: f32,
    /// How far along the fade in or out the focus is (0 to 1), whatever its strength: how
    /// much of a see-through wire it shows.
    fade: f32,
}

/// FLEXINPUT PATCH: the nodes in focus: the selected ones and the one under the pointer
/// (the topmost, and only when nothing else, a menu or a window, is over it). The
/// emphasis fades in and out, and keeps the last focus while it fades out.
fn wire_focus<T>(
    ui: &Ui,
    snarl: &Snarl<T>,
    snarl_id: Id,
    snarl_state: &SnarlState,
    node_rects: &[(NodeId, Rect)],
    strength: Option<f32>,
    host_layer: LayerId,
    reveal: bool,
) -> WireFocus {
    let strength = strength.unwrap_or(0.0).clamp(0.0, 1.0);
    let none = WireFocus { nodes: Vec::new(), neighbors: Vec::new(), amount: 0.0, fade: 0.0 };
    // With see-through wires, focus shows a node's wires whatever its strength.
    if strength <= 0.0 && !reveal {
        return none;
    }
    let mut nodes: Vec<NodeId> = snarl_state.selected_nodes().to_vec();
    // Under the pointer, with nothing (a menu, a window) over the graph there.
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let to_global = snarl_state.to_global();
    let over_graph = pointer.is_some_and(|p| ui.ctx().layer_id_at(p) == Some(host_layer));
    let hovered = pointer
        .filter(|_| over_graph)
        .and_then(|p| node_rects.iter().rev().find(|(_, r)| (to_global * *r).contains(p)));
    if let Some(&(hovered, _)) = hovered
        && !nodes.contains(&hovered)
    {
        nodes.push(hovered);
    }
    let id = snarl_id.with("flexinput_wire_focus");
    let fade = ui.ctx().animate_bool_with_time(id, !nodes.is_empty(), 0.15);
    if nodes.is_empty() {
        nodes = ui.ctx().data(|d| d.get_temp::<Vec<NodeId>>(id)).unwrap_or_default();
    } else {
        let kept = nodes.clone();
        ui.ctx().data_mut(|d| d.insert_temp(id, kept));
    }
    if fade <= 0.0 || nodes.is_empty() {
        return none;
    }
    let mut neighbors = Vec::new();
    for wire in snarl.wires.iter() {
        let (a, b) = (wire.out_pin.node, wire.in_pin.node);
        if nodes.contains(&a) && !neighbors.contains(&b) {
            neighbors.push(b);
        }
        if nodes.contains(&b) && !neighbors.contains(&a) {
            neighbors.push(a);
        }
    }
    WireFocus { nodes, neighbors, amount: strength * fade, fade }
}

/// FLEXINPUT PATCH: a node gliding from where it was drawn to where it is now.
#[derive(Clone, Copy, Default)]
struct Glide {
    from: Pos2,
    to: Pos2,
    start: f64,
}

/// How far along a glide of `ease` seconds, started at `start`, is at `now`, eased out (fast,
/// then settling): 0 to 1, 1 from `ease` on.
fn glide_t(now: f64, start: f64, ease: f32) -> f32 {
    let t = (((now - start) / f64::from(ease.max(1e-3))) as f32).clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// FLEXINPUT PATCH: where a node at `target` is drawn: gliding from where it was drawn when it
/// moved, all the way in `ease` seconds; moved again on the way, from where it is then. A node
/// settled where it was that moves only a little - dragged freely, a point or two a frame -
/// follows at once: a glide would only make it trail the pointer. At once with `ease` 0.
fn eased_pos(ctx: &egui::Context, node_id: Id, target: Pos2, ease: f32) -> Pos2 {
    eased_at(ctx, node_id.with("flexinput-glide"), target, ease)
}

/// FLEXINPUT PATCH: the glide nodes move with, for a viewer to ease something of its own
/// the same way (a size its grip asked for): `target` as drawn this frame, under `key`,
/// over `ease` seconds. See [`eased_pos`].
pub fn ease_toward(ctx: &egui::Context, key: Id, target: Pos2, ease: f32) -> Pos2 {
    eased_at(ctx, key, target, ease)
}

/// FLEXINPUT PATCH: [`eased_pos`] for any point kept under `key` - a node's place, or its
/// size taken as a point.
fn eased_at(ctx: &egui::Context, key: Id, target: Pos2, ease: f32) -> Pos2 {
    if ease <= 0.0 {
        ctx.data_mut(|d| d.remove_temp::<Glide>(key));
        return target;
    }
    // A glide starts a frame back, so the frame that sees the move already shows some of it.
    let (now, dt) = ctx.input(|i| (i.time, f64::from(i.stable_dt.clamp(0.0, 0.1))));
    let at = |g: &Glide| g.from + (g.to - g.from) * glide_t(now, g.start, ease);
    let still = Glide { from: target, to: target, start: now - f64::from(ease) };
    let mut g = ctx.data(|d| d.get_temp::<Glide>(key)).unwrap_or(still);
    if (target - g.to).length() > 0.01 {
        let shown = at(&g);
        let settled = (shown - g.to).length() < 0.5;
        g = if (settled && (target - g.to).length() <= EASE_SMALL_STEP) || !shown.is_finite() {
            still
        } else {
            Glide { from: shown, to: target, start: now - dt }
        };
    }
    let shown = at(&g);
    if (shown - g.to).length() > 0.01 {
        ctx.request_repaint();
    }
    ctx.data_mut(|d| d.insert_temp(key, g));
    shown
}

/// FLEXINPUT PATCH: a node holding more than it was laid out at, down an axis its viewer has
/// (`snaps_down`): not, for the first frame (a row re-wrapping is measured a frame late, and
/// spills for that frame), or on its way to the grid line above what it holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Overflow {
    #[default]
    None,
    Spilled,
    Gliding,
}

/// FLEXINPUT PATCH: how much more than it was laid out at a node can hold, down an axis its
/// viewer has (`snaps_down`), before the frame sets off for the grid line above it: a row that
/// re-wraps as the width eases is measured a frame late, and spills a few points for it.
const SNAP_DOWN_SLACK: f32 = 8.0;

/// FLEXINPUT PATCH: a move or a change of route this small, from where things had settled,
/// is drawn at once rather than eased.
const EASE_SMALL_STEP: f32 = 6.0;

/// FLEXINPUT PATCH: a routed wire as drawn: its latest route, and - while it's on its way
/// from an old one - the shape it set off from and when.
#[derive(Clone, Default)]
struct EasedWire {
    route: Vec<Pos2>,
    from: Option<(Vec<Pos2>, f64)>,
}

/// How many points an easing wire is resampled to, its old shape and its new alike.
const EASE_POINTS: usize = 64;

/// FLEXINPUT PATCH: the wires in `drawn` on their way from an old route to a new one, as
/// they're drawn this frame: each point of the old shape going to the same point along the new
/// (by length), all the way in `ease` seconds, the ends held on the pins. A route that changes
/// again on the way is the new end of the same glide (a node gliding moves its wires' routes
/// every frame); a wire seen for the first time, a small change from a settled wire, or `ease`
/// 0, draws its route at once.
fn eased_routes(
    ctx: &egui::Context,
    snarl_id: Id,
    drawn: &HashMap<(OutPinId, InPinId), Vec<Pos2>>,
    ease: f32,
) -> HashMap<(OutPinId, InPinId), Vec<Pos2>> {
    let key = snarl_id.with("flexinput-eased-wires");
    let mut out = HashMap::new();
    if ease <= 0.0 {
        ctx.data_mut(|d| d.remove_temp::<HashMap<(OutPinId, InPinId), EasedWire>>(key));
        return out;
    }
    let (now, dt) = ctx.input(|i| (i.time, f64::from(i.stable_dt.clamp(0.0, 0.1))));
    let mut state: HashMap<(OutPinId, InPinId), EasedWire> = ctx.data_mut(|d| d.remove_temp(key)).unwrap_or_default();
    state.retain(|w, _| drawn.contains_key(w));
    for (w, route) in drawn {
        let Some(e) = state.get_mut(w) else {
            state.insert(*w, EasedWire { route: route.clone(), from: None });
            continue;
        };
        if e.route != *route {
            if e.from.is_none() {
                let (old, new) = (resample(&e.route, EASE_POINTS), resample(route, EASE_POINTS));
                let apart = old.iter().zip(&new).map(|(a, b)| (*a - *b).length()).fold(0.0f32, f32::max);
                if old.len() == new.len() && !new.is_empty() && apart > EASE_SMALL_STEP {
                    e.from = Some((old, now - dt));
                }
            }
            e.route = route.clone();
        }
        let Some((from, start)) = &e.from else { continue };
        let t = glide_t(now, *start, ease);
        let to = resample(route, EASE_POINTS);
        if t >= 1.0 || from.len() != to.len() || to.is_empty() {
            e.from = None;
            continue;
        }
        let mut next: Vec<Pos2> = from.iter().zip(&to).map(|(a, b)| *a + (*b - *a) * t).collect();
        // The ends on the pins, the correction spread along the wire.
        let (d0, d1) = (to[0] - next[0], to[to.len() - 1] - next[next.len() - 1]);
        let last = (next.len() - 1).max(1) as f32;
        for (i, p) in next.iter_mut().enumerate() {
            let s = i as f32 / last;
            *p += d0 * (1.0 - s) + d1 * s;
        }
        out.insert(*w, next);
    }
    if !out.is_empty() {
        ctx.request_repaint();
    }
    ctx.data_mut(|d| d.insert_temp(key, state));
    out
}

/// `path` as `n` points evenly spaced along its length.
fn resample(path: &[Pos2], n: usize) -> Vec<Pos2> {
    if path.len() < 2 || n < 2 {
        return path.to_vec();
    }
    let lens: Vec<f32> = path.windows(2).map(|w| (w[1] - w[0]).length()).collect();
    let total: f32 = lens.iter().sum();
    if total <= 0.0 {
        return vec![path[0]; n];
    }
    let mut out = Vec::with_capacity(n);
    let (mut seg, mut walked) = (0usize, 0.0f32);
    for i in 0..n {
        let at = total * i as f32 / (n - 1) as f32;
        while seg + 1 < lens.len() && walked + lens[seg] < at {
            walked += lens[seg];
            seg += 1;
        }
        let t = if lens[seg] > 0.0 { ((at - walked) / lens[seg]).clamp(0.0, 1.0) } else { 0.0 };
        out.push(path[seg] + (path[seg + 1] - path[seg]) * t);
    }
    out
}

/// FLEXINPUT PATCH: how many times the style's wire width a routed wire is drawn, at
/// least.
const ROUTED_WIRE_WIDTH: f32 = 2.2;

/// FLEXINPUT PATCH: a routed wire's width factor, for a pin's `factor`.
fn routed_factor(factor: f32) -> f32 {
    factor.max(ROUTED_WIRE_WIDTH)
}

/// FLEXINPUT PATCH: how far each side of a routed wire its dark underlay reaches: where
/// wires cross, the one on top cuts a dark edge into the one below. Less than half the
/// clear space between lanes, so wires side by side don't shade each other.
const WIRE_SHADE: f32 = 1.3;

/// FLEXINPUT PATCH: moves pushed nodes by how far they were pushed.
fn apply_pushes<T>(snarl: &mut Snarl<T>, pushes: &[(NodeId, Vec2)], cx: &egui::Context) {
    for &(node, by) in pushes {
        if let Some(n) = snarl.nodes.get_mut(node.0) {
            n.pos += by;
        }
    }
    if !pushes.is_empty() {
        cx.request_repaint();
    }
}

/// FLEXINPUT PATCH: draws a routed wire along its (already rounded) path, over a dark
/// underlay `shade` that sets it off from wires it crosses.
fn draw_routed_wire(ui: &Ui, shapes: &mut Vec<Shape>, path: &[Pos2], mut stroke: Stroke, shade: Color32) {
    if path.len() < 2 || !ui.is_visible() {
        return;
    }
    if stroke.width < 1.0 {
        stroke.color = stroke.color.gamma_multiply(stroke.width);
        stroke.width = 1.0;
    }
    if ui.is_rect_visible(Rect::from_points(path).expand(stroke.width + WIRE_SHADE)) {
        let under = Stroke::new(stroke.width + 2.0 * WIRE_SHADE, shade.gamma_multiply(0.85));
        shapes.push(Shape::line(path.to_vec(), under));
        shapes.push(Shape::line(path.to_vec(), stroke));
    }
}

/// The wire nearest `pos`, if one is within `reach`. Wires only answer whether a point is
/// within some distance, so this asks at growing distances, which is plenty for a fingertip
/// and only happens when one comes down.
#[allow(clippy::too_many_arguments)]
fn nearest_wire<T>(
    ctx: &egui::Context,
    snarl: &Snarl<T>,
    snarl_id: Id,
    style: &SnarlStyle,
    pos: Pos2,
    reach: f32,
    frame_size: f32,
    inputs: &HashMap<InPinId, PinResponse>,
    outputs: &HashMap<OutPinId, PinResponse>,
    routes: Option<&RouteSet>,
) -> Option<(OutPinId, InPinId)> {
    const STEPS: usize = 7;
    (1..=STEPS).find_map(|step| {
        let threshold = reach * step as f32 / STEPS as f32;
        snarl.wires.iter().find_map(|wire| {
            let (from, to) = (outputs.get(&wire.out_pin)?, inputs.get(&wire.in_pin)?);
            // FLEXINPUT PATCH: a routed wire is where its route is.
            if let Some(path) = routes.and_then(|r| r.paths.get(&(wire.out_pin, wire.in_pin))) {
                return (route::distance_sq(path, pos) < threshold * threshold)
                    .then_some((wire.out_pin, wire.in_pin));
            }
            hit_wire(
                ctx,
                WireId::Connected { snarl_id, out_pin: wire.out_pin, in_pin: wire.in_pin },
                frame_size,
                style.get_upscale_wire_frame(),
                style.get_downscale_wire_frame(),
                from.pos,
                to.pos,
                pos,
                threshold,
                pick_wire_style(from.wire_style, to.wire_style),
            )
            .then_some((wire.out_pin, wire.in_pin))
        })
    })
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
fn draw_inputs<T, V>(
    snarl: &mut Snarl<T>,
    viewer: &mut V,
    node: NodeId,
    inputs: &[InPin],
    pin_size: f32,
    style: &SnarlStyle,
    node_ui: &mut Ui,
    inputs_rect: Rect,
    payload_clip_rect: Rect,
    input_x: f32,
    min_pin_y_top: f32,
    min_pin_y_bottom: f32,
    input_spacing: Option<f32>,
    snarl_state: &mut SnarlState,
    modifiers: Modifiers,
    input_positions: &mut HashMap<InPinId, PinResponse>,
    heights: Heights,
) -> DrawPinsResponse
where
    V: SnarlViewer<T>,
{
    let mut drag_released = false;
    let mut pin_hovered = None;

    // Input pins on the left.
    let mut inputs_ui = node_ui.new_child(
        UiBuilder::new()
            .max_rect(inputs_rect.round_ui())
            .layout(Layout::top_down(Align::Min))
            .id_salt("inputs"),
    );

    let snarl_clip_rect = node_ui.clip_rect();
    inputs_ui.shrink_clip_rect(payload_clip_rect);

    let touch = touch::is_active(node_ui.ctx());
    let scale = snarl_state.to_global().scaling;

    let pin_layout = Layout::left_to_right(Align::Min);
    let mut new_heights = SmallVec::with_capacity(inputs.len());

    for in_pin in inputs {
        // Show input pin.
        let cursor = inputs_ui.cursor();
        let (height, height_outer) = heights.get(in_pin.id.input);

        let margin = (height_outer - height) / 2.0;
        let outer_rect = cursor.with_max_y(cursor.top() + height_outer);
        let inner_rect = outer_rect.shrink2(vec2(0.0, margin));

        let builder = UiBuilder::new().layout(pin_layout).max_rect(inner_rect);

        inputs_ui.scope_builder(builder, |pin_ui| {
            if let Some(input_spacing) = input_spacing {
                let min = pin_ui.next_widget_position();
                pin_ui.advance_cursor_after_rect(Rect::from_min_size(
                    min,
                    vec2(input_spacing, pin_size),
                ));
            }

            let y0 = pin_ui.max_rect().min.y;
            let y1 = pin_ui.max_rect().max.y;

            // Show input content
            let snarl_pin = viewer.show_input(in_pin, pin_ui, snarl);
            if !snarl.nodes.contains(node.0) {
                // If removed
                return;
            }

            let pin_rect = snarl_pin.pin_rect(
                input_x,
                min_pin_y_top.max(y0),
                min_pin_y_bottom.max(y1),
                pin_size,
            );

            // Interact with pin shape.
            pin_ui.set_clip_rect(snarl_clip_rect);

            // FLEXINPUT PATCH: a finger gets a bigger target than the pin it's for.
            let hit_rect = if touch {
                touch_pin_rect(pin_rect, outer_rect, scale, -1.0)
            } else {
                pin_rect
            };
            let r = pin_ui.interact(hit_rect, pin_ui.next_auto_id(), Sense::click_and_drag());

            pin_ui.skip_ahead_auto_ids(1);

            if r.dragged() {
                snarl_state.mark_chrome_dragged();
            }

            if r.clicked_by(PointerButton::Secondary) {
                if snarl_state.has_new_wires() {
                    snarl_state.remove_new_wire_in(in_pin.id);
                } else {
                    viewer.drop_inputs(in_pin, snarl);
                    if !snarl.nodes.contains(node.0) {
                        // If removed
                        return;
                    }
                }
            }

            // FLEXINPUT PATCH: a finger on a wired pin could be dragging a new wire out or
            // holding to pick up the ones there, and egui's drag can't tell: it starts
            // on small wobbles, and egui ends it at a long touch, before the hold is up.
            // So the hold decides, and ends with or without egui's drag.
            let this_pin = AnyPin::In(in_pin.id);
            if touch
                && !modifiers.command
                && !in_pin.remotes.is_empty()
                && r.is_pointer_button_down_on()
                && !snarl_state.has_new_wires()
                && !snarl_state.pinching()
            {
                snarl_state.begin_wire_hold(HoldOn::Pin(this_pin), pin_ui.ctx());
            }
            let hold = snarl_state.wire_hold().filter(|hold| hold.on() == HoldOn::Pin(this_pin));

            if r.drag_started_by(PointerButton::Primary) && hold.is_none() && !snarl_state.pinching() {
                if modifiers.command {
                    snarl_state.start_new_wires_out(&in_pin.remotes);
                    if !modifiers.shift {
                        snarl_state.set_moved_from(this_pin);
                    }
                } else {
                    snarl_state.start_new_wire_in(in_pin.id);
                }
            }

            // A long touch ends egui's drag on an unwired pin too.
            if touch && r.long_touched() && hold.is_none() && !snarl_state.has_new_wires() && !snarl_state.pinching() {
                snarl_state.start_new_wire_in(in_pin.id);
            }

            let mut hold_progress = None;
            if let Some(hold) = hold {
                match hold.step(pin_ui.ctx()) {
                    WireHoldStep::Holding(progress) => {
                        hold_progress = Some(progress);
                        pin_ui.ctx().request_repaint();
                    }
                    WireHoldStep::Moved => {
                        snarl_state.stop_wire_hold();
                        snarl_state.start_new_wire_in(in_pin.id);
                    }
                    WireHoldStep::Lifted => {
                        snarl_state.stop_wire_hold();
                        snarl_state.start_new_wires_out(&in_pin.remotes);
                        snarl_state.set_moved_from(this_pin);
                    }
                    WireHoldStep::Gone => snarl_state.stop_wire_hold(),
                }
            }

            if r.drag_stopped() {
                drag_released = true;
            }

            let mut visual_pin_rect = pin_rect;

            if r.contains_pointer() {
                if snarl_state.has_new_wires_in() {
                    if modifiers.shift && !modifiers.command {
                        snarl_state.add_new_wire_in(in_pin.id);
                    }
                    if !modifiers.shift && modifiers.command {
                        snarl_state.remove_new_wire_in(in_pin.id);
                    }
                }
                pin_hovered = Some(AnyPin::In(in_pin.id));
                visual_pin_rect = visual_pin_rect.scale_from_center(1.2);
            }

            let wire_info =
                snarl_pin.draw(style, pin_ui.style(), visual_pin_rect, pin_ui.painter());

            if let Some(progress) = hold_progress {
                let center = pin_rect.center();
                touch::paint_hold_ring(pin_ui.painter(), center, scale, progress, wire_info.color);
            }

            input_positions.insert(
                in_pin.id,
                PinResponse {
                    pos: pin_rect.center(),
                    wire_color: wire_info.color,
                    wire_style: wire_info.style,
                    wire_width_factor: wire_info.width_factor,
                    activity: wire_info.activity,
                },
            );

            new_heights.push(pin_ui.min_rect().height());

            pin_ui.expand_to_include_y(outer_rect.bottom());
        });
    }

    let final_rect = inputs_ui.min_rect();
    node_ui.expand_to_include_rect(final_rect.intersect(payload_clip_rect));

    DrawPinsResponse {
        drag_released,
        pin_hovered,
        final_rect,
        new_heights,
    }
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
fn draw_outputs<T, V>(
    snarl: &mut Snarl<T>,
    viewer: &mut V,
    node: NodeId,
    outputs: &[OutPin],
    pin_size: f32,
    style: &SnarlStyle,
    node_ui: &mut Ui,
    outputs_rect: Rect,
    payload_clip_rect: Rect,
    output_x: f32,
    min_pin_y_top: f32,
    min_pin_y_bottom: f32,
    output_spacing: Option<f32>,
    snarl_state: &mut SnarlState,
    modifiers: Modifiers,
    output_positions: &mut HashMap<OutPinId, PinResponse>,
    heights: Heights,
) -> DrawPinsResponse
where
    V: SnarlViewer<T>,
{
    let mut drag_released = false;
    let mut pin_hovered = None;

    let mut outputs_ui = node_ui.new_child(
        UiBuilder::new()
            .max_rect(outputs_rect.round_ui())
            .layout(Layout::top_down(Align::Max))
            .id_salt("outputs"),
    );

    let snarl_clip_rect = node_ui.clip_rect();
    outputs_ui.shrink_clip_rect(payload_clip_rect);

    let touch = touch::is_active(node_ui.ctx());
    let scale = snarl_state.to_global().scaling;

    let pin_layout = Layout::right_to_left(Align::Min);
    let mut new_heights = SmallVec::with_capacity(outputs.len());

    // Output pins on the right.
    for out_pin in outputs {
        // Show output pin.
        let cursor = outputs_ui.cursor();
        let (height, height_outer) = heights.get(out_pin.id.output);

        let margin = (height_outer - height) / 2.0;
        let outer_rect = cursor.with_max_y(cursor.top() + height_outer);
        let inner_rect = outer_rect.shrink2(vec2(0.0, margin));

        let builder = UiBuilder::new().layout(pin_layout).max_rect(inner_rect);

        outputs_ui.scope_builder(builder, |pin_ui| {
            // Allocate space for pin shape.
            // The pin column is on the RIGHT in right_to_left layout. We must place
            // the spacing rect to the LEFT of the right edge, not rightward from it.
            // Using next_widget_position() (right edge) as from_min_size's origin would
            // create a rect extending further right, expanding min_rect past the node
            // boundary and causing cumulative x-drift on every subsequent output row.
            if let Some(output_spacing) = output_spacing {
                let right = pin_ui.max_rect().max.x;
                let top   = pin_ui.cursor().min.y;
                pin_ui.advance_cursor_after_rect(Rect::from_min_max(
                    pos2(right - output_spacing, top),
                    pos2(right, top + pin_size),
                ));
            }

            let y0 = pin_ui.max_rect().min.y;
            let y1 = pin_ui.max_rect().max.y;

            // Show output content
            let snarl_pin = viewer.show_output(out_pin, pin_ui, snarl);
            if !snarl.nodes.contains(node.0) {
                // If removed
                return;
            }

            let pin_rect = snarl_pin.pin_rect(
                output_x,
                min_pin_y_top.max(y0),
                min_pin_y_bottom.max(y1),
                pin_size,
            );

            pin_ui.set_clip_rect(snarl_clip_rect);

            // FLEXINPUT PATCH: see `draw_inputs`.
            let hit_rect = if touch {
                touch_pin_rect(pin_rect, outer_rect, scale, 1.0)
            } else {
                pin_rect
            };
            let r = pin_ui.interact(hit_rect, pin_ui.next_auto_id(), Sense::click_and_drag());

            pin_ui.skip_ahead_auto_ids(1);

            if r.dragged() {
                snarl_state.mark_chrome_dragged();
            }

            if r.clicked_by(PointerButton::Secondary) {
                if snarl_state.has_new_wires() {
                    snarl_state.remove_new_wire_out(out_pin.id);
                } else {
                    viewer.drop_outputs(out_pin, snarl);
                    if !snarl.nodes.contains(node.0) {
                        // If removed
                        return;
                    }
                }
            }

            let this_pin = AnyPin::Out(out_pin.id);
            if touch
                && !modifiers.command
                && !out_pin.remotes.is_empty()
                && r.is_pointer_button_down_on()
                && !snarl_state.has_new_wires()
                && !snarl_state.pinching()
            {
                snarl_state.begin_wire_hold(HoldOn::Pin(this_pin), pin_ui.ctx());
            }
            let hold = snarl_state.wire_hold().filter(|hold| hold.on() == HoldOn::Pin(this_pin));

            if r.drag_started_by(PointerButton::Primary) && hold.is_none() && !snarl_state.pinching() {
                if modifiers.command {
                    snarl_state.start_new_wires_in(&out_pin.remotes);

                    if !modifiers.shift {
                        snarl_state.set_moved_from(this_pin);
                    }
                } else {
                    snarl_state.start_new_wire_out(out_pin.id);
                }
            }

            if touch && r.long_touched() && hold.is_none() && !snarl_state.has_new_wires() && !snarl_state.pinching() {
                snarl_state.start_new_wire_out(out_pin.id);
            }

            let mut hold_progress = None;
            if let Some(hold) = hold {
                match hold.step(pin_ui.ctx()) {
                    WireHoldStep::Holding(progress) => {
                        hold_progress = Some(progress);
                        pin_ui.ctx().request_repaint();
                    }
                    WireHoldStep::Moved => {
                        snarl_state.stop_wire_hold();
                        snarl_state.start_new_wire_out(out_pin.id);
                    }
                    WireHoldStep::Lifted => {
                        snarl_state.stop_wire_hold();
                        snarl_state.start_new_wires_in(&out_pin.remotes);
                        snarl_state.set_moved_from(this_pin);
                    }
                    WireHoldStep::Gone => snarl_state.stop_wire_hold(),
                }
            }

            if r.drag_stopped() {
                drag_released = true;
            }

            let mut visual_pin_rect = pin_rect;

            if r.contains_pointer() {
                if snarl_state.has_new_wires_out() {
                    if modifiers.shift && !modifiers.command {
                        snarl_state.add_new_wire_out(out_pin.id);
                    }
                    if !modifiers.shift && modifiers.command {
                        snarl_state.remove_new_wire_out(out_pin.id);
                    }
                }
                pin_hovered = Some(AnyPin::Out(out_pin.id));
                visual_pin_rect = visual_pin_rect.scale_from_center(1.2);
            }

            let wire_info =
                snarl_pin.draw(style, pin_ui.style(), visual_pin_rect, pin_ui.painter());

            if let Some(progress) = hold_progress {
                let center = pin_rect.center();
                touch::paint_hold_ring(pin_ui.painter(), center, scale, progress, wire_info.color);
            }

            output_positions.insert(
                out_pin.id,
                PinResponse {
                    pos: pin_rect.center(),
                    wire_color: wire_info.color,
                    wire_style: wire_info.style,
                    wire_width_factor: wire_info.width_factor,
                    activity: wire_info.activity,
                },
            );

            new_heights.push(pin_ui.min_rect().height());

            pin_ui.expand_to_include_y(outer_rect.bottom());
        });
    }
    let final_rect = outputs_ui.min_rect();
    node_ui.expand_to_include_rect(final_rect.intersect(payload_clip_rect));

    DrawPinsResponse {
        drag_released,
        pin_hovered,
        final_rect,
        new_heights,
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_body<T, V>(
    snarl: &mut Snarl<T>,
    viewer: &mut V,
    node: NodeId,
    inputs: &[InPin],
    outputs: &[OutPin],
    ui: &mut Ui,
    body_rect: Rect,
    payload_clip_rect: Rect,
    _snarl_state: &SnarlState,
) -> DrawBodyResponse
where
    V: SnarlViewer<T>,
{
    let mut body_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(body_rect.round_ui())
            .layout(Layout::left_to_right(Align::Min))
            .id_salt("body"),
    );

    body_ui.shrink_clip_rect(payload_clip_rect);

    viewer.show_body(node, inputs, outputs, &mut body_ui, snarl);

    let final_rect = body_ui.min_rect();
    ui.expand_to_include_rect(final_rect.intersect(payload_clip_rect));
    // node_state.set_body_width(body_size.x);

    DrawBodyResponse { final_rect }
}

//First step for split big function to parts
/// Draw one node. Return Pins info
#[inline]
#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)]
fn draw_node<T, V>(
    snarl: &mut Snarl<T>,
    ui: &mut Ui,
    node: NodeId,
    viewer: &mut V,
    snarl_state: &mut SnarlState,
    style: &SnarlStyle,
    snarl_id: Id,
    input_positions: &mut HashMap<InPinId, PinResponse>,
    modifiers: Modifiers,
    output_positions: &mut HashMap<OutPinId, PinResponse>,
) -> Option<DrawNodeResponse>
where
    V: SnarlViewer<T>,
{
    let Node {
        pos,
        open,
        ref value,
    } = snarl.nodes[node.0];

    // Collect pins
    let inputs_count = viewer.inputs(value);
    let outputs_count = viewer.outputs(value);

    let inputs = (0..inputs_count)
        .map(|idx| InPin::new(snarl, InPinId { node, input: idx }))
        .collect::<Vec<_>>();

    let outputs = (0..outputs_count)
        .map(|idx| OutPin::new(snarl, OutPinId { node, output: idx }))
        .collect::<Vec<_>>();

    // Generate persistent id for the node.
    let node_id = snarl_id.with(("snarl-node", node));

    // FLEXINPUT PATCH: a node that has moved - a grid step while it's dragged, a push, an
    // arrange, an undo - is drawn easing from where it was to where it is.
    let ease = style.get_grid().map_or(0.0, |g| g.ease);
    let node_pos = eased_pos(ui.ctx(), node_id, pos, ease).round_ui();
    // Set before the viewer draws any of it: what `drawn_offset` reads.
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(DRAWN_OFFSET_KEY), (node, node_pos - pos)));

    let openness = ui.ctx().animate_bool(node_id, open);

    let mut node_state = NodeState::load(ui.ctx(), node_id, ui.spacing());

    let mut node_to_top = None;
    let mut node_moved = None;
    let mut drag_released = false;
    let mut pin_hovered = None;

    let node_frame = viewer.node_frame(
        style.get_node_frame(ui.style()),
        node,
        &inputs,
        &outputs,
        snarl,
    );

    let header_frame = viewer.header_frame(
        style.get_header_frame(ui.style()),
        node,
        &inputs,
        &outputs,
        snarl,
    );

    // FLEXINPUT PATCH: on the layout grid the frame is a whole number of blocks: the
    // node's size is rounded up (see the end of this function), and a collapsed node,
    // whose height is its header's, is rounded up here.
    let frame_margin = node_frame.total_margin();
    let margins = frame_margin.sum();
    let grid_snap = style.get_grid().filter(|g| g.snap);

    // FLEXINPUT PATCH: the size the frame is DRAWN at eases to a size it changed to, as its
    // place does - a module grown by what it holds - where it used to jump a block at a
    // time. Drawn only: what the node holds is laid out at the size it has (`node_rect`).
    // Laid out in the easing frame, a body that sizes itself to its room measured a new size
    // every frame of the glide, which started another: modules strobed between two sizes,
    // and a frame filled out to its old size while shrinking could ratchet one a block
    // bigger. Not the first time a node is drawn either: its size is a placeholder then.
    //
    // A size the GRIP asked for is the viewer's to ease (`node_frame_now`): on those axes
    // the node is laid out and drawn at the frame the viewer is easing through this frame,
    // and nothing is eased here - the frame, its pins and what it holds move together.
    let frame_now = viewer.node_frame_now(node, snarl);
    // Width can't follow its contents within a frame (the payload is clipped to the frame's
    // width); height can.
    let frame_now = [
        if frame_now[0] == FrameNow::Contents { FrameNow::Snarl } else { frame_now[0] },
        frame_now[1],
    ];
    let viewers = [frame_now[0] != FrameNow::Snarl, frame_now[1] != FrameNow::Snarl];
    let floor_key = node_id.with("flexinput-content-floor");
    let floor: Vec2 = ui.ctx().data(|d| d.get_temp(floor_key)).unwrap_or(Vec2::ZERO);
    let size_key = node_id.with("flexinput-size-glide");
    let target_size = node_state.size();
    let fresh = node_state.fresh();
    // Across, never narrower than what it held last frame: the payload is clipped to the
    // frame's width. Down it isn't, and the frame takes in what it holds this frame.
    let along = |k: usize, target: f32, margin: f32| match frame_now[k] {
        FrameNow::Eased(f) if k == 0 => (f - margin).max(floor[k]).max(0.0),
        FrameNow::Eased(f) => (f - margin).max(0.0),
        // As little as the header: the frame is what the contents take this frame.
        FrameNow::Contents => node_state.header_height(),
        FrameNow::Snarl => target,
    };
    let laid_out = vec2(along(0, target_size.x, margins.x), along(1, target_size.y, margins.y));
    // Down, on an axis the viewer has, the frame is what it holds whenever that's more than
    // the viewer laid it out at - and that's rounded onto the grid on the way, a glide kept
    // under the same key as snarl's own (`eased_at` below, after the contents are known), so
    // the axis carries on gliding to the same place once the viewer lets go of it. Rounded
    // only after, the frame sat on what it held, then set off for the grid line.
    let snaps_down = viewers[1] && !fresh && ease > 0.0 && grid_snap.is_some();
    // How that went last frame (`Overflow`), and how tall it was laid out then.
    let over_key = size_key.with("over");
    let (was_over, was_laid_h): (Overflow, f32) = if snaps_down {
        ui.ctx().data(|d| d.get_temp(over_key)).unwrap_or((Overflow::None, 0.0))
    } else {
        (Overflow::None, 0.0)
    };
    // A spill is taken at its word at once where the frame is what it holds anyway, or the
    // frame was laid out shorter than last frame: the grip's taking it in past what it holds.
    let spill_now = |laid_h: f32| frame_now[1] == FrameNow::Contents || laid_h < was_laid_h - 0.5;
    let axis = |k: usize, v: f32| {
        let key = size_key.with(k);
        if fresh || viewers[k] {
            if !(k == 1 && snaps_down) {
                ui.ctx().data_mut(|d| d.remove_temp::<Glide>(key));
            }
            v
        } else {
            eased_at(ui.ctx(), key, egui::pos2(v, 0.0), ease).x
        }
    };
    let shown_size = vec2(axis(0, laid_out.x), axis(1, laid_out.y));
    let size_gliding = (shown_size - laid_out).length() > 0.5;
    let mut node_rect = node_state.node_rect_sized(node_pos, openness, laid_out);
    let mut shown_rect = node_state.node_rect_sized(node_pos, openness, shown_size);

    if let Some(g) = grid_snap
        && !open
        && openness <= 0.0
    {
        node_rect.max.y = node_rect.min.y + g.snap_len(node_rect.height() + margins.y) - margins.y;
        shown_rect.max.y = shown_rect.min.y + g.snap_len(shown_rect.height() + margins.y) - margins.y;
    }

    // Rect for node + frame margin.
    let node_frame_rect = node_rect + frame_margin;
    // FLEXINPUT PATCH: and as drawn, easing to it (the same rect once it's there).
    let shown_frame_rect = shown_rect + frame_margin;
    // The frame the node is laid out in, for the viewer to measure its body against
    // (`laid_out_frame`): last frame's size, or the one it's easing through.
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(LAID_OUT_KEY), (node, node_frame_rect.size())));

    // FLEXINPUT PATCH: the selection outline goes round the frame as it's drawn, which isn't
    // known yet while it follows what it holds down (`snaps_down`): a slot for it now, filled
    // once it is.
    let select_slot = snarl_state.selected_nodes().contains(&node).then(|| {
        let select_style = style.get_select_style(ui.style());
        let select_rect = shown_frame_rect + select_style.margin;
        ui.painter().add(egui::epaint::RectShape::new(
            select_rect,
            select_style.rounding,
            select_style.fill,
            select_style.stroke,
            StrokeKind::Inside,
        ))
    });

    // Size of the pin.
    // Side of the square or diameter of the circle.
    let pin_size = style.get_pin_size(ui.style()).max(0.0);

    let pin_placement = style.get_pin_placement();

    let header_drag_space = style.get_header_drag_space(ui.style()).max(Vec2::ZERO);

    // Interact with node frame.
    let r = ui.interact(
        node_frame_rect,
        node_id.with("frame"),
        Sense::click_and_drag(),
    );

    if !modifiers.shift && !modifiers.command && r.dragged_by(PointerButton::Primary) {
        node_moved = Some((node, r.drag_delta()));
    }
    if r.dragged() {
        snarl_state.mark_chrome_dragged();
    }

    if r.clicked_by(PointerButton::Primary) || r.dragged_by(PointerButton::Primary) {
        if modifiers.shift {
            snarl_state.select_one_node(modifiers.command, node);
        } else if modifiers.command {
            snarl_state.deselect_one_node(node);
        }
    }

    if r.clicked() || r.dragged() {
        node_to_top = Some(node);
    }

    if viewer.has_node_menu(&snarl.nodes[node.0].value) {
        touch::context_menu(&r, |ui| {
            viewer.show_node_menu(node, &inputs, &outputs, ui, snarl);
        });
    }

    if !snarl.nodes.contains(node.0) {
        node_state.clear(ui.ctx());
        // If removed
        return None;
    }

    if viewer.has_on_hover_popup(&snarl.nodes[node.0].value) {
        r.on_hover_ui_at_pointer(|ui| {
            viewer.show_on_hover_popup(node, &inputs, &outputs, ui, snarl);
        });
    }

    if !snarl.nodes.contains(node.0) {
        node_state.clear(ui.ctx());
        // If removed
        return None;
    }

    let node_ui = &mut ui.new_child(
        UiBuilder::new()
            .max_rect(node_frame_rect.round_ui())
            .layout(Layout::top_down(Align::Center))
            .id_salt(node_id),
    );

    let mut new_pins_size = Vec2::ZERO;
    // FLEXINPUT PATCH: what the node holds this frame, before it's rounded onto the grid.
    let mut content_inner = Vec2::ZERO;
    // FLEXINPUT PATCH: how wide the header's contents are, which the width grip can't
    // make the node narrower than.
    let mut header_w = 0.0_f32;

    // FLEXINPUT PATCH: while the size eases, the frame is painted here at the size it's drawn
    // at (`frame_backdrop`, below), and what it holds is cut to it, with room for the pins
    // on its edges: laid out at the size it's going to, a module growing spilled past its
    // frame until the frame got there. egui's own frame keeps its margins and draws nothing.
    // Following what it holds down (`snaps_down`), it's painted here too, as tall as that
    // came out or the grid line it's gliding to, whichever is more.
    let frame_backdrop = (size_gliding || snaps_down).then(|| node_ui.painter().add(Shape::Noop));
    let mut shown_frame = node_frame;
    if size_gliding || snaps_down {
        shown_frame.fill = Color32::TRANSPARENT;
        // Its width kept: without one the frame came out a stroke smaller all round.
        shown_frame.stroke = Stroke::new(node_frame.stroke.width, Color32::TRANSPARENT);
        shown_frame.shadow = egui::epaint::Shadow::NONE;
    }
    if size_gliding {
        let room = vec2(pin_size + 4.0, 2.0);
        node_ui.set_clip_rect(node_ui.clip_rect().intersect(shown_frame_rect.expand2(room)));
    }

    let r = shown_frame.show(node_ui, |ui| {
        if viewer.has_node_style(node, &inputs, &outputs, snarl) {
            viewer.apply_node_style(ui.style_mut(), node, &inputs, &outputs, snarl);
        }

        // Input pins' center side by X axis.
        let input_x = match pin_placement {
            PinPlacement::Inside => pin_size.mul_add(
                0.5,
                node_frame_rect.left() + node_frame.inner_margin.leftf(),
            ),
            PinPlacement::Edge => node_frame_rect.left(),
            PinPlacement::Outside { margin } => {
                pin_size.mul_add(-0.5, node_frame_rect.left() - margin)
            }
        };

        // Input pins' spacing required.
        let input_spacing = match pin_placement {
            PinPlacement::Inside => Some(pin_size),
            PinPlacement::Edge => Some(
                pin_size
                    .mul_add(0.5, -node_frame.inner_margin.leftf())
                    .max(0.0),
            ),
            PinPlacement::Outside { .. } => None,
        };

        // Output pins' center side by X axis. FLEXINPUT PATCH: on the frame's edge as drawn,
        // which eases to its size, so the pins (and their wires) ride the edge with it.
        let output_x = match pin_placement {
            PinPlacement::Inside => pin_size.mul_add(
                -0.5,
                shown_frame_rect.right() - node_frame.inner_margin.rightf(),
            ),
            PinPlacement::Edge => shown_frame_rect.right(),
            PinPlacement::Outside { margin } => {
                pin_size.mul_add(0.5, shown_frame_rect.right() + margin)
            }
        };

        // Output pins' spacing required.
        let output_spacing = match pin_placement {
            PinPlacement::Inside => Some(pin_size),
            PinPlacement::Edge => Some(
                pin_size
                    .mul_add(0.5, -node_frame.inner_margin.rightf())
                    .max(0.0),
            ),
            PinPlacement::Outside { .. } => None,
        };

        // Input/output pin block

        if (openness < 1.0 && open) || (openness > 0.0 && !open) {
            ui.ctx().request_repaint();
        }

        // FLEXINPUT PATCH: half of what the grid's rounding added to the height last frame
        // goes above the pins and body of a node that asks to be centred. The size is
        // measured from the content's own heights, so the pad never feeds back into it.
        let payload_pad = if grid_snap.is_some() && open && viewer.node_centers_payload(node, snarl) {
            let slack: f32 = ui.ctx().data(|d| d.get_temp(node_id.with("grid-slack-y"))).unwrap_or(0.0);
            (slack * 0.5).floor().max(0.0)
        } else {
            0.0
        };

        // Pins are placed under the header and must not go outside of the header frame.
        let payload_rect = Rect::from_min_max(
            pos2(
                node_rect.min.x,
                node_rect.min.y
                    + node_state.header_height()
                    + header_frame.total_margin().bottom
                    + ui.spacing().item_spacing.y
                    + payload_pad
                    - node_state.payload_offset(openness),
            ),
            node_rect.max,
        );

        let node_layout =
            viewer.node_layout(style.get_node_layout(), node, &inputs, &outputs, snarl);

        let payload_clip_rect =
            Rect::from_min_max(node_rect.min, pos2(node_rect.max.x, f32::INFINITY));

        let pins_rect = match node_layout.kind {
            NodeLayoutKind::Coil => {
                // Show input pins.
                let r = draw_inputs(
                    snarl,
                    viewer,
                    node,
                    &inputs,
                    pin_size,
                    style,
                    ui,
                    payload_rect,
                    payload_clip_rect,
                    input_x,
                    node_rect.min.y,
                    node_rect.min.y + node_state.header_height(),
                    input_spacing,
                    snarl_state,
                    modifiers,
                    input_positions,
                    node_layout.input_heights(&node_state),
                );

                let new_input_heights = r.new_heights;

                drag_released |= r.drag_released;

                if r.pin_hovered.is_some() {
                    pin_hovered = r.pin_hovered;
                }

                let inputs_rect = r.final_rect;
                let inputs_size = inputs_rect.size();

                if !snarl.nodes.contains(node.0) {
                    // If removed
                    return;
                }

                // Show output pins.

                let r = draw_outputs(
                    snarl,
                    viewer,
                    node,
                    &outputs,
                    pin_size,
                    style,
                    ui,
                    payload_rect,
                    payload_clip_rect,
                    output_x,
                    node_rect.min.y,
                    node_rect.min.y + node_state.header_height(),
                    output_spacing,
                    snarl_state,
                    modifiers,
                    output_positions,
                    node_layout.output_heights(&node_state),
                );

                let new_output_heights = r.new_heights;

                drag_released |= r.drag_released;

                if r.pin_hovered.is_some() {
                    pin_hovered = r.pin_hovered;
                }

                let outputs_rect = r.final_rect;
                let outputs_size = outputs_rect.size();

                if !snarl.nodes.contains(node.0) {
                    // If removed
                    return;
                }

                node_state.set_input_heights(new_input_heights);
                node_state.set_output_heights(new_output_heights);

                new_pins_size = vec2(
                    inputs_size.x + outputs_size.x + ui.spacing().item_spacing.x,
                    f32::max(inputs_size.y, outputs_size.y),
                );

                let mut pins_rect = inputs_rect.union(outputs_rect);

                // Show body if there's one.
                if viewer.has_body(&snarl.nodes.get(node.0).unwrap().value) {
                    let body_rect = Rect::from_min_max(
                        pos2(
                            inputs_rect.right() + ui.spacing().item_spacing.x,
                            payload_rect.top(),
                        ),
                        pos2(
                            outputs_rect.left() - ui.spacing().item_spacing.x,
                            payload_rect.bottom(),
                        ),
                    );

                    let r = draw_body(
                        snarl,
                        viewer,
                        node,
                        &inputs,
                        &outputs,
                        ui,
                        body_rect,
                        payload_clip_rect,
                        snarl_state,
                    );

                    new_pins_size.x += r.final_rect.width() + ui.spacing().item_spacing.x;
                    new_pins_size.y = f32::max(new_pins_size.y, r.final_rect.height());

                    pins_rect = pins_rect.union(body_rect);

                    if !snarl.nodes.contains(node.0) {
                        // If removed
                        return;
                    }
                }

                pins_rect
            }
            NodeLayoutKind::Sandwich => {
                // Show input pins.

                let r = draw_inputs(
                    snarl,
                    viewer,
                    node,
                    &inputs,
                    pin_size,
                    style,
                    ui,
                    payload_rect,
                    payload_clip_rect,
                    input_x,
                    node_rect.min.y,
                    node_rect.min.y + node_state.header_height(),
                    input_spacing,
                    snarl_state,
                    modifiers,
                    input_positions,
                    node_layout.input_heights(&node_state),
                );

                let new_input_heights = r.new_heights;

                drag_released |= r.drag_released;

                if r.pin_hovered.is_some() {
                    pin_hovered = r.pin_hovered;
                }

                let inputs_rect = r.final_rect;

                new_pins_size = inputs_rect.size();

                let mut next_y = inputs_rect.bottom() + ui.spacing().item_spacing.y;

                if !snarl.nodes.contains(node.0) {
                    // If removed
                    return;
                }

                let mut pins_rect = inputs_rect;

                // Show body if there's one.
                if viewer.has_body(&snarl.nodes.get(node.0).unwrap().value) {
                    let body_rect = payload_rect.intersect(Rect::everything_below(next_y));

                    let r = draw_body(
                        snarl,
                        viewer,
                        node,
                        &inputs,
                        &outputs,
                        ui,
                        body_rect,
                        payload_clip_rect,
                        snarl_state,
                    );

                    let body_rect = r.final_rect;

                    new_pins_size.x = f32::max(new_pins_size.x, body_rect.width());
                    // FLEXINPUT PATCH: a body that drew nothing — one that paints a
                    // glyph into the room its pins leave over, taking no space of its
                    // own — shouldn't pay for the gap between rows either. Without
                    // this, giving a node such a body makes it a row taller for
                    // nothing, and a node already short of room grows a block.
                    new_pins_size.y += body_rect.height()
                        + if body_rect.height() > 0.0 { ui.spacing().item_spacing.y } else { 0.0 };

                    if !snarl.nodes.contains(node.0) {
                        // If removed
                        return;
                    }

                    pins_rect = pins_rect.union(body_rect);
                    next_y = body_rect.bottom() + ui.spacing().item_spacing.y;
                }

                // Show output pins.

                let outputs_rect = payload_rect.intersect(Rect::everything_below(next_y));

                let r = draw_outputs(
                    snarl,
                    viewer,
                    node,
                    &outputs,
                    pin_size,
                    style,
                    ui,
                    outputs_rect,
                    payload_clip_rect,
                    output_x,
                    node_rect.min.y,
                    node_rect.min.y + node_state.header_height(),
                    output_spacing,
                    snarl_state,
                    modifiers,
                    output_positions,
                    node_layout.output_heights(&node_state),
                );

                let new_output_heights = r.new_heights;

                drag_released |= r.drag_released;

                if r.pin_hovered.is_some() {
                    pin_hovered = r.pin_hovered;
                }

                let outputs_rect = r.final_rect;

                if !snarl.nodes.contains(node.0) {
                    // If removed
                    return;
                }

                node_state.set_input_heights(new_input_heights);
                node_state.set_output_heights(new_output_heights);

                new_pins_size.x = f32::max(new_pins_size.x, outputs_rect.width());
                new_pins_size.y += outputs_rect.height() + ui.spacing().item_spacing.y;

                pins_rect = pins_rect.union(outputs_rect);

                pins_rect
            }
            NodeLayoutKind::FlippedSandwich => {
                // Show input pins.

                let outputs_rect = payload_rect;
                let r = draw_outputs(
                    snarl,
                    viewer,
                    node,
                    &outputs,
                    pin_size,
                    style,
                    ui,
                    outputs_rect,
                    payload_clip_rect,
                    output_x,
                    node_rect.min.y,
                    node_rect.min.y + node_state.header_height(),
                    output_spacing,
                    snarl_state,
                    modifiers,
                    output_positions,
                    node_layout.output_heights(&node_state),
                );

                let new_output_heights = r.new_heights;

                drag_released |= r.drag_released;

                if r.pin_hovered.is_some() {
                    pin_hovered = r.pin_hovered;
                }

                let outputs_rect = r.final_rect;

                new_pins_size = outputs_rect.size();

                let mut next_y = outputs_rect.bottom() + ui.spacing().item_spacing.y;

                if !snarl.nodes.contains(node.0) {
                    // If removed
                    return;
                }

                let mut pins_rect = outputs_rect;

                // Show body if there's one.
                if viewer.has_body(&snarl.nodes.get(node.0).unwrap().value) {
                    let body_rect = payload_rect.intersect(Rect::everything_below(next_y));

                    let r = draw_body(
                        snarl,
                        viewer,
                        node,
                        &inputs,
                        &outputs,
                        ui,
                        body_rect,
                        payload_clip_rect,
                        snarl_state,
                    );

                    let body_rect = r.final_rect;

                    new_pins_size.x = f32::max(new_pins_size.x, body_rect.width());
                    // FLEXINPUT PATCH: a body that drew nothing — one that paints a
                    // glyph into the room its pins leave over, taking no space of its
                    // own — shouldn't pay for the gap between rows either. Without
                    // this, giving a node such a body makes it a row taller for
                    // nothing, and a node already short of room grows a block.
                    new_pins_size.y += body_rect.height()
                        + if body_rect.height() > 0.0 { ui.spacing().item_spacing.y } else { 0.0 };

                    if !snarl.nodes.contains(node.0) {
                        // If removed
                        return;
                    }

                    pins_rect = pins_rect.union(body_rect);
                    next_y = body_rect.bottom() + ui.spacing().item_spacing.y;
                }

                // Show output pins.

                let inputs_rect = payload_rect.intersect(Rect::everything_below(next_y));

                let r = draw_inputs(
                    snarl,
                    viewer,
                    node,
                    &inputs,
                    pin_size,
                    style,
                    ui,
                    inputs_rect,
                    payload_clip_rect,
                    input_x,
                    node_rect.min.y,
                    node_rect.min.y + node_state.header_height(),
                    input_spacing,
                    snarl_state,
                    modifiers,
                    input_positions,
                    node_layout.input_heights(&node_state),
                );

                let new_input_heights = r.new_heights;

                drag_released |= r.drag_released;

                if r.pin_hovered.is_some() {
                    pin_hovered = r.pin_hovered;
                }

                let inputs_rect = r.final_rect;

                if !snarl.nodes.contains(node.0) {
                    // If removed
                    return;
                }

                node_state.set_input_heights(new_input_heights);
                node_state.set_output_heights(new_output_heights);

                new_pins_size.x = f32::max(new_pins_size.x, inputs_rect.width());
                new_pins_size.y += inputs_rect.height() + ui.spacing().item_spacing.y;

                pins_rect = pins_rect.union(inputs_rect);

                pins_rect
            }
        };

        if viewer.has_footer(&snarl.nodes[node.0].value) {
            let footer_rect = Rect::from_min_max(
                pos2(
                    node_rect.left(),
                    pins_rect.bottom() + ui.spacing().item_spacing.y,
                ),
                pos2(node_rect.right(), node_rect.bottom()),
            );

            let mut footer_ui = ui.new_child(
                UiBuilder::new()
                    .max_rect(footer_rect.round_ui())
                    .layout(Layout::left_to_right(Align::Min))
                    .id_salt("footer"),
            );
            footer_ui.shrink_clip_rect(payload_clip_rect);

            viewer.show_footer(node, &inputs, &outputs, &mut footer_ui, snarl);

            let final_rect = footer_ui.min_rect();
            ui.expand_to_include_rect(final_rect.intersect(payload_clip_rect));
            let footer_size = final_rect.size();

            new_pins_size.x = f32::max(new_pins_size.x, footer_size.x);
            new_pins_size.y += footer_size.y + ui.spacing().item_spacing.y;

            if !snarl.nodes.contains(node.0) {
                // If removed
                return;
            }
        }

        // Render header frame.
        let mut header_rect = Rect::NAN;

        let mut header_frame_rect = Rect::NAN; //node_rect + header_frame.total_margin();

        // Show node's header
        let header_ui: &mut Ui = &mut ui.new_child(
            UiBuilder::new()
                .max_rect(node_rect.round_ui() + header_frame.total_margin())
                .layout(Layout::top_down(Align::Center))
                .id_salt("header"),
        );

        header_frame.show(header_ui, |ui: &mut Ui| {
            ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                if style.get_collapsible() {
                    let (_, r) = ui.allocate_exact_size(
                        vec2(ui.spacing().icon_width, ui.spacing().icon_width),
                        Sense::click(),
                    );
                    paint_default_icon(ui, openness, &r);

                    if r.clicked_by(PointerButton::Primary) {
                        // Toggle node's openness.
                        snarl.open_node(node, !open);
                    }
                }

                ui.allocate_exact_size(header_drag_space, Sense::hover());

                viewer.show_header(node, &inputs, &outputs, ui, snarl);

                header_rect = ui.min_rect();
            });

            header_frame_rect = header_rect + header_frame.total_margin();

            ui.advance_cursor_after_rect(Rect::from_min_max(
                header_rect.min,
                pos2(
                    f32::max(header_rect.max.x, node_rect.max.x),
                    header_rect.min.y,
                ),
            ));
        });

        ui.expand_to_include_rect(header_rect);
        let header_size = header_rect.size();
        // FLEXINPUT PATCH: what the header contributes to the node's width. Its used rect
        // spans the frame whenever the viewer puts a control against the header's right
        // edge, which is what we do for a note's font size — and then the node's width
        // formula keeps it at the width it already has, so it can be dragged wider and
        // never narrower. A viewer that knows its header's real content says so here.
        header_w = viewer.node_header_width(node, snarl).unwrap_or(header_size.x);
        // FLEXINPUT PATCH: left where the viewer's bodies can read it next frame. A body
        // sized from what the grip asked for has to know when the header is what holds the
        // frame wider than that, or it sits narrow inside a frame that can't shrink.
        ui.ctx().data_mut(|d| d.insert_temp(node_id.with("header-w"), header_w));
        node_state.set_header_height(header_size.y);

        let mut size = vec2(
            f32::max(header_w, new_pins_size.x),
            header_size.y
                + header_frame.total_margin().bottom
                + ui.spacing().item_spacing.y
                + new_pins_size.y,
        );
        // FLEXINPUT PATCH: contents bigger than the frame the viewer eased to hold it at their
        // size next frame: dragged in past what it holds, the frame stops there rather than
        // squeezing them every frame. Their size THIS frame, measured afresh every frame - a
        // size kept until the grip asked for as much again held the frame there after the
        // contents had shrunk (a height squeezed straight after a width hung for the drag).
        {
            let mut next = floor;
            for k in 0..2 {
                next[k] = match frame_now[k] {
                    FrameNow::Eased(f) if k == 0 && size[k] > f - margins[k] + 0.5 => size[k],
                    _ => 0.0,
                };
            }
            if next != floor {
                ui.ctx().data_mut(|d| d.insert_temp(floor_key, next));
            }
        }
        // FLEXINPUT PATCH: see the top of this function. An axis the viewer is easing toward
        // the size its grip asked for isn't rounded on the way (`node_frame_now`): the frame
        // is what it holds, and the size it ends at is whole blocks already. Down, holding
        // more than it was laid out at (`snaps_down`), it's kept rounded: the grid line the
        // frame is gliding to, for snarl's glide to carry on to once the viewer lets go.
        content_inner = size;
        let overflows_down = snaps_down
            && size.y > node_rect.height() + SNAP_DOWN_SLACK
            && (was_over != Overflow::None || spill_now(node_rect.height()));
        if let Some(g) = grid_snap {
            let raw = size;
            size = g.snap_inner_size(size, margins);
            if viewers[0] {
                size.x = raw.x;
            }
            // What the rounding added, for `payload_pad` next frame.
            let slack = if viewers[1] { 0.0 } else { (size.y - raw.y).max(0.0) };
            // Following the viewer's frame, its size is that frame - or what it holds, if
            // that came out more: stored as what it held when that came out less (a row
            // measured a frame short), the frame let go of a block short and glided back.
            if viewers[1] && !overflows_down {
                size.y = raw.y.max(node_rect.height());
            }
            ui.ctx().data_mut(|d| d.insert_temp(node_id.with("grid-slack-y"), slack));
        }
        // FLEXINPUT PATCH: grown only once it's held more two frames running: a row that
        // re-wraps is measured a frame late, and its spill, rounded onto the grid, grew the
        // frame a block for one frame and glided it back.
        if !viewers[1] && !fresh {
            let wait_key = node_id.with("flexinput-grow-wait");
            let grows = size.y > target_size.y + 0.5;
            let waited: bool = ui.ctx().data(|d| d.get_temp(wait_key)).unwrap_or(false);
            if grows && !waited {
                size.y = target_size.y;
                ui.ctx().request_repaint();
            }
            if grows != waited {
                ui.ctx().data_mut(|d| d.insert_temp(wait_key, grows));
            }
        }
        node_state.set_size(size);
        if grid_snap.is_some() {
            // The frame fills the node's rect, not just what's in it.
            ui.expand_to_include_rect(node_rect);
        }
    });

    if !snarl.nodes.contains(node.0) {
        ui.ctx().request_repaint();
        node_state.clear(ui.ctx());
        // If removed
        return None;
    }

    // FLEXINPUT PATCH: where the frame is drawn: on its way to its size while that eases,
    // else what it holds - and following what it holds down (`snaps_down`), as tall as that
    // or the grid line above it it's gliding to, whichever is more: never inside it, and on
    // the grid when the viewer lets go.
    let mut paint_rect = shown_rect;
    if snaps_down {
        let key = size_key.with(1);
        let now = ui.input(|i| i.time);
        let laid_h = node_rect.height();
        // As tall as it was laid out, or as what it holds if that's more.
        let mut h = laid_h.max(content_inner.y);
        let still = |ui: &Ui, h: f32| {
            let at = egui::pos2(h, 0.0);
            ui.ctx().data_mut(|d| d.insert_temp(key, Glide { from: at, to: at, start: now - f64::from(ease) }));
        };
        let over;
        match grid_snap {
            // Holding more than it was laid out at - more than a row re-wrapping a frame
            // late spills, and for longer than the frame it spills for - it glides to the
            // grid line above what it holds.
            Some(_) if content_inner.y > laid_h + SNAP_DOWN_SLACK && was_over == Overflow::None && !spill_now(laid_h) => {
                still(ui, h);
                over = Overflow::Spilled;
            }
            Some(g) if content_inner.y > laid_h + SNAP_DOWN_SLACK => {
                let line = g.snap_len(content_inner.y + margins.y) - margins.y;
                let eased = eased_at(ui.ctx(), key, egui::pos2(line, 0.0), ease).x;
                if eased < h - 0.5 {
                    // Behind what it holds: off again from there.
                    ui.ctx().data_mut(|d| d.insert_temp(key, Glide { from: egui::pos2(h, 0.0), to: egui::pos2(line, 0.0), start: now }));
                    ui.ctx().request_repaint();
                }
                h = h.max(eased);
                over = Overflow::Gliding;
            }
            // Back inside the frame it was laid out at: down to that from where it got to.
            _ if was_over == Overflow::Gliding => {
                let eased = eased_at(ui.ctx(), key, egui::pos2(h, 0.0), ease).x;
                over = if eased > h + 0.5 { Overflow::Gliding } else { Overflow::None };
                h = h.max(eased);
            }
            // The frame is the viewer's, and a glide onto the grid sets off from there.
            _ => {
                still(ui, h);
                over = Overflow::None;
            }
        }
        if (over, laid_h) != (was_over, was_laid_h) {
            ui.ctx().data_mut(|d| d.insert_temp(over_key, (over, laid_h)));
        }
        paint_rect = Rect::from_x_y_ranges(shown_rect.x_range(), shown_rect.top()..=shown_rect.top() + h);
    }
    if let Some(idx) = frame_backdrop {
        node_ui.painter().set(idx, node_frame.paint(paint_rect));
    }
    let drawn_frame = if size_gliding || snaps_down { paint_rect + frame_margin } else { r.response.rect };
    if let Some(idx) = select_slot {
        let select_style = style.get_select_style(ui.style());
        ui.painter().set(
            idx,
            egui::epaint::RectShape::new(
                drawn_frame + select_style.margin,
                select_style.rounding,
                select_style.fill,
                select_style.stroke,
                StrokeKind::Inside,
            ),
        );
    }
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(DRAWN_FRAME_KEY).with(node.0), drawn_frame));

    viewer.final_node_rect(node, r.response.rect, ui, snarl);

    // FLEXINPUT PATCH: the corner grip, over everything else in the node. The viewer's
    // own floor wins where it has one: the header's laid-out width can be the frame's
    // width, which would let the grip grow a node and never bring it back.
    if viewer.node_resizable(node, snarl) {
        // Never narrower than the header either: the grip would go on writing widths the
        // frame can't take, and the body would follow them down inside it.
        let narrowest = viewer
            .node_narrowest(node, snarl)
            .map_or(header_w + margins.x, |n| n.max(header_w + margins.x));
        let uniform = viewer.node_resize_uniform(node, snarl);
        size_grip(ui, node, node_id, r.response.rect, drawn_frame, narrowest, grid_snap, modifiers, snarl_state, viewer, snarl, uniform);
    }

    node_state.store(ui.ctx());
    Some(DrawNodeResponse {
        node_moved,
        node_to_top,
        drag_released,
        pin_hovered,
        final_rect: r.response.rect,
        frame_offset: frame_margin.left_top(),
        ease_offset: pos - node_pos,
    })
}

const fn mix_colors(a: Color32, b: Color32) -> Color32 {
    #![allow(clippy::cast_possible_truncation)]

    Color32::from_rgba_premultiplied(
        u8::midpoint(a.r(), b.r()),
        u8::midpoint(a.g(), b.g()),
        u8::midpoint(a.b(), b.b()),
        u8::midpoint(a.a(), b.a()),
    )
}

// fn mix_colors(mut colors: impl Iterator<Item = Color32>) -> Option<Color32> {
//     let color = colors.next()?;

//     let mut r = color.r() as u32;
//     let mut g = color.g() as u32;
//     let mut b = color.b() as u32;
//     let mut a = color.a() as u32;
//     let mut w = 1;

//     for c in colors {
//         r += c.r() as u32;
//         g += c.g() as u32;
//         b += c.b() as u32;
//         a += c.a() as u32;
//         w += 1;
//     }

//     Some(Color32::from_rgba_premultiplied(
//         (r / w) as u8,
//         (g / w) as u8,
//         (b / w) as u8,
//         (a / w) as u8,
//     ))
// }

// fn mix_sizes(mut sizes: impl Iterator<Item = f32>) -> Option<f32> {
//     let mut size = sizes.next()?;
//     let mut w = 1;

//     for s in sizes {
//         size += s;
//         w += 1;
//     }

//     Some(size / w as f32)
// }

// fn mix_strokes(mut strokes: impl Iterator<Item = Stroke>) -> Option<Stroke> {
//     let stoke = strokes.next()?;

//     let mut width = stoke.width;
//     let mut r = stoke.color.r() as u32;
//     let mut g = stoke.color.g() as u32;
//     let mut b = stoke.color.b() as u32;
//     let mut a = stoke.color.a() as u32;

//     let mut w = 1;

//     for s in strokes {
//         width += s.width;
//         r += s.color.r() as u32;
//         g += s.color.g() as u32;
//         b += s.color.b() as u32;
//         a += s.color.a() as u32;
//         w += 1;
//     }

//     Some(Stroke {
//         width: width / w as f32,
//         color: Color32::from_rgba_premultiplied(
//             (r / w) as u8,
//             (g / w) as u8,
//             (b / w) as u8,
//             (a / w) as u8,
//         ),
//     })
// }

impl<T> Snarl<T> {
    /// Render [`Snarl`] using given viewer and style into the [`Ui`].
    #[inline]
    pub fn show<V>(&mut self, viewer: &mut V, style: &SnarlStyle, id_salt: impl Hash, ui: &mut Ui)
    where
        V: SnarlViewer<T>,
    {
        show_snarl(
            ui.make_persistent_id(id_salt),
            *style,
            Vec2::ZERO,
            Vec2::INFINITY,
            self,
            viewer,
            ui,
        );
    }
}

#[inline]
fn clamp_scale(to_global: &mut TSTransform, min_scale: f32, max_scale: f32, ui_rect: Rect) {
    if to_global.scaling >= min_scale && to_global.scaling <= max_scale {
        return;
    }

    let new_scaling = to_global.scaling.clamp(min_scale, max_scale);
    *to_global = scale_transform_around(to_global, new_scaling, ui_rect.center());
}

#[inline]
#[must_use]
fn transform_matching_points(from: Pos2, to: Pos2, scaling: f32) -> TSTransform {
    TSTransform {
        scaling,
        translation: to.to_vec2() - from.to_vec2() * scaling,
    }
}

#[inline]
#[must_use]
fn scale_transform_around(transform: &TSTransform, scaling: f32, point: Pos2) -> TSTransform {
    let from = (point - transform.translation) / transform.scaling;
    transform_matching_points(from, point, scaling)
}

#[test]
const fn snarl_style_is_send_sync() {
    const fn is_send_sync<T: Send + Sync>() {}
    is_send_sync::<SnarlStyle>();
}
