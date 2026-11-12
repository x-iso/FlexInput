use egui::Color32;
use egui_snarl::{
    ui::{AnyPin, AnyPins, PinInfo, SnarlViewer},
    InPin, InPinId, NodeId, OutPin, OutPinId, Snarl,
};
use flexinput_core::{ModuleDescriptor, PinDescriptor, Signal, SignalType, automap as am_canon};
use flexinput_devices::{ControllerKind, PhysicalDevice, midi::cc_display_name};
use flexinput_engine::current_sample_rate;
use serde_json::{Number, Value};

use super::{
    curve::{sample_curve, curve_insert_point, curve_remove_point, vec_reshape_apply, VEC_RESHAPE_BOUNDARY_DEFAULT, VEC_RESHAPE_GAIN_DEFAULT},
    node::{LayoutDecoration, LayoutItem, NodeData, TextAlign},
};
use crate::app::request_repaint_throttled;

// ── Split modules (mechanical extraction from this file; the glob re-exports
// keep every pre-split `viewer::item` path resolving unchanged) ──
mod asth;
mod curve_bodies;
mod curve_support;
mod scopes;
mod waveform;
mod automap_bodies;
mod chrome;
mod conflicts;
mod controller3d;
mod expose;
mod glow;
mod gyro_body;
mod jsm;
mod jsm_cal;
mod jsm_widgets;
// Gamepad nav reaches into the JSM faders: a pinned setting is driven like a
// Knob, but its value lives in the config text rather than in a param.
pub(crate) use jsm::active_text as jsm_active_text;
pub(crate) use jsm::set_active_text as jsm_set_active_text;
pub(crate) use scale::{publish_jsm_chord, publish_jsm_cursor, publish_jsm_pane};
pub(crate) use jsm::knobs_of as jsm_knobs_of;
pub(crate) use jsm::nav_nudge_knob as jsm_nav_nudge_knob;
pub(crate) use jsm::nav_toggle_pair as jsm_nav_toggle_pair;
pub(crate) use jsm_widgets::knob_name_of as jsm_knob_name_of;
// The pad drives the command list by asking it to move, not by index: see
// `CommandList`.
pub(crate) use jsm_widgets::{command_list_state, set_command_list_state};
pub(crate) mod kit; // the grid tests read the pin-label rules off it
mod kit_bodies;
mod module_icons;
pub(crate) use kit_bodies::*;
mod layout_edit;
mod midi;
mod net;
mod osc_envelope;
mod pad_hints;
mod pins;
mod raw_motion;
mod raw_pad;
mod raw_rumble;
mod pinned;
mod remapper_bodies;
mod remapper_card;
mod remapper_filter;
mod remapper_presets;
mod remapper_reorder;
mod remapper_support;
mod card_options;
pub(crate) use card_options::CardOptions;
mod reshape;
mod rws;
mod scale;
pub(crate) mod simple_bodies; // `Xysr` is read back by the grid tests
mod stick_rotation;
mod subpatch_body;
mod touch_zones;
pub(crate) use asth::*;
pub(crate) use curve_bodies::*;
pub(crate) use curve_support::*;
pub(crate) use scopes::*;
pub(crate) use waveform::*;
pub(crate) use automap_bodies::*;
pub(crate) use chrome::*;
pub(crate) use conflicts::*;
pub(crate) use controller3d::*;
pub(crate) use expose::*;
pub(crate) use glow::*;
pub(crate) use gyro_body::*;
pub(crate) use jsm::*;
pub(crate) use layout_edit::*;
pub(crate) use midi::*;
pub(crate) use net::*;
pub(crate) use osc_envelope::*;
pub(crate) use pins::*;
pub(crate) use raw_pad::*;
pub(crate) use pinned::*;
pub(crate) use remapper_bodies::*;
pub(crate) use remapper_card::*;
pub(crate) use remapper_filter::*;
pub(crate) use remapper_presets::*;
pub(crate) use remapper_reorder::*;
pub(crate) use remapper_support::*;
pub(crate) use reshape::*;
pub(crate) use rws::*;
pub(crate) use stick_rotation::*;
pub(crate) use scale::*;
pub(crate) use simple_bodies::*;
pub(crate) use subpatch_body::*;
pub(crate) use touch_zones::*;
pub(crate) use crate::widgets::{
    fxi_color_swatch, popup_below_widget, slider_label, slider_track_double_clicked,
};


pub struct FlexViewer<'a> {
    pub descriptors: &'a [ModuleDescriptor],
    pub ctx: egui::Context,
    /// IDs of currently-live physical and virtual devices.  Used to render status dots.
    pub live_device_ids: &'a std::collections::HashSet<String>,
    /// Latest raw device signals, keyed by (device_id, pin_id). Refreshed each frame
    /// from the processing thread. Read by module bodies that need to observe live
    /// canonical pin values (e.g. Remapper's capture state machine).
    pub live_signals: &'a std::collections::HashMap<(String, String), Signal>,
    /// What the engine routed to each `(device_id, sink pin)` on the last tick —
    /// direct wires, Auto-Map and the feedback channel alike. Keyed the same way
    /// as `live_signals` but a SEPARATE map: a virtual pad's sink pins share pin
    /// names with the source pins it publishes as a device, so the two would
    /// collide if merged. Read by device nodes to colour and glow their inlets
    /// (see `device_inlet_drive`).
    pub sink_bus: &'a std::collections::HashMap<(String, String), Signal>,
    /// Per-device measured polling rate (device_id → Hz). Populated by the
    /// device-io thread.
    pub device_rates: &'a std::collections::HashMap<String, u32>,
    /// Snapshot of the user-configurable Panic-mode shortcut. Read by the
    /// Remapper body so that learning a mapping cannot accidentally rebind the
    /// emergency-stop chord onto a controller button.
    pub panic_shortcut: &'a crate::app::PanicShortcut,
    /// Physical devices available as hot-swap candidates in the node context menu.
    pub physical_devices: &'a [PhysicalDevice],
    /// Set by the `disconnect` override when the user right-clicks a wire.
    /// Canvas::show() reads this after snarl.show() and renders the context menu.
    pub pending_wire_menu: Option<(OutPinId, InPinId, egui::Pos2)>,
    /// The wire whose context menu is open, drawn thicker so it's clear which it is.
    pub highlighted_wire: Option<(OutPinId, InPinId)>,
    /// Set by show_node_menu when the user clicks "Rename…".
    pub rename_request: Option<NodeId>,
    /// Set by show_node_menu when the user picks "Replace with…".
    /// Carries (node id, index into physical_devices).
    pub replace_request: Option<(NodeId, usize)>,
    /// Set by show_node_menu when the user clicks "Remove node": Canvas::show()
    /// removes it, keeping a pad's calibration in the tab's memory first.
    pub remove_request: Option<NodeId>,
    /// Set when the user clicks "Edit…" on a subpatch node.
    pub edit_subpatch_request: Option<NodeId>,
    /// Set when the user clicks "Save…" on a subpatch node on a TAB canvas: the
    /// app writes the `.fxsp` so it can bake in the tab's overlay/config items,
    /// which this viewer can't reach.
    pub save_subpatch_request: Option<NodeId>,
    /// True when rendering the inner canvas of a sub-patch editor.
    /// When false, the "SubPatch" module category (Inlet/Outlet) is hidden.
    pub is_inner_canvas: bool,
    /// Set when the user picks something from the "Pin element" submenu (or
    /// "Unpin"). (NodeId, element_id, source_size). `source_size = [0,0]`
    /// means "no measured size" — placement uses a default.
    pub expose_module_request: Option<(NodeId, String, [f32; 2])>,
    /// NodeId.0 values currently pinned to the outer body (used to toggle menu label).
    pub pinned_inner_ids: std::collections::HashSet<usize>,
    /// Set by show_node_menu when the user clicks "Group into sub-patch…".
    /// Canvas::show() reads this, looks up the snarl selection, and calls group_selected_into_subpatch.
    pub group_request: bool,
    /// Set by response-curve body when the user clicks Reset. Canvas::show() pushes the
    /// pre_snapshot (captured before snarl.show()) onto the undo stack so Reset is reversible.
    pub push_undo_request: bool,
    /// Per-device-type seeds the user picked in Settings → Device defaults.
    /// Double-clicking a header slider resets that node's param to the value here.
    pub param_defaults: crate::canvas::DeviceParamDefaults,
    /// Set by a device.source header when the user clicks "Calibrate".
    /// Canvas::show() reads it and the app surfaces a calibration window for that node.
    pub calibrate_request: Option<NodeId>,
    /// Parent snarl frame for resolving AutoMap pin glow that crosses sub-patch
    /// boundaries. `None` at the root canvas; set by the inner sub-patch editor.
    pub automap_parent: Option<AutomapGlowParent<'a>>,
    /// Shared rumble-ping queue. A click on a device.source's icon pushes that
    /// device's id here for the I/O thread to pulse. `None` on inner sub-patch
    /// canvases (no physical device sources to ping).
    pub ping_requests: Option<&'a crate::easy::io_panel::PingRequests>,
    /// This canvas's per-view salt (mirrors `Canvas::view_salt`). Used to key the
    /// stash of measured node rects (`final_node_rect`) so the Easy-mode I/O
    /// layout can position nodes against the sub-patch's REAL rendered size
    /// instead of guessed constants.
    pub view_salt: u64,
    /// The id the canvas shows its snarl with, which keys each node's state.
    pub snarl_id: egui::Id,
    /// Left plus right margin of a node's frame around its contents.
    pub node_margin_x: f32,
    /// The node frame's own vertical margin, so a body can work out how much of its
    /// node's height is actually its to fill (see [`Self::show_body`]).
    pub node_margin_y: f32,
    /// How narrow each resizable node's body says its frame may be, written as the body
    /// is laid out and read back by the corner grip in the same frame. The grip needs a
    /// floor that comes from the content: its own fallback is the header's width, and a
    /// header that spans the frame would otherwise set the floor to the size the node
    /// happens to be — which grows and never comes back.
    pub size_floors: std::collections::HashMap<usize, f32>,
    /// How much width each header actually holds, measured as it is drawn, for the nodes
    /// whose header puts something against its right edge. Read back by snarl as
    /// [`SnarlViewer::node_header_width`]; without it those nodes' headers would claim
    /// the whole frame and the node could never be dragged narrower.
    pub header_content: std::collections::HashMap<usize, f32>,
}

/// egui temp-data key for this canvas's measured node rects, in CANVAS space.
/// Written by `final_node_rect` each frame, read by `easy::layout`.
pub fn node_rects_id(view_salt: u64) -> egui::Id {
    egui::Id::new(("flexinput_node_rects", view_salt))
}

/// The measured node-rect map: NodeId.0 → canvas-space [x, y, w, h].
pub type NodeRectMap = std::collections::HashMap<usize, [f32; 4]>;

/// What the node measures this frame: the frame as it is laid out now, not the one the
/// canvas last drew. A body that sized itself off last frame's rect would overshoot
/// every other frame, so this reads the layout state for the current one.
fn node_frame_size(
    node_id: NodeId,
    ui: &egui::Ui,
    snarl: &Snarl<NodeData>,
    snarl_id: egui::Id,
    margin_x: f32,
    margin_y: f32,
) -> Option<(f32, f32)> {
    let info = snarl.get_node_info(node_id)?;
    let state = egui_snarl::ui::NodeState::load(ui.ctx(), snarl_id.with(("snarl-node", node_id)), &ui.style().spacing);
    let rect = state.node_rect(info.pos, 1.0);
    Some((rect.width() + margin_x, rect.height() + margin_y))
}

impl<'a> SnarlViewer<NodeData> for FlexViewer<'a> {
    fn title(&mut self, node: &NodeData) -> String {
        node.display_name.clone()
    }

    fn show_header(
        &mut self,
        node: NodeId,
        inputs: &[egui_snarl::InPin],
        outputs: &[egui_snarl::OutPin],
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeData>,
    ) {
        // Extract what we need before any UI calls so the borrow of snarl is released.
        let data = &snarl[node];
        let title = data.display_name.clone();
        // A module the kit knows by a symbol wears that mark instead of its name, and
        // the name goes to the hover text. A renamed module keeps what it's called.
        let mark = kit::node_mark(data);
        // What the header row had taken before the name was drawn - the fold chevron and a
        // status dot. That, not the name, is what the header *contributes* to the node's
        // width: a name that fits is drawn, a name that doesn't is left out, and neither is
        // allowed to be the reason the module can't be dragged narrower.
        let mut header_room_before_name = 0.0f32;
        let status_dot = if matches!(data.module_id.as_str(), "device.source" | "device.sink") {
            let live = data.params.get("device_id")
                .and_then(|v| v.as_str())
                .map(|id| self.live_device_ids.contains(id))
                .unwrap_or(false);
            Some(live)
        } else {
            None
        };
        let icon_spec = if matches!(data.module_id.as_str(), "device.source" | "device.sink") {
            data.params.get("device_id")
                .and_then(|v| v.as_str())
                .and_then(crate::canvas::remapper_icons::device_node_icon_for_id)
        } else {
            None
        };
        let is_device_source = data.module_id == "device.source";
        let is_device_sink   = data.module_id == "device.sink";
        // Index of the AutoMap input pin on a device.sink (always last per
        // crates/virtual/src/layouts.rs, but locate it by type so we don't
        // hard-code the position).
        let sink_automap_in_idx: Option<usize> = if is_device_sink {
            data.inputs.iter().position(|p|
                p.signal_type == SignalType::AutoMap && p.name == "Auto-Map")
        } else { None };
        // Index of the AutoMap output pin on a device.source. Locate by type
        // and name so we don't hard-code the position.
        let source_automap_out_idx: Option<usize> = if is_device_source {
            data.outputs.iter().position(|p|
                p.signal_type == SignalType::AutoMap && p.name == "Auto-Map")
        } else { None };
        let deadzone_initial = data.params.get("deadzone")
            .and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
        let deadzones_differ = stick_deadzones_differ(&data.params);
        let gyro_mult_initial = data.params.get("gyro_multiplier")
            .and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
        let mouse_sens_initial = data.params.get("mouse_sensitivity")
            .and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
        // Mouse-sensitivity slider applies to virtual.keymouse sinks only.
        let is_keymouse_sink = is_device_sink && data.params.get("device_id")
            .and_then(|v| v.as_str()).map(|s| s.starts_with("virtual.keymouse")).unwrap_or(false);
        // Capability flags driven by the physical device family. MIDI ports
        // get no per-device sliders; XInput (Xbox) has no gyro/sticks
        // calibration support; gyro/sticks-capable controllers get both.
        let dev_id_owned: String = data.params.get("device_id")
            .and_then(|v| v.as_str()).map(|s| s.to_string()).unwrap_or_default();
        let dev_id_str: &str = &dev_id_owned;
        let (has_deadzone, has_gyro, _has_sticks_cal) = device_source_caps(dev_id_str, is_device_source);
        // Estimate the body width so the AutoMap chip can right-align to it.
        // Computed up-front (outside the closure that mutates the snarl) so
        // the `data` borrow doesn't outlive the snarl.get_node_mut calls.
        let device_body_w: f32 = if is_device_source {
            let header_w: f32 = ui.ctx().data(|d| d.get_temp(midi_header_w_key(node))).unwrap_or(0.0);
            estimate_device_body_width(ui, data).max(header_w)
        } else { 0.0 };

        let is_subpatch = snarl.get_node(node).map(|n| n.module_id == "subpatch").unwrap_or(false);
        let (inner_count, has_pinned, is_unlocked) = if is_subpatch {
            let sp = snarl.get_node(node).and_then(|n| n.subpatch.as_ref());
            (
                sp.map(|s| s.snarl.nodes_ids_data().count()).unwrap_or(0),
                sp.map(|s| !s.is_layout_empty()).unwrap_or(false),
                snarl.get_node(node).map(|n| n.extra.layout_unlocked).unwrap_or(false),
            )
        } else {
            (0, false, false)
        };
        let is_label          = snarl.get_node(node).map(|n| n.module_id == "module.label").unwrap_or(false);
        let is_knob           = snarl.get_node(node).map(|n| n.module_id == "module.knob").unwrap_or(false);
        let is_svg            = snarl.get_node(node).map(|n| n.module_id == "module.svg").unwrap_or(false);
        let is_remapper       = snarl.get_node(node).map(|n| n.module_id == "module.remapper").unwrap_or(false);
        let is_map_action     = snarl.get_node(node).map(|n| n.module_id == "module.map_action").unwrap_or(false);
        let is_input_viewer   = snarl.get_node(node).map(|n| n.module_id == "module.input_viewer").unwrap_or(false);
        let is_menu           = snarl.get_node(node).map(|n| n.module_id == "module.menu").unwrap_or(false);
        let is_automap_curve  = snarl.get_node(node).is_some_and(|n| is_automap_curve(&n.module_id));
        let is_response_curve = snarl.get_node(node).map(|n| {
            matches!(curve_ui_module_id(n),
                "module.response_curve" | "module.vec_response_curve" | "module.twoway_response_curve")
        }).unwrap_or(false);
        let curve_is_float    = snarl.get_node(node).map(|n| curve_ui_module_id(n) == "module.response_curve").unwrap_or(false);
        let is_rws            = snarl.get_node(node).map(|n| n.module_id == "processing.rws").unwrap_or(false);
        let is_feedback_control = snarl.get_node(node).map(|n| n.module_id == "module.feedback_control").unwrap_or(false);
        let is_jsm            = snarl.get_node(node).map(|n| n.module_id == "module.jsm").unwrap_or(false);
        // Math operations whose input count is the user's to change. Their `− +` goes
        // at the right of this header and they draw no body at all (see
        // `kit::variadic_inputs`), so their height is just this header plus their
        // pins and a pin row is what the stepper adds and takes away.
        let pin_count = snarl
            .get_node(node)
            .and_then(|n| kit::variadic_inputs(&n.module_id).map(|range| (n.inputs.len(), range)));
        // A module whose ports are a list of strips in its body adds them from here: one
        // `+` at the header's right edge. Dropping one is the `×` on its own strip.
        let port_noun = snarl.get_node(node).and_then(|n| kit::header_plus(&n.module_id));
        let port_module = snarl.get_node(node).map(|n| n.module_id.clone()).unwrap_or_default();
        // A per-channel filter (Delay, Average, DC Filter): its channels are a matched
        // In/Out pair each, and the `− +` for them sits in this header rather than in a row
        // across the body - a "Channels" row with a stepper in it was costing these modules
        // 174 points of width to hold one number.
        let channel_count = snarl
            .get_node(node)
            .and_then(|n| kit::channel_range(&n.module_id).map(|range| (n.inputs.len(), range)));
        // The Dropdown's options are a list it edits in its body. The `+` that adds one sits
        // in the header with the other list controls: at the end of the list it read as one
        // more option, and it made the list's last row a button rather than a port.
        let option_add = snarl.get_node(node).map_or(false, |n| n.module_id == "module.dropdown");
        // The Macro Output's ports are its output pins, listed in its body; its `+` belongs in
        // the header with the other lists' too, where a button named "+ Add output" was one of
        // the things holding every row wide.
        let macro_add = snarl.get_node(node).map_or(false, |n| n.module_id == "module.macro");

        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.horizontal(|ui| {
                if let Some(spec) = &icon_spec {
                    use crate::canvas::remapper_icons::NodeIconSpec;
                    use crate::panels::device_icon::{ping_device_icon, render_device_icon};
                    const ICON_H: f32 = 40.0;
                    match spec {
                        // A physical device.source icon doubles as a rumble-ping
                        // button (click → 200 ms pulse) so the user can identify
                        // which hardware this node maps to.
                        NodeIconSpec::Single(bytes) if is_device_source
                            && self.ping_requests.is_some()
                            && crate::canvas::remapper_icons::phys_pad_slug(dev_id_str).is_some() =>
                        {
                            if ping_device_icon(ui, bytes, ICON_H).clicked() {
                                if let Some(q) = self.ping_requests {
                                    if let Ok(mut q) = q.lock() {
                                        q.push(dev_id_owned.clone());
                                    }
                                }
                            }
                        }
                        NodeIconSpec::Single(bytes) => render_device_icon(ui, bytes, ICON_H),
                    }
                }
                // Device source: dot + [title / (Calibrate + Hz)] stack,
                // vertically centered next to the icon. The Calibrate button +
                // measured polling rate occupy the row directly under the title.
                if is_device_source {
                    if let Some(live) = status_dot {
                        let color = if live { Color32::from_rgb(80, 200, 100) } else { Color32::from_rgb(220, 80, 60) };
                        ui.label(egui::RichText::new("●").color(color).small());
                    }
                    let (has_dz_c, has_gy_c, has_st_c) = device_source_caps(dev_id_str, is_device_source);
                    let has_cal_here = has_gy_c || has_st_c || has_dz_c;
                    ui.vertical(|ui| {
                        ui.label(&title);
                        // A keyboard unplugged mid-chord never sends its Note
                        // Offs, leaving those notes held for good.
                        if dev_id_str.starts_with("midi_in:") {
                            if ui.small_button("Flush")
                                .on_hover_text("Release every note this port still holds — for notes left \
                                                stuck when a keyboard was unplugged or dropped its Note Off.")
                                .clicked()
                            {
                                request_midi_flush(ui.ctx(), dev_id_str);
                            }
                        }
                        let has_eye = crate::hidhide_toggle::state_of(dev_id_str).is_some();
                        if has_cal_here || has_eye {
                            ui.horizontal(|ui| {
                                if has_cal_here {
                                    let cal_resp = ui.small_button("Calibrate")
                                        .on_hover_text("Open the Device Calibration window");
                                    crate::panels::calibration::calibrate_button_activity(
                                        ui, node, cal_resp.rect);
                                    if cal_resp.clicked() {
                                        self.calibrate_request = Some(node);
                                    }
                                    let hz = self.device_rates.get(&dev_id_owned).copied().unwrap_or(0);
                                    // Fixed-width cell, so the eye after it doesn't
                                    // shift as the measured rate changes.
                                    ui.allocate_ui_with_layout(
                                        egui::vec2(52.0, ui.spacing().interact_size.y),
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            ui.set_min_width(52.0);
                                            ui.label(egui::RichText::new(format!("{} Hz", hz))
                                                .color(Color32::from_rgb(220, 160, 40)).small())
                                                .on_hover_text("Measured per-device polling rate (raw events/sec)");
                                        },
                                    );
                                }
                                // Manual HidHide toggle (see `hidhide_toggle`), and
                                // the default light-bar colour on pads that have one.
                                crate::hidhide_toggle::eye_button(ui, dev_id_str);
                                crate::device_led::swatch_button(ui, dev_id_str);
                            });
                        }
                    });
                } else if is_device_sink {
                    if let Some(live) = status_dot {
                        let color = if live { Color32::from_rgb(80, 200, 100) } else { Color32::from_rgb(220, 80, 60) };
                        ui.label(egui::RichText::new("●").color(color).small());
                    }
                    ui.label(&title);
                } else if is_menu {
                    // Virtual Menu: identity (icon + editable name) + the
                    // Ports/Mapping switch live in the header.
                    super::menu_body::show_menu_header(node, ui, snarl);
                } else {
                    if let Some(live) = status_dot {
                        let color = if live { Color32::from_rgb(80, 200, 100) } else { Color32::from_rgb(220, 80, 60) };
                        ui.label(egui::RichText::new("●").color(color).small());
                    }
                    // The mark or the name, never both.
                    let header_module =
                        snarl.get_node(node).map(|n| n.module_id.clone()).unwrap_or_default();
                    let knob_collapsed = is_knob
                        && snarl.get_node_info(node).map_or(true, |i| !i.open);
                    let reserved = if is_knob && !knob_collapsed {
                        // The Uni/Bi switch that will sit at the header's right edge.
                        crate::canvas::viewer::kit_bodies::KNOB_TOGGLE.x + ui.spacing().item_spacing.x
                    } else if channel_count.is_some() || option_add || macro_add {
                        // The `− n +` or the `+` that will sit at the header's right edge. It
                        // is also reserved in the flow after the name, but the name is painted
                        // rather than laid out, so this is where its room comes off the name.
                        (if channel_count.is_some() { kit::stepper_w(ui) } else { kit::stepper_h(ui) })
                            + ui.spacing().item_spacing.x
                    } else {
                        0.0
                    };
                    // What the row has already taken - the fold chevron, a status dot - is
                    // off the room too: the name has to fit what's left, not what the frame
                    // is, or it gets painted under the control at the right edge. A folded
                    // module pays for its sockets again: with no body to hang them on, they
                    // are drawn at the ends of the header row itself.
                    let folded = snarl.get_node_info(node).map_or(true, |i| !i.open);
                    let used = ui.min_rect().width();
                    header_room_before_name = used;
                    // What the header's tooltip says: the name the patch calls this module by,
                    // and under a line what the module does. Both arms of the match get it, so
                    // a module known by its mark is as explainable as one known by its name.
                    let help = kit::module_help(&header_module);
                    let tip_name = title.clone();
                    let tooltip = move |ui: &mut egui::Ui| {
                        ui.set_max_width(260.0);
                        ui.label(egui::RichText::new(tip_name.as_str()).strong());
                        if let Some(help) = help {
                            ui.separator();
                            ui.label(egui::RichText::new(help).weak().small());
                        }
                    };
                    match mark {
                        Some(mark) => {
                            let r = ui.label(egui::RichText::new(mark).strong());
                            simple_bodies::trace_rect(ui, node, "header_title", r.rect);
                            r.on_hover_ui(tooltip)
                        }
                        None => {
                            // Painted, not laid out. A name added to the flow adds its own width
                            // to the header row, and the header row is one of the things the
                            // node's width is measured against: that is why "DC Filter" needed a
                            // 168-point module for one number where "Average" manages with 140.
                            // The row keeps its height and the name is drawn where the chevron
                            // leaves off. What it does with the room it's given is the same on
                            // every module: whole if it fits, cut with dots if it nearly does,
                            // and left out entirely when the room is too small for a word that
                            // means anything - half a letter under the control at the right edge
                            // says less than nothing.
                            let (slot, resp) = ui.allocate_exact_size(
                                egui::vec2(0.0, ui.spacing().interact_size.y),
                                egui::Sense::hover(),
                            );
                            // The room is measured from where the name will actually be painted -
                            // after the fold chevron and anything else the row has taken - to the
                            // frame's right edge, less what the control at that edge will need, and
                            // less the socket a folded module draws at the end of its header.
                            let room = (ui.max_rect().right()
                                - reserved
                                - if folded { ui.spacing().interact_size.y + 6.0 } else { 0.0 }
                                - slot.left())
                                .max(0.0);
                            match kit::title_galley(ui, &title, room) {
                                Some(galley) => {
                                    let size = galley.size();
                                    let at = egui::pos2(slot.left(), slot.center().y - size.y * 0.5);
                                    ui.painter().galley(at, galley, ui.visuals().text_color());
                                    simple_bodies::trace_rect(
                                        ui,
                                        node,
                                        "header_title",
                                        egui::Rect::from_min_size(at, size),
                                    );
                                }
                                None => {
                                    // Traced empty so a test can tell "hidden" from "not drawn
                                    // at all", and so the header's height is on record.
                                    simple_bodies::trace_rect(ui, node, "header_title", egui::Rect::NOTHING);
                                }
                            }
                            resp.on_hover_ui(tooltip)
                        }
                    };

                }

                // The `− +` for a module whose pins are its content, at the right of
                // the header row. The row first reserves the box the stepper takes, so
                // the title and the buttons can't run into each other and the header
                // reports the width those two need — a width worked out from its own
                // contents, never from the width the node happens to have. Then the
                // stepper draws against the row's right edge, which is where it reads
                // as right-aligned however wide the node turns out to be.
                if let Some((n, range)) = pin_count {
                    let (fewest, most) = (*range.start(), *range.end());
                    let (row, _) = ui.allocate_exact_size(
                        egui::vec2(kit::stepper_w(ui), kit::stepper_h(ui)),
                        egui::Sense::hover(),
                    );
                    let right = ui.max_rect().right();
                    let step = kit::header_stepper(
                        ui,
                        n,
                        fewest..=most,
                        ("Remove the last input", "Add an input"),
                        row,
                        right,
                    );
                    match step {
                        Some(1) => {
                            if let Some(node) = snarl.get_node_mut(node) {
                                let name = pin_letter(node.inputs.len());
                                node.inputs.push(PinDescriptor::new(name, SignalType::Any));
                            }
                        }
                        Some(-1) if n > fewest => {
                            remove_input_pin(node, n - 1, inputs, snarl);
                        }
                        _ => {}
                    }
                }

                // The channels of a per-channel filter: same stepper, same place, one step
                // adds or drops the pair of pins together so the two columns never fall out
                // of step. The names are the ones the body's stepper used to give them,
                // because a saved patch holds them.
                if let Some((n, range)) = channel_count {
                    let (fewest, most) = (*range.start(), *range.end());
                    let (row, _) = ui.allocate_exact_size(
                        egui::vec2(kit::stepper_w(ui), kit::stepper_h(ui)),
                        egui::Sense::hover(),
                    );
                    let right = ui.max_rect().right();
                    let step = kit::header_stepper(
                        ui,
                        n,
                        fewest..=most,
                        ("Remove the last channel", "Add a channel"),
                        row,
                        right,
                    );
                    match step {
                        Some(1) => {
                            if let Some(node) = snarl.get_node_mut(node) {
                                let next = node.inputs.len() + 1;
                                node.inputs.push(PinDescriptor::new(format!("In {next}"), SignalType::Float));
                                node.outputs.push(PinDescriptor::new(format!("Out {next}"), SignalType::Float));
                            }
                        }
                        Some(-1) if n > fewest => {
                            remove_input_pin(node, n - 1, inputs, snarl);
                            remove_output_pin(node, n - 1, outputs, snarl);
                        }
                        _ => {}
                    }
                }

                // The `+` for a module whose ports are a column of strips, and the box it
                // reserves beside the title - same shape and size as the stepper's squares,
                // so the header reads the same wherever it appears.
                if let Some(noun) = port_noun {
                    let h = kit::stepper_h(ui);
                    let (row, _) = ui.allocate_exact_size(egui::vec2(h, h), egui::Sense::hover());
                    let right = ui.max_rect().right();
                    if kit::header_plus_button(ui, &format!("Add a {noun}"), row, right) {
                        add_port(node, &port_module, snarl);
                    }
                }

                if option_add || macro_add {
                    let h = kit::stepper_h(ui);
                    let (row, _) = ui.allocate_exact_size(egui::vec2(h, h), egui::Sense::hover());
                    let right = ui.max_rect().right();
                    let hover = if option_add { "Add an option" } else { "Add an output" };
                    if kit::header_plus_button(ui, hover, row, right) {
                        if option_add {
                            if let Some(n) = snarl.get_node_mut(node) {
                                let mut opts: Vec<Value> = n
                                    .params
                                    .get("options")
                                    .and_then(|v| v.as_array().cloned())
                                    .unwrap_or_default();
                                let next = opts.len() + 1;
                                opts.push(Value::String(format!("Option {next}")));
                                n.params.insert("options".into(), Value::Array(opts));
                            }
                        } else {
                            simple_bodies::add_macro_port(snarl, node);
                        }
                    }
                    simple_bodies::trace_rect(
                        ui,
                        node,
                        "header_add",
                        egui::Rect::from_min_max(egui::pos2(right - h, row.top()), egui::pos2(right, row.bottom())),
                    );
                }

                if is_subpatch {
                    let _ = has_pinned;
                    let edit_tooltip = format!("{} module{} inside", inner_count, if inner_count == 1 { "" } else { "s" });
                    if ui.small_button("Edit…").on_hover_text(edit_tooltip).clicked() {
                        self.edit_subpatch_request = Some(node);
                    }
                    let (lbl, tip) = if is_unlocked {
                        ("Lock", "Lock element positions and restore interactivity")
                    } else {
                        ("Layout", "Click highlighted elements in the editor to pin; drag/resize on the body")
                    };
                    if ui.small_button(lbl).on_hover_text(tip).clicked() {
                        if let Some(n) = snarl.get_node_mut(node) {
                            n.extra.layout_unlocked = !is_unlocked;
                        }
                    }
                    // Save / Load buttons — round-trip the sub-patch
                    // to/from a .fxsp file. Mirror the same dialogs
                    // used by the Easy-mode header so behavior stays
                    // identical regardless of where the user triggers
                    // it.
                    if ui.small_button("Save…")
                        .on_hover_text("Save sub-patch to a .fxsp file")
                        .clicked()
                    {
                        if !self.is_inner_canvas {
                            self.save_subpatch_request = Some(node);
                        } else if let Some(sp) = snarl.get_node(node).and_then(|n| n.subpatch.as_ref()) {
                            // Nested sub-patches carry no overlay items to bake.
                            let _ = crate::app::save_subpatch_file(sp);
                        }
                    }
                    if ui.small_button("Load…")
                        .on_hover_text("Load sub-patch from a .fxsp file (replaces current contents)")
                        .clicked()
                    {
                        if let Some(loaded) = crate::app::load_subpatch_file() {
                            if let Some(n) = snarl.get_node_mut(node) {
                                // Update the node's display_name from
                                // the loaded sub-patch so the canvas
                                // header reflects the new identity.
                                if !loaded.display_name.is_empty() {
                                    n.display_name = loaded.display_name.clone();
                                }
                                // Mirror pins so outer wires can
                                // remain valid if pin layouts match.
                                use flexinput_core::PinDescriptor;
                                n.inputs = loaded.pins_in.iter()
                                    .map(|p| PinDescriptor::new(&p.name, p.signal_type))
                                    .collect();
                                n.outputs = loaded.pins_out.iter()
                                    .map(|p| PinDescriptor::new(&p.name, p.signal_type))
                                    .collect();
                                n.subpatch = Some(Box::new(loaded));
                            }
                        }
                    }
                }

                // Response Curve: Save / Load / Reset in the header.
                if is_response_curve {
                    if ui.small_button("Save…").on_hover_text("Save curve to a .fxc file").clicked() {
                        curve_header_save(node, snarl);
                    }
                    if ui.small_button("Load…").on_hover_text("Load curve from a .fxc file").clicked() {
                        curve_header_load(node, curve_is_float, snarl);
                    }
                    if ui.small_button("Reset").on_hover_text("Reset to default (undoable)").clicked() {
                        curve_header_reset(node, curve_is_float, snarl);
                        self.push_undo_request = true;
                    }
                }

                // RWS Aim: Save/Load preset buttons live in the header so the body
                // stays clean for the knobs + ruler. The Rotation input is always
                // read as a gyro rate.
                if is_rws {
                    // Save / load the RWS feel set (calibration + sensitivity +
                    // flick/aim) as a portable .fxrws preset — e.g. one per game.
                    if ui.small_button("Save…")
                        .on_hover_text("Save this RWS calibration + feel set to a .fxrws preset")
                        .clicked()
                    {
                        if let Some(path) = crate::overlay::with_overlay_not_topmost(|| {
                            rfd::FileDialog::new()
                                .add_filter("FlexInput RWS preset", &["fxrws"])
                                .set_file_name("aim.fxrws")
                                .save_file()
                        }) {
                            let _ = rws_save_preset(snarl, node, &path);
                        }
                    }
                    if ui.small_button("Load…")
                        .on_hover_text("Load RWS settings from a .fxrws preset")
                        .clicked()
                    {
                        if let Some(path) = crate::overlay::with_overlay_not_topmost(|| {
                            rfd::FileDialog::new()
                                .add_filter("FlexInput RWS preset", &["fxrws"])
                                .pick_file()
                        }) {
                            let _ = rws_load_preset(snarl, node, &path);
                        }
                    }
                    // Mouse Scale unit — display only: `scale` stays stored per
                    // degree, so patches and presets are unaffected.
                    ui.separator();
                    let per_360 = RwsScaleUnit::of(snarl, node).per_360();
                    let tip = "Mouse Scale unit: mouse dots per degree of turn, or per full 360° turn\n(as Steam Input and sensitivity databases list it). Display only — the\nstored value and .fxrws presets don't change.";
                    for (val, lbl, on) in [("deg", "dots/°", !per_360), ("360", "dots/360°", per_360)] {
                        if ui.selectable_label(on, egui::RichText::new(lbl).small())
                            .on_hover_text(tip)
                            .clicked()
                        {
                            if let Some(n) = snarl.get_node_mut(node) {
                                n.params.insert("scale_unit".into(), Value::String(val.into()));
                            }
                        }
                    }
                }

                // SVG module: Load… / Clear / tint picker live in the header.
                // The body area is reserved for the rendered image only.
                if is_svg {
                    if ui.small_button("Load…")
                        .on_hover_text("Load an .svg file (the source is embedded in the patch)")
                        .clicked()
                    {
                        if let Some(path) = crate::overlay::with_overlay_not_topmost(|| {
                            rfd::FileDialog::new().add_filter("SVG", &["svg"]).pick_file()
                        }) {
                            if let Ok(text) = std::fs::read_to_string(&path) {
                                if let Some(n) = snarl.get_node_mut(node) {
                                    n.params.insert("svg_data".into(), Value::String(text));
                                    // New payload → bump cache key so the loader re-decodes.
                                    let prev = n.params.get("svg_rev").and_then(|v| v.as_u64()).unwrap_or(0);
                                    n.params.insert("svg_rev".into(), serde_json::json!(prev + 1));
                                }
                            }
                        }
                    }
                    let has_data = snarl.get_node(node)
                        .and_then(|n| n.params.get("svg_data").and_then(|v| v.as_str()))
                        .map(|s| !s.is_empty())
                        .unwrap_or(false);
                    if has_data && ui.small_button("Clear").on_hover_text("Remove the loaded SVG").clicked() {
                        if let Some(n) = snarl.get_node_mut(node) {
                            n.params.remove("svg_data");
                            let prev = n.params.get("svg_rev").and_then(|v| v.as_u64()).unwrap_or(0);
                            n.params.insert("svg_rev".into(), serde_json::json!(prev + 1));
                        }
                    }
                    let mut tint = snarl.get_node(node)
                        .map(read_svg_tint)
                        .unwrap_or(egui::Color32::TRANSPARENT);
                    let mut mode = snarl.get_node(node)
                        .and_then(|n| n.params.get("color_mode").and_then(|v| v.as_str()).map(|s| s.to_string()))
                        .unwrap_or_else(|| "override".to_string());

                    // Custom color picker: opens a popup with the standard
                    // egui color square + RGBA sliders, but no Additive toggle
                    // (we provide our own Normal / Additive selector right
                    // next to the swatch since their meaning is application-
                    // specific here, not egui's premultiplied-alpha intent).
                    let id = ui.id().with(("svg_color_popup", node.0));
                    let swatch_size = egui::vec2(24.0, 16.0);
                    let (rect, resp) = ui.allocate_exact_size(swatch_size, egui::Sense::click());
                    let painter = ui.painter_at(rect);
                    // Checkerboard background so transparent picks read clearly.
                    let cb = 4.0_f32;
                    for ix in 0..((swatch_size.x / cb).ceil() as i32) {
                        for iy in 0..((swatch_size.y / cb).ceil() as i32) {
                            let dark = (ix + iy) % 2 == 0;
                            let r = egui::Rect::from_min_size(
                                egui::pos2(rect.left() + ix as f32 * cb, rect.top() + iy as f32 * cb),
                                egui::vec2(cb, cb),
                            ).intersect(rect);
                            painter.rect_filled(r, 0.0,
                                if dark { egui::Color32::from_gray(60) } else { egui::Color32::from_gray(110) });
                        }
                    }
                    painter.rect_filled(rect, 2.0, tint);
                    painter.rect_stroke(rect, 2.0,
                        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(80)),
                        egui::StrokeKind::Inside);
                    if resp.clicked() {
                        egui::Popup::toggle_id(ui.ctx(), id);
                    }
                    let mut changed = false;
                    popup_below_widget(&resp, id,
                        egui::PopupCloseBehavior::CloseOnClickOutside,
                        |ui| {
                            ui.set_min_width(220.0);
                            ui.label(egui::RichText::new("Mode").small().weak());
                            ui.horizontal(|ui| {
                                changed |= ui.selectable_value(&mut mode, "override".into(),
                                    egui::RichText::new("Override")).on_hover_text(
                                    "Replace SVG colors with this color; alpha = blend amount toward original").changed();
                                changed |= ui.selectable_value(&mut mode, "additive".into(),
                                    egui::RichText::new("Additive")).on_hover_text(
                                    "Add this color on top of original SVG; alpha = added intensity").changed();
                            });
                            ui.separator();
                            if egui::widgets::color_picker::color_picker_color32(
                                ui, &mut tint, egui::widgets::color_picker::Alpha::OnlyBlend,
                            ) {
                                changed = true;
                            }
                        });
                    if changed {
                        if let Some(n) = snarl.get_node_mut(node) {
                            n.params.insert("tint".into(), serde_json::json!([tint.r() as u64, tint.g() as u64, tint.b() as u64, tint.a() as u64]));
                            n.params.insert("color_mode".into(), Value::String(mode));
                        }
                    }
                }

                // Remapper / Map Action / Input Viewer: skin selector lives in the
                // header so the body stays compact and the label/dropdown can sit next to the title.
                if is_remapper || is_map_action || is_input_viewer {
                    use super::remapper_icons::Skin;
                    let current_str = snarl.get_node(node)
                        .and_then(|n| n.params.get("skin").and_then(|v| v.as_str()).map(|s| s.to_string()))
                        .unwrap_or_else(|| "auto".to_string());
                    let current = Skin::from_str(&current_str);
                    let resolved = remapper_resolve_skin(snarl, node, &current_str, self.automap_parent.as_ref());
                    let selected_text = if current == Skin::Auto {
                        format!("auto · {}", resolved.label())
                    } else {
                        current.label().to_string()
                    };
                    let mut new_skin = current;
                    // Right-align: build the chip+label as a right-to-left layout
                    // so it pins to the far edge of the header row regardless of
                    // node width.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        egui::ComboBox::from_id_salt((node, "remapper_skin_hdr"))
                            .selected_text(egui::RichText::new(selected_text).small())
                            .width(140.0)
                            .show_ui(ui, |ui| {
                                // Skin governs only gamepad chip rendering; KBM has
                                // no controller equivalent so it isn't offered here.
                                for opt in [Skin::Auto, Skin::Xbox, Skin::Playstation, Skin::SwitchPro, Skin::Steam] {
                                    if ui.selectable_label(current == opt,
                                        egui::RichText::new(opt.label()).small()).clicked()
                                    {
                                        new_skin = opt;
                                    }
                                }
                            });
                        ui.label(egui::RichText::new(if is_input_viewer { "Skin" } else { "Labels" })
                            .small().weak());
                    });
                    if new_skin != current {
                        if let Some(n) = snarl.get_node_mut(node) {
                            n.params.insert("skin".to_string(), Value::String(new_skin.as_str().to_string()));
                        }
                    }
                }

                // Label module: font size + text color in the header so they
                // don't compete with the text edit area.
                // Label module: its settings live at the right edge of the header, the
                // way the other header controls do, so the body is nothing but the text
                // itself - the field the corner grip sizes.
                if is_label {
                    let (mut size, mut col) = snarl.get_node(node).map(|n| {
                        let sz = n.params.get("font_size").and_then(|v| v.as_f64()).unwrap_or(14.0) as f32;
                        let arr = n.params.get("color").and_then(|v| v.as_array()).cloned();
                        let c: [u8; 4] = match arr.as_deref() {
                            Some([r, g, b, a]) => [
                                r.as_u64().unwrap_or(220) as u8,
                                g.as_u64().unwrap_or(220) as u8,
                                b.as_u64().unwrap_or(220) as u8,
                                a.as_u64().unwrap_or(255) as u8,
                            ],
                            _ => [220, 220, 220, 255],
                        };
                        (sz, c)
                    }).unwrap_or((14.0, [220, 220, 220, 255]));
                    // What the header is entitled to, measured rather than guessed:
                    // everything laid out before the right-edge block, plus what that
                    // block consumed. A block placed right-to-left draws against the
                    // frame's right edge, so the header's used rect spans the whole
                    // node - and the node's width formula would then hold it at whatever
                    // width it already has, which is a ratchet with a title on it.
                    let left_w = ui.min_rect().width();
                    let (size_changed, colour_changed, controls_w) = ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            let from = ui.cursor().min.x;
                            // Right-to-left, so the size reads as the rightmost thing:
                            // the first control added is the one nearest the corner.
                            let s = ui
                                .add(
                                    egui::DragValue::new(&mut size)
                                        .speed(0.25)
                                        .range(8.0..=72.0)
                                        .suffix("px"),
                                )
                                .on_hover_text("Font size")
                                .changed();
                            let c = fxi_color_swatch(ui, &mut col, "Text colour", true);
                            (s, c, (from - ui.cursor().min.x).max(0.0))
                        },
                    )
                    .inner;
                    self.header_content.insert(
                        node.0,
                        left_w + controls_w + ui.spacing().item_spacing.x,
                    );
                    if size_changed || colour_changed {
                        if let Some(n) = snarl.get_node_mut(node) {
                            if let Some(num) = Number::from_f64(size as f64) {
                                n.params.insert("font_size".into(), Value::Number(num));
                            }
                            let arr = serde_json::json!([col[0] as u64, col[1] as u64, col[2] as u64, col[3] as u64]);
                            n.params.insert("color".into(), arr);
                        }
                    }
                }

                // Knob: the Uni/Bi range switch sits at the header's right edge so the
                // dial and its reading have the body to themselves - at two blocks there
                // is no row to spare under the face. It's the width of its two words: the
                // house button reached over the fold chevron at one block wide. A collapsed
                // knob has no switch - collapsing one is how you stop it being nudged while
                // you're moving other things about it - and shows its number where the
                // switch was, which is the one thing worth reading off a folded knob.
                if is_knob {
                    let collapsed = snarl.get_node_info(node).map_or(true, |i| !i.open);
                    let bipolar = snarl
                        .get_node(node)
                        .and_then(|n| n.params.get("bipolar").and_then(|v| v.as_bool()))
                        .unwrap_or(false);
                    let flipped = ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            let from = ui.cursor().min.x;
                            // Nothing at the right edge while the knob is folded: its number
                            // is written in the body band under the header, which a folded
                            // node still has.
                            let resp = if collapsed {
                                ui.label("")
                            } else {
                                crate::canvas::viewer::kit_bodies::toggle_button(
                                    ui,
                                    bipolar,
                                    if bipolar { "bi" } else { "uni" },
                                    crate::canvas::viewer::kit_bodies::KNOB_TOGGLE,
                                )
                                .on_hover_text(
                                    "Uni: the dial turns across 0..1.\nBi: it turns across -1..1, centred on 0.",
                                )
                            };
                            self.header_content.insert(
                                node.0,
                                // The chevron and the switch - the name is drawn if it fits,
                                // but it is not what sets the module's narrowest width.
                                header_room_before_name
                                    + (from - ui.cursor().min.x).max(0.0)
                                    + ui.spacing().item_spacing.x,
                            );
                            resp.clicked()
                        },
                    )
                    .inner;
                    if flipped {
                        if let Some(n) = snarl.get_node_mut(node) {
                            let next = !bipolar;
                            n.params.insert("bipolar".into(), Value::Bool(next));
                            // Changing the range pulls the value back inside it.
                            let lo = if next { -1.0f64 } else { 0.0 };
                            let cur = n.params.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
                            n.params.insert("value".into(), Value::from(cur.clamp(lo, 1.0)));
                        }
                    }
                }

                // device.source: AutoMap pin. Render the "Auto-Map" text in
                // the right side of the header. The pin itself is drawn by
                // snarl in `show_output` as a half-circle outside the frame,
                // using `header_y` to override its Y to this row's center
                // (X stays column-aligned, matching every other column pin).
                if is_device_source && source_automap_out_idx.is_some() {
                    // Suffix the arrow so the label visually points toward
                    // the right-side AutoMap pin (matches the sink layout's
                    // "← Auto-Map" convention).
                    let text = "Auto-Map →";
                    let label_w = ui.painter().layout_no_wrap(
                        text.to_string(),
                        egui::TextStyle::Small.resolve(ui.style()),
                        Color32::WHITE,
                    ).size().x;
                    let row_left = ui.min_rect().left();
                    let target_x = row_left + device_body_w - label_w;
                    let spacer = (target_x - ui.cursor().min.x).max(16.0);
                    ui.add_space(spacer);
                    let resp = ui.label(egui::RichText::new(text).small().weak());
                    ui.ctx().data_mut(|d| d.insert_temp(automap_label_abs_y_key(node), resp.rect.center().y));
                }
            });

            // XInput player-slot circles for XInput device nodes (a physical Xbox
            // `device.source` or our Virtual Xbox `device.sink`). The widget is a
            // no-op for every other device/module, so this call is safe to make
            // for any device node. Clicking a circle queues a slot request keyed
            // by this node's device_id; the app routes it to the reorder engine.
            if is_device_source || is_device_sink {
                crate::easy::io_panel::canvas_node_xinput_slots(ui, dev_id_str);
            }

            // MIDI ports: everything that adds or manages the node's messages
            // lives in the header; the body is just the message list.
            let is_midi_in = is_device_source && dev_id_str.starts_with("midi_in:");
            if is_midi_in {
                // Its width (next frame) right-aligns the Auto-Map label above,
                // which otherwise only knows the body's width.
                let w = ui.scope(|ui| show_midi_in_header_controls(node, outputs, ui, snarl))
                    .response.rect.width();
                ui.ctx().data_mut(|d| d.insert_temp(midi_header_w_key(node), w));
            }
            if is_device_sink && dev_id_str.starts_with("midi_out:") {
                show_midi_out_header_controls(node, inputs, ui, snarl);
            }

            // Mouse-sensitivity slider for the virtual keyboard+mouse sink.
            // Rendered ABOVE the "Auto-Map" label so the AutoMap pin (Y =
            // bottom-of-header) sits in line with the label, not crowded
            // against the slider.
            // Linear 0..3000; double-click on the slider track resets
            // to the global default (Settings → Device defaults).
            // Double-click on the value box keeps egui's inline-edit behavior.
            if is_keymouse_sink {
                const LABEL_CELL_W: f32 = 60.0;
                let mut ms_edit = mouse_sens_initial;
                let ms_initial = ms_edit;
                ui.horizontal(|ui| {
                    slider_label(ui, "Mouse ×", LABEL_CELL_W);
                    let resp = ui.add(egui::Slider::new(&mut ms_edit, 0.0_f32..=3000.0)
                        .show_value(false)
                        .clamping(egui::SliderClamping::Always));
                    if slider_track_double_clicked(ui, &resp) { ms_edit = self.param_defaults.mouse_sensitivity; }
                    ui.add(egui::DragValue::new(&mut ms_edit)
                        .speed(0.5)
                        .range(0.0_f32..=3000.0)
                        .fixed_decimals(2));
                });
                if (ms_edit - ms_initial).abs() > f32::EPSILON {
                    if let Some(n) = snarl.get_node_mut(node) {
                        n.params.insert("mouse_sensitivity".into(), Value::from(ms_edit as f64));
                    }
                }
                if let Some(n) = snarl.get_node_mut(node) {
                    touchpad_route_header_controls(ui, &mut n.params, LABEL_CELL_W);
                    optical_mouse_route_header_controls(ui, &mut n.params, LABEL_CELL_W);
                }
            }

            // Rumble-feedback shaping for virtual gamepad sinks (everything but
            // keymouse). A two-handle range slider (floor..max) + a compact
            // Curve box. Rendered in the header — like the keymouse Mouse ×
            // slider — and above the "← Auto-Map" label. Shapes ONLY the
            // game/app rumble this virtual pad forwards to a physical pad via
            // Auto-Map; the user's own direct rumble wiring is sent full-scale.
            if is_device_sink && !is_keymouse_sink && dev_id_str.starts_with("virtual.") {
                if let Some(n) = snarl.get_node_mut(node) {
                    crate::canvas::header_controls::render_rumble_feedback_controls(ui, &mut n.params, self.param_defaults);
                }
                // Switching USB audio swaps the node to the pad's other variant;
                // the per-frame reconcile builds it and retires the old one.
                if let Some(id) = crate::canvas::header_controls::render_usb_audio_toggle(ui, dev_id_str) {
                    crate::canvas::switch_virtual_sink_device(snarl, node, &id);
                }
            }

            // device.sink: "Auto-Map" label anchored to the bottom of the
            // header. The AutoMap pin's Y is derived from this label's
            // absolute screen Y combined with a per-frame pin-row Y anchor
            // (also captured this frame, but read next frame in show_input)
            // — so even though snarl runs `show_input` BEFORE `show_header`,
            // the delta between the two cached values translates with drag
            // and yields the correct screen Y when applied to show_input's
            // current pin-row Y.
            //
            // X position: pulled flush-left against the node body so it
            // aligns with the column-pin labels below it. Snarl's outer
            // header layout starts AFTER the collapse chevron + drag-space
            // (Layout::left_to_right), so a normal `ui.label` inherits that
            // post-chevron X. We step back to the node body's left edge by
            // computing the row's absolute left from the outer container's
            // clip rect and using `ui.painter().text()` to draw at that X.
            if is_device_sink && sink_automap_in_idx.is_some() {
                // Prefix with a left-pointing arrow so users can identify the
                // pin direction without us having to align the label X with
                // the column-pin labels (snarl's column labels sit at a
                // different X than the header band — chasing that anchor
                // through collapse animations breaks more than it fixes).
                let row_left = ui.max_rect().left();
                let row_y0 = ui.cursor().top();
                let painter = ui.painter().clone();
                let font_id = egui::TextStyle::Small.resolve(ui.style());
                let color = ui.visuals().weak_text_color();
                let galley = painter.layout_no_wrap("← Auto-Map".to_string(), font_id, color);
                let label_h = galley.size().y;
                let row_h = label_h.max(ui.spacing().interact_size.y * 0.6);
                let label_pos = egui::pos2(row_left, row_y0 + (row_h - label_h) * 0.5);
                painter.galley(label_pos, galley, color);
                // Reserve vertical space so following header rows advance past.
                ui.allocate_exact_size(egui::vec2(0.0, row_h), egui::Sense::hover());
                let center_y = row_y0 + row_h * 0.5;
                ui.ctx().data_mut(|d| d.insert_temp(automap_label_abs_y_key(node), center_y));
            }

            // A physical pad's per-device rows: Deadzone / Gyro ×, digital
            // triggers, capacitive mute. Laid out beside the UI-navigation toggle
            // (below), so it's a closure the two layouts share.
            let param_defaults = self.param_defaults;
            let has_touch_misc = is_device_source
                && self.physical_devices.iter()
                    .any(|d| d.id == dev_id_str && has_capacitive_suppression(d));
            let source_rows = |ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>| {
                // Deadzone / Gyro × slider rows.
                //
                // Each row is rendered as a fixed-width label cell + a Slider with
                // its value box suppressed + a separate DragValue. This split lets
                // us recognise double-click on the slider track alone (reset to
                // global default), while a double-click on the DragValue keeps
                // egui's built-in inline edit behaviour.
                // Pad the header for device.source nodes that have fewer than
                // two slider rows so every device.source ends up the same
                // height. Without this, a collapsed short-header node (e.g.
                // XInput, which has no gyro) is so short that pin rows
                // protrude past the header bottom and remain visible
                // through the translucent body when collapsed. Each slider
                // row is ~22 px tall.
                // (A MIDI In header is already that tall with its controls.)
                if is_device_source && !is_midi_in {
                    const SLIDER_ROW_H: f32 = 22.0;
                    let present_rows = (has_deadzone as u8) + (has_gyro as u8);
                    let missing_rows = 2 - present_rows.min(2);
                    if missing_rows > 0 {
                        ui.add_space(SLIDER_ROW_H * missing_rows as f32);
                    }
                }

                if is_device_source && (has_deadzone || has_gyro) {
                    const LABEL_CELL_W: f32 = 60.0;
                    let mut dz_edit = deadzone_initial;
                    let dz_initial  = dz_edit;
                    if has_deadzone {
                        ui.horizontal(|ui| {
                            slider_label(ui, "Deadzone", LABEL_CELL_W);
                            let resp = ui.add(egui::Slider::new(&mut dz_edit, 0.0_f32..=0.5)
                                .show_value(false)
                                .clamping(egui::SliderClamping::Always));
                            if slider_track_double_clicked(ui, &resp) { dz_edit = param_defaults.stick_deadzone; }
                            // Orange while the sticks' deadzones differ (the
                            // Calibration window sets them apart); editing it
                            // sets both again.
                            if deadzones_differ {
                                ui.visuals_mut().override_text_color = Some(MIXED_VALUE_COLOR);
                            }
                            let dv = ui.add(egui::DragValue::new(&mut dz_edit)
                                .speed(0.005)
                                .range(0.0_f32..=0.5)
                                .fixed_decimals(2));
                            if deadzones_differ {
                                dv.on_hover_text("The sticks have different deadzones (Calibration                                     window). Changing this sets both.");
                            }
                        });
                    }
                    let mut gm_edit = gyro_mult_initial;
                    let gm_initial  = gm_edit;
                    if has_gyro {
                        ui.horizontal(|ui| {
                            slider_label(ui, "Gyro ×", LABEL_CELL_W);
                            let resp = ui.add(egui::Slider::new(&mut gm_edit, 0.1_f32..=50.0)
                                .logarithmic(true)
                                .show_value(false)
                                .clamping(egui::SliderClamping::Always));
                            if slider_track_double_clicked(ui, &resp) { gm_edit = param_defaults.gyro_mult; }
                            ui.add(egui::DragValue::new(&mut gm_edit)
                                .speed(0.05)
                                .range(0.1_f32..=50.0)
                                .fixed_decimals(2));
                        });
                    }

                    if (dz_edit - dz_initial).abs() > f32::EPSILON
                       || (gm_edit - gm_initial).abs() > f32::EPSILON
                    {
                        if let Some(n) = snarl.get_node_mut(node) {
                            if (dz_edit - dz_initial).abs() > f32::EPSILON {
                                set_both_stick_deadzones(&mut n.params, dz_edit);
                            }
                            if (gm_edit - gm_initial).abs() > f32::EPSILON {
                                n.params.insert("gyro_multiplier".into(), Value::from(gm_edit as f64));
                            }
                        }
                    }
                }

                // Digital-trigger override toggle for physical gamepad sources.
                // See `digital_trigger_toggle` in easy::io_panel for the semantics —
                // forced ON + disabled for digital-only pads (Switch Pro), opt-in
                // elsewhere. Stored on the node's `digital_triggers` param.
                // Both gilrs AND sdl physical pads (phys_pad_slug handles both
                // prefixes) — an SDL-surfaced pad needs the same digital-trigger
                // option as its gilrs twin (was gilrs-only: the toggle vanished when
                // dedup kept the SDL node of a DInput pad).
                if is_device_source
                    && crate::canvas::remapper_icons::phys_pad_slug(dev_id_str).is_some()
                {
                    digital_trigger_header_toggle(ui, snarl, node, dev_id_str);
                }

                // Capacitive stick-cap / grip mute — pads that have them (see
                // `has_capacitive_suppression`). Sits next to the digital-trigger
                // toggle because both are per-device input-conditioning opt-ins.
                if has_touch_misc {
                    touch_misc_header_toggle(ui, snarl, node);
                }
            };

            // UI navigation: lets this pad drive FlexInput's own UI, including the
            // config overlay — which can be summoned in Advanced mode too, where
            // there is no Easy-mode device card to reach the switch from. Same
            // icon, same state (`gamepad_nav::nav_state_of`), and the same place
            // as on the Easy card: under the device icon, the rows to its right.
            let nav_state = if is_device_source {
                crate::gamepad_nav::nav_state_of(dev_id_str)
            } else {
                None
            };
            // Width of the nav block's rows, when drawn this frame.
            let mut rows_w: Option<f32> = None;
            if let Some((nav_on, nav_disabled)) = nav_state {
                // The device icon (40 px square) draws its pad in the middle of
                // the square; tuck the chevron up into the empty bottom margin.
                // Not under the XInput slot circles, which sit in between. Only
                // the chevron column tucks: the rows beside it stay under the
                // title column (Calibrate / HidHide eye / LED swatch), which
                // reaches the icon's bottom.
                let tuck = if crate::easy::io_panel::device_id_is_xinput(dev_id_str) { 0.0 } else { 8.0 };
                const ICON_COL_W: f32 = 40.0;
                const NAV_W: f32 = 34.0;
                // Breathing room between the Calibrate row and the slider rows.
                const ROWS_GAP: f32 = 4.0;
                let nav_h = crate::easy::io_panel::nav_toggle_height(NAV_W);
                let block = ui.horizontal_top(|ui| {
                    let (col, _) = ui.allocate_exact_size(
                        egui::vec2(ICON_COL_W, (nav_h - tuck).max(0.0)),
                        egui::Sense::hover(),
                    );
                    let nav_rect = egui::Rect::from_min_size(
                        egui::pos2(col.center().x - NAV_W * 0.5, col.top() - tuck),
                        egui::vec2(NAV_W, nav_h),
                    );
                    if let Some(v) = crate::easy::io_panel::nav_toggle_icon(
                        ui, nav_rect, dev_id_str, nav_on, nav_disabled,
                    ) {
                        crate::gamepad_nav::request_nav_toggle(dev_id_str, v);
                    }
                    ui.vertical(|ui| {
                        ui.add_space(ROWS_GAP);
                        source_rows(ui, snarl);
                    });
                });
                // Read next frame by the Auto-Map label's right alignment: these
                // rows can now be wider than the pin columns below.
                let w = block.response.rect.width();
                ui.ctx().data_mut(|d| d.insert_temp(midi_header_w_key(node), w));
                rows_w = Some(w);
            } else {
                source_rows(ui, snarl);
            }

            // Raw joysticks: Learn and what it caught, in the header like a
            // MIDI In's message controls. Its width joins the rows' for the
            // Auto-Map label's alignment.
            if is_device_source && is_raw_pad_node("device.source", dev_id_str) {
                let w = ui.scope(|ui| show_raw_pad_header_controls(node, outputs, ui, snarl, self.live_signals))
                    .response.rect.width();
                let w = rows_w.map_or(w, |r| r.max(w));
                ui.ctx().data_mut(|d| d.insert_temp(midi_header_w_key(node), w));
            }

            // Feedback Control: add to the game's feedback, or override it.
            if is_feedback_control {
                feedback_override_header_toggle(ui, snarl, node);
            }

            // RWS Aim: whether this module turns the flicks mapping cards send.
            if is_rws {
                rws_map_flicks_header_toggle(ui, snarl, node);
            }

            // JSM Config: pass the rest of the pad through, or only what the config says.
            if is_jsm {
                ui.horizontal(|ui| {
                    jsm_strict_header_toggle(ui, snarl, node);
                    if jsm_macros_header_button(ui, snarl, node) {
                        self.push_undo_request = true;
                    }
                });
            }

            // AutoMap curves: the bus signal the curve reshapes.
            if is_automap_curve {
                if automap_curve_pin_picker(ui, snarl, node, self.live_signals, self.automap_parent.as_ref()) {
                    self.push_undo_request = true;
                }
            }

            // Second header row — only visible while in Layout mode for this
            // sub-patch. Snap settings live on the sub-patch itself (they
            // belong to its body's drag/resize behavior, not to the editor).
            // Factored into `layout_editing_controls` so Easy mode renders the
            // same controls below its preset bar.
            if is_subpatch && is_unlocked {
                layout_editing_controls(ui, snarl, node);
            }
        });
    }

    fn inputs(&mut self, node: &NodeData) -> usize {
        node.inputs.len()
    }

    fn outputs(&mut self, node: &NodeData) -> usize {
        node.outputs.len()
    }

    fn show_input(
        &mut self,
        pin: &InPin,
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeData>,
    ) -> impl egui_snarl::ui::SnarlPin + 'static {
        let node = &snarl[pin.id.node];
        let desc = &node.inputs[pin.id.input];
        let is_relocated_automap = node.module_id == "device.sink"
            && desc.signal_type == SignalType::AutoMap
            && desc.name == "Auto-Map";
        ui.spacing_mut().item_spacing.y = 0.0;
        // A module drawn as a column of port strips (Selector, Split, Combiner): the strip
        // carries the name of a numbered port, so the socket beside it draws no label twice,
        // and every row - named or not - is drawn at the strips' pitch, which is what keeps
        // a socket level with the strip that names the same port.
        let pitch = kit::strip_pitch(
            &node.module_id,
            kit::strip_bands(&node.module_id, node.inputs.len(), node.outputs.len()),
            ui,
        );
        let strip_pin = kit::strip_pin(&node.module_id, true, pin.id.input);
        if !is_relocated_automap && !strip_pin {
            let name = if node.module_id == "module.sample_hold" && pin.id.input == 1 {
                sample_hold_gate_label(node)
            } else {
                desc.name.as_str()
            };
            let (shown, shortened) = if kit::pin_abbreviates(&node.module_id) {
                kit::pin_short(name)
            } else {
                (name.to_string(), false)
            };
            let col = channel_label_color(&node.module_id, pin.id.input).or_else(|| {
                // A device inlet says what drives it: the Auto-Map bus's own
                // orange when the bus covers it, the brighter plain text colour
                // when you have wired it by hand (a wire beats the bus in the
                // engine, so the orange drops the moment one lands), and the
                // default dim label when nothing reaches it at all.
                match device_inlet_drive(self.sink_bus, node, pin.id.input, !pin.remotes.is_empty()) {
                    InletDrive::AutoMap => Some(AUTOMAP_INLET_LABEL),
                    InletDrive::Wired   => Some(ui.visuals().widgets.inactive.fg_stroke.color),
                    InletDrive::Idle    => None,
                }
            });
            if let Some(p) = pitch {
                pin_label_at_row(ui, &shown, col, p, shortened, name);
            } else {
                let text = egui::RichText::new(shown.as_str()).small();
                let text = match col {
                    Some(c) => text.color(c),
                    None => text,
                };
                let resp = ui.label(text);
                // The label is short so the module can be; the name it stands for is here.
                if shortened {
                    resp.on_hover_text(name);
                }
            }
        } else if let Some(p) = pitch {
            ui.allocate_exact_size(egui::vec2(0.0, p), egui::Sense::hover());
        }
        let header_y = if is_relocated_automap {
            // Dual-stash delta recovery. Snarl runs show_input BEFORE
            // show_header in the same frame, so this frame's label Y
            // isn't available here — but last frame's label_y AND last
            // frame's pin_row_y both are. Their delta is purely
            // layout-derived (constant under drag, pan, and scroll —
            // none of those change the spacing of rows inside the
            // node). Read the stale pair FIRST, derive the layout
            // delta, then overwrite pin_row_y with this frame's value
            // for the next frame's read.
            let cur_pin_y = automap_chevron_y(ui);
            let (prev_label, prev_pin) = ui.ctx().data(|d| (
                d.get_temp::<f32>(automap_label_abs_y_key(pin.id.node)),
                d.get_temp::<f32>(automap_pin_row_y_key(pin.id.node)),
            ));
            let delta = match (prev_label, prev_pin) {
                (Some(l), Some(p)) => l - p,
                _ => 0.0,
            };
            ui.ctx().data_mut(|d| d.insert_temp(
                automap_pin_row_y_key(pin.id.node), cur_pin_y));
            Some(cur_pin_y + delta)
        } else {
            None
        };
        let glow = input_pin_glow(self.live_signals, self.sink_bus, snarl, node, pin.id.node, pin.id.input, self.automap_parent.as_ref())
            .map(|(col, t)| (col, pin_glow_smoothed(ui.ctx(), pin.id.node, pin.id.input, true, t)));
        // Half-shape (flat right edge against the node) for every column
        // pin and every header-relocated AutoMap pin. Half-circle for scalar
        // types, half-square for AutoMap (shape is chosen inside
        // MaybeHeaderPin::draw based on `inner.shape`), so AutoMap pins on
        // utility modules (Splitter / Collector / Fork / Selector / Combiner
        // / Remapper / Inlet) get the same visual language as device chips.
        let half = Some(HalfSide::Right);
        MaybeHeaderPin { inner: pin_info(desc.signal_type), glow, half, header_y }
    }

    fn show_output(
        &mut self,
        pin: &OutPin,
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeData>,
    ) -> impl egui_snarl::ui::SnarlPin + 'static {
        let node = &snarl[pin.id.node];
        let desc = &node.outputs[pin.id.output];
        let is_relocated_automap = node.module_id == "device.source"
            && desc.signal_type == SignalType::AutoMap
            && desc.name == "Auto-Map";
        ui.spacing_mut().item_spacing.y = 0.0;
        let pitch = kit::strip_pitch(
            &node.module_id,
            kit::strip_bands(&node.module_id, node.inputs.len(), node.outputs.len()),
            ui,
        );
        let strip_pin = kit::strip_pin(&node.module_id, false, pin.id.output);
        if strip_pin {
            // The port's own row: no label (the strip says the same word), but the pitch
            // the strips are drawn at.
            if let Some(p) = pitch {
                ui.allocate_exact_size(egui::vec2(0.0, p), egui::Sense::hover());
            }
        } else if !is_relocated_automap && !kit::pin_hides_outputs(&node.module_id) {
            let (shown, shortened) = if kit::pin_abbreviates(&node.module_id) {
                kit::pin_short(&desc.name)
            } else {
                (desc.name.clone(), false)
            };
            let col = channel_label_color(&node.module_id, pin.id.output);
            if let Some(p) = pitch {
                pin_label_at_row(ui, &shown, col, p, shortened, desc.name.as_str());
            } else {
                let text = egui::RichText::new(shown.as_str()).small();
                let text = match col {
                    Some(c) => text.color(c),
                    None => text,
                };
                let resp = ui.label(text);
                if shortened {
                    resp.on_hover_text(desc.name.as_str());
                }
            }
        }
        let header_y = if is_relocated_automap {
            let cur_pin_y = automap_chevron_y(ui);
            let (prev_label, prev_pin) = ui.ctx().data(|d| (
                d.get_temp::<f32>(automap_label_abs_y_key(pin.id.node)),
                d.get_temp::<f32>(automap_pin_row_y_key(pin.id.node)),
            ));
            let delta = match (prev_label, prev_pin) {
                (Some(l), Some(p)) => l - p,
                _ => 0.0,
            };
            ui.ctx().data_mut(|d| d.insert_temp(
                automap_pin_row_y_key(pin.id.node), cur_pin_y));
            Some(cur_pin_y + delta)
        } else {
            None
        };
        let glow = output_pin_glow(self.live_signals, snarl, pin.id.node, pin.id.output, self.automap_parent.as_ref())
            .map(|(col, t)| (col, pin_glow_smoothed(ui.ctx(), pin.id.node, pin.id.output, false, t)));
        // Half-shape with flat LEFT edge for every output column pin —
        // including AutoMap outputs on utility modules — so AutoMap pins
        // everywhere get the same half-chip visual as the device source/sink
        // header chips. MaybeHeaderPin::draw picks square vs circle based
        // on `inner.shape`.
        let half = Some(HalfSide::Left);
        MaybeHeaderPin { inner: pin_info(desc.signal_type), glow, half, header_y }
    }

    fn connect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<NodeData>) {
        let from_type = snarl[from.id.node].outputs[from.id.output].signal_type;
        let to_type = snarl[to.id.node].inputs[to.id.input].signal_type;
        if !to_type.accepts(from_type) { return; }
        // AutoMap input pins accept exactly ONE upstream wire. Multiple wires
        // route ambiguously (only `pin.remotes.first()` is honored by eval and
        // graph-build), so silently dropping later wires confuses users. Drop
        // the existing wire(s) so the new connection visibly replaces them.
        if to_type == SignalType::AutoMap {
            let existing: Vec<OutPinId> = snarl.in_pin(to.id).remotes.iter().copied().collect();
            for src in existing {
                snarl.disconnect(src, to.id);
            }
        }
        snarl.connect(from.id, to.id);
    }

    fn disconnect(&mut self, from: &OutPin, to: &InPin, _snarl: &mut Snarl<NodeData>) {
        // Intercept right-click-on-wire, or a finger held on it: show a context menu instead
        // of disconnecting immediately.
        let pos = self.ctx.input(|i| i.pointer.latest_pos()).unwrap_or_default();
        self.pending_wire_menu = Some((from.id, to.id, pos));
    }

    // ── Node bodies ──────────────────────────────────────────────────────────

    fn node_resizable(&mut self, node: NodeId, snarl: &Snarl<NodeData>) -> bool {
        snarl.get_node(node).is_some_and(|n| {
            !kit::fixed_size(&n.module_id)
                && (kit::uses_kit(&n.module_id) || kit::stretchy(&n.module_id))
        })
    }

    /// What the header actually holds, for the nodes that put a control against its right
    /// edge (the Text module's font size and colour). Measured in `show_header`.
    fn node_header_width(&mut self, node: NodeId, _snarl: &Snarl<NodeData>) -> Option<f32> {
        self.header_content.get(&node.0).copied()
    }

    /// The floor the corner grip is held to, for a body that measured one this frame
    /// (`kit::body_width`'s second value). Stretchy bodies and kit rows both do; nothing
    /// else asks, and the grip keeps its header-width fallback for those.
    fn node_narrowest(&mut self, node: NodeId, snarl: &Snarl<NodeData>) -> Option<f32> {
        let id = snarl.get_node(node)?;
        if kit::uses_kit(&id.module_id) || kit::stretchy(&id.module_id) {
            self.size_floors.get(&node.0).copied()
        } else {
            None
        }
    }

    fn resize_node(&mut self, node: NodeId, frame_width: f32, frame_height: f32, snarl: &mut Snarl<NodeData>) {
        let Some(n) = snarl.get_node_mut(node) else { return };
        if let Some(w) = Number::from_f64(frame_width.round() as f64) {
            if n.params.get(kit::WIDTH_PARAM) != Some(&Value::Number(w.clone())) {
                n.params.insert(kit::WIDTH_PARAM.into(), Value::Number(w));
            }
        }
        if kit::stretchy(&n.module_id.clone()) {
            if let Some(h) = Number::from_f64(frame_height.round() as f64) {
                if n.params.get(kit::HEIGHT_PARAM) != Some(&Value::Number(h.clone())) {
                    n.params.insert(kit::HEIGHT_PARAM.into(), Value::Number(h));
                }
            }
        }
    }

    fn has_body(&mut self, node: &NodeData) -> bool {
        let dev_id = node.params.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
        let is_midi_source = node.module_id == "device.source" && dev_id.starts_with("midi_in:");
        let is_midi_sink   = node.module_id == "device.sink"   && dev_id.starts_with("midi_out:");
        is_midi_source || is_midi_sink || module_has_body(&node.module_id)
    }

    fn show_body(
        &mut self,
        node_id: NodeId,
        inputs: &[InPin],
        outputs: &[OutPin],
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeData>,
    ) {
        let module_id = snarl
            .get_node(node_id)
            .map(|n| n.module_id.clone())
            .unwrap_or_default();
        let device_id = snarl
            .get_node(node_id)
            .and_then(|n| n.params.get("device_id").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();

        if module_id == "device.source" && device_id.starts_with("midi_in:") {
            show_midi_in_body(node_id, outputs, ui, snarl);
            return;
        }
        if is_raw_pad_node(&module_id, &device_id) {
            show_raw_pad_body(node_id, outputs, ui, snarl);
            return;
        }
        if module_id == "device.sink" && device_id.starts_with("midi_out:") {
            show_midi_out_body(node_id, inputs, ui, snarl);
            return;
        }

        // Kit bodies are handed their width (see `kit::body_width`), and stretchy ones
        // their height as well (see `kit::body_height`).
        let frame_size = node_frame_size(node_id, ui, snarl, self.snarl_id, self.node_margin_x, self.node_margin_y);
        let kit_w = if kit::uses_kit(&module_id) {
            let avail = ui.available_width();
            snarl.get_node(node_id).map_or(kit::MIN_W, |n| {
                let block = crate::canvas::canvas_grid(ui.ctx()).filter(|g| g.snap).map(|g| g.block);
                // A fixed-size module takes the width its cells ask for and no other: no grip
                // to set ui_w, and no ratchet on the width it was handed.
                let (w, floor) = if kit::fixed_size(&module_id) {
                    let mine = kit_min_width(&module_id);
                    (mine, mine)
                } else {
                    kit::body_width(n, avail, frame_size.map(|s| s.0), kit_min_width(&module_id), block, None)
                };
                self.size_floors.insert(node_id.0, floor);
                w
            })
        } else {
            0.0
        };
        // A stretchy body is one surface that fills the frame it's given, so it takes
        // both axes: the width the text wraps to, and the height as a floor the text can
        // overrun by growing the module another block.
        let (body_w, body_h) = if kit::stretchy(&module_id) {
            let block = crate::canvas::canvas_grid(ui.ctx()).filter(|g| g.snap).map(|g| g.block);
            snarl.get_node(node_id).map_or((0.0, 0.0), |n| {
                // How wide the surface has to be whatever the grip asks for. A note is
                // useless below a field that can hold a word; a readout's floor is its
                // own reading at the smallest size worth showing, so a long number asks
                // for a wider module instead of becoming a squint.
                let min_w = match module_id.as_str() {
                    "module.label" => 96.0,
                    "display.readout" => crate::canvas::viewer::scopes::readout_floor_w(snarl, node_id),
                    // A fader's thin way: a knob dragged to one block in one direction is a
                    // strip of track, which is what a slim slider is. The dial itself won't
                    // go this small (`simple_bodies::show_knob_body`), but the frame can.
                    "module.knob" => 20.0,
                    // The narrowest curve worth putting points on: the envelope's own body
                    // adds the rows it needs on top of this.
                    "generator.envelope" => 140.0,
                    _ => 0.0,
                };
                // Both need room for more than one line to be worth the module: a display
                // for the number to be large in, a note for the two lines it opens with.
                // A knob's floor is a track - a wide fader squeezed to one block tall is
                // still a usable slider - and what it starts at is a block of body. The
                // other stretchy bodies are held at their old floors in points: their type
                // sizes follow the body they're given, so a floor raised to the module's
                // four-block minimum asks for bigger type, which asks for a wider floor,
                // and the readout runs away across the canvas. The minimum height those
                // modules used to get from the grid is topped up elsewhere instead, in the
                // body pass (`module_id != "module.knob"` below), which doesn't reach back
                // into their own arithmetic.
                let min_h = match module_id.as_str() {
                    "module.knob" => 12.0,
                    // The envelope asks for its own height in its body - the rows it needs
                    // plus a graph worth editing - so the floor here is only what stops the
                    // grip taking the frame to nothing.
                    "generator.envelope" => 40.0,
                    // What a two-old-block frame leaves under the header and the frame's
                    // margins: the body floor that used to come for free from a 56-point
                    // grid. Stated in body points, not frame blocks - asking for the frame
                    // here (112) asks the body for 112, which makes the frame 168, which
                    // makes a readout's type bigger, which makes its width floor bigger,
                    // and it runs away sideways.
                    _ => 72.0,
                };
                // The default is a *frame* size, like what the grip writes: a knob starts
                // 112 points tall - two of the old blocks, four of these - and the floor
                // above lets it come down to one.
                let default_h = match module_id.as_str() {
                    "module.knob" => Some(4.0 * block.unwrap_or(28.0)),
                    // Where an envelope starts is what it drew before it followed its frame:
                    // a curve of about 180 by 150 plus six stacked rows, which is ten blocks
                    // and change. Nothing about the module's use asks for more or less.
                    "generator.envelope" => Some(11.0 * block.unwrap_or(28.0)),
                    _ => None,
                };
                let legacy = n
                    .params
                    .get("box_width")
                    .and_then(|v| v.as_f64())
                    .map(|w| w as f32);
                // Only a note has a starting size: an untouched readout sits at its own
                // floor, which is two blocks for an ordinary number. A knob starts at two
                // blocks too - one block of dial isn't worth dragging - but its floor is a
                // fader's thin way, so the grip can take it there.
                let default_w = match module_id.as_str() {
                    "module.label" => legacy.or(Some(160.0)),
                    // A knob starts 112 points wide for the same reason: one old block of
                    // dial isn't worth dragging.
                    "module.knob" => Some(4.0 * crate::canvas::canvas_grid(ui.ctx()).map_or(28.0, |g| g.block)),
                    // An empty readout used to be held at two blocks by the word "Readout" in
                    // its header, which is the same accident as the Counter's handed-out width:
                    // a name is painted now, so nothing in an untouched readout asked for the
                    // room. Ask for it by name - two blocks, like the knob's dial - and the
                    // floor below still lets the grip take it down to what the number needs.
                    "display.readout" => Some(4.0 * crate::canvas::canvas_grid(ui.ctx()).map_or(28.0, |g| g.block)),
                    "generator.envelope" => Some(10.0 * crate::canvas::canvas_grid(ui.ctx()).map_or(28.0, |g| g.block)),
                    _ => None,
                };
                let (w, floor) = kit::body_width(n, ui.available_width(), frame_size.map(|s| s.0), min_w, block, default_w);
                self.size_floors.insert(node_id.0, floor);
                (
                    w,
                    kit::body_height(n, ui.available_height(), frame_size.map(|s| s.1), min_h, block, default_h),
                )
            })
        } else {
            (0.0, 0.0)
        };

        match module_id.as_str() {
            "device.sink"          => show_sink_body(node_id, inputs, ui, snarl),
            "module.constant"      => show_constant_body(node_id, ui, snarl, self.view_salt),
            "module.switch"        => show_switch_body(node_id, ui, snarl, self.view_salt),
            "module.knob"          => show_knob_body(node_id, ui, snarl, self.view_salt, body_w, body_h),
            "display.readout"       => show_readout_body(node_id, ui, snarl, self.view_salt, body_w, body_h),
            "display.oscilloscope"  => show_oscilloscope_body(node_id, inputs, ui, snarl),
            "display.vectorscope"   => show_vectorscope_body(node_id, inputs, ui, snarl),
            "display.trigscope"     => show_trigscope_body(node_id, inputs, ui, snarl),
            "display.controller3d"  => show_controller3d_body(node_id, ui, snarl, self.live_signals, self.automap_parent.as_ref()),
            "processing.rws"   => show_rws_body(node_id, ui, snarl),
            "module.delay"     => show_delay_body(node_id, inputs, outputs, ui, snarl, self.view_salt),
            "module.average"   => show_average_body(node_id, inputs, outputs, ui, snarl, self.view_salt),
            "module.dc_filter" => show_dc_filter_body(node_id, inputs, outputs, ui, snarl, self.view_salt),
            "module.sample_hold" => show_sample_hold_body(node_id, ui, snarl, self.view_salt),
            "module.response_curve" => {
                if show_response_curve_body(node_id, inputs, outputs, ui, snarl) {
                    self.push_undo_request = true;
                }
            }
            "module.vec_response_curve" => {
                if show_vec_response_curve_body(node_id, inputs, outputs, ui, snarl) {
                    self.push_undo_request = true;
                }
            }
            "module.vec_reshape" => {
                if show_vec_reshape_body(node_id, inputs, outputs, ui, snarl) {
                    self.push_undo_request = true;
                }
            }
            "module.twoway_response_curve" => {
                if show_twoway_response_curve_body(node_id, inputs, outputs, ui, snarl) {
                    self.push_undo_request = true;
                }
            }
            "module.automap_response_curve" | "module.automap_twoway_response_curve" => {
                if show_automap_curve_body(node_id, inputs, outputs, ui, snarl) {
                    self.push_undo_request = true;
                }
            }
            "math.negate" => show_inverse_body(node_id, ui, snarl, kit_w, self.view_salt),
            "math.quantize" => show_quantize_body(node_id, inputs, ui, snarl, kit_w, self.view_salt),
            "module.vec_to_deflection" => show_vec_to_deflection_body(node_id, ui, snarl, kit_w, self.view_salt),
            "module.selector" => show_selector_body(node_id, inputs, outputs, ui, snarl),
            "module.split"    => show_split_body(node_id, inputs, outputs, ui, snarl),
            "module.dropdown" => show_dropdown_body(node_id, ui, snarl, self.view_salt),
            "module.macro"    => show_macro_body(node_id, outputs, ui, snarl, self.view_salt),
            "module.label"    => show_label_body(node_id, ui, snarl, body_w, body_h, self.view_salt),
            "module.svg"      => show_svg_body(node_id, ui, snarl),
            "logic.greater_than" | "logic.less_than" => show_or_equal_body(node_id, ui, snarl, self.view_salt),
            "logic.has_changed" => show_has_changed_body(node_id, ui, snarl, kit_w, self.view_salt),
            "logic.delay"   => show_logic_delay_body(node_id, ui, snarl, kit_w, self.view_salt),
            "logic.counter"        => show_counter_body(node_id, inputs, ui, snarl, kit_w, self.view_salt),
            "generator.oscillator"  => show_oscillator_body(node_id, inputs, ui, snarl, self.view_salt),
            "generator.envelope"    => show_envelope_body(node_id, inputs, ui, snarl, body_w, body_h),
            "processing.gyro_3dof"  => show_gyro_3dof_body(
                node_id, inputs, ui, snarl, self.live_signals,
                self.panic_shortcut, self.automap_parent.as_ref(),
            ),
            "module.automap_split"     => show_automap_split_body(node_id, outputs, ui, snarl),
            "module.automap_collect"   => show_automap_collect_body(node_id, inputs, ui, snarl),
            "module.automap_fork"      => show_automap_fork_body(node_id, outputs, ui, snarl),
            "module.automap_selector"  => show_automap_selector_body(node_id, inputs, ui, snarl),
            "module.automap_combiner"  => show_automap_combiner_body(node_id, inputs, ui, snarl, self.live_signals, self.automap_parent.as_ref(), false),
            "module.audio_stream_haptics" => show_audio_stream_haptics_body(node_id, ui, snarl, self.automap_parent.as_ref()),
            "module.jsm" => show_jsm_body(
                node_id, ui, snarl, self.live_signals, self.automap_parent.as_ref(),
            ),
            "module.network_send" => show_net_send_body(node_id, ui, snarl, self.automap_parent.as_ref()),
            "module.network_recv" => show_net_recv_body(node_id, ui, snarl, self.automap_parent.as_ref()),
            "module.remapper" => show_remapper_body(node_id, inputs, ui, snarl, self.live_signals, self.panic_shortcut, self.automap_parent.as_ref()),
            "module.map_action" => show_map_action_body(node_id, inputs, ui, snarl, self.live_signals, self.panic_shortcut, self.automap_parent.as_ref()),
            "module.touch_zones" => show_touch_zones_body(node_id, ui, snarl, self.live_signals, self.automap_parent.as_ref()),
            "module.input_viewer" => super::input_viewer::show_input_viewer_body(
                node_id, ui, snarl, self.live_signals, self.automap_parent.as_ref()),
            "module.menu" => super::menu_body::show_menu_body(
                node_id, ui, snarl, self.live_signals, self.automap_parent.as_ref()),
            "module.stick_rotation" => {
                if show_stick_rotation_body(node_id, ui, snarl) {
                    self.push_undo_request = true;
                }
            }
            "module.area_mapper" => {
                if super::area_body::show_area_mapper_body(
                    node_id, ui, snarl, self.live_signals, self.automap_parent.as_ref(),
                ) {
                    self.push_undo_request = true;
                }
            }
            "subpatch" => {
                if show_subpatch_body(
                    node_id, ui, snarl,
                    self.live_signals, self.panic_shortcut, self.automap_parent.as_ref(),
                ) {
                    self.edit_subpatch_request = Some(node_id);
                }
            }
            "subpatch.inlet" | "subpatch.outlet" => show_inlet_outlet_body(node_id, ui, snarl),
            // Last, so a module with a body of its own keeps it: an operation or gate
            // drawn with its glyph and nothing else in the body.
            m if module_icons::has_icon(m) => show_glyph_body(node_id, ui, snarl, self.view_salt),
            _ => {}
        }

        // The minimum height, said out loud. A module that paints a mark and a couple of
        // pills - Quantize, Sample and Hold, the small operations and gates - reserves
        // nothing of its own, and asked for nothing; the grid's 56-point step happened to
        // put every one of them at 112 tall whatever its content was. Halving the block for
        // finer snapping lets the same content settle at 84, so each of them quietly loses a
        // quarter of its height. This tops the frame up to four blocks - the same 112 points
        // - and does it *after* the body has laid itself out, so the body saw the same room
        // it always did: a constant that puts its box and its pill side by side when it has
        // room doesn't suddenly decide to, because a reservation asked for more.
        // `frame − available` now counts the body's own height, so the top-up is only ever
        // what's missing, and a module with more to show is untouched. The knob is exempt:
        // going slim is the point of it.
        // The minimum height, said out loud, for the bodies that paint a mark and reserve
        // nothing of their own - the small operations and gates, Quantize, Sample and Hold,
        // Vec to Deflection. They asked for nothing; the grid's 56-point step put every one
        // of them at 112 tall whatever their content was. Halving the step lets the same
        // content settle at 84, so each quietly loses a quarter of its height, and this tops
        // the frame back up to four blocks - the same 112 points.
        //
        // It is deliberately only for the mark bodies. A body that lays its own rows out
        // against the room it is given reacts to the extra height: a Constant moves its box
        // and pill into one row, a kit module's rows unwrap and take the width with them, and
        // the width then feeds the frame and the frame feeds the layout - measured, a fresh
        // Delay walked from 112 to 420 points wide across a twenty-frame idle. A mark has
        // nothing to reflow, so the top-up lands and stops.
        if kit::mark_body(&module_id) {
            if let (Some(b), Some(frame_h)) = (
                crate::canvas::canvas_grid(ui.ctx()).filter(|g| g.snap).map(|g| g.block),
                frame_size.map(|s| s.1),
            ) {
                let want = 4.0 * b - (frame_h - ui.available_height());
                if want > 0.0 {
                    // Height only. Asking for the width too hands the width machinery a body
                    // as wide as everything left on the canvas, and it grows.
                    ui.allocate_exact_size(egui::vec2(0.0, want), egui::Sense::hover());
                }
            }
        }
    }

    // ── Node footer (below all pins) ─────────────────────────────────────────

    fn has_footer(&mut self, _node: &NodeData) -> bool {
        // The Deadzone slider was relocated into the device.source header so
        // it survives node collapse. No other node currently needs a footer.
        false
    }

    fn show_footer(
        &mut self,
        _node_id: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        _ui: &mut egui::Ui,
        _snarl: &mut Snarl<NodeData>,
    ) {}

    /// Snarl reports each node's final rendered rect here. Snarl draws nodes in
    /// a layer that carries the pan/zoom as a registered transform
    /// (`set_transform_layer`), so widget rects inside it are already in the
    /// layer's LOCAL = CANVAS space — the SAME space as `info.pos`. So we store
    /// the rect's size as-is (NO zoom division), letting `easy::layout` position
    /// the I/O nodes against the sub-patch's real size (and reflow after the
    /// Layout editor resizes it) instead of guessing.
    fn final_node_rect(
        &mut self,
        node: NodeId,
        rect: egui::Rect,
        ui: &mut egui::Ui,
        _snarl: &mut Snarl<NodeData>,
    ) {
        let salt = self.view_salt;
        ui.ctx().data_mut(|d| {
            let map = d.get_temp_mut_or_default::<NodeRectMap>(node_rects_id(salt));
            map.insert(node.0, [rect.min.x, rect.min.y, rect.width(), rect.height()]);
        });
    }

    // ── Graph context menu ───────────────────────────────────────────────────

    fn has_graph_menu(&mut self, _pos: egui::Pos2, _snarl: &mut Snarl<NodeData>) -> bool {
        true
    }

    fn show_graph_menu(
        &mut self,
        pos: egui::Pos2,
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeData>,
    ) {
        ui.label("Add module");
        ui.separator();
        show_module_menu(pos, ui, snarl, self.descriptors, None, self.is_inner_canvas);
    }

    // ── Drop-wire menu ───────────────────────────────────────────────────────

    fn has_dropped_wire_menu(&mut self, _src_pins: AnyPins, _snarl: &mut Snarl<NodeData>) -> bool {
        true
    }

    fn show_dropped_wire_menu(
        &mut self,
        pos: egui::Pos2,
        ui: &mut egui::Ui,
        src_pins: AnyPins,
        moved_from: Option<AnyPin>,
        snarl: &mut Snarl<NodeData>,
    ) {
        if ui.button("Cancel").clicked() {
            ui.close();
        }
        // Wires being moved are still connected where they came from, so doing nothing
        // puts them back and deleting them takes this. A new wire has nothing to delete.
        if let Some(from) = moved_from {
            let n = match &src_pins {
                AnyPins::Out(pins) => pins.len(),
                AnyPins::In(pins) => pins.len(),
            };
            if ui.button(if n == 1 { "Delete wire" } else { "Delete wires" }).clicked() {
                delete_moved_wires(snarl, &src_pins, from);
                ui.close();
            }
        }
        ui.separator();

        let wire = match src_pins {
            AnyPins::Out(srcs) => srcs.first().map(|&src| WireDir::FromOutput {
                srcs,
                from_type: snarl[src.node].outputs[src.output].signal_type,
                moved_from: match moved_from { Some(AnyPin::In(pin)) => Some(pin), _ => None },
            }),
            AnyPins::In(dsts) => dsts.first().map(|&dst| WireDir::FromInput {
                dsts,
                to_type: snarl[dst.node].inputs[dst.input].signal_type,
                moved_from: match moved_from { Some(AnyPin::Out(pin)) => Some(pin), _ => None },
            }),
        };
        let Some(wire) = wire else { return };
        let (descriptors, is_inner) = (self.descriptors, self.is_inner_canvas);
        if crate::touch::is_active(ui.ctx()) {
            // A level of its own, or the categories push the choices above off a small screen.
            ui.menu_button("Add module", |ui| {
                crate::touch::menu_list_scroll(ui, |ui| {
                    show_module_menu(pos, ui, snarl, descriptors, Some(wire), is_inner);
                });
            });
        } else {
            ui.label(match &wire {
                WireDir::FromOutput { .. } => "Connect to input of…",
                WireDir::FromInput { .. } => "Connect to output of…",
            });
            ui.separator();
            show_module_menu(pos, ui, snarl, descriptors, Some(wire), is_inner);
        }
    }

    fn highlighted_wire(&mut self) -> Option<(OutPinId, InPinId)> {
        self.highlighted_wire
    }

    fn can_connect(&mut self, from: &OutPin, to: &InPin, snarl: &Snarl<NodeData>) -> bool {
        let from_type = snarl[from.id.node].outputs[from.id.output].signal_type;
        snarl[to.id.node].inputs[to.id.input].signal_type.accepts(from_type)
    }

    // ── Node context menu ────────────────────────────────────────────────────

    fn has_node_menu(&mut self, _node: &NodeData) -> bool {
        true
    }

    fn show_node_menu(
        &mut self,
        node: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeData>,
    ) {
        if ui.button("Rename…").clicked() {
            self.rename_request = Some(node);
            ui.close();
        }

        let module_id = snarl.get_node(node).map(|n| n.module_id.as_str()).unwrap_or("").to_string();

        // Inner canvas only — show "Unpin from body" if the node has any pins.
        // Pinning happens exclusively through Layout mode + highlight click;
        // the menu doesn't offer a "Pin to body" option anymore so the user
        // can't accidentally pin a whole-body crop.
        if self.is_inner_canvas
            && !matches!(module_id.as_str(), "subpatch.inlet" | "subpatch.outlet" | "device.source" | "device.sink")
        {
            let already = self.pinned_inner_ids.contains(&node.0);
            if already {
                if ui.button("Unpin from body").clicked() {
                    self.expose_module_request = Some((node, "default".to_string(), [0.0, 0.0]));
                    ui.close();
                }
            } else {
                ui.label(egui::RichText::new("Use Layout mode to pin elements").small().weak());
            }
        }

        // "Edit…" for sub-patch nodes.
        if module_id == "subpatch" {
            if ui.button("Edit…").clicked() {
                self.edit_subpatch_request = Some(node);
                ui.close();
            }
        }

        // Switch: per-state caption + SVG icon editor. Available from the
        // node's right-click menu on any canvas (top-level or inside a
        // sub-patch). The sub-patch *layout* editor uses a separate inspector
        // strip for per-pin colors — see `switch_pin_inspector_strip_item`.
        //
        // Submenus override the default close behavior to `CloseOnClickOutside`
        // so clicking on the inline TextEdit / radio buttons doesn't dismiss
        // the menu mid-edit. Only the explicit `ui.close()` calls (or clicking
        // outside the popup) will close it.
        if module_id == "module.switch" {
            use egui::containers::menu::{MenuConfig, SubMenuButton};
            use egui::PopupCloseBehavior;
            ui.separator();
            // `find`, so they keep this menu's style.
            let cfg = MenuConfig::find(ui).close_behavior(PopupCloseBehavior::CloseOnClickOutside);
            SubMenuButton::new("ON state…").config(cfg.clone()).ui(ui, |ui| {
                switch_state_submenu(ui, node, snarl, true);
            });
            SubMenuButton::new("OFF state…").config(cfg).ui(ui, |ui| {
                switch_state_submenu(ui, node, snarl, false);
            });
        }

        // "Replace with…" for physical device source/sink nodes.
        let device_id = snarl.get_node(node)
            .and_then(|n| n.params.get("device_id").and_then(|v| v.as_str()))
            .map(|s| s.to_string());

        if matches!(module_id.as_str(), "device.source" | "device.sink") {
            let is_virtual = device_id.as_deref().map(|id| id.starts_with("virtual.")).unwrap_or(false);
            if !is_virtual {
                let is_source = module_id == "device.source";
                let candidates: Vec<(usize, &str)> = self.physical_devices
                    .iter()
                    .enumerate()
                    .filter(|(_, d)| {
                        Some(d.id.as_str()) != device_id.as_deref()
                            && if is_source {
                                !d.outputs.is_empty()
                            } else {
                                // sink: MidiOut or devices that only have inputs
                                matches!(d.kind, ControllerKind::MidiOut)
                                    || (d.outputs.is_empty() && !d.inputs.is_empty())
                            }
                    })
                    .map(|(i, d)| (i, d.display_name.as_str()))
                    .collect();

                if !candidates.is_empty() {
                    ui.separator();
                    ui.menu_button("Replace with…", |ui| {
                        for (idx, name) in candidates {
                            if ui.button(name).clicked() {
                                self.replace_request = Some((node, idx));
                                ui.close();
                            }
                        }
                    });
                }
            }
        }

        ui.separator();
        if ui.button("Group into sub-patch…").clicked() {
            self.group_request = true;
            ui.close();
        }
        if ui.button("Remove node").clicked() {
            self.remove_request = Some(node);
            ui.close();
        }
    }
}

// ── Body renderers ────────────────────────────────────────────────────────────





//  interaction layer.)

/// Which modules draw a body under their header. A module missing from this
/// list renders as a bare header however complete its body renderer is — the
/// gate is easy to forget when adding one, so it lives on its own and is tested.
///
/// Everything `module_icons` knows is in it by construction: those modules' bodies
/// are their glyphs, painted into the room the pins leave over, allocating nothing.
pub(crate) fn module_has_body(module_id: &str) -> bool {
    module_icons::has_icon(module_id)
        || matches!(
            module_id,
            "device.sink" | "module.constant" | "module.switch" | "module.knob" | "module.label" | "module.svg"
            | "display.readout" | "display.oscilloscope" | "display.vectorscope" | "display.trigscope"
            | "display.controller3d"
            | "module.delay" | "module.average" | "module.dc_filter" | "module.sample_hold" | "module.response_curve" | "module.vec_response_curve" | "module.vec_reshape" | "module.twoway_response_curve"
            | "module.automap_response_curve" | "module.automap_twoway_response_curve"
            | "module.area_mapper" | "module.stick_rotation"
            | "math.negate" | "math.quantize" | "module.vec_to_deflection"
            | "module.selector" | "module.split" | "module.dropdown" | "module.macro"
        | "logic.greater_than" | "logic.less_than" | "logic.has_changed" | "logic.delay" | "logic.counter"
        | "generator.oscillator" | "generator.envelope" | "processing.gyro_3dof"
        | "processing.rws"
        | "module.automap_split" | "module.automap_collect"
        | "module.automap_fork" | "module.automap_selector"
        | "module.automap_combiner" | "module.audio_stream_haptics" | "module.jsm"
        | "module.network_send" | "module.network_recv"
        | "module.remapper" | "module.map_action"
        | "module.touch_zones" | "module.input_viewer" | "module.menu"
        | "subpatch" | "subpatch.inlet" | "subpatch.outlet"
    )
}

#[cfg(test)]
mod body_gate_tests {
    use super::kit::variadic_inputs;
    use super::module_has_body;
    use super::module_icons;

    // A module whose body renderer exists but which is missing from this gate
    // draws a bare header — the JSM node shipped that way once.
    #[test]
    fn modules_with_a_body_are_gated_in() {
        for m in ["module.jsm", "module.remapper", "module.audio_stream_haptics",
                  "module.label", "module.touch_zones", "processing.rws",
                  "module.automap_response_curve", "module.automap_twoway_response_curve",
                  "module.area_mapper", "module.stick_rotation", "module.sample_hold",
                  "logic.has_changed"] {
            assert!(module_has_body(m), "{m} draws a body");
        }
        assert!(!module_has_body("module.feedback_control"), "a pins-only module draws none");
        // Abs draws its mark now, and reserves the band for it, where it used to be a
        // bare header a block shorter than its siblings.
        assert!(module_has_body("math.abs"), "Abs draws its mark");
    }

    // The small operations are described in three places that have to agree: the
    // header's stepper (`kit::variadic_inputs`), the glyph (`module_icons::ICONS`)
    // and this gate — a module with a glyph but no body has no room to paint one in.
    #[test]
    fn the_small_operations_agree_across_stepper_glyph_and_gate() {
        let registered: Vec<&str> = flexinput_modules::all_modules()
            .into_iter()
            .map(|r| r.descriptor.id)
            .collect();
        // Everything the glyph table names is a real module, and gets a body for it.
        for (id, _) in module_icons::ICONS {
            assert!(registered.contains(id), "{id} is not a registered module");
            assert!(module_has_body(id), "{id} has the body its glyph is painted into");
        }
        for m in ["math.add", "math.subtract", "math.multiply", "math.divide", "math.min_max"] {
            assert!(variadic_inputs(m).is_some(), "{m} counts its inputs in the header");
        }
        // The gates keep their fixed pair — the engine reads two inputs, so there is
        // no count for a stepper to set — but they take a glyph like the rest.
        for m in ["logic.and", "logic.or", "logic.not", "logic.xor", "logic.nand", "logic.nor", "logic.xnor",
                  "logic.equal", "logic.not_equal"] {
            assert!(variadic_inputs(m).is_none(), "{m} has a fixed input count");
            assert!(module_icons::has_icon(m), "{m} is drawn with a glyph");
        }
        // The comparisons have a glyph, a toggle of their own, and no stepper.
        for m in ["logic.greater_than", "logic.less_than"] {
            assert!(variadic_inputs(m).is_none(), "{m} has a fixed input count");
            assert!(module_icons::has_icon(m), "{m} is drawn with a glyph");
            assert!(module_has_body(m), "{m} draws its toggle");
        }
    }
}
