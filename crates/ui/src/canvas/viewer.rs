use egui::Color32;
use egui_snarl::{
    ui::{AnyPins, NodeLayout, PinInfo, SnarlViewer},
    InPin, InPinId, NodeId, OutPin, OutPinId, Snarl,
};
use flexinput_core::{ModuleDescriptor, PinDescriptor, Signal, SignalType, automap as am_canon};
use flexinput_devices::{ControllerKind, PhysicalDevice, midi::cc_display_name};
use flexinput_engine::current_sample_rate;
use serde_json::{Number, Value};

use super::{
    curve::{sample_curve, vec_reshape_apply, VEC_RESHAPE_BOUNDARY_DEFAULT, VEC_RESHAPE_GAIN_DEFAULT},
    node::{LayoutDecoration, LayoutItem, NodeData, TextAlign},
};
use crate::app::request_repaint_throttled;

// ── Split modules (mechanical extraction from this file; the glob re-exports
// keep every pre-split `viewer::item` path resolving unchanged) ──
mod asth;
mod curve_bodies;
mod curve_support;
mod scopes;
mod automap_bodies;
mod chrome;
mod controller3d;
mod expose;
mod glow;
mod gyro_body;
mod midi;
mod net;
mod osc_envelope;
mod pins;
mod remapper_bodies;
mod remapper_card;
mod remapper_filter;
mod remapper_reorder;
mod remapper_support;
mod reshape;
mod scale;
mod simple_bodies;
mod subpatch_body;
pub(crate) use asth::*;
pub(crate) use curve_bodies::*;
pub(crate) use curve_support::*;
pub(crate) use scopes::*;
pub(crate) use automap_bodies::*;
pub(crate) use chrome::*;
pub(crate) use controller3d::*;
pub(crate) use expose::*;
pub(crate) use glow::*;
pub(crate) use gyro_body::*;
pub(crate) use midi::*;
pub(crate) use net::*;
pub(crate) use osc_envelope::*;
pub(crate) use pins::*;
pub(crate) use remapper_bodies::*;
pub(crate) use remapper_card::*;
pub(crate) use remapper_filter::*;
pub(crate) use remapper_reorder::*;
pub(crate) use remapper_support::*;
pub(crate) use reshape::*;
pub(crate) use scale::*;
pub(crate) use simple_bodies::*;
pub(crate) use subpatch_body::*;
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
    /// Set by show_node_menu when the user clicks "Rename…".
    pub rename_request: Option<NodeId>,
    /// Set by show_node_menu when the user picks "Replace with…".
    /// Carries (node id, index into physical_devices).
    pub replace_request: Option<(NodeId, usize)>,
    /// Set when the user clicks "Edit…" on a subpatch node.
    pub edit_subpatch_request: Option<NodeId>,
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
}

/// egui temp-data key for this canvas's measured node rects, in CANVAS space.
/// Written by `final_node_rect` each frame, read by `easy::layout`.
pub fn node_rects_id(view_salt: u64) -> egui::Id {
    egui::Id::new(("flexinput_node_rects", view_salt))
}

/// The measured node-rect map: NodeId.0 → canvas-space [x, y, w, h].
pub type NodeRectMap = std::collections::HashMap<usize, [f32; 4]>;

impl<'a> SnarlViewer<NodeData> for FlexViewer<'a> {
    fn title(&mut self, node: &NodeData) -> String {
        node.display_name.clone()
    }

    fn node_layout(
        &mut self,
        default: NodeLayout,
        node_id: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        snarl: &Snarl<NodeData>,
    ) -> NodeLayout {
        if snarl.get_node(node_id).map(|n| n.module_id == "module.knob").unwrap_or(false) {
            NodeLayout::flipped_sandwich()
        } else {
            default
        }
    }

    fn show_header(
        &mut self,
        node: NodeId,
        _inputs: &[egui_snarl::InPin],
        _outputs: &[egui_snarl::OutPin],
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeData>,
    ) {
        // Extract what we need before any UI calls so the borrow of snarl is released.
        let data = &snarl[node];
        let title = data.display_name.clone();
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
            estimate_device_body_width(ui, data)
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
        let is_svg            = snarl.get_node(node).map(|n| n.module_id == "module.svg").unwrap_or(false);
        let is_remapper       = snarl.get_node(node).map(|n| n.module_id == "module.remapper").unwrap_or(false);
        let is_map_action     = snarl.get_node(node).map(|n| n.module_id == "module.map_action").unwrap_or(false);
        let is_input_viewer   = snarl.get_node(node).map(|n| n.module_id == "module.input_viewer").unwrap_or(false);
        let is_menu           = snarl.get_node(node).map(|n| n.module_id == "module.menu").unwrap_or(false);
        let is_response_curve = snarl.get_node(node).map(|n| {
            n.module_id == "module.response_curve"
                || n.module_id == "module.vec_response_curve"
                || n.module_id == "module.twoway_response_curve"
        }).unwrap_or(false);
        let curve_is_float    = snarl.get_node(node).map(|n| n.module_id == "module.response_curve").unwrap_or(false);

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
                            && dev_id_str.starts_with("gilrs:") =>
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
                        if has_cal_here {
                            ui.horizontal(|ui| {
                                let cal_resp = ui.small_button("Calibrate")
                                    .on_hover_text("Open the Device Calibration window");
                                crate::panels::calibration::calibrate_button_activity(
                                    ui, node, cal_resp.rect);
                                if cal_resp.clicked() {
                                    self.calibrate_request = Some(node);
                                }
                                let hz = self.device_rates.get(&dev_id_owned).copied().unwrap_or(0);
                                ui.label(egui::RichText::new(format!("{} Hz", hz))
                                    .color(Color32::from_rgb(220, 160, 40)).small())
                                    .on_hover_text("Measured per-device polling rate (raw events/sec)");
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
                    ui.label(&title);
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
                        if let Some(sp) = snarl.get_node(node).and_then(|n| n.subpatch.as_ref()) {
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

                // SVG module: Load… / Clear / tint picker live in the header.
                // The body area is reserved for the rendered image only.
                if is_svg {
                    if ui.small_button("Load…")
                        .on_hover_text("Load an .svg file (the source is embedded in the patch)")
                        .clicked()
                    {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("SVG", &["svg"])
                            .pick_file()
                        {
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
                        egui::Stroke::new(1.0, egui::Color32::from_gray(80)),
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
                                for opt in [Skin::Auto, Skin::Xbox, Skin::Playstation, Skin::SwitchPro] {
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
                    let mut changed = false;
                    if ui.add(egui::DragValue::new(&mut size).speed(0.25).range(8.0..=72.0).suffix("px"))
                        .on_hover_text("Font size").changed()
                    {
                        changed = true;
                    }
                    if fxi_color_swatch(ui, &mut col, "Text colour", true) {
                        changed = true;
                    }
                    if changed {
                        if let Some(n) = snarl.get_node_mut(node) {
                            if let Some(num) = Number::from_f64(size as f64) {
                                n.params.insert("font_size".into(), Value::Number(num));
                            }
                            let arr = serde_json::json!([col[0] as u64, col[1] as u64, col[2] as u64, col[3] as u64]);
                            n.params.insert("color".into(), arr);
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
            if is_device_source {
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
                        if slider_track_double_clicked(ui, &resp) { dz_edit = self.param_defaults.stick_deadzone; }
                        ui.add(egui::DragValue::new(&mut dz_edit)
                            .speed(0.005)
                            .range(0.0_f32..=0.5)
                            .fixed_decimals(2));
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
                        if slider_track_double_clicked(ui, &resp) { gm_edit = self.param_defaults.gyro_mult; }
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
                            n.params.insert("deadzone".into(), Value::from(dz_edit as f64));
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
            if is_device_source && dev_id_str.starts_with("gilrs:") {
                digital_trigger_header_toggle(ui, snarl, node, dev_id_str);
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
        if !is_relocated_automap {
            let text = egui::RichText::new(&desc.name).small();
            let text = match channel_label_color(&node.module_id, pin.id.input) {
                Some(col) => text.color(col),
                None      => text,
            };
            ui.label(text);
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
        let glow = input_pin_glow(self.live_signals, snarl, node, pin.id.node, pin.id.input, self.automap_parent.as_ref())
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
        if !is_relocated_automap {
            let text = egui::RichText::new(&desc.name).small();
            let text = match channel_label_color(&node.module_id, pin.id.output) {
                Some(col) => text.color(col),
                None      => text,
            };
            ui.label(text);
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
        // Intercept right-click-on-wire: show a context menu instead of disconnecting immediately.
        let pos = self.ctx.input(|i| i.pointer.latest_pos()).unwrap_or_default();
        self.pending_wire_menu = Some((from.id, to.id, pos));
    }

    // ── Node bodies ──────────────────────────────────────────────────────────

    fn has_body(&mut self, node: &NodeData) -> bool {
        let dev_id = node.params.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
        let is_midi_source = node.module_id == "device.source" && dev_id.starts_with("midi_in:");
        let is_midi_sink   = node.module_id == "device.sink"   && dev_id.starts_with("midi_out:");
        is_midi_source || is_midi_sink || matches!(
            node.module_id.as_str(),
            "device.sink" | "module.constant" | "module.switch" | "module.knob" | "module.label" | "module.svg"
                | "display.readout" | "display.oscilloscope" | "display.vectorscope" | "display.trigscope"
                | "display.controller3d"
                | "module.delay" | "module.average" | "module.dc_filter" | "module.response_curve" | "module.vec_response_curve" | "module.vec_reshape" | "module.twoway_response_curve"
                | "math.add" | "math.subtract" | "math.multiply" | "math.divide"
                | "module.selector" | "module.split" | "module.dropdown" | "module.macro"
                | "logic.greater_than" | "logic.less_than" | "logic.delay" | "logic.counter"
                | "generator.oscillator" | "generator.envelope" | "processing.gyro_3dof"
                | "module.automap_split" | "module.automap_collect"
                | "module.automap_fork" | "module.automap_selector"
                | "module.automap_combiner" | "module.audio_stream_haptics"
                | "module.network_send" | "module.network_recv"
                | "module.remapper" | "module.map_action"
                | "module.touch_zones" | "module.input_viewer" | "module.menu"
                | "subpatch" | "subpatch.inlet" | "subpatch.outlet"
        )
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
        if module_id == "device.sink" && device_id.starts_with("midi_out:") {
            show_midi_out_body(node_id, inputs, ui, snarl);
            return;
        }

        match module_id.as_str() {
            "device.sink"          => show_sink_body(node_id, inputs, ui, snarl),
            "module.constant"      => show_constant_body(node_id, ui, snarl),
            "module.switch"        => show_switch_body(node_id, ui, snarl),
            "module.knob"          => show_knob_body(node_id, ui, snarl),
            "display.readout"       => show_readout_body(node_id, ui, snarl),
            "display.oscilloscope"  => show_oscilloscope_body(node_id, inputs, ui, snarl),
            "display.vectorscope"   => show_vectorscope_body(node_id, inputs, ui, snarl),
            "display.trigscope"     => show_trigscope_body(node_id, inputs, ui, snarl),
            "display.controller3d"  => show_controller3d_body(node_id, ui, snarl, self.live_signals),
            "module.delay"     => show_delay_body(node_id, inputs, outputs, ui, snarl),
            "module.average"   => show_average_body(node_id, inputs, outputs, ui, snarl),
            "module.dc_filter" => show_dc_filter_body(node_id, inputs, outputs, ui, snarl),
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
            "math.add" | "math.subtract" | "math.multiply" | "math.divide" => {
                show_math_variadic_body(node_id, inputs, ui, snarl);
            }
            "module.selector" => show_selector_body(node_id, inputs, ui, snarl),
            "module.split"    => show_split_body(node_id, outputs, ui, snarl),
            "module.dropdown" => show_dropdown_body(node_id, ui, snarl),
            "module.macro"    => show_macro_body(node_id, outputs, ui, snarl),
            "module.label"    => show_label_body(node_id, ui, snarl),
            "module.svg"      => show_svg_body(node_id, ui, snarl),
            "logic.greater_than" | "logic.less_than" => show_or_equal_body(node_id, ui, snarl),
            "logic.delay"   => show_logic_delay_body(node_id, ui, snarl),
            "logic.counter"        => show_counter_body(node_id, inputs, ui, snarl),
            "generator.oscillator"  => show_oscillator_body(node_id, inputs, ui, snarl),
            "generator.envelope"    => show_envelope_body(node_id, inputs, ui, snarl),
            "processing.gyro_3dof"  => show_gyro_3dof_body(
                node_id, inputs, ui, snarl, self.live_signals,
                self.panic_shortcut, self.automap_parent.as_ref(),
            ),
            "module.automap_split"     => show_automap_split_body(node_id, outputs, ui, snarl),
            "module.automap_collect"   => show_automap_collect_body(node_id, inputs, ui, snarl),
            "module.automap_fork"      => show_automap_fork_body(node_id, outputs, ui, snarl),
            "module.automap_selector"  => show_automap_selector_body(node_id, inputs, ui, snarl),
            "module.automap_combiner"  => show_automap_combiner_body(node_id, inputs, ui, snarl, self.live_signals),
            "module.audio_stream_haptics" => show_audio_stream_haptics_body(node_id, ui, snarl, self.automap_parent.as_ref()),
            "module.network_send" => show_net_send_body(node_id, ui, snarl, self.automap_parent.as_ref()),
            "module.network_recv" => show_net_recv_body(node_id, ui, snarl, self.automap_parent.as_ref()),
            "module.remapper" => show_remapper_body(node_id, inputs, ui, snarl, self.live_signals, self.panic_shortcut, self.automap_parent.as_ref()),
            "module.map_action" => show_map_action_body(node_id, inputs, ui, snarl, self.live_signals, self.panic_shortcut, self.automap_parent.as_ref()),
            "module.touch_zones" => show_touch_zones_body(node_id, ui, snarl, self.live_signals, self.automap_parent.as_ref()),
            "module.input_viewer" => super::input_viewer::show_input_viewer_body(
                node_id, ui, snarl, self.live_signals, self.automap_parent.as_ref()),
            "module.menu" => super::menu_body::show_menu_body(
                node_id, ui, snarl, self.live_signals, self.automap_parent.as_ref()),
            "subpatch" => {
                if show_subpatch_body(
                    node_id, ui, snarl,
                    self.live_signals, self.panic_shortcut, self.automap_parent.as_ref(),
                ) {
                    self.edit_subpatch_request = Some(node_id);
                }
            }
            "subpatch.inlet" | "subpatch.outlet" => show_inlet_outlet_body(node_id, ui, snarl),
            _ => {}
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
        snarl: &mut Snarl<NodeData>,
    ) {
        match src_pins {
            AnyPins::Out(out_pins) => {
                if let Some(&src) = out_pins.first() {
                    let from_type = snarl[src.node].outputs[src.output].signal_type;
                    ui.label("Connect to input of…");
                    ui.separator();
                    show_module_menu(pos, ui, snarl, self.descriptors, Some(WireDir::FromOutput { src, from_type }), self.is_inner_canvas);
                }
            }
            AnyPins::In(in_pins) => {
                if let Some(&dst) = in_pins.first() {
                    let to_type = snarl[dst.node].inputs[dst.input].signal_type;
                    ui.label("Connect to output of…");
                    ui.separator();
                    show_module_menu(pos, ui, snarl, self.descriptors, Some(WireDir::FromInput { dst, to_type }), self.is_inner_canvas);
                }
            }
        }
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
            let cfg = || MenuConfig::new().close_behavior(PopupCloseBehavior::CloseOnClickOutside);
            SubMenuButton::new("ON state…").config(cfg()).ui(ui, |ui| {
                switch_state_submenu(ui, node, snarl, true);
            });
            SubMenuButton::new("OFF state…").config(cfg()).ui(ui, |ui| {
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
            snarl.remove_node(node);
            ui.close();
        }
    }
}

// ── Body renderers ────────────────────────────────────────────────────────────



// ── Touch Zones body ──────────────────────────────────────────────────────────

/// Read a field's divider edges from a node. Field 0 uses `col_edges`/`row_edges`;
/// field N>0 uses `col_edges{N}`/`row_edges{N}`.
fn tz_node_edges(node: &NodeData, field: usize, which: &str) -> Vec<f32> {
    let key = if field == 0 { which.to_string() } else { format!("{which}{field}") };
    node.params.get(&key).and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect())
        .unwrap_or_default()
}

pub(crate) fn tz_read_field_edges(snarl: &Snarl<NodeData>, node_id: NodeId, field: usize, which: &str) -> Vec<f32> {
    snarl.get_node(node_id).map(|n| tz_node_edges(n, field, which)).unwrap_or_default()
}

fn tz_write_field_edges(node: &mut NodeData, field: usize, which: &str, edges: &[f32]) {
    let key = if field == 0 { which.to_string() } else { format!("{which}{field}") };
    node.params.insert(key, Value::Array(edges.iter().map(|&v| Value::from(v as f64)).collect()));
}

/// Number of touch fields on the node (2 in split mode, else 1).
fn tz_n_fields(snarl: &Snarl<NodeData>, node_id: NodeId) -> usize {
    let split = snarl.get_node(node_id)
        .and_then(|n| n.params.get("field_mode").and_then(|v| v.as_str())) == Some("split");
    if split { 2 } else { 1 }
}

/// Reconstruct per-(field,zone) live state from a node's OWN computed outputs
/// (`extra.last_out`). Source-agnostic — works for physical, network, and
/// collector touch. Returns `(field, zone) → (local_x, local_y, active)`.
pub(crate) fn tz_zone_live(node: &NodeData) -> std::collections::HashMap<(usize, usize), (f32, f32, bool)> {
    use flexinput_core::touchzones as tz;
    let mut m = std::collections::HashMap::new();
    if let Some(ids) = node.params.get("output_pin_ids").and_then(|v| v.as_array()) {
        for (i, idv) in ids.iter().enumerate() {
            let Some(tz::Pin::Zone { field, idx, comp }) = idv.as_str().and_then(tz::parse_pin) else { continue };
            let Some(sig) = node.extra.last_out.get(i).copied().flatten() else { continue };
            let e = m.entry((field, idx)).or_insert((0.0, 0.0, false));
            match comp {
                tz::ZoneComp::X => e.0 = sig.as_float(),
                tz::ZoneComp::Y => e.1 = sig.as_float(),
                tz::ZoneComp::Active => e.2 = sig.as_bool(),
            }
        }
    }
    m
}

/// Live per-(field,zone) finger state resolved from the upstream device's touch
/// pins in `live_signals` — used by MAPPING mode, which has no zone output ports
/// for [`tz_zone_live`] to read. Mirrors the engine's zone resolution: single
/// mode folds both fingers onto field 0 (touch1 last so it wins); split mode maps
/// touch1→field0, touch2→field1. Returns local (x,y,active) per occupied zone.
/// Live per-(field,zone) finger state for the mapping-mode field: which zone each
/// finger is ACTIVATING (hold-aware) + its local position. Under "Hold" a finger
/// stays attributed to its ORIGIN zone even after sliding into a neighbour (the
/// neighbour reports no hit), mirroring the eval — so the glow / analog preview
/// track ACTUAL output, not mere presence. Local coords are relative to the
/// effective (origin-if-held) zone, clamped to 0..1 (a held finger dragged out
/// saturates at the zone edge). Per-finger start zones persist in ctx temp,
/// advanced once per pass so multiple widgets sharing a node don't double-step.
fn tz_live_hits(
    snarl: &Snarl<NodeData>,
    node_id: NodeId,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    automap_parent: Option<&AutomapGlowParent<'_>>,
    ctx: &egui::Context,
) -> std::collections::HashMap<(usize, usize), (f32, f32, bool)> {
    use flexinput_core::touchzones as tz;
    let mut m = std::collections::HashMap::new();
    let Some(dev) = remapper_upstream_device_id(snarl, node_id, 0, automap_parent) else { return m; };
    let node = snarl.get_node(node_id);
    let split = node.and_then(|n| n.params.get("field_mode").and_then(|v| v.as_str())) == Some("split");
    let hold_zones: std::collections::HashSet<(usize, usize)> = node
        .and_then(|n| n.params.get("hold_zones").and_then(|v| v.as_array()))
        .map(|a| a.iter().filter_map(|p| {
            let q = p.as_array()?;
            Some((q.first()?.as_u64()? as usize, q.get(1)?.as_u64()? as usize))
        }).collect())
        .unwrap_or_default();
    let readf = |pin: &str| live_signals.get(&(dev.clone(), pin.to_string())).map(|s| s.as_float()).unwrap_or(0.0);
    let readb = |pin: &str| live_signals.get(&(dev.clone(), pin.to_string())).map(|s| s.as_bool()).unwrap_or(false);
    // Per-finger [active, start_zone, centre_x, centre_y] × 2, advanced once per
    // pass. The centre is the adaptive relative origin captured at touchdown
    // (mirrors eval's analog_by_zone): a landing inside the zone's inner region
    // becomes the centre (relative), otherwise the zone centre (absolute). It lets
    // the live vectorscope + curve preview reflect the zone's relative/absolute
    // setting instead of a raw absolute position.
    let pass = ctx.cumulative_pass_nr();
    let track_id = egui::Id::new(("tz_live_track", node_id.0));
    let (stored_pass, mut track): (u64, Vec<f32>) =
        ctx.data(|d| d.get_temp(track_id)).unwrap_or((0, vec![0.0; 8]));
    if track.len() < 8 { track.resize(8, 0.0); }
    let advance = stored_pass != pass;
    let mut just_down: Option<(usize, usize)> = None;
    // Adaptive-centre deflection per START zone (unit space, +Y DOWN — callers
    // flip to +Y up). Keyed like eval's analog_by_zone and published in ctx so the
    // vectorscope + curve preview read the SAME value the engine emits.
    let mut defl: std::collections::HashMap<(usize, usize), (f32, f32)> = std::collections::HashMap::new();
    let adaptive_cards: Vec<Value> = node
        .and_then(|n| n.params.get("zone_maps").and_then(|v| v.as_array()).cloned())
        .unwrap_or_default();
    for finger in 0..2usize {
        let (px, py, pa) = [("touch1_x", "touch1_y", "touch1_active"),
                            ("touch2_x", "touch2_y", "touch2_active")][finger];
        let field = if split { finger } else { 0 };
        let base = finger * 4;
        let active = readb(pa);
        let prev_active = track[base] > 0.5;
        if !active {
            if advance { track[base] = 0.0; }
            continue;
        }
        let tree = tz_field_tree(snarl, node_id, field);
        let (x, y) = tz::pad_point_to_unit(readf(px), readf(py));
        let (cur_id, _, _) = tree.locate(x, y);
        let cur_idx = cur_id as usize;
        let start_zone = if !prev_active { cur_idx } else { track[base + 1] as usize };
        // START zone geometry drives both the absolute centre and the deflection
        // scale (matches eval: a half-zone move = full deflection).
        let [sx0, sy0, sx1, sy1] = tree.zone_rect(start_zone as u32).unwrap_or([0.0, 0.0, 1.0, 1.0]);
        let (scx, scy) = ((sx0 + sx1) * 0.5, (sy0 + sy1) * 0.5);
        let (shw, shh) = (((sx1 - sx0) * 0.5).max(1e-3), ((sy1 - sy0) * 0.5).max(1e-3));
        if advance {
            if !prev_active {
                track[base + 1] = cur_idx as f32;
                let inner = tz_zone_adaptive(&adaptive_cards, field, cur_idx);
                let (cx, cy) = if (x - scx).abs() <= inner * shw && (y - scy).abs() <= inner * shh {
                    (x, y)
                } else { (scx, scy) };
                track[base + 2] = cx;
                track[base + 3] = cy;
                // Newest touchdown wins the tab-follow (see render_touch_zones_*),
                // so two fingers don't flicker the cards panel between zones.
                just_down = Some((field, cur_idx));
            }
            track[base] = 1.0;
        }
        let eff = if hold_zones.contains(&(field, start_zone)) { start_zone } else { cur_idx };
        let [x0, y0, x1, y1] = tree.zone_rect(eff as u32).unwrap_or([0.0, 0.0, 1.0, 1.0]);
        let lx = if x1 > x0 { ((x - x0) / (x1 - x0)).clamp(0.0, 1.0) } else { 0.5 };
        let ly = if y1 > y0 { ((y - y0) / (y1 - y0)).clamp(0.0, 1.0) } else { 0.5 };
        m.insert((field, eff), (lx, ly, true));
        // Adaptive deflection about the captured centre, scaled by the START zone.
        let (cx, cy) = (track[base + 2], track[base + 3]);
        let dfx = ((x - cx) / shw).clamp(-1.0, 1.0);
        let dfy = ((y - cy) / shh).clamp(-1.0, 1.0);
        defl.insert((field, start_zone), (dfx, dfy));
    }
    if advance {
        ctx.data_mut(|d| d.insert_temp(track_id, (pass, track)));
        // Publish the last touched-down origin (pass-stamped) so the tab-follow
        // locks to it, and a pick-mode tap can act on a FRESH touchdown only.
        if let Some((f, z)) = just_down {
            ctx.data_mut(|d| d.insert_temp(
                egui::Id::new(("tz_last_origin", node_id.0)), (pass, f, z)));
        }
    }
    // Publish the adaptive deflection map for the live vectorscope + curve preview
    // (pass-stamped so a stale frame doesn't leak a phantom deflection).
    ctx.data_mut(|d| d.insert_temp(
        egui::Id::new(("tz_live_defl", node_id.0)), (pass, defl.clone())));

    // Per-zone ACTIVE output pins for the on-pad activation glow: a finger is in
    // the zone (hold-aware, from `m`) AND the card's trigger is satisfied this
    // frame (touch = finger present; click = pad also pressed). Swipes are
    // transient and skipped. Stashed in ctx so `tz_paint_zone_mapping` can light
    // the exact icons that are firing — including a click's button alongside the
    // analog vectorscope. Keyed by node so both fields/widgets read their own.
    let cards = node.and_then(|n| n.params.get("zone_maps").and_then(|v| v.as_array()).cloned())
        .unwrap_or_default();
    let mut active_out: std::collections::HashMap<(usize, usize), Vec<String>> = std::collections::HashMap::new();
    for (&(f, z), &(_, _, act)) in &m {
        if !act { continue; }
        let clicked = readb(if f == 0 { "btn_touchpad" } else { "btn_touchpad2" });
        for c in cards.iter().filter(|c|
            c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) == f as u64 &&
            c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) == z as u64)
        {
            let trig = c.get("in").and_then(|v| v.as_array()).and_then(|a| a.first())
                .and_then(|v| v.as_str()).unwrap_or("tz_touch");
            let fire = match trig {
                "tz_click" => clicked,
                t if t.starts_with("tz_swipe") => false, // transient — not shown
                _ => true, // tz_touch
            };
            if !fire { continue; }
            let e = active_out.entry((f, z)).or_default();
            for p in c.get("out").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|v| v.as_str()) {
                if !e.iter().any(|x| x == p) { e.push(p.to_string()); }
            }
        }
    }
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(("tz_active_out", node_id.0)), active_out));

    m
}

/// The BSP zone tree for a field: an explicit `zone_tree`/`zone_tree{field}` param
/// (once the user has added partial dividers), else derived from the legacy grid
/// (`col_edges`/`row_edges`). Single source of truth shared with the eval so zone
/// hit-testing, drawing and mapping stay in lock-step.
pub(crate) fn tz_field_tree(snarl: &Snarl<NodeData>, node_id: NodeId, field: usize)
    -> flexinput_core::touchzones::ZoneNode
{
    use flexinput_core::touchzones as tz;
    let key = if field == 0 { "zone_tree".to_string() } else { format!("zone_tree{field}") };
    if let Some(t) = snarl.get_node(node_id)
        .and_then(|n| n.params.get(&key)).and_then(tz::ZoneNode::from_value)
    {
        return t;
    }
    let col = tz_read_field_edges(snarl, node_id, field, "col_edges");
    let row = tz_read_field_edges(snarl, node_id, field, "row_edges");
    tz::ZoneNode::from_grid(&col, &row)
}

/// Write a field's zone tree back to its param, dropping the legacy grid edges for
/// that field so the tree becomes authoritative.
pub(crate) fn tz_set_field_tree(snarl: &mut Snarl<NodeData>, node_id: NodeId,
    field: usize, tree: &flexinput_core::touchzones::ZoneNode)
{
    let key = if field == 0 { "zone_tree".to_string() } else { format!("zone_tree{field}") };
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.params.insert(key, tree.to_value());
    }
}

/// Cards (this field) bound to any of `zones`.
fn tz_cards_in_zones(snarl: &Snarl<NodeData>, node_id: NodeId, field: usize, zones: &[u32]) -> usize {
    snarl.get_node(node_id).and_then(|n| n.params.get("zone_maps").and_then(|v| v.as_array()))
        .map(|cards| cards.iter().filter(|c|
            c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) == field as u64 &&
            zones.contains(&(c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) as u32))).count())
        .unwrap_or(0)
}

/// Remove the divider at `path`. If the zones it would merge away carry no
/// mappings, apply immediately; otherwise stash a pending-merge so the module
/// shows a confirm popup (`tz_render_merge_popup`).
pub(crate) fn tz_request_or_apply_merge(snarl: &mut Snarl<NodeData>, node_id: NodeId,
    field: usize, tree: &flexinput_core::touchzones::ZoneNode, path: &[u8])
{
    let mut probe = tree.clone();
    let Some((_, removed)) = probe.remove_split(path, None) else { return; };
    if tz_cards_in_zones(snarl, node_id, field, &removed) == 0 {
        tz_set_field_tree(snarl, node_id, field, &probe); // nothing to lose — merge now
    } else if let Some(node) = snarl.get_node_mut(node_id) {
        node.params.insert("_tz_merge".into(), Value::Object(serde_json::Map::from_iter([
            ("field".to_string(), Value::from(field as u64)),
            ("path".to_string(), Value::Array(path.iter().map(|&b| Value::from(b as u64)).collect())),
        ])));
    }
}

/// If a merge is pending (`_tz_merge`), draw the confirm popup: the removed
/// zone(s) carry mappings, so the user picks whether to DELETE those mappings,
/// keep them by re-homing onto the surviving zone, or CANCEL.
pub(crate) fn tz_render_merge_popup(ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, node_id: NodeId) {
    let Some(m) = snarl.get_node(node_id).and_then(|n| n.params.get("_tz_merge").cloned()) else { return; };
    let field = m.get("field").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let path: Vec<u8> = m.get("path").and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_u64().map(|b| b as u8)).collect()).unwrap_or_default();
    let mut tree = tz_field_tree(snarl, node_id, field);
    let mut probe = tree.clone();
    let Some((kept, removed)) = probe.remove_split(&path, None) else {
        if let Some(n) = snarl.get_node_mut(node_id) { n.params.remove("_tz_merge"); }
        return;
    };
    let _ = kept;
    let n_cards = tz_cards_in_zones(snarl, node_id, field, &removed);
    let mut choice: Option<&'static str> = None;
    egui::Window::new("Remove divider")
        .id(egui::Id::new(("tz_merge_popup", node_id.0)))
        .collapsible(false).resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ui.ctx(), |ui| {
            ui.label(format!("Merging removes {} zone(s) that carry {} mapping(s).",
                removed.len(), n_cards));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("Choose inheritor (tap a zone)")
                    .on_hover_text("Pick which zone the mappings should move to — then the divider is removed.")
                    .clicked() { choice = Some("pick"); }
                if ui.button("Delete mappings").clicked() { choice = Some("delete"); }
                if ui.button("Cancel").clicked() { choice = Some("cancel"); }
            });
        });
    let Some(choice) = choice else { return; };
    match choice {
        // Enter merge-pick mode; the zone tap handler runs remove + re-home.
        "pick" => {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("_tz_pick".into(), serde_json::json!({
                    "kind": "merge", "field": field,
                    "path": path.iter().map(|&b| b as u64).collect::<Vec<_>>(),
                }));
            }
        }
        "delete" => {
            tree.remove_split(&path, None);
            tz_set_field_tree(snarl, node_id, field, &tree);
            if let Some(node) = snarl.get_node_mut(node_id) {
                if let Some(cards) = node.params.get_mut("zone_maps").and_then(|v| v.as_array_mut()) {
                    cards.retain(|c| !(c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) == field as u64 &&
                        removed.contains(&(c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) as u32))));
                }
            }
        }
        _ => {} // cancel
    }
    if let Some(node) = snarl.get_node_mut(node_id) { node.params.remove("_tz_merge"); }
}

// ── "Pick a destination zone" mode — shared by Migrate (move a zone's mappings)
// and the delete-inheritor flow (merge, then re-home the removed zones' cards).
// While active, clicking / tapping a zone applies the action instead of selecting.

fn tz_pick_kind(snarl: &Snarl<NodeData>, node_id: NodeId) -> Option<String> {
    snarl.get_node(node_id).and_then(|n| n.params.get("_tz_pick"))
        .and_then(|v| v.get("kind")).and_then(|v| v.as_str()).map(String::from)
}

fn tz_cancel_pick(snarl: &mut Snarl<NodeData>, node_id: NodeId) {
    if let Some(n) = snarl.get_node_mut(node_id) { n.params.remove("_tz_pick"); }
}

/// Begin a Migrate: the SELECTED zone's mappings will move onto the next zone the
/// user clicks / taps.
fn tz_start_migrate(snarl: &mut Snarl<NodeData>, node_id: NodeId) {
    let (field, zone) = tz_read_selection(snarl, node_id);
    if let Some(n) = snarl.get_node_mut(node_id) {
        n.params.insert("_tz_pick".into(), serde_json::json!({
            "kind": "migrate", "field": field, "src": zone,
        }));
    }
}

/// Re-home every card in `from_zones` (this field) onto `dest`.
fn tz_move_cards(snarl: &mut Snarl<NodeData>, node_id: NodeId, field: usize,
    from_zones: &[u32], dest: u32)
{
    if let Some(node) = snarl.get_node_mut(node_id) {
        if let Some(cards) = node.params.get_mut("zone_maps").and_then(|v| v.as_array_mut()) {
            for c in cards.iter_mut() {
                let f = c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let z = c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                if f == field && from_zones.contains(&z) {
                    if let Some(o) = c.as_object_mut() { o.insert("z".into(), Value::from(dest as u64)); }
                }
            }
        }
    }
}

/// Apply the pending pick to destination zone id `dest`, then clear the mode.
fn tz_apply_pick(snarl: &mut Snarl<NodeData>, node_id: NodeId, dest: usize) {
    let Some(pick) = snarl.get_node(node_id).and_then(|n| n.params.get("_tz_pick").cloned()) else { return; };
    let kind = pick.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    let field = pick.get("field").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    match kind {
        "migrate" => {
            let src = pick.get("src").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            if dest as u32 != src { tz_move_cards(snarl, node_id, field, &[src], dest as u32); }
        }
        "merge" => {
            let path: Vec<u8> = pick.get("path").and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_u64().map(|b| b as u8)).collect()).unwrap_or_default();
            let mut tree = tz_field_tree(snarl, node_id, field);
            if let Some((kept, removed)) = tree.remove_split(&path, None) {
                tz_set_field_tree(snarl, node_id, field, &tree);
                // Tapping one of the merged-away zones means "the survivor".
                let dest = if removed.contains(&(dest as u32)) { kept } else { dest as u32 };
                tz_move_cards(snarl, node_id, field, &removed, dest);
            }
        }
        _ => {}
    }
    tz_cancel_pick(snarl, node_id);
}

/// While picking, draw a banner over the pad prompting the user to choose a zone,
/// with a Cancel. Returns nothing; the zone click/tap handlers do the applying.
fn tz_pick_banner(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>,
    painter: &egui::Painter, rect: egui::Rect, accent: egui::Color32)
{
    let Some(kind) = tz_pick_kind(snarl, node_id) else { return; };
    painter.rect_stroke(rect, 4.0, egui::Stroke::new(2.0, accent), egui::epaint::StrokeKind::Inside);
    let msg = if kind == "migrate" { "Tap a zone to MOVE the mappings there" }
              else { "Tap a zone to INHERIT the mappings, then merge" };
    let br = egui::Rect::from_center_size(egui::pos2(rect.center().x, rect.top() + 14.0),
        egui::vec2(rect.width().min(300.0), 22.0));
    painter.rect_filled(br, 4.0, egui::Color32::from_black_alpha(180));
    painter.text(br.center(), egui::Align2::CENTER_CENTER, msg,
        egui::FontId::proportional(12.0), accent);
    let cancel = egui::Rect::from_min_size(egui::pos2(rect.right() - 60.0, rect.bottom() - 24.0), egui::vec2(52.0, 18.0));
    if ui.interact(cancel, ui.id().with((node_id, "tzpickcancel")), egui::Sense::click()).clicked() {
        tz_cancel_pick(snarl, node_id);
    }
    painter.rect_filled(cancel, 3.0, egui::Color32::from_black_alpha(180));
    painter.text(cancel.center(), egui::Align2::CENTER_CENTER, "Cancel",
        egui::FontId::proportional(11.0), egui::Color32::WHITE);
    ui.ctx().request_repaint();
}

/// Subdivide the zone under unit point `(ux, uy)` at its own centre along `axis`.
/// `new_low` puts the new EMPTY cell on the low side (left/top) so a "+" on a
/// zone's left/top edge adds the empty cell there and pushes the mapping the
/// other way.
pub(crate) fn tz_subdivide_at(snarl: &mut Snarl<NodeData>, node_id: NodeId, field: usize,
    ux: f32, uy: f32, axis: flexinput_core::touchzones::Axis, new_low: bool)
{
    use flexinput_core::touchzones::Axis;
    let mut tree = tz_field_tree(snarl, node_id, field);
    let (id, _, _) = tree.locate(ux, uy);
    let center = tree.zone_rect(id).map(|[x0, y0, x1, y1]| match axis {
        Axis::V => (x0 + x1) * 0.5,
        Axis::H => (y0 + y1) * 0.5,
    }).unwrap_or(0.5);
    if tree.subdivide_side(id, axis, center, new_low).is_some() {
        tz_set_field_tree(snarl, node_id, field, &tree);
    }
}

/// Tree version of the hover-revealed +/- overlay (mapping mode): a "−" on each
/// divider removes/merges it (raising the mapped-zone confirm popup when needed),
/// and — for the zone under the cursor — a "+" on the NEAREST edge splits that
/// zone, adding the new empty cell on THAT side (so the "+" you reach for is the
/// side the cell appears). The "+" tracks each zone's edges, not just the pad
/// border.
#[allow(clippy::too_many_arguments)]
fn tz_tree_line_overlay(
    node_id: NodeId,
    field: usize,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    painter: &egui::Painter,
    rect: egui::Rect,
    accent: egui::Color32,
    visuals: &egui::Visuals,
) {
    use flexinput_core::touchzones::Axis;
    let tree = tz_field_tree(snarl, node_id, field);
    let to_x = |u: f32| rect.left() + u * rect.width();
    let to_y = |u: f32| rect.top() + u * rect.height();
    let edge = 28.0;     // edge-proximity threshold (px) that reveals the "+"
    let inset = 12.0;    // "+" inset from the zone edge so it sits inside the cell
    let from_global = ui.ctx().layer_transform_to_global(ui.layer_id())
        .unwrap_or(egui::emath::TSTransform::IDENTITY)
        .inverse();
    let ptr = ui.input(|i| i.pointer.hover_pos()).map(|p| from_global * p);
    // (ux, uy, axis, new_low)
    let mut sub: Option<(f32, f32, Axis, bool)> = None;
    let mut rem: Option<Vec<u8>> = None;
    let divs = tree.dividers();

    // Nearest divider under the pointer (within a few px of its line) → the "−"
    // shows there dynamically, like the "+", instead of one marker per divider.
    let near_div: Option<usize> = ptr.filter(|p| rect.contains(*p)).and_then(|p| {
        divs.iter().enumerate().filter_map(|(di, div)| {
            let (d, on_span) = match div.axis {
                Axis::V => ((p.x - to_x(div.pos)).abs(),
                    p.y >= to_y(div.span_lo) - 6.0 && p.y <= to_y(div.span_hi) + 6.0),
                Axis::H => ((p.y - to_y(div.pos)).abs(),
                    p.x >= to_x(div.span_lo) - 6.0 && p.x <= to_x(div.span_hi) + 6.0),
            };
            (on_span && d <= 10.0).then_some((di, d))
        }).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(di, _)| di)
    });

    if let Some(di) = near_div {
        // "−" on the hovered divider → remove/merge.
        let div = &divs[di];
        let mid = (div.span_lo + div.span_hi) * 0.5;
        let c = match div.axis {
            Axis::V => egui::pos2(to_x(div.pos), to_y(mid)),
            Axis::H => egui::pos2(to_x(mid), to_y(div.pos)),
        };
        if tz_mini_button(ui, painter, ui.id().with((node_id, "tztm", field, di)),
            c, "−", accent, visuals) { rem = Some(div.path.clone()); }
    } else if let Some(p) = ptr.filter(|p| rect.contains(*p)) {
        // "+" on the hovered zone's nearest edge.
        for (id, [x0, y0, x1, y1]) in tree.zones() {
            let zr = egui::Rect::from_min_max(egui::pos2(to_x(x0), to_y(y0)), egui::pos2(to_x(x1), to_y(y1)));
            if !zr.contains(p) { continue; }
            let (dl, dr, dt, db) = (p.x - zr.left(), zr.right() - p.x, p.y - zr.top(), zr.bottom() - p.y);
            let m = dl.min(dr).min(dt).min(db);
            if m <= edge {
                let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
                let (pos, axis, new_low) = if m == dl {
                    (egui::pos2(zr.left() + inset, zr.center().y), Axis::V, true)
                } else if m == dr {
                    (egui::pos2(zr.right() - inset, zr.center().y), Axis::V, false)
                } else if m == dt {
                    (egui::pos2(zr.center().x, zr.top() + inset), Axis::H, true)
                } else {
                    (egui::pos2(zr.center().x, zr.bottom() - inset), Axis::H, false)
                };
                if tz_mini_button(ui, painter, ui.id().with((node_id, "tztp", field, id)),
                    pos, "+", accent, visuals) { sub = Some((cx, cy, axis, new_low)); }
            }
            let _ = id;
            break; // only the hovered zone
        }
    }

    if let Some((ux, uy, axis, new_low)) = sub {
        tz_subdivide_at(snarl, node_id, field, ux, uy, axis, new_low);
    }
    if let Some(path) = rem { tz_request_or_apply_merge(snarl, node_id, field, &tree, &path); }
}

/// The centred square used for a zone's analog viz (response-curve graph when
/// idle, vectorscope when active) — shared by the painter and the interactive
/// curve editor so their geometry matches exactly.
fn tz_zone_scope_rect(zr: egui::Rect) -> egui::Rect {
    let sz = (zr.width().min(zr.height()) * 0.62).clamp(20.0, 64.0);
    egui::Rect::from_center_size(zr.center(), egui::vec2(sz, sz))
}

/// The response-curve control points for a zone's analog card (over the 0..1
/// deflection magnitude). Defaults to linear when none stored.
pub(crate) fn tz_zone_curve(zone_maps: &[Value], field: usize, idx: usize) -> Vec<[f32; 2]> {
    for c in zone_maps.iter().filter(|c|
        c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) == field as u64
            && c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) == idx as u64)
    {
        if let Some(arr) = c.get("curve").and_then(|v| v.as_array()) {
            let pts: Vec<[f32; 2]> = arr.iter().filter_map(|p| {
                let q = p.as_array()?;
                Some([q.first()?.as_f64()? as f32, q.get(1)?.as_f64()? as f32])
            }).collect();
            if pts.len() >= 2 { return pts; }
        }
    }
    vec![[0.0, 0.0], [1.0, 1.0]]
}

/// True when a zone has at least one analog (mouse / stick) output card.
/// Whether a Touch Zones OUT pin receives the zone's analog deflection —
/// the gate for the response curve, the Relative-center slider, and the
/// on-zone vectorscope. Static analog bus pins always do; a macro pin does
/// when its port's declared type carries the deflection (Vec2 / Float / Any —
/// Bool ports only take the gate). Resolved through the per-frame macro
/// registry, so a dangling id counts as digital.
pub(crate) fn tz_out_pin_is_analog(pin: &str) -> bool {
    if matches!(pin,
        "mouse" | "mouse_x" | "mouse_y" | "left_stick" | "right_stick" | "scroll_x" | "scroll_y")
    {
        return true;
    }
    if flexinput_core::macros::parse_macro_pin(pin).is_some() {
        return crate::macro_icons::registry_entry(pin).is_some_and(|e| matches!(
            e.signal_type,
            SignalType::Vec2 | SignalType::Float | SignalType::Any
        ));
    }
    false
}

pub(crate) fn tz_zone_is_analog(zone_maps: &[Value], field: usize, idx: usize) -> bool {
    zone_maps.iter().any(|c|
        c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) == field as u64
            && c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) == idx as u64
            && c.get("out").and_then(|v| v.as_array())
                .map(|a| a.iter().any(|p| p.as_str().map(tz_out_pin_is_analog).unwrap_or(false)))
                .unwrap_or(false))
}

/// Store `pts` as the response `curve` on the first analog card of (field, zone).
pub(crate) fn tz_set_zone_curve(snarl: &mut Snarl<NodeData>, node_id: NodeId,
    field: usize, idx: usize, pts: &[[f32; 2]])
{
    let is_analog = tz_out_pin_is_analog;
    let Some(node) = snarl.get_node_mut(node_id) else { return };
    let Some(cards) = node.params.get_mut("zone_maps").and_then(|v| v.as_array_mut()) else { return };
    for c in cards.iter_mut() {
        let f = c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let z = c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        if f != field || z != idx { continue; }
        let analog = c.get("out").and_then(|v| v.as_array())
            .map(|a| a.iter().any(|p| p.as_str().map(is_analog).unwrap_or(false)))
            .unwrap_or(false);
        if !analog { continue; }
        if let Some(obj) = c.as_object_mut() {
            obj.insert("curve".to_string(), Value::Array(pts.iter()
                .map(|p| Value::Array(vec![Value::from(p[0] as f64), Value::from(p[1] as f64)]))
                .collect()));
        }
        return;
    }
}

/// The zone's adaptive-centre inner fraction (0..1): how much of the zone acts as
/// a RELATIVE centre for analog deflection (0 = absolute from zone centre, 1 =
/// wherever you touch is the centre). Stored on the analog card. Default 0.30.
fn tz_zone_adaptive(zone_maps: &[Value], field: usize, idx: usize) -> f32 {
    zone_maps.iter().filter(|c|
        c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) == field as u64 &&
        c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) == idx as u64)
        .find_map(|c| c.get("adaptive").and_then(|v| v.as_f64()))
        .map(|v| (v as f32).clamp(0.0, 1.0)).unwrap_or(0.30)
}

/// Store the adaptive-centre inner fraction on the first analog card of the zone.
fn tz_set_zone_adaptive(snarl: &mut Snarl<NodeData>, node_id: NodeId,
    field: usize, idx: usize, val: f32)
{
    let is_analog = tz_out_pin_is_analog;
    let Some(node) = snarl.get_node_mut(node_id) else { return };
    let Some(cards) = node.params.get_mut("zone_maps").and_then(|v| v.as_array_mut()) else { return };
    for c in cards.iter_mut() {
        let f = c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let z = c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        if f != field || z != idx { continue; }
        let analog = c.get("out").and_then(|v| v.as_array())
            .map(|a| a.iter().any(|p| p.as_str().map(is_analog).unwrap_or(false)))
            .unwrap_or(false);
        if !analog { continue; }
        if let Some(obj) = c.as_object_mut() {
            obj.insert("adaptive".to_string(), Value::from(val.clamp(0.0, 1.0) as f64));
        }
        return;
    }
}

/// True when zone `(field, zone)` is marked "hold" (a gesture starting there
/// stays bound to it even if the finger slides into a neighbouring zone).
pub(crate) fn tz_zone_held(snarl: &Snarl<NodeData>, node_id: NodeId, field: usize, zone: usize) -> bool {
    snarl.get_node(node_id)
        .and_then(|n| n.params.get("hold_zones").and_then(|v| v.as_array()))
        .map(|a| a.iter().any(|p| p.as_array().map(|q|
            q.first().and_then(|v| v.as_u64()) == Some(field as u64)
                && q.get(1).and_then(|v| v.as_u64()) == Some(zone as u64)).unwrap_or(false)))
        .unwrap_or(false)
}

/// Set/clear the "hold" flag for zone `(field, zone)` in the `hold_zones` param
/// (a list of `[field, zone]` pairs).
pub(crate) fn tz_set_zone_held(snarl: &mut Snarl<NodeData>, node_id: NodeId,
    field: usize, zone: usize, held: bool)
{
    let Some(node) = snarl.get_node_mut(node_id) else { return };
    let mut list: Vec<Value> = node.params.get("hold_zones")
        .and_then(|v| v.as_array()).cloned().unwrap_or_default();
    list.retain(|p| p.as_array().map(|q| !(
        q.first().and_then(|v| v.as_u64()) == Some(field as u64)
            && q.get(1).and_then(|v| v.as_u64()) == Some(zone as u64))).unwrap_or(true));
    if held {
        list.push(Value::Array(vec![Value::from(field as u64), Value::from(zone as u64)]));
    }
    node.params.insert("hold_zones".into(), Value::Array(list));
}

/// A full-size interactive response-curve editor for a zone's analog output,
/// shown in the CARD ROW (below the pad) where there's room — the tiny on-zone
/// graph is a read-only preview. Behaves like the Response Curve module's graph:
/// drag points (endpoints move in Y, interior in X+Y), double-click empty space
/// to add a point, right-click a point to remove it. X = deflection magnitude
/// 0..1, Y = output 0..1. Writes the first analog card's `curve`; `live_mag`
/// draws the current input→output dot when the zone is active.
/// Default (identity) card curve — a card carrying exactly this stores no
/// `curve` param at all, keeping saved patches lean.
fn identity_curve() -> Vec<[f32; 2]> {
    vec![[0.0, 0.0], [1.0, 1.0]]
}

/// Normalize points pasted/loaded from elsewhere so the card sampler's
/// invariants hold: clamp to the 0..1 unit box, sort by x, pin the endpoints
/// to x=0 / x=1, and guarantee at least two points.
fn sanitize_card_curve(pts: &mut Vec<[f32; 2]>) {
    for p in pts.iter_mut() {
        p[0] = p[0].clamp(0.0, 1.0);
        p[1] = p[1].clamp(0.0, 1.0);
    }
    pts.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap_or(std::cmp::Ordering::Equal));
    if pts.len() < 2 {
        *pts = identity_curve();
        return;
    }
    pts.first_mut().unwrap()[0] = 0.0;
    pts.last_mut().unwrap()[0] = 1.0;
}

/// Save a card curve as a `.fxc` file — same format the Response Curve
/// module writes, so files are interchangeable (card curves live in the
/// 0..1 magnitude space, hence the 0-based ranges).
fn card_curve_save(pts: &[[f32; 2]]) {
    let cf = CurveFile {
        points: pts.iter().map(|p| [p[0] as f64, p[1] as f64]).collect(),
        biases: vec![],
        absolute: true,
        in_min: 0.0,
        in_max: 1.0,
        out_min: 0.0,
        out_max: 1.0,
        grid_x: 4,
        grid_y: 4,
        snap: false,
        scale_t: 0.0,
        trail_ms: 300,
        show_scaled_grid: false,
        show_grid_labels: false,
    };
    if let Some(path) = rfd::FileDialog::new()
        .add_filter("FlexInput Curve", &["fxc"])
        .set_file_name("curve.fxc")
        .save_file()
    {
        if let Ok(json) = serde_json::to_string_pretty(&cf) {
            let _ = std::fs::write(path, json);
        }
    }
}

/// Load ONLY the points from a `.fxc` file (module settings in the file are
/// ignored, mirroring the module-side "Load only the curve" semantics).
fn card_curve_load() -> Option<Vec<[f32; 2]>> {
    let path = rfd::FileDialog::new()
        .add_filter("FlexInput Curve", &["fxc"])
        .pick_file()?;
    let text = std::fs::read_to_string(path).ok()?;
    let cf: CurveFile = serde_json::from_str(&text).ok()?;
    let mut pts: Vec<[f32; 2]> = cf.points.iter().map(|p| [p[0] as f32, p[1] as f32]).collect();
    sanitize_card_curve(&mut pts);
    Some(pts)
}

/// Per-mapping-card response-curve editor. Drag points, double-click to add,
/// right-click a point to remove; right-click the background for the shared
/// curve menu (Reset / Copy / Paste / Save… / Load… — same clipboard and
/// `.fxc` files as the Response Curve module).
///
/// `threshold`: `Some(slot)` shows the manual-activation controls — a
/// HORIZONTAL line over the curve's OUTPUT plus a checkbox row. While set,
/// a digital binding is held whenever the shaped magnitude sits on/above the
/// line and releases the moment it dips below (see the matching engine
/// logic in `eval.rs`). Drag the line vertically to tune it.
///
/// `nav_uid`: publishes the gamepad-nav graph geometry / selection rings on
/// the channels the TZ zone editor used (`gp_nav_curve_geom` etc.) — passed
/// only by the Touch Zones first-analog card so controller curve editing
/// keeps working; Remapper/Lean card curves are mouse-edited for now.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
fn mapping_curve_editor(
    ui: &mut egui::Ui,
    id_salt: egui::Id,
    pts: &mut Vec<[f32; 2]>,
    threshold: Option<&mut Option<f32>>,
    live_mag: Option<f32>,
    accent: egui::Color32,
    visuals: &egui::Visuals,
    nav_uid: Option<usize>,
    // Gamepad-focused curve field on this card: Some(6) = threshold (highlight the
    // line + enable row so the user sees what up/down / South act on).
    nav_curve_field: Option<u64>,
) -> bool {
    let mut changed = false;
    let w = ui.available_width().clamp(140.0, 360.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 104.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, visuals.extreme_bg_color);
    painter.rect_stroke(rect, 3.0, egui::Stroke::new(1.0, visuals.weak_text_color()), egui::StrokeKind::Inside);
    let g = rect.shrink(8.0);
    let to = |x: f32, y: f32| egui::pos2(
        g.left() + x.clamp(0.0, 1.0) * g.width(),
        g.bottom() - y.clamp(0.0, 1.0) * g.height());
    let unto = |p: egui::Pos2| (
        ((p.x - g.left()) / g.width()).clamp(0.0, 1.0),
        ((g.bottom() - p.y) / g.height()).clamp(0.0, 1.0));

    // ── Gamepad-nav integration (nav_uid channels only) ───────────────────
    // Publish the graph geometry (GLOBAL space, 0..1 both axes) so the shared
    // curve driver (`nav_drive_curve_dots`/`_dot`) can add/move/delete dots here
    // exactly like the Response Curve module's graph. Read back the selected dot
    // (while entered) + a focus flag (while the curve row is highlighted but not
    // yet entered) to draw the matching rings.
    let pass = ui.ctx().cumulative_pass_nr();
    let mut nav_sel_dot: Option<usize> = None;
    let mut nav_editing = false;
    if let Some(uid) = nav_uid {
        let to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
            .unwrap_or(egui::emath::TSTransform::IDENTITY);
        ui.ctx().data_mut(|d| d.insert_temp(
            egui::Id::new(("gp_nav_curve_geom", uid)),
            (pass, to_global * g, 0.0f32, 1.0f32, 0.0f32, 1.0f32)));
        let nav_sel: Option<(u64, usize, bool)> = ui.ctx()
            .data(|d| d.get_temp(egui::Id::new(("gp_nav_curve_sel", uid))));
        let nav_sel = nav_sel.filter(|(p, _, _)| pass.saturating_sub(*p) <= 1);
        nav_sel_dot = nav_sel.map(|(_, i, _)| i);
        nav_editing = nav_sel.map(|(_, _, e)| e).unwrap_or(false);
        let nav_focused: bool = ui.ctx()
            .data(|d| d.get_temp::<u64>(egui::Id::new(("gp_nav_tz_curve_focus", uid))))
            .map(|p| pass.saturating_sub(p) <= 1).unwrap_or(false);
        if nav_focused || nav_sel_dot.is_some() {
            painter.rect_stroke(rect, 3.0, egui::Stroke::new(1.5, accent), egui::StrokeKind::Inside);
        }
        // Gamepad users can't scroll manually, and the whole-module wrapper uses a
        // MANUAL scroll offset (not an egui ScrollArea, so `scroll_to_rect` is a
        // no-op). Publish a body-space scroll delta on the same channel the cards
        // use (`gp_nav_remap_scroll`) so the wrapper keeps the focused element in
        // view — the graph while editing, and the threshold enable-row (which sits
        // ~30px BELOW the graph) while the threshold field is focused.
        if nav_focused || nav_sel_dot.is_some() || nav_curve_field.is_some() {
            let extra = if nav_curve_field == Some(6) { 32.0 } else { 0.0 };
            let target = egui::Rect::from_min_max(rect.min, rect.max + egui::vec2(0.0, extra));
            let clip = ui.clip_rect();
            let mut need = 0.0f32;
            if target.top() < clip.top() + 4.0 {
                need = target.top() - (clip.top() + 4.0);
            } else if target.bottom() > clip.bottom() - 4.0 {
                need = target.bottom() - (clip.bottom() - 4.0);
            }
            if need.abs() > 1.0 {
                ui.ctx().data_mut(|d| d.insert_temp(
                    egui::Id::new(("gp_nav_remap_scroll", uid)), (pass, need)));
                request_repaint_throttled(ui.ctx());
            }
        }
    }

    // Grid + identity reference.
    let grid = visuals.weak_text_color().gamma_multiply(0.35);
    for k in 1..4 {
        let t = k as f32 / 4.0;
        painter.line_segment([to(t, 0.0), to(t, 1.0)], egui::Stroke::new(0.5, grid));
        painter.line_segment([to(0.0, t), to(1.0, t)], egui::Stroke::new(0.5, grid));
    }
    painter.line_segment([to(0.0, 0.0), to(1.0, 1.0)], egui::Stroke::new(0.5, grid));

    // Add-area first (bottom of z-order); handles after so they win their rects.
    // NOTE: `interact_pointer_pos()` is ALREADY in this UI's local space (even
    // inside the whole-module scale layer), so it maps directly through `unto` —
    // do NOT apply the layer transform here (that double-transforms and scatters
    // the points).
    let bg = ui.interact(g, id_salt.with("bg"), egui::Sense::click());
    // Threshold-line drag band — registered BEFORE the point handles so a
    // handle sitting on the line still wins its own 16px rect.
    let mut thr_val: Option<f32> = threshold.as_ref().and_then(|s| **s);
    if let Some(t) = thr_val {
        let ly = to(0.0, t).y;
        let band = egui::Rect::from_min_max(
            egui::pos2(g.left(), ly - 6.0), egui::pos2(g.right(), ly + 6.0));
        let tr = ui.interact(band, id_salt.with("thr"), egui::Sense::drag());
        if tr.hovered() || tr.dragged() {
            tr.clone().on_hover_cursor(egui::CursorIcon::ResizeVertical);
        }
        if tr.dragged() {
            if let Some(p) = tr.interact_pointer_pos() {
                let (_, ny) = unto(p);
                thr_val = Some(ny.clamp(0.01, 1.0));
                changed = true;
            }
        }
    }
    let mut remove: Option<usize> = None;
    let n = pts.len();
    for i in 0..n {
        let hp = to(pts[i][0], pts[i][1]);
        let r = ui.interact(egui::Rect::from_center_size(hp, egui::vec2(16.0, 16.0)),
            id_salt.with(("pt", i)), egui::Sense::click_and_drag());
        let hot = r.hovered() || r.dragged();
        if hot { r.clone().on_hover_cursor(egui::CursorIcon::Grab); }
        if r.dragged() {
            if let Some(p) = r.interact_pointer_pos() {
                let (nx, ny) = unto(p);
                // Interior points stay ordered; guard the clamp so a crowded pair
                // (lo > hi) can't panic — pin to the midpoint of the neighbours.
                let x = if i == 0 { 0.0 } else if i + 1 == n { 1.0 } else {
                    let lo = pts[i - 1][0] + 0.03;
                    let hi = pts[i + 1][0] - 0.03;
                    if lo <= hi { nx.clamp(lo, hi) } else { (pts[i - 1][0] + pts[i + 1][0]) * 0.5 }
                };
                pts[i] = [x, ny];
                changed = true;
            }
        }
        if r.secondary_clicked() && i != 0 && i + 1 != n { remove = Some(i); }
    }
    if let Some(i) = remove {
        pts.remove(i);
        changed = true;
    } else if bg.double_clicked() {
        if let Some(p) = bg.interact_pointer_pos() {
            let (nx, ny) = unto(p);
            let at = pts.iter().position(|q| q[0] > nx).unwrap_or(pts.len());
            if at > 0 && at < pts.len() {
                pts.insert(at, [nx.clamp(0.02, 0.98), ny]);
                changed = true;
            }
        }
    }

    // Threshold line (under the curve): dashed, warm color, right-edge knob.
    // Thickens + gains an accent halo while the gamepad has the threshold field
    // focused (field 6) so the user sees it's the drag target.
    let thr_focused = nav_curve_field == Some(6);
    if let Some(t) = thr_val {
        let ly = to(0.0, t).y;
        let thr_col = egui::Color32::from_rgb(255, 170, 60);
        painter.add(egui::Shape::dashed_line(
            &[egui::pos2(g.left(), ly), egui::pos2(g.right(), ly)],
            egui::Stroke::new(if thr_focused { 2.2 } else { 1.2 }, thr_col), 5.0, 4.0));
        let knob = egui::pos2(g.right() - 4.0, ly);
        painter.circle_filled(knob, if thr_focused { 4.5 } else { 3.5 }, thr_col);
        if thr_focused {
            painter.circle_stroke(knob, 6.5, egui::Stroke::new(1.5, accent));
        }
    } else if thr_focused {
        // Threshold OFF but focused → hint the line at the default so South-to-
        // enable has an anchor.
        let ly = to(0.0, 0.5).y;
        painter.add(egui::Shape::dashed_line(
            &[egui::pos2(g.left(), ly), egui::pos2(g.right(), ly)],
            egui::Stroke::new(1.0, accent.gamma_multiply(0.6)), 4.0, 4.0));
    }

    // Curve polyline (over the possibly-just-edited points) + handles.
    for wnd in pts.windows(2) {
        painter.line_segment([to(wnd[0][0], wnd[0][1]), to(wnd[1][0], wnd[1][1])],
            egui::Stroke::new(1.8, accent));
    }
    for (i, p) in pts.iter().enumerate() {
        painter.circle_filled(to(p[0], p[1]), 4.0, accent);
        painter.circle_stroke(to(p[0], p[1]), 4.0, egui::Stroke::new(1.0, visuals.extreme_bg_color));
        // Gamepad-selected dot: accent ring (thicker while being moved/entered).
        if nav_sel_dot == Some(i) {
            painter.circle_stroke(to(p[0], p[1]), if nav_editing { 8.0 } else { 6.5 },
                egui::Stroke::new(2.0, accent));
        }
    }
    // Live input→output dot; green while it would hold a thresholded binding.
    if let Some(m) = live_mag {
        let m = m.clamp(0.0, 1.0);
        let y = flexinput_engine::sample_curve(pts, m, &[]).clamp(0.0, 1.0);
        let on = thr_val.map(|t| y >= t).unwrap_or(false);
        let col = if on { egui::Color32::from_rgb(110, 230, 130) }
                  else  { egui::Color32::from_rgb(90, 200, 255) };
        painter.circle_filled(to(m, y), 3.0, col);
        request_repaint_throttled(ui.ctx());
    }

    // Shared curve menu — same clipboard and .fxc files as the Response
    // Curve module, so shapes travel freely between modules and cards.
    bg.context_menu(|ui| {
        if ui.button("Reset").clicked() {
            *pts = identity_curve();
            changed = true;
            ui.close();
        }
        ui.separator();
        if ui.button("Copy").clicked() {
            curve_clipboard_set(ui.ctx(), CurveClip { points: pts.clone(), biases: vec![] });
            ui.close();
        }
        let has_clip = curve_clipboard_get(ui.ctx()).is_some();
        if ui.add_enabled(has_clip, egui::Button::new("Paste")).clicked() {
            if let Some(clip) = curve_clipboard_get(ui.ctx()) {
                *pts = clip.points;
                sanitize_card_curve(pts);
                changed = true;
            }
            ui.close();
        }
        ui.separator();
        if ui.button("Save…").clicked() {
            card_curve_save(pts);
            ui.close();
        }
        if ui.button("Load…").clicked() {
            if let Some(p) = card_curve_load() {
                *pts = p;
                changed = true;
            }
            ui.close();
        }
    });

    // Threshold enable + readout row (shown only where a manual activation
    // point is meaningful — the caller decides via `threshold`).
    if let Some(slot) = threshold {
        let mut on = thr_val.is_some();
        let row = ui.horizontal(|ui| {
            if ui.checkbox(&mut on, egui::RichText::new("Activation threshold").small())
                .on_hover_text("Manual activation point for digital outputs: the binding is held while the curve's output sits on/above the orange line and releases the moment it dips below. Off = default behaviour (freq-modulated taps for analog mode, built-in stick threshold otherwise). Drag the line on the graph to tune it.")
                .changed()
            {
                thr_val = if on { Some(0.5) } else { None };
                changed = true;
            }
            if let Some(t) = thr_val.as_mut() {
                let mut pct = *t * 100.0;
                if ui.add(egui::DragValue::new(&mut pct).speed(1.0).range(1.0..=100.0)
                    .suffix("%").fixed_decimals(0)).changed()
                {
                    *t = pct / 100.0;
                    changed = true;
                }
            }
        });
        // Accent ring around the enable row while the gamepad has it focused, so
        // the "toggle / adjust threshold" target is unmistakable.
        if thr_focused {
            ui.painter().rect_stroke(row.response.rect.expand(2.0), 3.0,
                egui::Stroke::new(1.5, accent), egui::StrokeKind::Outside);
        }
        *slot = thr_val;
    }

    changed
}

/// `Some(node.0)` when card `idx` (scope) is the gamepad-nav ENTERED card, so its
/// curve section should publish geometry to the shared curve nav channel and ring
/// while being dot-edited. Only the entered card publishes (one per node), so the
/// per-node geometry channel never collides across cards.
fn curve_nav_uid(ctx: &egui::Context, node_id: NodeId, scope: &str, idx: usize) -> Option<usize> {
    let pass = ctx.cumulative_pass_nr();
    ctx.data(|d| d.get_temp::<(u64, usize, bool)>(
            egui::Id::new(("gp_nav_remap_card", node_id.0, scope))))
        .filter(|(p, sel, ent)| *ent && *sel == idx && pass.saturating_sub(*p) <= 1)
        .map(|_| node_id.0)
}

/// Slim expander strip + (when open) the curve editor for one mapping card.
/// Reads/writes `curve` + `threshold` on the card's `working` map; returns
/// true when the card changed. Identity curve + no threshold stores nothing,
/// so untouched cards stay lean and take the engine's default paths.
///
/// `nav_uid`: forwarded to the editor AND force-opens the section while the
/// gamepad-nav curve row is focused/entered (the Touch Zones controller flow
/// needs the graph visible to ring it).
#[allow(clippy::too_many_arguments)]
fn mapping_card_curve_section(
    ui: &mut egui::Ui,
    node_id: NodeId,
    scope: &str,
    idx: usize,
    working: &mut serde_json::Map<String, Value>,
    show_threshold: bool,
    live_mag: Option<f32>,
    nav_uid: Option<usize>,
) -> bool {
    // Render as a flush continuation of the header card above: zero the inter-item
    // gap (on THIS child ui only) and wrap the content in a frame with the card's
    // body fill + black border, bottom corners rounded and top square — so the
    // card (which squares its bottom when a section follows) and this section
    // share ONE continuous border and read as a single mapping card.
    ui.spacing_mut().item_spacing.y = 0.0;
    const C_BODY_BG: egui::Color32 = egui::Color32::from_rgb(0x3C, 0x3C, 0x3C);
    const C_BORDER:  egui::Color32 = egui::Color32::BLACK;
    let accent = ui.visuals().selection.stroke.color;
    // Gamepad selection/focus. `selected_here` = this card is the nav selection
    // (used to extend the card GLOW around this section so header + section share
    // ONE ring, drawn by the nav driver — not a second border here). `nav_field` =
    // the focused curve field on the ENTERED card (4 toggle / 5 graph / 6
    // threshold), driving the per-element highlights inside.
    let pass = ui.ctx().cumulative_pass_nr();
    let card_sel: Option<(usize, bool)> = ui.ctx()
        .data(|d| d.get_temp::<(u64, usize, bool)>(
            egui::Id::new(("gp_nav_remap_card", node_id.0, scope))))
        .filter(|(p, _, _)| pass.saturating_sub(*p) <= 1)
        .map(|(_, sel, ent)| (sel, ent));
    let selected_here = card_sel.map(|(sel, _)| sel == idx).unwrap_or(false);
    let entered_here = card_sel.map(|(sel, ent)| ent && sel == idx).unwrap_or(false);
    let nav_field: Option<u64> = if !entered_here { None } else {
        ui.ctx().data(|d| d.get_temp::<(u64, u64)>(
                egui::Id::new(("gp_nav_remap_card_field", node_id.0, scope))))
            .filter(|(p, f)| *f >= 4 && pass.saturating_sub(*p) <= 1)
            .map(|(_, f)| f)
    };
    let out = egui::Frame::default()
        .fill(C_BODY_BG)
        .stroke(egui::Stroke::new(1.0, C_BORDER))
        .corner_radius(egui::CornerRadius { nw: 0, ne: 0, sw: 5, se: 5 })
        .inner_margin(egui::Margin { left: 0, right: 0, top: 1, bottom: 2 })
        .show(ui, |ui| {
    let open_id = egui::Id::new(("card_curve_open", node_id.0, scope.to_string(), idx));
    let mut open = ui.ctx().data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
    if let Some(uid) = nav_uid {
        let pass = ui.ctx().cumulative_pass_nr();
        let focus = ui.ctx()
            .data(|d| d.get_temp::<u64>(egui::Id::new(("gp_nav_tz_curve_focus", uid))))
            .map(|p| pass.saturating_sub(p) <= 1).unwrap_or(false);
        let entered = ui.ctx()
            .data(|d| d.get_temp::<(u64, usize, bool)>(egui::Id::new(("gp_nav_curve_sel", uid))))
            .map(|(p, _, _)| pass.saturating_sub(p) <= 1).unwrap_or(false);
        if focus || entered { open = true; }
    }

    let has_custom = working.contains_key("curve") || working.contains_key("threshold");
    let (row, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 15.0), egui::Sense::click());
    {
        let painter = ui.painter_at(row);
        let col = if nav_field == Some(4) {
            accent
        } else if resp.hovered() {
            ui.visuals().text_color()
        } else {
            ui.visuals().weak_text_color()
        };
        let tri = if open { "⏷" } else { "⏵" };
        painter.text(row.left_center() + egui::vec2(6.0, 0.0), egui::Align2::LEFT_CENTER,
            format!("{tri} Response curve"), egui::FontId::proportional(10.5), col);
        // Accent dot: this card carries a custom curve and/or threshold.
        if has_custom {
            painter.circle_filled(row.left_center() + egui::vec2(96.0, 0.0), 2.5, accent);
        }
    }
    if resp.clicked() { open = !open; }
    ui.ctx().data_mut(|d| d.insert_temp(open_id, open));
    if !open {
        return false;
    }

    let mut pts: Vec<[f32; 2]> = working.get("curve").and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|p| {
            let q = p.as_array()?;
            Some([q.first()?.as_f64()? as f32, q.get(1)?.as_f64()? as f32])
        }).collect())
        .unwrap_or_default();
    if pts.len() < 2 {
        pts = identity_curve();
    }
    let mut thr: Option<f32> = working.get("threshold").and_then(|v| v.as_f64()).map(|v| v as f32);

    let vis = ui.visuals().clone();
    let changed = mapping_curve_editor(
        ui, open_id.with("ed"), &mut pts,
        if show_threshold { Some(&mut thr) } else { None },
        live_mag, accent, &vis, nav_uid, nav_field,
    );
    if changed {
        if pts == identity_curve() {
            working.remove("curve");
        } else {
            working.insert("curve".into(),
                Value::Array(pts.iter().map(|p| serde_json::json!([p[0], p[1]])).collect()));
        }
        match thr.and_then(|t| Number::from_f64(t as f64)) {
            Some(t) => { working.insert("threshold".into(), Value::Number(t)); }
            None => { working.remove("threshold"); }
        }
    }
    changed
    });
    // Publish this section's GLOBAL rect for the selected card so the nav driver's
    // card glow expands to wrap header + section as ONE ring (instead of a separate
    // border here). Keyed per node+scope; only the selected card publishes.
    if selected_here {
        let to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
            .unwrap_or(egui::emath::TSTransform::IDENTITY);
        ui.ctx().data_mut(|d| d.insert_temp(
            egui::Id::new(("gp_nav_card_section_rect", node_id.0, scope.to_string())),
            (pass, to_global * out.response.rect)));
    }
    out.inner
}

/// Largest live analog-input magnitude across a mapping's in pins, read from
/// the upstream device's live signals — drives the editor's preview dot.
fn live_analog_in_mag(
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    dev: Option<&str>,
    in_pins: &[String],
) -> Option<f32> {
    let dev = dev?;
    let mut best: Option<f32> = None;
    for p in in_pins {
        let v = if let Some((axis, sign)) = flexinput_engine::analog_axis_for_cardinal(p) {
            live_signals.get(&(dev.to_string(), axis.to_string()))
                .map(|s| s.as_float())
                .or_else(|| {
                    // Vec2 fallback: some sources publish the stick as a Vec2
                    // ("left_stick") rather than separate axis floats.
                    let (vpin, comp) = axis.rsplit_once('_')?;
                    match live_signals.get(&(dev.to_string(), vpin.to_string()))? {
                        Signal::Vec2(v) => Some(if comp == "x" { v.x } else { v.y }),
                        _ => None,
                    }
                })
                .map(|r| (r * sign).clamp(0.0, 1.0))
        } else if matches!(p.as_str(), "left_trigger" | "right_trigger") {
            live_signals.get(&(dev.to_string(), p.to_string()))
                .map(|s| s.as_float().clamp(0.0, 1.0))
        } else {
            None
        };
        if let Some(v) = v {
            best = Some(best.map_or(v, |b: f32| b.max(v)));
        }
    }
    best
}

/// Paint one zone's MAPPING content (mapping mode): the output icon(s) of the
/// zone's cards, or — for an analog output (mouse / stick) — the response-curve
/// graph (idle) that swaps to a live vectorscope while the zone is active, with
/// the output icon in the corner. Empty zones show a faint index. The zone's
/// active highlight (drawn by the caller) is the "lit when activated" cue.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
fn tz_paint_zone_mapping(
    painter: &egui::Painter,
    ctx: &egui::Context,
    node_id: NodeId,
    zr: egui::Rect,
    field: usize,
    idx: usize,
    zone_maps: &[Value],
    skin: super::remapper_icons::Skin,
    deflect: Option<(f32, f32)>,
    accent: egui::Color32,
    visuals: &egui::Visuals,
    // Virtual Menu per-zone overrides (empty for Touch Zones). A zone icon
    // override REPLACES the destination icons; a zone name reserves a bottom
    // band so the icon lifts clear of it (the label text is drawn by the
    // menu's own post-pass, which knows the same band).
    zone_meta: &std::collections::HashMap<u32, super::menu_body::ZoneMeta>,
) {
    // Icon override (menu only): drawn instead of the mapping destination
    // icons, lifted above the zone-name band when the zone is also named.
    if let Some(m) = zone_meta.get(&(idx as u32)) {
        if !m.icon.is_empty() || !m.svg.is_empty() {
            let band = if m.label.is_empty() { 0.0 } else { (zr.height() * 0.22).clamp(10.0, 15.0) + 3.0 };
            let region = egui::Rect::from_min_max(zr.min, egui::pos2(zr.max.x, zr.max.y - band));
            let ic = (region.height() * 0.6).clamp(14.0, 34.0).min(region.width() - 6.0).max(10.0);
            if let Some(tex) = crate::macro_icons::macro_port_icon_texture(ctx, &m.icon, &m.svg, ic) {
                painter.image(
                    tex.id(),
                    egui::Rect::from_center_size(region.center(), egui::vec2(ic, ic)),
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
            return;
        }
    }
    let is_analog = tz_out_pin_is_analog;
    // Output pins across every card bound to this (field, zone), split by kind so
    // a click's button icon shows ALONGSIDE the analog vectorscope (not hidden by
    // it). Order preserved (first-seen) so it matches the card list.
    let mut analog_pins: Vec<String> = Vec::new();
    let mut digital_pins: Vec<String> = Vec::new();
    for c in zone_maps.iter().filter(|c|
        c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) == field as u64
            && c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) == idx as u64)
    {
        for p in c.get("out").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|v| v.as_str()) {
            let bucket = if is_analog(p) { &mut analog_pins } else { &mut digital_pins };
            if !bucket.iter().any(|x| x == p) { bucket.push(p.to_string()); }
        }
    }

    if analog_pins.is_empty() && digital_pins.is_empty() {
        // Unmapped zone: faint index so it's still identifiable as a target.
        painter.text(zr.center(), egui::Align2::CENTER_CENTER, format!("{idx}"),
            egui::FontId::proportional(11.0), visuals.weak_text_color().gamma_multiply(0.5));
        return;
    }

    // Which output pins are actually FIRING this frame (per-trigger; computed in
    // tz_live_hits). Drives the per-icon activation glow — a click lights its
    // button independently of the analog deflection.
    let active_set: Vec<String> = ctx.data(|d| d
        .get_temp::<std::collections::HashMap<(usize, usize), Vec<String>>>(
            egui::Id::new(("tz_active_out", node_id.0))))
        .and_then(|mm| mm.get(&(field, idx)).cloned())
        .unwrap_or_default();
    let is_on = |p: &str| active_set.iter().any(|x| x == p);
    // Small icon in a rect with an optional activation glow behind it.
    let icon = |pos: egui::Pos2, ic: f32, pin: &str, on: bool| {
        if on {
            painter.rect_filled(egui::Rect::from_min_size(pos, egui::vec2(ic, ic)).expand(2.5),
                3.0, accent.gamma_multiply(0.55));
        }
        paint_chord_chip_to_rect(painter, ctx, pos, ic, pin, skin);
    };

    if !analog_pins.is_empty() {
        let bx = tz_zone_scope_rect(zr);
        painter.rect_filled(bx, 2.0, visuals.extreme_bg_color.gamma_multiply(0.7));
        let grid = visuals.weak_text_color().gamma_multiply(0.5);
        painter.rect_stroke(bx, 2.0, egui::Stroke::new(1.0, visuals.weak_text_color()), egui::StrokeKind::Inside);
        if let Some((dx, dy)) = deflect {
            // ACTIVE → vectorscope: crosshair + live dot.
            painter.line_segment([egui::pos2(bx.center().x, bx.top()), egui::pos2(bx.center().x, bx.bottom())],
                egui::Stroke::new(0.5, grid));
            painter.line_segment([egui::pos2(bx.left(), bx.center().y), egui::pos2(bx.right(), bx.center().y)],
                egui::Stroke::new(0.5, grid));
            let p = egui::pos2(
                bx.center().x + dx.clamp(-1.0, 1.0) * 0.5 * bx.width(),
                bx.center().y - dy.clamp(-1.0, 1.0) * 0.5 * bx.height()); // +Y up
            painter.line_segment([bx.center(), p], egui::Stroke::new(1.5, accent.gamma_multiply(0.6)));
            painter.circle_filled(p, 3.0, accent);
        } else {
            // IDLE → response curve over the 0..1 deflection magnitude.
            let pts = tz_zone_curve(zone_maps, field, idx);
            let to = |x: f32, y: f32| egui::pos2(
                bx.left() + x.clamp(0.0, 1.0) * bx.width(),
                bx.bottom() - y.clamp(0.0, 1.5) * bx.height());
            // Faint linear reference (identity).
            painter.line_segment([to(0.0, 0.0), to(1.0, 1.0)], egui::Stroke::new(0.5, grid));
            for w in pts.windows(2) {
                painter.line_segment([to(w[0][0], w[0][1]), to(w[1][0], w[1][1])],
                    egui::Stroke::new(1.5, accent));
            }
            for p in &pts {
                painter.circle_filled(to(p[0], p[1]), 2.0, accent);
            }
        }
        // Analog output icon in the scope's bottom-right corner (glows while it
        // drives — deflect present ⇒ a live hold-aware hit).
        if let Some(ap) = analog_pins.first() {
            let ic = (bx.width() * 0.42).clamp(12.0, 22.0);
            let pos = egui::pos2(bx.right() - ic - 1.0, bx.bottom() - ic - 1.0);
            icon(pos, ic, ap, deflect.is_some() || is_on(ap));
        }
        // Digital outputs (e.g. a touchpad-click button) share the zone: a small
        // row across the TOP, each lighting when its own trigger fires.
        if !digital_pins.is_empty() {
            let n = digital_pins.len();
            let ic = (zr.width() / (n as f32 + 0.5)).clamp(10.0, 18.0);
            let total_w = n as f32 * ic + (n as f32 - 1.0) * 2.0;
            let mut x = zr.center().x - total_w * 0.5;
            let y = zr.top() + 2.0;
            for p in &digital_pins {
                icon(egui::pos2(x, y), ic, p, is_on(p));
                x += ic + 2.0;
            }
        }
    } else {
        // Digital-only zone: icon(s) centred in a row, each lit by its own trigger.
        let n = digital_pins.len();
        let ic = (zr.height() * 0.46).clamp(14.0, 30.0).min(zr.width() / n.max(1) as f32 - 2.0).max(10.0);
        let total_w = n as f32 * ic + (n as f32 - 1.0) * 3.0;
        let mut x = zr.center().x - total_w * 0.5;
        for p in &digital_pins {
            let pos = egui::pos2(x, zr.center().y - ic * 0.5);
            // Fall back to the finger-active cue if the fine-grained set is absent.
            icon(pos, ic, p, is_on(p) || (active_set.is_empty() && deflect.is_some()));
            x += ic + 3.0;
        }
    }
}

/// Draw one field's pad into `rect`: background, zone cells + index labels, the
/// active-zone highlight, live finger dots, the frame, and draggable dividers
/// (line MOVING — never changes the zone count, so it's wiring-safe). Persists
/// any divider drag. Shared by the in-canvas body and the pinned widget;
/// `id_salt` keeps their interaction ids distinct.
#[allow(clippy::too_many_arguments)]
fn tz_draw_field(
    node_id: NodeId,
    field: usize,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    painter: &egui::Painter,
    rect: egui::Rect,
    col_edges: &[f32],
    row_edges: &[f32],
    zone_live: &std::collections::HashMap<(usize, usize), (f32, f32, bool)>,
    visuals: &egui::Visuals,
    accent: egui::Color32,
    main_override: Option<egui::Color32>,
    // `Some(a)` = "touched zones only": non-active zones (plate + content) paint
    // at opacity `a`, active zones stay full. `None` = normal full render.
    inactive_alpha: Option<f32>,
    id_salt: &'static str,
) {
    use flexinput_core::touchzones as tz;
    let to_x = |u: f32| rect.left() + u * rect.width();
    let to_y = |u: f32| rect.top() + u * rect.height();

    // Dimmed painter clone for non-active zone content in "touched zones only"
    // mode (`multiply_opacity` fades fills/text/icons alike). `None` → the real
    // painter, so the normal path is byte-for-byte unchanged.
    let dim = inactive_alpha.map(|a| {
        let mut p = painter.clone();
        p.set_opacity(a);
        p
    });

    // Optional `main_color` theming (menu + Touch Zones): tints the plate
    // (additively) and the frame. Absent → the plain themed look, unchanged.
    // `main_override` (per-pin style) wins over the module's own param.
    let main_col = main_override.or_else(|| snarl.get_node(node_id)
        .filter(|n| n.params.contains_key("main_color"))
        .map(|n| super::menu_body::pcolor(n, "main_color", super::menu_body::MENU_MAIN_DEFAULT)));
    let plate = main_col
        .map(super::menu_body::plate_fill)
        .unwrap_or(visuals.extreme_bg_color);
    let frame_col = main_col.unwrap_or(visuals.widgets.noninteractive.bg_stroke.color);

    // Whole-pad plate — but in "touched zones only" mode the plate is painted
    // PER ZONE inside the loop (active full, inactive dimmed) so the game shows
    // through the faded zones.
    if dim.is_none() {
        painter.rect_filled(rect, 4.0, plate);
    }

    // In mapping mode each zone shows its mapping OUTPUT (icon, or a live mini
    // vectorscope for analog) instead of the bare index. Ports mode keeps the
    // numbers (the ports ARE the zones' identity there).
    let mapping = snarl.get_node(node_id)
        .and_then(|n| n.params.get("zone_mode").and_then(|v| v.as_str())) == Some("mapping");
    let zone_maps: Vec<Value> = if mapping {
        snarl.get_node(node_id)
            .and_then(|n| n.params.get("zone_maps").and_then(|v| v.as_array()).cloned())
            .unwrap_or_default()
    } else { Vec::new() };
    // Virtual Menu per-zone icon/name overrides (empty for Touch Zones — the
    // icon override then shows on the module body + pinned grid too, matching
    // the overlay).
    let zone_meta = snarl.get_node(node_id)
        .filter(|n| n.module_id == "module.menu")
        .map(super::menu_body::menu_zone_meta)
        .unwrap_or_default();
    let skin = remapper_resolve_skin(snarl, node_id, "auto", None);
    let ctx = ui.ctx().clone();

    // Zone rects: from the BSP tree in mapping mode (supports partial dividers),
    // else the legacy grid (ports mode). `idx` is the tree leaf id (== the old
    // grid index after migration), which is also the card `z`.
    let tree = if mapping { Some(tz_field_tree(snarl, node_id, field)) } else { None };
    let zones: Vec<(usize, [f32; 4])> = match &tree {
        Some(t) => t.zones().into_iter().map(|(id, r)| (id as usize, r)).collect(),
        None => {
            let zn = tz::zone_count(col_edges, row_edges);
            (0..zn).map(|idx| {
                let (x0, y0, x1, y1) = tz::zone_rect(idx, col_edges, row_edges);
                (idx, [x0, y0, x1, y1])
            }).collect()
        }
    };
    for &(idx, [x0, y0, x1, y1]) in &zones {
        let zr = egui::Rect::from_min_max(egui::pos2(to_x(x0), to_y(y0)), egui::pos2(to_x(x1), to_y(y1)));
        let live = zone_live.get(&(field, idx)).copied();
        let active = live.map(|z| z.2).unwrap_or(false);
        // "Touched zones only": non-active zones (plate + content) render through
        // the dimmed painter; the touched zone stays full.
        let zp: &egui::Painter = match (&dim, active) { (Some(d), false) => d, _ => painter };
        // Per-zone plate (only in dim mode — the normal path painted it whole).
        if dim.is_some() {
            zp.rect_filled(zr.shrink(0.5), 3.0, plate);
        }
        if active {
            painter.rect_filled(zr.shrink(1.0), 0.0, accent.gamma_multiply(0.35));
        }
        if mapping {
            // Adaptive-centre deflection published by tz_live_hits (relative or
            // absolute per the zone's setting) → the analog vectorscope, so it
            // matches the engine's stick value. +Y up (flip the unit-space +Y-down
            // value). Keyed by the START zone, so the scope stays on the zone the
            // finger began in even if it drifts to a neighbour.
            let deflect = ctx.data(|d| d.get_temp::<(u64, std::collections::HashMap<(usize, usize), (f32, f32)>)>(
                    egui::Id::new(("tz_live_defl", node_id.0))))
                .and_then(|(_, mp)| mp.get(&(field, idx)).copied())
                .map(|(dx, dy)| (dx, -dy));
            tz_paint_zone_mapping(zp, &ctx, node_id, zr, field, idx, &zone_maps, skin, deflect, accent, visuals, &zone_meta);
        } else {
            zp.text(zr.center(), egui::Align2::CENTER_CENTER, format!("{idx}"),
                egui::FontId::proportional(12.0), visuals.weak_text_color());
        }
    }
    for (&(f, idx), &(lx, ly, act)) in zone_live {
        if f != field || !act { continue; }
        if let Some(&(_, [x0, y0, x1, y1])) = zones.iter().find(|(zid, _)| *zid == idx) {
            painter.circle_filled(
                egui::pos2(to_x(x0 + lx * (x1 - x0)), to_y(y0 + ly * (y1 - y0))),
                5.0, egui::Color32::from_rgb(90, 200, 255));
        }
    }
    painter.rect_stroke(rect, 4.0,
        egui::Stroke::new(1.0, frame_col), egui::StrokeKind::Inside);

    // Gamepad-nav focus: the driver publishes (pass, field, axis, line, grabbed)
    // keyed by this node id when line-editing this pad in Easy mode. Highlight
    // the focused divider (accent, or green while grabbed). Keyed by node id, so
    // the in-canvas body (different node-id space) never false-matches.
    let nav_tz: Option<(u64, u64, u64, u64, bool)> =
        ui.ctx().data(|d| d.get_temp(egui::Id::new(("gp_nav_tz", node_id.0))));
    let cur_pass = ui.ctx().cumulative_pass_nr();
    let nav_focus = move |axis: u64, line: usize| -> Option<bool> {
        match nav_tz {
            Some((pass, f, a, l, grabbed))
                if cur_pass.saturating_sub(pass) <= 2
                    && f == field as u64 && a == axis && l == line as u64 => Some(grabbed),
            _ => None,
        }
    };
    let nav_stroke = |grabbed: bool| -> (f32, egui::Color32) {
        if grabbed { (3.0, egui::Color32::from_rgb(90, 220, 120)) } else { (3.0, accent) }
    };
    // Per-divider global-space hit-rects, published for the gamepad RS-cursor
    // hover-select in `nav_drive_touch_zones`. (axis: 0=col/1=row, index, rect).
    let to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
        .unwrap_or(egui::emath::TSTransform::IDENTITY);
    let mut nav_line_rects: Vec<(u8, u32, egui::Rect)> = Vec::new();

    // ── Mapping mode: partial dividers from the tree (drag to move, right-click
    // to remove/merge). Ports mode falls through to the full-cut grid editing. ──
    if let Some(tree) = &tree {
        let mut edited: Option<tz::ZoneNode> = None;
        let mut want_remove: Option<Vec<u8>> = None;
        // Per-axis divider index (V's and H's counted separately) so the gamepad
        // focus highlight + hit-rects line up with the nav driver's (axis, line)
        // model, which walks `dividers()` per axis.
        let (mut vcount, mut hcount) = (0u32, 0u32);
        for (di, div) in tree.dividers().iter().enumerate() {
            // The "−" removal button (from tz_tree_line_overlay) sits at the
            // divider midpoint; carve that band out of the drag handle so the
            // button always wins the pointer there and the drag is grabbable
            // anywhere else along the line.
            let mid = (div.span_lo + div.span_hi) * 0.5;
            let btn = 11.0; // half-band excluded around the midpoint button (px)
            let (p0, p1, hitr, segs, axis_v) = match div.axis {
                tz::Axis::V => {
                    let x = to_x(div.pos);
                    let (lo, hi, cy) = (to_y(div.span_lo), to_y(div.span_hi), to_y(mid));
                    let full = egui::Rect::from_min_max(egui::pos2(x - 4.0, lo), egui::pos2(x + 4.0, hi));
                    let mut segs = Vec::new();
                    if cy - btn > lo + 2.0 { segs.push(egui::Rect::from_min_max(egui::pos2(x - 4.0, lo), egui::pos2(x + 4.0, cy - btn))); }
                    if hi > cy + btn + 2.0 { segs.push(egui::Rect::from_min_max(egui::pos2(x - 4.0, cy + btn), egui::pos2(x + 4.0, hi))); }
                    (egui::pos2(x, lo), egui::pos2(x, hi), full, segs, true)
                }
                tz::Axis::H => {
                    let y = to_y(div.pos);
                    let (lo, hi, cx) = (to_x(div.span_lo), to_x(div.span_hi), to_x(mid));
                    let full = egui::Rect::from_min_max(egui::pos2(lo, y - 4.0), egui::pos2(hi, y + 4.0));
                    let mut segs = Vec::new();
                    if cx - btn > lo + 2.0 { segs.push(egui::Rect::from_min_max(egui::pos2(lo, y - 4.0), egui::pos2(cx - btn, y + 4.0))); }
                    if hi > cx + btn + 2.0 { segs.push(egui::Rect::from_min_max(egui::pos2(cx + btn, y - 4.0), egui::pos2(hi, y + 4.0))); }
                    (egui::pos2(lo, y), egui::pos2(hi, y), full, segs, false)
                }
            };
            let axis_idx = if axis_v { let i = vcount; vcount += 1; i }
                           else       { let i = hcount; hcount += 1; i };
            nav_line_rects.push((if axis_v { 0 } else { 1 }, axis_idx, to_global * hitr));
            // Union the (up to two) segments flanking the button into one response.
            let r = segs.iter().enumerate().fold(None::<egui::Response>, |acc, (si, seg)| {
                let resp = ui.interact(*seg, ui.id().with((node_id, id_salt, "tzdiv", field, di, si)),
                    egui::Sense::click_and_drag());
                Some(match acc { Some(a) => a | resp, None => resp })
            });
            let Some(r) = r else {
                painter.line_segment([p0, p1], egui::Stroke::new(1.0, visuals.weak_text_color()));
                continue;
            };
            let hot = r.hovered() || r.dragged();
            if hot {
                r.clone().on_hover_cursor(if axis_v { egui::CursorIcon::ResizeHorizontal }
                    else { egui::CursorIcon::ResizeVertical })
                    .on_hover_text("Drag to move · right-click to remove (merge)");
            }
            if r.dragged() {
                if let Some(p) = r.interact_pointer_pos() {
                    let want = if axis_v { (p.x - rect.left()) / rect.width() }
                               else { (p.y - rect.top()) / rect.height() };
                    let (lo, hi) = (div.lo + 0.03, div.hi - 0.03);
                    let t = if lo <= hi { want.clamp(lo, hi) } else { (div.lo + div.hi) * 0.5 };
                    let mut nt = tree.clone();
                    if nt.set_divider_t(&div.path, t) { edited = Some(nt); }
                }
            }
            if r.double_clicked() {
                // Recentre between the divider's IMMEDIATE neighbours (not the
                // midpoint of its parent span, which overshoots and squashes the
                // next zone in a 3+-zone tree).
                let target = tree.centered_divider_pos(&div.path)
                    .unwrap_or((div.lo + div.hi) * 0.5);
                let mut nt = tree.clone();
                if nt.set_divider_t(&div.path, target) { edited = Some(nt); }
            }
            if r.secondary_clicked() { want_remove = Some(div.path.clone()); }
            let (w, c) = if let Some(grabbed) = nav_focus(if axis_v { 0 } else { 1 }, axis_idx as usize) {
                nav_stroke(grabbed)
            } else if hot { (2.0, accent) } else { (1.0, visuals.weak_text_color()) };
            painter.line_segment([p0, p1], egui::Stroke::new(w, c));
        }
        if let Some(t) = edited { tz_set_field_tree(snarl, node_id, field, &t); }
        if let Some(path) = want_remove { tz_request_or_apply_merge(snarl, node_id, field, tree, &path); }

        let pass_nr = ui.ctx().cumulative_pass_nr();
        ui.ctx().data_mut(|d| d.insert_temp(
            egui::Id::new(("gp_nav_tz_lines", node_id.0, field)),
            (pass_nr, nav_line_rects)));
        return;
    }

    let mut new_cols = col_edges.to_vec();
    let mut cols_changed = false;
    for i in 0..col_edges.len() {
        let x = to_x(col_edges[i]);
        let hit = egui::Rect::from_min_max(egui::pos2(x - 4.0, rect.top()), egui::pos2(x + 4.0, rect.bottom()));
        nav_line_rects.push((0, i as u32, to_global * hit));
        let r = ui.interact(hit, ui.id().with((node_id, id_salt, "col", field, i)), egui::Sense::click_and_drag());
        let hot = r.hovered() || r.dragged();
        if hot { r.clone().on_hover_cursor(egui::CursorIcon::ResizeHorizontal); }
        if r.dragged() {
            if let Some(p) = r.interact_pointer_pos() {
                let lo = if i == 0 { 0.05 } else { new_cols[i - 1] + 0.04 };
                let hi = if i + 1 == col_edges.len() { 0.95 } else { col_edges[i + 1] - 0.04 };
                let want = (p.x - rect.left()) / rect.width();
                // Crowded neighbours can invert lo/hi — clamp would panic; pin to
                // the midpoint instead.
                new_cols[i] = if lo <= hi { want.clamp(lo, hi) } else { (lo + hi) * 0.5 };
                cols_changed = true;
            }
        }
        // Double-click the line (away from the "−") → recenter between its two
        // adjacent borders (neighbouring dividers or the field edge). Mirrors the
        // gamepad "recenter" (North) action.
        if r.double_clicked() {
            let lo = if i == 0 { 0.0 } else { col_edges[i - 1] };
            let hi = if i + 1 == col_edges.len() { 1.0 } else { col_edges[i + 1] };
            new_cols[i] = (lo + hi) * 0.5;
            cols_changed = true;
        }
        let (w, c) = if let Some(grabbed) = nav_focus(0, i) {
            nav_stroke(grabbed)
        } else if hot { (2.0, accent) } else { (1.0, visuals.weak_text_color()) };
        painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(w, c));
    }
    let mut new_rows = row_edges.to_vec();
    let mut rows_changed = false;
    for i in 0..row_edges.len() {
        let y = to_y(row_edges[i]);
        let hit = egui::Rect::from_min_max(egui::pos2(rect.left(), y - 4.0), egui::pos2(rect.right(), y + 4.0));
        nav_line_rects.push((1, i as u32, to_global * hit));
        let r = ui.interact(hit, ui.id().with((node_id, id_salt, "row", field, i)), egui::Sense::click_and_drag());
        let hot = r.hovered() || r.dragged();
        if hot { r.clone().on_hover_cursor(egui::CursorIcon::ResizeVertical); }
        if r.dragged() {
            if let Some(p) = r.interact_pointer_pos() {
                let lo = if i == 0 { 0.05 } else { new_rows[i - 1] + 0.04 };
                let hi = if i + 1 == row_edges.len() { 0.95 } else { row_edges[i + 1] - 0.04 };
                let want = (p.y - rect.top()) / rect.height();
                new_rows[i] = if lo <= hi { want.clamp(lo, hi) } else { (lo + hi) * 0.5 };
                rows_changed = true;
            }
        }
        if r.double_clicked() {
            let lo = if i == 0 { 0.0 } else { row_edges[i - 1] };
            let hi = if i + 1 == row_edges.len() { 1.0 } else { row_edges[i + 1] };
            new_rows[i] = (lo + hi) * 0.5;
            rows_changed = true;
        }
        let (w, c) = if let Some(grabbed) = nav_focus(1, i) {
            nav_stroke(grabbed)
        } else if hot { (2.0, accent) } else { (1.0, visuals.weak_text_color()) };
        painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(w, c));
    }
    if cols_changed || rows_changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if cols_changed { tz_write_field_edges(node, field, "col_edges", &new_cols); }
            if rows_changed { tz_write_field_edges(node, field, "row_edges", &new_rows); }
        }
    }
    // Publish this pad's divider hit-rects (global space) for gamepad RS-cursor
    // hover-select. Keyed per (node, field) so split pads don't clobber each other.
    // NOTE: read the pass number BEFORE `data_mut` — calling a ctx accessor
    // inside the data lock re-enters it and deadlocks epaint's RwLock.
    let pass_nr = ui.ctx().cumulative_pass_nr();
    ui.ctx().data_mut(|d| d.insert_temp(
        egui::Id::new(("gp_nav_tz_lines", node_id.0, field)),
        (pass_nr, nav_line_rects)));
}

/// Pinned-widget renderer (Easy-mode sub-patch layout). Ports mode shows the
/// pad(s) with live dots and MOVE-only dividers — no add/remove (that would
/// require rewiring / could break bindings), no resize grip (the pin frame
/// resizes it), no mode toggle. Scales the single/split layout into `container`.
fn render_touch_zones_pinned(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    automap_parent: Option<&AutomapGlowParent<'_>>,
    style: Option<&crate::canvas::node::MenuStyleOverride>,
    edit_mode: bool,
) {
    use crate::canvas::node::ZoneVisibility;
    let visuals = ui.visuals().clone();
    // Colour resolution: per-pin override falls back FIELD-BY-FIELD to the
    // module's own colour params; both absent = the plain themed look.
    let node_main = inner_snarl.get_node(inner_id)
        .filter(|n| n.params.contains_key("main_color"))
        .map(|n| super::menu_body::pcolor_bytes(
            n, "main_color", super::menu_body::MENU_MAIN_DEFAULT));
    let node_hi = inner_snarl.get_node(inner_id)
        .filter(|n| n.params.contains_key("highlight_color"))
        .map(|n| super::menu_body::pcolor_bytes(
            n, "highlight_color", super::menu_body::MENU_HIGHLIGHT_DEFAULT));
    let main_b = style.and_then(|s| s.main).or(node_main);
    let hi_b = style.and_then(|s| s.hi).or(node_hi);
    // Highlight colour (opaque, for editor affordances) when themed, else
    // theme selection.
    let accent = hi_b
        .map(|h| super::menu_body::ZoneColors::build(
            main_b.unwrap_or(super::menu_body::MENU_MAIN_DEFAULT), h).accent)
        .unwrap_or(visuals.selection.bg_fill);
    // Plate/frame colour override for the grid painter (`None` = the field
    // painter's own param read — which matches when no pin override is set).
    let main_c32 = main_b
        .map(|c| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]));
    // Visibility gating applies to LIVE views only — the layout editor always
    // paints the pad so it can be seen, selected, and styled.
    let vis = if edit_mode { ZoneVisibility::Always }
              else { style.map(|s| s.visibility).unwrap_or_default() };
    let split = inner_snarl.get_node(inner_id)
        .and_then(|n| n.params.get("field_mode").and_then(|v| v.as_str())) == Some("split");
    let mapping = inner_snarl.get_node(inner_id)
        .and_then(|n| n.params.get("zone_mode").and_then(|v| v.as_str())) == Some("mapping");

    // Radial Virtual Menu: its zone tree is a synthetic 1×N strip, so the grid
    // painter below would show a flat row of columns. Paint the sector ring
    // instead — same shared geometry as the node body and the menu overlay,
    // with hover from the node's own eval mirror.
    let radial_menu = inner_snarl.get_node(inner_id)
        .map(|n| n.module_id == "module.menu"
            && n.params.get("menu_radial").and_then(|v| v.as_bool()).unwrap_or(false))
        .unwrap_or(false);
    if radial_menu {
        let (rect, resp) = ui.allocate_exact_size(container, egui::Sense::click());
        let zones = tz_field_tree(inner_snarl, inner_id, 0).zones();
        let (deadzone, origin, sel_zone, zone_maps) = inner_snarl.get_node(inner_id)
            .map(|n| (
                n.params.get("pointer_deadzone").and_then(|v| v.as_f64()).unwrap_or(0.25) as f32,
                n.params.get("menu_radial_origin").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                n.params.get("sel_zone").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
                n.params.get("zone_maps").and_then(|v| v.as_array()).cloned().unwrap_or_default(),
            ))
            .unwrap_or((0.25, 0.0, 0, Vec::new()));
        let (menu_live, zone_meta, ptr) = inner_snarl.get_node(inner_id)
            .map(|n| (
                super::menu_body::menu_zone_live(n),
                super::menu_body::menu_zone_meta(n),
                super::menu_body::menu_pointer(n),
            ))
            .unwrap_or_else(|| (Default::default(), Default::default(), None));
        // Per-pin override colours over the module's own (both default-filled —
        // a menu is always themed).
        let colors = super::menu_body::ZoneColors::build(
            main_b.unwrap_or(super::menu_body::MENU_MAIN_DEFAULT),
            hi_b.unwrap_or(super::menu_body::MENU_HIGHLIGHT_DEFAULT));
        let hover = menu_live.iter()
            .find(|(_, (_, _, act))| *act)
            .map(|((_, z), _)| *z as i32)
            .unwrap_or(-1);
        // Visibility gating: for a radial menu "touch active" = open + hovering
        // a zone (the eval mirror), so both non-Always modes hide the ring
        // until the menu is actually in use. The rect stays allocated so the
        // pin's geometry is stable.
        if !matches!(vis, ZoneVisibility::Always) && hover < 0 {
            return;
        }
        let picked = super::menu_body::paint_radial_ring(
            ui, rect, &zones, deadzone, origin, hover,
            if mapping { Some(sel_zone) } else { None },
            &zone_maps, &zone_meta, colors, resp.interact_pointer_pos(), ptr,
        );
        if mapping && resp.clicked() {
            if let Some(z) = picked {
                if tz_pick_kind(inner_snarl, inner_id).is_some() {
                    tz_apply_pick(inner_snarl, inner_id, z);
                } else if let Some(node) = inner_snarl.get_node_mut(inner_id) {
                    node.params.insert("sel_field".to_string(), Value::from(0u64));
                    node.params.insert("sel_zone".to_string(), Value::from(z as u64));
                }
            }
        }
        return;
    }

    // Mapping mode has no zone output ports, so dots come from the resolved
    // device's live touch (same as the module body).
    let zone_live = if mapping {
        tz_live_hits(inner_snarl, inner_id, live_signals, automap_parent, ui.ctx())
    } else {
        inner_snarl.get_node(inner_id).map(tz_zone_live).unwrap_or_default()
    };

    // OnTouch / TouchedZones with nothing active: keep the pin's footprint
    // (stable layout, resizable frame) but paint nothing at all.
    if !matches!(vis, ZoneVisibility::Always) && !zone_live.values().any(|v| v.2) {
        ui.allocate_exact_size(container, egui::Sense::hover());
        return;
    }

    // Live-touch tab-follow (mapping mode, no capture in flight): touching a
    // zone selects it, mirroring the module body, so the pinned cards widget
    // filters to the zone under the finger.
    if mapping {
        // Suppress the follow ONLY while a gesture is being demonstrated
        // ("learning") — that swipe can cross zones and must not hijack the tab.
        // Once "captured", output is picked via buttons, so browsing/re-selecting
        // is safe (the trigger is zone-independent, so it just re-targets the
        // pending mapping to the touched zone).
        let follow_ok = inner_snarl.get_node(inner_id)
            .and_then(|n| n.params.get("_tz_phase").and_then(|v| v.as_str()))
            .unwrap_or("idle") != "learning";
        if follow_ok {
            let last: Option<(u64, usize, usize)> = ui.ctx()
                .data(|d| d.get_temp(egui::Id::new(("tz_last_origin", inner_id.0))));
            let cur_pass = ui.ctx().cumulative_pass_nr();
            if tz_pick_kind(inner_snarl, inner_id).is_some() {
                // Pick mode: a FRESH finger tap applies the pick to the touched zone.
                if let Some((p, _, z)) = last {
                    if cur_pass.saturating_sub(p) <= 1 { tz_apply_pick(inner_snarl, inner_id, z); }
                }
            } else {
                // Select the LAST touched-down zone. A FRESH touchdown wins outright
                // (a quick tap whose finger already lifted still selects — the
                // pass-stamp says it just happened), else fall back to its zone while
                // active → keep the current selection while active → the LOWEST active
                // zone (never `HashMap::iter` unordered, which flickers between two
                // fingers' zones).
                let sel = tz_read_selection(inner_snarl, inner_id);
                let fresh = last.filter(|(p, _, _)| cur_pass.saturating_sub(*p) <= 2)
                    .map(|(_, f, z)| (f, z));
                let follow = fresh
                    .or_else(|| last.map(|(_, f, z)| (f, z))
                        .filter(|fz| zone_live.get(fz).map(|v| v.2).unwrap_or(false)))
                    .or_else(|| zone_live.get(&sel).filter(|v| v.2).map(|_| sel))
                    .or_else(|| zone_live.iter().filter(|(_, v)| v.2).map(|(k, _)| *k).min());
                if let Some((f, z)) = follow {
                    if sel != (f, z) {
                        if let Some(node) = inner_snarl.get_node_mut(inner_id) {
                            node.params.insert("sel_field".to_string(), Value::from(f as u64));
                            node.params.insert("sel_zone".to_string(), Value::from(z as u64));
                        }
                        ui.ctx().request_repaint();
                    }
                }
            }
        }
    }

    let (rect, _) = ui.allocate_exact_size(container, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let (sel_field, sel_zone) = tz_read_selection(inner_snarl, inner_id);

    // "Touched zones only" renders the full pad EXACTLY like "show on touch",
    // but fades every non-active zone (plate + icon/label) to 20% so the
    // touched zone stands out — all visuals stay in place, structure (frame,
    // dividers) is untouched. (Radial menus were handled above.)
    let inactive_alpha = matches!(vis, ZoneVisibility::TouchedZones).then_some(0.2_f32);

    let draw = |field: usize, r: egui::Rect, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>| {
        let col = tz_read_field_edges(snarl, inner_id, field, "col_edges");
        let row = tz_read_field_edges(snarl, inner_id, field, "row_edges");
        let to_x = |u: f32| r.left() + u * r.width();
        let to_y = |u: f32| r.top() + u * r.height();
        // Mapping mode: click a zone → select it (registered BEFORE the
        // dividers so those thin drag handles stay on top and win clicks).
        let mtree = if mapping { Some(tz_field_tree(snarl, inner_id, field)) } else { None };
        if let Some(tree) = &mtree {
            let mut clicked: Option<usize> = None;
            for (id, [x0, y0, x1, y1]) in tree.zones() {
                let zr = egui::Rect::from_min_max(egui::pos2(to_x(x0), to_y(y0)), egui::pos2(to_x(x1), to_y(y1)));
                let zresp = ui.interact(zr, ui.id().with((inner_id, "pin_tzselect", field, id)), egui::Sense::click());
                if zresp.hovered() { zresp.clone().on_hover_cursor(egui::CursorIcon::PointingHand); }
                if zresp.clicked() { clicked = Some(id as usize); }
            }
            if let Some(idx) = clicked {
                if tz_pick_kind(snarl, inner_id).is_some() {
                    tz_apply_pick(snarl, inner_id, idx);
                } else if let Some(node) = snarl.get_node_mut(inner_id) {
                    node.params.insert("sel_field".to_string(), Value::from(field as u64));
                    node.params.insert("sel_zone".to_string(), Value::from(idx as u64));
                }
            }
        }
        tz_draw_field(inner_id, field, ui, snarl, &painter, r, &col, &row, &zone_live, &visuals, accent, main_c32, inactive_alpha, "pin");
        if mapping { tz_pick_banner(inner_id, ui, snarl, &painter, r, accent); }
        // Selected-zone outline on top of the pad fill.
        if let Some(tree) = &mtree {
            if sel_field == field {
                if let Some([x0, y0, x1, y1]) = tree.zone_rect(sel_zone as u32) {
                    let zr = egui::Rect::from_min_max(egui::pos2(to_x(x0), to_y(y0)), egui::pos2(to_x(x1), to_y(y1)));
                    painter.rect_stroke(zr.shrink(1.5), 2.0, egui::Stroke::new(2.0, accent), egui::StrokeKind::Inside);
                }
            }
        }
        // Hover-revealed +/- handles: tree ops in mapping mode, full-cut grid in
        // ports mode.
        if mapping {
            tz_tree_line_overlay(inner_id, field, ui, snarl, &painter, r, accent, &visuals);
        } else {
            tz_line_edit_overlay(inner_id, field, ui, snarl, &painter, r, &col, &row, accent, &visuals);
        }
    };

    if split {
        let gap = 12.0;
        let fw = ((rect.width() - gap) * 0.5).max(20.0);
        let a = egui::Rect::from_min_size(rect.min, egui::vec2(fw, rect.height()));
        let b = egui::Rect::from_min_size(egui::pos2(rect.min.x + fw + gap, rect.min.y), egui::vec2(fw, rect.height()));
        draw(0, a, ui, inner_snarl);
        draw(1, b, ui, inner_snarl);
    } else {
        draw(0, rect, ui, inner_snarl);
    }

    // Virtual Menu pinned in grid mode: zone-name labels over the field (the
    // TZ field painter predates `zone_meta`; the radial shape returned above
    // and paints labels inside the ring painter). Menus are always single
    // field.
    if inner_snarl.get_node(inner_id).map(|n| n.module_id == "module.menu").unwrap_or(false) {
        let metas = inner_snarl.get_node(inner_id)
            .map(super::menu_body::menu_zone_meta).unwrap_or_default();
        if !metas.is_empty() {
            // Dim non-active labels in "touched zones only" mode, matching the
            // field painter's per-zone fade.
            let dim = inactive_alpha.map(|a| { let mut p = painter.clone(); p.set_opacity(a); p });
            for (zid, [x0, _y0, x1, y1]) in tz_field_tree(inner_snarl, inner_id, 0).zones() {
                let Some(m) = metas.get(&zid) else { continue };
                if m.label.is_empty() { continue; }
                let active = zone_live.get(&(0usize, zid as usize)).map(|z| z.2).unwrap_or(false);
                let lp: &egui::Painter = match (&dim, active) { (Some(d), false) => d, _ => &painter };
                let cx = rect.left() + (x0 + x1) * 0.5 * rect.width();
                let by = rect.top() + y1 * rect.height();
                let (f, txt) = super::menu_body::fit_zone_label(
                    lp, &m.label, 11.0, (x1 - x0) * rect.width() - 6.0);
                lp.text(egui::pos2(cx, by - 3.0), egui::Align2::CENTER_BOTTOM, txt,
                    f, egui::Color32::from_gray(210));
            }
        }
    }

    // Mapping mode: the merge-confirm popup (raised by a "−" removal that would
    // drop mapped zones).
    if mapping {
        tz_render_merge_popup(ui, inner_snarl, inner_id);
    }
}

/// Rebuild the Touch Zones node's dynamic output ports so they match the current
/// per-field grids (rows × cols) plus a click port per field. Idempotent — bails
/// when already in sync, so it's safe to call every frame. Slot 0 (the AutoMap
/// passthrough) is always kept; changing a grid drops existing zone-port wiring
/// for now (zone indices reshuffle row-major).
pub(crate) fn regenerate_touch_zone_ports(node_id: NodeId, snarl: &mut Snarl<NodeData>) {
    use flexinput_core::touchzones as tz;

    // Build desired (id, label, type) triples from field_mode + per-field grids.
    let Some(node) = snarl.get_node(node_id) else { return };
    let split = node.params.get("field_mode").and_then(|v| v.as_str()) == Some("split");
    let single = !split;
    let n_fields = if split { 2 } else { 1 };
    let mut want_ids: Vec<String> = vec![tz::PASS_PIN_ID.to_string()];
    let mut want: Vec<(String, SignalType)> = Vec::new(); // for outputs[1..]
    // Mapping mode injects per-zone behaviours straight onto the AutoMap bus
    // (Remapper-style), so the node exposes ONLY the AutoMap passthrough — no
    // typed zone ports. Ports mode builds the per-zone X/Y/Active + Click ports.
    let mapping = node.params.get("zone_mode").and_then(|v| v.as_str()) == Some("mapping");
    if !mapping {
    for field in 0..n_fields {
        let col = tz_node_edges(node, field, "col_edges");
        let row = tz_node_edges(node, field, "row_edges");
        for idx in 0..tz::zone_count(&col, &row) {
            for comp in [tz::ZoneComp::X, tz::ZoneComp::Y, tz::ZoneComp::Active] {
                want_ids.push(tz::zone_pin_id(field, idx, comp));
                let ty = if matches!(comp, tz::ZoneComp::Active) { SignalType::Bool } else { SignalType::Float };
                want.push((tz::zone_pin_label(field, idx, comp, single), ty));
            }
        }
        want_ids.push(tz::click_pin_id(field));
        want.push((tz::click_pin_label(field, single), SignalType::Bool));
    }
    }

    let cur_ids: Vec<String> = node.params.get("output_pin_ids").and_then(|v| v.as_array())
        .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
        .unwrap_or_default();
    if cur_ids == want_ids {
        return;
    }

    let old_len = snarl.get_node(node_id).map_or(0, |n| n.outputs.len());
    for i in 1..old_len {
        snarl.drop_outputs(OutPinId { node: node_id, output: i });
    }
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.outputs.truncate(1); // keep AutoMap passthrough at slot 0
        for (label, ty) in &want {
            node.outputs.push(PinDescriptor::new(label.clone(), *ty));
        }
        node.params.insert(
            "output_pin_ids".to_string(),
            Value::Array(want_ids.into_iter().map(Value::String).collect()),
        );
    }
}

/// Perform a relative grid edit on `field`, PRESERVING existing port wiring: a
/// wire to a surviving zone follows that zone to its new index. Snapshots all
/// output connections by stable pin id, rewrites the grid, rebuilds the ports,
/// then reconnects — remapping only the mutated field's zones (other fields and
/// the click ports keep their ids). Index remap math lives in
/// `flexinput_core::touchzones::apply_grid_op` (unit-tested there).
pub(crate) fn tz_restructure(node_id: NodeId, field: usize, op: flexinput_core::touchzones::GridOp, snarl: &mut Snarl<NodeData>) {
    use flexinput_core::touchzones as tz;
    let col = tz_read_field_edges(snarl, node_id, field, "col_edges");
    let row = tz_read_field_edges(snarl, node_id, field, "row_edges");
    let Some((new_col, new_row, remap)) = tz::apply_grid_op(op, &col, &row) else { return };

    // Snapshot current connections keyed by stable pin id (before regen drops them).
    let ids_before: Vec<String> = snarl.get_node(node_id)
        .and_then(|n| n.params.get("output_pin_ids").and_then(|v| v.as_array()))
        .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
        .unwrap_or_default();
    let mut snapshot: std::collections::HashMap<String, Vec<egui_snarl::InPinId>> = std::collections::HashMap::new();
    for (i, id) in ids_before.iter().enumerate() {
        let remotes = snarl.out_pin(OutPinId { node: node_id, output: i }).remotes.clone();
        if !remotes.is_empty() {
            snapshot.insert(id.clone(), remotes);
        }
    }

    if let Some(node) = snarl.get_node_mut(node_id) {
        tz_write_field_edges(node, field, "col_edges", &new_col);
        tz_write_field_edges(node, field, "row_edges", &new_row);
    }
    regenerate_touch_zone_ports(node_id, snarl);

    // Reconnect: for the mutated field, map new zone idx → old id via the inverse
    // remap; everything else keeps its id.
    let inv: std::collections::HashMap<usize, usize> = remap.iter().map(|(&o, &n)| (n, o)).collect();
    let ids_after: Vec<String> = snarl.get_node(node_id)
        .and_then(|n| n.params.get("output_pin_ids").and_then(|v| v.as_array()))
        .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
        .unwrap_or_default();
    for (i, id) in ids_after.iter().enumerate() {
        let src_id: Option<String> = match tz::parse_pin(id) {
            Some(tz::Pin::Zone { field: f, idx: nidx, comp }) if f == field => {
                inv.get(&nidx).map(|&oidx| tz::zone_pin_id(field, oidx, comp))
            }
            Some(_) => Some(id.clone()),
            None => None,
        };
        if let Some(remotes) = src_id.and_then(|s| snapshot.get(&s)) {
            let out = OutPinId { node: node_id, output: i };
            for &rem in remotes {
                snarl.connect(out, rem);
            }
        }
    }
}

/// Small painted +/- button overlaid on the field. Returns true when clicked.
pub(crate) fn tz_mini_button(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    id: egui::Id,
    center: egui::Pos2,
    glyph: &str,
    accent: egui::Color32,
    visuals: &egui::Visuals,
) -> bool {
    let hit = egui::Rect::from_center_size(center, egui::vec2(16.0, 16.0));
    let resp = ui.interact(hit, id, egui::Sense::click());
    let hot = resp.hovered();
    painter.circle_filled(center, 7.5, if hot { accent } else { visuals.widgets.inactive.bg_fill });
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::proportional(12.0),
        if hot { egui::Color32::WHITE } else { visuals.text_color() },
    );
    resp.clicked()
}

fn show_touch_zones_body(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) {
    // Lazy default init: single-field 2×2 grid, ports mode.
    if let Some(node) = snarl.get_node_mut(node_id) {
        if !node.params.contains_key("col_edges") {
            node.params.insert("zone_mode".to_string(), Value::String("ports".to_string()));
            node.params.insert("field_mode".to_string(), Value::String("single".to_string()));
            node.params.insert("col_edges".to_string(), Value::Array(vec![Value::from(0.5)]));
            node.params.insert("row_edges".to_string(), Value::Array(vec![Value::from(0.5)]));
        }
    }

    // Keep dynamic ports in sync with the grids (no-op when unchanged).
    regenerate_touch_zone_ports(node_id, snarl);

    let visuals = ui.visuals().clone();
    // Highlight colour: the `highlight_color` swatch (opaque, for editor
    // affordances) when set, else the theme selection colour (preserves
    // existing patches — real Touch Zones nodes carry no colour params).
    let accent = snarl.get_node(node_id)
        .filter(|n| n.params.contains_key("highlight_color"))
        .map(|n| super::menu_body::ZoneColors::read(n).accent)
        .unwrap_or(visuals.selection.bg_fill);
    let split = snarl.get_node(node_id)
        .and_then(|n| n.params.get("field_mode").and_then(|v| v.as_str())) == Some("split");
    let mapping = snarl.get_node(node_id)
        .and_then(|n| n.params.get("zone_mode").and_then(|v| v.as_str())) == Some("mapping");

    // Live per-(field,zone) finger state for dots + active highlight. Ports mode
    // reconstructs from the node's OWN zone outputs (works for network/collector
    // touch). Mapping mode has no zone ports, so it reads the resolved upstream
    // device's touch pins directly (local device; network-forwarded touch in
    // mapping mode shows no dot — a known gap).
    let zone_live = if mapping {
        tz_live_hits(snarl, node_id, live_signals, automap_parent, ui.ctx())
    } else {
        snarl.get_node(node_id).map(tz_zone_live).unwrap_or_default()
    };

    // Live-touch tab selection: touching a zone selects it so its cards show.
    // Suppressed while a Learn capture is in flight — the tab must stay on the
    // zone the Learn started on, and the gesture may be demonstrated ANYWHERE on
    // the pad without hijacking the selection.
    // Suppress the follow ONLY during a gesture demo ("learning") — see the pinned
    // widget's copy for the rationale. "captured" browses freely (trigger is
    // zone-independent, so re-selecting just re-targets the pending mapping).
    let follow_ok = snarl.get_node(node_id)
        .and_then(|n| n.params.get("_tz_phase").and_then(|v| v.as_str())).unwrap_or("idle") != "learning";
    if mapping && follow_ok {
        let last: Option<(u64, usize, usize)> = ui.ctx()
            .data(|d| d.get_temp(egui::Id::new(("tz_last_origin", node_id.0))));
        let cur_pass = ui.ctx().cumulative_pass_nr();
        if tz_pick_kind(snarl, node_id).is_some() {
            if let Some((p, _, z)) = last {
                if cur_pass.saturating_sub(p) <= 1 { tz_apply_pick(snarl, node_id, z); }
            }
        } else {
            // Select the LAST touched-down zone. A FRESH touchdown wins outright —
            // even a quick tap whose finger has already lifted by the time we read
            // (the pass-stamp says it just happened), which the "still-active" check
            // below would otherwise miss. For a held/sliding finger the origin goes
            // stale, so we fall back to: its zone while still active → keep the
            // current selection while active → the LOWEST active zone (never
            // `HashMap::iter` unordered, which flickers between two fingers' zones).
            let sel = tz_read_selection(snarl, node_id);
            let fresh = last.filter(|(p, _, _)| cur_pass.saturating_sub(*p) <= 2)
                .map(|(_, f, z)| (f, z));
            let follow = fresh
                .or_else(|| last.map(|(_, f, z)| (f, z))
                    .filter(|fz| zone_live.get(fz).map(|v| v.2).unwrap_or(false)))
                .or_else(|| zone_live.get(&sel).filter(|v| v.2).map(|_| sel))
                .or_else(|| zone_live.iter().filter(|(_, v)| v.2).map(|(k, _)| *k).min());
            if let Some((f, z)) = follow {
                if sel != (f, z) {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert("sel_field".to_string(), Value::from(f as u64));
                        node.params.insert("sel_zone".to_string(), Value::from(z as u64));
                    }
                    ui.ctx().request_repaint();
                }
            }
        }
    }

    let field_w = snarl.get_node(node_id)
        .and_then(|n| n.params.get("field_w").and_then(|v| v.as_f64()))
        .unwrap_or(420.0) as f32;
    let field_h = snarl.get_node(node_id)
        .and_then(|n| n.params.get("field_h").and_then(|v| v.as_f64()))
        .unwrap_or(260.0) as f32;

    ui.vertical(|ui| {
        // ── Mode toggles ─────────────────────────────────────────
        ui.horizontal(|ui| {
            // Ports ⇄ Mapping. Ports = typed per-zone outputs. Mapping = inject
            // per-zone behaviours onto the AutoMap bus (Remapper-style).
            let mut want_mapping = mapping;
            ui.label("Mode:");
            if ui.selectable_label(!want_mapping, "Ports")
                .on_hover_text("Expose typed X / Y / Active outputs per zone.").clicked() { want_mapping = false; }
            if ui.selectable_label(want_mapping, "Mapping")
                .on_hover_text("Map each zone to gamepad/key/stick inputs on the AutoMap bus.").clicked() { want_mapping = true; }
            if want_mapping != mapping {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert("zone_mode".to_string(),
                        Value::String(if want_mapping { "mapping" } else { "ports" }.to_string()));
                }
                regenerate_touch_zone_ports(node_id, snarl);
            }

            ui.separator();
            let mut split_v = split;
            if ui.checkbox(&mut split_v, "Split pads")
                .on_hover_text("Track touch 1 and touch 2 on separate fields, each with its own click (e.g. Steam Controller).")
                .changed()
            {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert("field_mode".to_string(),
                        Value::String(if split_v { "split" } else { "single" }.to_string()));
                    // Initialise the second field's grid on first enable.
                    if split_v && !node.params.contains_key("col_edges1") {
                        tz_write_field_edges(node, 1, "col_edges", &[0.5]);
                        tz_write_field_edges(node, 1, "row_edges", &[0.5]);
                    }
                }
                regenerate_touch_zone_ports(node_id, snarl);
            }
            ui.separator();
            super::menu_body::show_zone_color_row(node_id, ui, snarl);
        });

        // Re-read mode flags AFTER the toggles (regen already ran) so the rest of
        // this frame renders consistently in the new mode — never a mixed frame
        // where the field/cards run with stale `mapping` while the ports/params
        // already flipped.
        let split = snarl.get_node(node_id)
            .and_then(|n| n.params.get("field_mode").and_then(|v| v.as_str())) == Some("split");
        let single = !split;
        let mapping = snarl.get_node(node_id)
            .and_then(|n| n.params.get("zone_mode").and_then(|v| v.as_str())) == Some("mapping");

        // Field area(s); the union is registered as the pinnable "field" element.
        let field_area = if tz_n_fields(snarl, node_id) == 2 {
            // Split: two pads side by side with a gap. Only the RIGHT pad shows the
            // resize grip; it writes the shared size so both resize symmetrically.
            ui.horizontal_top(|ui| {
                let a = ui.vertical(|ui| {
                    render_touch_field(node_id, 0, single, false, mapping, ui, snarl, &zone_live, &visuals, accent, field_w, field_h)
                }).inner;
                ui.add_space(16.0);
                let b = ui.vertical(|ui| {
                    render_touch_field(node_id, 1, single, true, mapping, ui, snarl, &zone_live, &visuals, accent, field_w, field_h)
                }).inner;
                a.union(b)
            }).inner
        } else {
            render_touch_field(node_id, 0, single, true, mapping, ui, snarl, &zone_live, &visuals, accent, field_w, field_h)
        };

        // Pinnable to a sub-patch/Easy-mode layout (ports mode = move-only field).
        register_exposable_element(ui, node_id, "field", field_area);

        // Confirm popup for a divider removal that would drop mapped zones.
        if mapping { tz_render_merge_popup(ui, snarl, node_id); }

        // ── Mapping mode: zone-tab card list (separately pinnable) ──────────
        if mapping {
            ui.add_space(6.0);
            let cards_area = ui.vertical(|ui| {
                render_touch_zone_cards(node_id, ui, snarl, &visuals, accent, live_signals, automap_parent);
            }).response.rect;
            register_exposable_element(ui, node_id, "cards", cards_area);
        }
    });
}

/// Render one touch field (in-canvas / advanced editing): the pad with draggable
/// dividers + live dots, the relative line +/- overlay, and (when `show_resize`)
/// the corner resize grip. Returns the field's rect (so the caller can register
/// the pinnable "field" element over the union of pads).
#[allow(clippy::too_many_arguments)]
/// Read the currently-selected (field, zone) for mapping mode (defaults 0,0).
pub(crate) fn tz_read_selection(snarl: &Snarl<NodeData>, node_id: NodeId) -> (usize, usize) {
    snarl.get_node(node_id).map(|n| (
        n.params.get("sel_field").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
        n.params.get("sel_zone").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
    )).unwrap_or((0, 0))
}

/// Mapping-mode card list for the selected zone (the zone is the filter "tab").
/// Cards live in `params["zone_maps"]` as Remapper-style objects tagged with
/// `f`/`z`; each renders through the SHARED [`remapper_mapping_card_pixel`] so the
/// look, press modes (tap/double/short/long/hold/analog), turbo, and delete match
/// the Remapper / Map Action / Lean cards. Trigger + target editing is supplied
/// here (the card's chords are paint-only), since the trigger is the zone gesture
/// rather than a captured device input.
/// Commit a captured Touch Zones mapping into `zone_maps` and reset the Learn
/// state. Shared by the mouse "＋ Add" button and the gamepad `_tz_commit_add`
/// path. Analog outputs (mouse / stick) default to "analog" press mode.
fn tz_commit_card(snarl: &mut Snarl<NodeData>, node_id: NodeId,
    f: usize, z: usize, trigger: &str, draft_out: &[String])
{
    let is_analog = tz_out_pin_is_analog;
    let mode = if draft_out.iter().any(|p| is_analog(p)) { "analog" } else { "down" };
    if let Some(node) = snarl.get_node_mut(node_id) {
        let mut m = serde_json::Map::new();
        m.insert("f".into(), Value::from(f as u64));
        m.insert("z".into(), Value::from(z as u64));
        m.insert("in".into(), Value::Array(vec![Value::from(trigger)]));
        m.insert("out".into(), Value::Array(draft_out.iter().map(|s| Value::from(s.as_str())).collect()));
        m.insert("mode".into(), Value::from(mode));
        let mut cards = node.params.get("zone_maps").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        cards.push(Value::Object(m));
        node.params.insert("zone_maps".into(), Value::Array(cards));
        node.params.insert("_tz_phase".into(), Value::from("idle"));
        for k in ["_tz_trig", "_tz_draft_out", "_tz_gp_arm", "_tz_gp_base", "_tz_gp_seen"] { node.params.remove(k); }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_touch_zone_cards(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    _visuals: &egui::Visuals,
    accent: egui::Color32,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) {
    use flexinput_core::touchzones as tz;
    // Fixed mapping-card width — the header rows pin their right-aligned controls
    // to this, so the layout stays put when the touchpad widget is scaled up.
    const TZ_CARD_W: f32 = 358.0;
    // Virtual Menu variant: the zone IS the trigger (token "menu_sel" — fires
    // on menu selection), so there is no touch-gesture Learn phase. "Learn"
    // arms the gamepad DESTINATION capture directly and "Assign…" opens the
    // picker with this menu's own target pins disabled (no self-targeting).
    let menu_mode = snarl.get_node(node_id).map(|n| n.module_id == "module.menu").unwrap_or(false);
    let menu_excl: Option<String> = if menu_mode {
        snarl.get_node(node_id)
            .and_then(|n| n.params.get("menu_id").and_then(|v| v.as_str()))
            .map(|id| format!("menu:{id}"))
    } else {
        None
    };
    let (sel_f, sel_z) = tz_read_selection(snarl, node_id);
    let single = snarl.get_node(node_id)
        .and_then(|n| n.params.get("field_mode").and_then(|v| v.as_str())) != Some("split");
    let skin = remapper_resolve_skin(snarl, node_id, "auto", None);
    let dev = remapper_upstream_device_id(snarl, node_id, 0, automap_parent);

    let getp = |snarl: &Snarl<NodeData>, k: &str| -> Option<Value> {
        snarl.get_node(node_id).and_then(|n| n.params.get(k).cloned())
    };
    let phase = getp(snarl, "_tz_phase").and_then(|v| v.as_str().map(String::from)).unwrap_or_else(|| "idle".into());

    // ── Learn state machine ───────────────────────────────────────────────
    // idle → Learn → (demonstrate on pad) → captured → Assign / gamepad → commit.
    // (Menu nodes never enter "learning" — their trigger is fixed.)
    if !menu_mode && phase == "learning" {
        if let Some(trig) = tz_learn_capture(snarl, node_id, live_signals, dev.as_deref()) {
            // The zone the gesture STARTED on becomes the mapping's target — matching
            // the Learn hint "demonstrate … on a zone". Located from the captured
            // touchdown point in the current field's tree. (The tab-follow was
            // suppressed during "learning", so sel_field still holds the active field.)
            let sx = getp(snarl, "_tz_cap_sx").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
            let sy = getp(snarl, "_tz_cap_sy").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
            let start_zone = tz_field_tree(snarl, node_id, sel_f).locate(sx, sy).0 as usize;
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("_tz_trig".into(), Value::from(trig.as_str()));
                node.params.insert("_tz_phase".into(), Value::from("captured"));
                node.params.insert("sel_zone".into(), Value::from(start_zone as u64));
            }
        }
    }
    // Gamepad-learn output: while armed, the pressed gamepad CHORD becomes the
    // draft output — accumulated while held and finalised on release, reusing the
    // Remapper's combo-capture shape so multi-button outputs work here too.
    if phase == "captured"
        && getp(snarl, "_tz_gp_arm").and_then(|v| v.as_bool()).unwrap_or(false)
    {
        // Suppress gamepad UI navigation this + next frame so the button the user
        // presses reaches THIS capture instead of driving the cursor/menus. Read
        // by `run_gamepad_nav` (goes inert while the flag is fresh).
        let pass = ui.ctx().cumulative_pass_nr();
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("fxi_tz_gp_learn"), pass));
        ui.ctx().request_repaint();
        let pressed_now: Vec<String> = dev.as_deref()
            .map(|d| remapper_pressed_now(live_signals, d)).unwrap_or_default();
        // Baseline: the pins already held at the instant we armed (typically the
        // button the user pressed to arm — South/🎮). We latch it once, then only
        // accept a pin that is NOT in the baseline, i.e. a FRESH press. Without
        // this the still-held arming button gets captured as the output the same
        // frame ("it just binds North immediately").
        let base: Option<Vec<String>> = getp(snarl, "_tz_gp_base")
            .and_then(|v| v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()));
        let base = match base {
            Some(b) => b,
            None => {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert("_tz_gp_base".into(),
                        Value::Array(pressed_now.iter().map(|p| Value::from(p.as_str())).collect()));
                }
                pressed_now.clone()
            }
        };
        // Fresh presses = held now but not part of the arming baseline. Accumulate
        // the peak chord (sticky) into the draft, then finalise when the whole
        // combo releases. `_tz_gp_seen` records that at least one fresh press has
        // landed this session, so a draft lingering from a prior pick can't latch
        // on the very first frame.
        let seen = getp(snarl, "_tz_gp_seen").and_then(|v| v.as_bool()).unwrap_or(false);
        let fresh: Vec<String> = pressed_now.iter().filter(|p| !base.contains(*p)).cloned().collect();
        if !fresh.is_empty() {
            // The first fresh press of the session replaces any prior draft;
            // further simultaneous presses extend the chord.
            let mut draft: Vec<String> = if seen {
                getp(snarl, "_tz_draft_out").and_then(|v| v.as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()))
                    .unwrap_or_default()
            } else { Vec::new() };
            for p in &fresh { if !draft.contains(p) { draft.push(p.clone()); } }
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("_tz_draft_out".into(),
                    Value::Array(draft.iter().map(|p| Value::from(p.as_str())).collect()));
                node.params.insert("_tz_gp_seen".into(), Value::from(true));
            }
        } else if seen {
            // Whole combo released → finalise the chord and disarm.
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("_tz_gp_arm".into(), Value::from(false));
                node.params.remove("_tz_gp_base");
                node.params.remove("_tz_gp_seen");
            }
        }
    }
    // Draft output chord accumulated by the picker / gamepad-learn. Unlike the
    // Remapper, we do NOT commit on the first pick — the user builds a chord in
    // the picker and presses "Add" (below) to commit, so multi-key outputs work
    // and the picker doesn't vanish mid-selection.
    let draft_out: Vec<String> = getp(snarl, "_tz_draft_out")
        .and_then(|v| v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()))
        .unwrap_or_default();
    let trigger = getp(snarl, "_tz_trig").and_then(|v| v.as_str().map(String::from)).unwrap_or_default();

    // Gamepad "Add": the nav driver sets `_tz_commit_add` to commit the captured
    // mapping (same path as the ＋ Add button).
    if getp(snarl, "_tz_commit_add").and_then(|v| v.as_bool()).unwrap_or(false) {
        if let Some(node) = snarl.get_node_mut(node_id) { node.params.remove("_tz_commit_add"); }
        if phase == "captured" && !draft_out.is_empty() && !trigger.is_empty() {
            tz_commit_card(snarl, node_id, sel_f, sel_z, &trigger, &draft_out);
        }
    }

    // Whether ANY card (any zone) drives a relative-mouse output — gates the
    // node-global mouse-speed control so pure keyboard/stick maps stay uncluttered.
    let has_mouse_card = snarl.get_node(node_id)
        .and_then(|n| n.params.get("zone_maps").and_then(|v| v.as_array()))
        .map(|cards| cards.iter().any(|c| c.get("out").and_then(|o| o.as_array())
            .map(|a| a.iter().any(|p| matches!(p.as_str(), Some("mouse") | Some("mouse_x") | Some("mouse_y"))))
            .unwrap_or(false)))
        .unwrap_or(false);

    // ── Header ────────────────────────────────────────────────────────────
    // Row 1: which zone + capture STATUS (listening / registered trigger →
    // picked output). Row 2 (below): the action BUTTONS + mouse multiplier.
    // Split so they stop competing for the pinned widget's limited width.
    let label = if single { format!("Zone {sel_z}") }
                else { format!("{}{}", tz::field_letter(sel_f), sel_z) };
    // Rect of the Hold checkbox — published LAST in the action-rect list so
    // gamepad nav can focus + toggle it (see `nav_tz_action_items`).
    let mut hold_rect: Option<egui::Rect> = None;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!("{label} mappings")).strong().color(accent));
        // Hold-zone toggle: keep a gesture that STARTS in this zone bound to it
        // even if the finger slides into a neighbour (so the neighbour's mapping
        // doesn't also fire). Affects touch/click triggers — a touch-gesture
        // concept, so hidden on menu nodes (a menu pointer just highlights).
        if !menu_mode {
            let mut hold = tz_zone_held(snarl, node_id, sel_f, sel_z);
            let cb = ui.checkbox(&mut hold, "Hold")
                .on_hover_text("Hold zone: a touch that starts in this zone stays bound to it for the whole gesture, even if the finger slides into another zone — so the other zone won't trigger. Gamepad: focus it and press South to toggle.");
            hold_rect = Some(cb.rect);
            if cb.changed() {
                tz_set_zone_held(snarl, node_id, sel_f, sel_z, hold);
            }
        }
        // Migrate: move THIS zone's mappings onto another zone you tap/click next.
        if tz_pick_kind(snarl, node_id).as_deref() == Some("migrate") {
            if ui.button("✖ Cancel move").clicked() { tz_cancel_pick(snarl, node_id); }
        } else if ui.button("⇄ Move…")
            .on_hover_text("Move this zone's mappings to another zone — click/tap the destination next.")
            .clicked()
        {
            tz_start_migrate(snarl, node_id);
        }
        match phase.as_str() {
            "learning" => {
                ui.label(egui::RichText::new("· listening — touch / click / swipe a zone…")
                    .italics().color(accent));
            }
            "captured" => {
                ui.label(egui::RichText::new("·").weak());
                if menu_mode {
                    // The trigger is implicit (this zone's selection) — show
                    // where the output goes instead of a trigger chip.
                    ui.label(egui::RichText::new("on select →").weak());
                } else {
                    remapper_render_chip(ui, &trigger, skin);
                    ui.label(egui::RichText::new("→").weak());
                }
                if draft_out.is_empty() {
                    let hint = if menu_mode
                        && getp(snarl, "_tz_gp_arm").and_then(|v| v.as_bool()).unwrap_or(false)
                    {
                        "press a gamepad button…"
                    } else {
                        "(pick output)"
                    };
                    ui.label(egui::RichText::new(hint).italics().weak());
                } else {
                    remapper_render_chord(ui, &draft_out, skin);
                }
            }
            _ => {}
        }
    });
    // Row 2: actions + mouse multiplier. Constrained to the mapping-card width
    // (TZ_CARD_W) so the right-aligned mouse multiplier pins to the CARD's right
    // edge — not the widget's available width, which grows when the touchpad is
    // scaled up (that made the control drift ever further right and jam against
    // the scrollbar).
    // Action-button rects, captured in the SAME order as `nav_tz_action_items`
    // (app.rs) so the gamepad-nav glow rings the focused button and scroll-into-
    // view lands on it. Order per phase: idle=[learn]; learning=[cancel];
    // captured=[assign, gamepad, add, cancel].
    let mut act_rects: Vec<egui::Rect> = Vec::new();
    let mut mouse_rect: Option<egui::Rect> = None;
    ui.allocate_ui_with_layout(
        egui::vec2(TZ_CARD_W, ui.spacing().interact_size.y.max(20.0)),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
        match phase.as_str() {
            "idle" if menu_mode => {
                // Menu: the trigger is this zone's selection — go straight to
                // picking the DESTINATION (gamepad learn or the picker).
                let b = ui.button("Learn")
                    .on_hover_text("Learn a gamepad button as this zone's output — press one on the pad.");
                act_rects.push(b.rect);
                if b.clicked() {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert("_tz_phase".into(), Value::from("captured"));
                        node.params.insert("_tz_trig".into(), Value::from("menu_sel"));
                        node.params.insert("_tz_gp_arm".into(), Value::from(true));
                        for k in ["_tz_draft_out", "_tz_gp_base", "_tz_gp_seen"] { node.params.remove(k); }
                    }
                }
                let b = ui.button("Assign…")
                    .on_hover_text("Pick keyboard / mouse / stick / macro outputs for this zone. Pick several for a chord, then Add.");
                act_rects.push(b.rect);
                if b.clicked() {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert("_tz_phase".into(), Value::from("captured"));
                        node.params.insert("_tz_trig".into(), Value::from("menu_sel"));
                        node.params.remove("_tz_draft_out");
                    }
                    request_special_picker(ui.ctx(), SpecialPickerRequest {
                        inner: node_id,
                        path: subpatch_path(automap_parent),
                        draft_key: "_tz_draft_out".to_string(),
                        phase_key: None,
                        touch_zones: true,
                        exclude_pin_prefix: menu_excl.clone(),
                    });
                }
            }
            "idle" => {
                let b = ui.button("Learn")
                    .on_hover_text("Demonstrate a touch, click, or swipe on a zone, then assign an output.");
                act_rects.push(b.rect);
                if b.clicked() {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert("_tz_phase".into(), Value::from("learning"));
                        for k in ["_tz_trig", "_tz_cap_active", "_tz_cap_sx", "_tz_cap_sy",
                                  "_tz_cap_click", "_tz_cap_moved", "_tz_cap_dir"] { node.params.remove(k); }
                    }
                }
            }
            "learning" => {
                let b = ui.button("Cancel");
                act_rects.push(b.rect);
                if b.clicked() {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert("_tz_phase".into(), Value::from("idle"));
                    }
                }
            }
            _ => { // captured
                let b = ui.button("Assign…")
                    .on_hover_text("Pick keyboard / mouse / mouse-delta / stick outputs. Pick several for a chord, then Add.");
                act_rects.push(b.rect);
                if b.clicked() {
                    request_special_picker(ui.ctx(), SpecialPickerRequest {
                        inner: node_id,
                        path: subpatch_path(automap_parent),
                        draft_key: "_tz_draft_out".to_string(),
                        phase_key: None,
                        touch_zones: true,
                        exclude_pin_prefix: menu_excl.clone(),
                    });
                }
                let armed = getp(snarl, "_tz_gp_arm").and_then(|v| v.as_bool()).unwrap_or(false);
                let b = ui.add(egui::Button::new(if armed { "🎮…" } else { "🎮" }))
                    .on_hover_text("Learn a gamepad button as the output — press one now.");
                act_rects.push(b.rect);
                if b.clicked() {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert("_tz_gp_arm".into(), Value::from(!armed));
                        node.params.remove("_tz_gp_base");
                        node.params.remove("_tz_gp_seen");
                    }
                }
                let add = ui.add_enabled(!draft_out.is_empty(),
                    egui::Button::new(egui::RichText::new("Add").strong()));
                act_rects.push(add.rect);
                if add.on_hover_text("Add this zone mapping").clicked() && !draft_out.is_empty() {
                    tz_commit_card(snarl, node_id, sel_f, sel_z, &trigger, &draft_out);
                }
                let b = ui.button("Cancel");
                act_rects.push(b.rect);
                if b.clicked() {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert("_tz_phase".into(), Value::from("idle"));
                        for k in ["_tz_draft_out", "_tz_gp_arm", "_tz_gp_base", "_tz_gp_seen"] { node.params.remove(k); }
                    }
                }
            }
        }
        // Node-global relative-mouse speed, right-aligned. Only when a card
        // actually drives a mouse output.
        if has_mouse_card {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut spd = getp(snarl, "mouse_speed").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
                let dv = ui.add(egui::DragValue::new(&mut spd).speed(0.02).range(0.1..=10.0).prefix("🖱 "))
                    .on_hover_text("Relative-mouse speed multiplier (1.0 ≈ a firm gyro/right-stick flick at full zone deflection). The sink's own mouse sensitivity still applies on top. Gamepad: focus it and nudge with LT/RT.");
                // Gamepad-nav target — appended LAST in action order (matches
                // `nav_tz_action_items` when has_mouse_card).
                mouse_rect = Some(dv.rect);
                if dv.changed() {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert("mouse_speed".into(), Value::from(spd as f64));
                    }
                }
            });
        }
    });
    if let Some(r) = mouse_rect { act_rects.push(r); }
    if let Some(r) = hold_rect { act_rects.push(r); }
    // Publish the action-button rects (scope "zone_maps") so the gamepad-nav
    // overlay rings the focused button + can scroll it into view — same channel
    // the Remapper's action row uses.
    publish_nav_action_rects_scoped(ui, node_id, "zone_maps", &act_rects);
    // Keep polling live input while a capture is in flight.
    if phase != "idle" { ui.ctx().request_repaint(); }

    // ── Existing cards for the selected zone (display + press-mode + delete +
    // drag-to-reorder). The list is a FILTERED subset of `zone_maps` (this zone
    // only), so reorder runs in DISPLAY-index space and the reordered subset is
    // written back into the same array slots — other zones' cards stay put. ──
    let mut cards: Vec<Value> = snarl.get_node(node_id)
        .and_then(|n| n.params.get("zone_maps").and_then(|v| v.as_array()).cloned())
        .unwrap_or_default();
    let display: Vec<usize> = cards.iter().enumerate().filter(|(_, c)|
        c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) == sel_f as u64 &&
        c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) == sel_z as u64)
        .map(|(i, _)| i).collect();
    let mut dirty = false;
    let mut remove: Option<usize> = None; // full-array index
    if display.is_empty() && phase == "idle" {
        let hint = if menu_mode {
            "No mappings — Learn a gamepad output or Assign… one for this zone."
        } else {
            "No mappings — press Learn, then demonstrate on a zone."
        };
        ui.label(egui::RichText::new(hint).weak());
    }
    let reorder_enabled = display.len() > 1;
    // Live deflection magnitude of the selected zone — the preview dot on any
    // open card curve editor. Computed once for the whole card list.
    let tz_live_mag: Option<f32> = if menu_mode {
        // Menu zones have no touch deflection; the curve preview dot stays put.
        None
    } else {
        // Adaptive-centre deflection magnitude of the selected zone (published by
        // tz_live_hits), so the preview dot's input matches the engine — relative
        // or absolute per the zone's setting, not a raw absolute position.
        let _ = tz_live_hits(snarl, node_id, live_signals, automap_parent, ui.ctx());
        ui.ctx().data(|d| d.get_temp::<(u64, std::collections::HashMap<(usize, usize), (f32, f32)>)>(
                egui::Id::new(("tz_live_defl", node_id.0))))
            .and_then(|(_, mp)| mp.get(&(sel_f, sel_z)).copied())
            .map(|(dx, dy)| (dx * dx + dy * dy).sqrt().min(1.0))
    };
    // Gamepad nav still edits ONE curve per zone (the shared driver has a
    // single geometry channel per node) — attach it to the FIRST analog card,
    // matching what `tz_zone_curve`/`tz_set_zone_curve` in the nav path edit.
    let mut nav_curve_given = false;
    let mut rv = ReorderView::begin(
        ui, egui::Id::new(("fxi_tz_reorder", node_id.0, sel_f, sel_z)), reorder_enabled);
    for (slot, &i) in display.iter().enumerate() {
        if let Some(h) = rv.gap_before(slot) { draw_insertion_gap(ui, h); }
        let mut working = cards[i].as_object().cloned().unwrap_or_default();
        let before = working.clone();
        let in_pins: Vec<String> = working.get("in").and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
        let out_pins: Vec<String> = working.get("out").and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
        let drag_off = rv.offset_for(slot);
        let card_analog = out_pins.iter().any(|p| tz_out_pin_is_analog(p));
        let nav_uid = if card_analog && !nav_curve_given {
            nav_curve_given = true;
            Some(node_id.0)
        } else {
            None
        };
        ui.allocate_ui_with_layout(
            egui::vec2(TZ_CARD_W, 1.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                let result = remapper_mapping_card_pixel(
                    ui, node_id, i, &mut working,
                    &in_pins, Some(&out_pins), skin,
                    true, reorder_enabled, drag_off, "zone_maps", card_analog,
                );
                if result.delete_clicked { remove = Some(i); }
                rv.observe(slot, &result);
                // Per-card response curve (analog outputs shape the zone's
                // deflection). Thresholds don't apply to TZ triggers — the
                // gate is touch presence, not a magnitude.
                if card_analog {
                    mapping_card_curve_section(
                        ui, node_id, "zone_maps", i, &mut working,
                        false, tz_live_mag, nav_uid,
                    );
                }
            },
        );
        if working != before {
            cards[i] = Value::Object(working);
            dirty = true;
        }
    }
    if let Some(h) = rv.gap_after_last(display.len()) { draw_insertion_gap(ui, h); }
    if let Some((from, to)) = rv.finish(ui) {
        // from/to are DISPLAY slots — reorder the subset, write back into slots.
        let mut sub: Vec<Value> = display.iter().map(|&fi| cards[fi].clone()).collect();
        reorder_array(&mut sub, from, to);
        for (k, &fi) in display.iter().enumerate() { cards[fi] = sub[k].clone(); }
        dirty = true;
    }
    if let Some(i) = remove { cards.remove(i); dirty = true; }
    if dirty {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("zone_maps".to_string(), Value::Array(cards));
        }
    }

    // The response curve now lives ON each analog card (expander strip in the
    // loop above). Only the zone-level Relative-center slider stays here — the
    // adaptive centre is a per-zone property, not per-card.
    let zmaps_now = snarl.get_node(node_id)
        .and_then(|n| n.params.get("zone_maps").and_then(|v| v.as_array()).cloned())
        .unwrap_or_default();
    if tz_zone_is_analog(&zmaps_now, sel_f, sel_z) {
        ui.add_space(4.0);
        // Adaptive-centre inner %: 0 = absolute deflection from the zone centre,
        // 100 = wherever your finger lands becomes the centre (fully relative).
        let mut pct = tz_zone_adaptive(&zmaps_now, sel_f, sel_z) * 100.0;
        if ui.add(egui::Slider::new(&mut pct, 0.0..=100.0)
            .text("Relative center").suffix("%").fixed_decimals(0))
            .on_hover_text("How much of the zone acts as a relative centre for analog deflection. 0% = the fixed zone centre (absolute across the whole zone); 100% = wherever your finger first lands becomes the centre (fully relative). In between, only a touch landing within that inner fraction re-centres.")
            .changed()
        {
            tz_set_zone_adaptive(snarl, node_id, sel_f, sel_z, pct / 100.0);
        }
    }
}

/// UI-side trigger capture during Learn: track the primary finger (touch1) from
/// touch-down to release and classify the gesture — swipe (moved past threshold),
/// click (pad pressed), else plain touch. Returns the trigger token on release.
/// Scratch persists in `_tz_cap_*` node params.
fn tz_learn_capture(
    snarl: &mut Snarl<NodeData>,
    node_id: NodeId,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    dev: Option<&str>,
) -> Option<String> {
    use flexinput_core::touchzones as tz;
    let dev = dev?;
    let readf = |pin: &str| live_signals.get(&(dev.to_string(), pin.to_string())).map(|s| s.as_float()).unwrap_or(0.0);
    let readb = |pin: &str| live_signals.get(&(dev.to_string(), pin.to_string())).map(|s| s.as_bool()).unwrap_or(false);
    const SWIPE_THRESH: f32 = 0.18;
    let active = readb("touch1_active");
    let click = readb("btn_touchpad");
    let (ux, uy) = tz::pad_point_to_unit(readf("touch1_x"), readf("touch1_y"));
    let node = snarl.get_node_mut(node_id)?;
    let prev_active = node.params.get("_tz_cap_active").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut result = None;
    if active {
        if !prev_active {
            node.params.insert("_tz_cap_sx".into(), Value::from(ux as f64));
            node.params.insert("_tz_cap_sy".into(), Value::from(uy as f64));
            node.params.insert("_tz_cap_click".into(), Value::from(false));
            node.params.insert("_tz_cap_moved".into(), Value::from(false));
            node.params.insert("_tz_cap_dir".into(), Value::from(0u64));
        } else {
            if click { node.params.insert("_tz_cap_click".into(), Value::from(true)); }
            if !node.params.get("_tz_cap_moved").and_then(|v| v.as_bool()).unwrap_or(false) {
                let sx = node.params.get("_tz_cap_sx").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                let sy = node.params.get("_tz_cap_sy").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                let dx = ux - sx;
                let dy = uy - sy;
                if dx.abs().max(dy.abs()) > SWIPE_THRESH {
                    let dir: u64 = if dx.abs() >= dy.abs() { if dx > 0.0 { 4 } else { 3 } }
                                   else if dy < 0.0 { 1 } else { 2 };
                    node.params.insert("_tz_cap_dir".into(), Value::from(dir));
                    node.params.insert("_tz_cap_moved".into(), Value::from(true));
                }
            }
        }
        node.params.insert("_tz_cap_active".into(), Value::from(true));
    } else {
        if prev_active {
            let moved = node.params.get("_tz_cap_moved").and_then(|v| v.as_bool()).unwrap_or(false);
            let clicked = node.params.get("_tz_cap_click").and_then(|v| v.as_bool()).unwrap_or(false);
            let dir = node.params.get("_tz_cap_dir").and_then(|v| v.as_u64()).unwrap_or(0);
            result = Some(if moved {
                match dir { 1 => "tz_swipe_up", 2 => "tz_swipe_down", 3 => "tz_swipe_left", _ => "tz_swipe_right" }.to_string()
            } else if clicked { "tz_click".to_string() } else { "tz_touch".to_string() });
        }
        node.params.insert("_tz_cap_active".into(), Value::from(false));
    }
    result
}

/// Hover-revealed +/- line-editing overlay over one pad `rect` (local layer
/// space). Only "−" marks show at rest (one per interior divider per crossing
/// band); hovering reveals flanking "+"; border "+" appears near a field edge.
/// Applies the resulting grid op wire-preservingly via `tz_restructure`. Shared
/// by the in-canvas body and the pinned widget (mapping mode). See
/// `render_touch_field` for the original prose.
#[allow(clippy::too_many_arguments)]
fn tz_line_edit_overlay(
    node_id: NodeId,
    field: usize,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    painter: &egui::Painter,
    rect: egui::Rect,
    col_edges: &[f32],
    row_edges: &[f32],
    accent: egui::Color32,
    visuals: &egui::Visuals,
) {
    use flexinput_core::touchzones as tz;
    let to_x = |u: f32| rect.left() + u * rect.width();
    let to_y = |u: f32| rect.top() + u * rect.height();
    let cols = tz::cols(col_edges);
    let rows = tz::rows(row_edges);
    let mut op: Option<tz::GridOp> = None;

    let off = 18.0;      // "+" flanking distance from the "−"
    let edge = 30.0;     // border-proximity threshold (px)
    let inset = 12.0;    // border "+" inset so it sits fully inside the field
    let from_global = ui.ctx().layer_transform_to_global(ui.layer_id())
        .unwrap_or(egui::emath::TSTransform::IDENTITY)
        .inverse();
    let ptr = ui.input(|i| i.pointer.hover_pos()).map(|p| from_global * p);
    let band_mid = |b: usize, n: usize, edges: &[f32]| -> f32 {
        let lo = if b == 0 { 0.0 } else { edges[b - 1] };
        let hi = if b == n - 1 { 1.0 } else { edges[b] };
        (lo + hi) * 0.5
    };
    let band_of = |u: f32, edges: &[f32]| edges.iter().filter(|e| u >= **e).count();

    for line in 1..cols {
        let x = to_x(col_edges[line - 1]);
        for band in 0..rows {
            let y = to_y(band_mid(band, rows, row_edges));
            let c = egui::pos2(x, y);
            let pill = egui::Rect::from_center_size(c, egui::vec2(2.0 * off + 20.0, 22.0));
            let expanded = ptr.is_some_and(|p| pill.contains(p));
            if expanded {
                if tz_mini_button(ui, painter, ui.id().with((node_id, "tzcL", field, line, band)),
                    egui::pos2(x - off, y), "+", accent, visuals) {
                    op = Some(tz::GridOp::InsertCol(line - 1));
                }
                if tz_mini_button(ui, painter, ui.id().with((node_id, "tzcR", field, line, band)),
                    egui::pos2(x + off, y), "+", accent, visuals) {
                    op = Some(tz::GridOp::InsertCol(line));
                }
                // "−" shows with the flanking "+", only on hover (dynamic).
                if tz_mini_button(ui, painter, ui.id().with((node_id, "tzc-", field, line, band)),
                    c, "−", accent, visuals) {
                    op = Some(tz::GridOp::RemoveCol(line - 1));
                }
            }
        }
    }

    for line in 1..rows {
        let y = to_y(row_edges[line - 1]);
        for band in 0..cols {
            let x = to_x(band_mid(band, cols, col_edges));
            let c = egui::pos2(x, y);
            let pill = egui::Rect::from_center_size(c, egui::vec2(22.0, 2.0 * off + 20.0));
            let expanded = ptr.is_some_and(|p| pill.contains(p));
            if expanded {
                if tz_mini_button(ui, painter, ui.id().with((node_id, "tzrU", field, line, band)),
                    egui::pos2(x, y - off), "+", accent, visuals) {
                    op = Some(tz::GridOp::InsertRow(line - 1));
                }
                if tz_mini_button(ui, painter, ui.id().with((node_id, "tzrD", field, line, band)),
                    egui::pos2(x, y + off), "+", accent, visuals) {
                    op = Some(tz::GridOp::InsertRow(line));
                }
                // "−" shows with the flanking "+", only on hover (dynamic).
                if tz_mini_button(ui, painter, ui.id().with((node_id, "tzr-", field, line, band)),
                    c, "−", accent, visuals) {
                    op = Some(tz::GridOp::RemoveRow(line - 1));
                }
            }
        }
    }

    if let Some(p) = ptr.filter(|p| rect.contains(*p)) {
        let ux = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        let uy = ((p.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
        if p.x - rect.left() < edge {
            let y = to_y(band_mid(band_of(uy, row_edges), rows, row_edges));
            if tz_mini_button(ui, painter, ui.id().with((node_id, "tzbL", field)),
                egui::pos2(rect.left() + inset, y), "+", accent, visuals) {
                op = Some(tz::GridOp::InsertCol(0));
            }
        }
        if rect.right() - p.x < edge {
            let y = to_y(band_mid(band_of(uy, row_edges), rows, row_edges));
            if tz_mini_button(ui, painter, ui.id().with((node_id, "tzbR", field)),
                egui::pos2(rect.right() - inset, y), "+", accent, visuals) {
                op = Some(tz::GridOp::InsertCol(cols - 1));
            }
        }
        if p.y - rect.top() < edge {
            let x = to_x(band_mid(band_of(ux, col_edges), cols, col_edges));
            if tz_mini_button(ui, painter, ui.id().with((node_id, "tzbT", field)),
                egui::pos2(x, rect.top() + inset), "+", accent, visuals) {
                op = Some(tz::GridOp::InsertRow(0));
            }
        }
        if rect.bottom() - p.y < edge {
            let x = to_x(band_mid(band_of(ux, col_edges), cols, col_edges));
            if tz_mini_button(ui, painter, ui.id().with((node_id, "tzbB", field)),
                egui::pos2(x, rect.bottom() - inset), "+", accent, visuals) {
                op = Some(tz::GridOp::InsertRow(rows - 1));
            }
        }
    }

    if let Some(op) = op {
        tz_restructure(node_id, field, op, snarl);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_touch_field(
    node_id: NodeId,
    field: usize,
    single: bool,
    show_resize: bool,
    mapping: bool,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    zone_live: &std::collections::HashMap<(usize, usize), (f32, f32, bool)>,
    visuals: &egui::Visuals,
    accent: egui::Color32,
    field_w: f32,
    field_h: f32,
) -> egui::Rect {
    use flexinput_core::touchzones as tz;

    let col_edges = tz_read_field_edges(snarl, node_id, field, "col_edges");
    let row_edges = tz_read_field_edges(snarl, node_id, field, "row_edges");

    if !single {
        ui.label(egui::RichText::new(format!("Pad {} — touch {}", tz::field_letter(field), field + 1))
            .small().strong().color(accent));
    }

    let (rect, _resp) = ui.allocate_exact_size(egui::vec2(field_w, field_h), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let to_x = |u: f32| rect.left() + u * rect.width();
    let to_y = |u: f32| rect.top() + u * rect.height();

    // Mapping mode: clicking a zone selects it (the card list below filters to
    // the selected zone — the "tab per zone" model). Registered BEFORE the
    // dividers / +/- overlay so those thin controls stay on top and win clicks.
    let (sel_field, sel_zone) = tz_read_selection(snarl, node_id);
    let mtree = if mapping { Some(tz_field_tree(snarl, node_id, field)) } else { None };
    if let Some(tree) = &mtree {
        let mut clicked: Option<usize> = None;
        for (id, [x0, y0, x1, y1]) in tree.zones() {
            let zr = egui::Rect::from_min_max(egui::pos2(to_x(x0), to_y(y0)), egui::pos2(to_x(x1), to_y(y1)));
            let zresp = ui.interact(zr, ui.id().with((node_id, "tzselect", field, id)), egui::Sense::click());
            if zresp.hovered() { zresp.clone().on_hover_cursor(egui::CursorIcon::PointingHand); }
            if zresp.clicked() { clicked = Some(id as usize); }
        }
        if let Some(idx) = clicked {
            if tz_pick_kind(snarl, node_id).is_some() {
                tz_apply_pick(snarl, node_id, idx);
            } else if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("sel_field".to_string(), Value::from(field as u64));
                node.params.insert("sel_zone".to_string(), Value::from(idx as u64));
            }
        }
    }

    // Pad visuals + dividers (tree-aware in mapping mode; grid in ports mode).
    tz_draw_field(node_id, field, ui, snarl, &painter, rect, &col_edges, &row_edges, zone_live, visuals, accent, None, None, "canvas");
    if mapping { tz_pick_banner(node_id, ui, snarl, &painter, rect, accent); }

    // Selected-zone outline (mapping mode) — drawn on top of the pad fill.
    if let Some(tree) = &mtree {
        if sel_field == field {
            if let Some([x0, y0, x1, y1]) = tree.zone_rect(sel_zone as u32) {
                let zr = egui::Rect::from_min_max(egui::pos2(to_x(x0), to_y(y0)), egui::pos2(to_x(x1), to_y(y1)));
                painter.rect_stroke(zr.shrink(1.5), 2.0, egui::Stroke::new(2.0, accent), egui::StrokeKind::Inside);
            }
        }
    }

    // Line editing: same hover-revealed +/- handles in both modes. Mapping mode
    // drives the tree (+ subdivides that zone, − removes/merges); ports mode drives
    // the full-cut grid.
    if mapping {
        tz_tree_line_overlay(node_id, field, ui, snarl, &painter, rect, accent, visuals);
    } else {
        tz_line_edit_overlay(node_id, field, ui, snarl, &painter, rect, &col_edges, &row_edges, accent, visuals);
    }

    // ── Resize grip (bottom-right corner). In split mode only the right pad
    // shows it; it writes the SHARED field size so both pads resize together. ─
    if show_resize {
        let hs = 14.0;
        let handle = egui::Rect::from_min_max(
            egui::pos2(rect.right() - hs, rect.bottom() - hs),
            egui::pos2(rect.right(), rect.bottom()),
        );
        let hr = ui.interact(handle, ui.id().with((node_id, "tzresize")), egui::Sense::drag());
        if hr.hovered() || hr.dragged() {
            hr.clone().on_hover_cursor(egui::CursorIcon::ResizeNwSe);
        }
        let grip = if hr.hovered() || hr.dragged() { accent } else { visuals.weak_text_color() };
        for k in 1..=3 {
            let o = k as f32 * 3.5;
            painter.line_segment(
                [egui::pos2(rect.right() - o, rect.bottom()), egui::pos2(rect.right(), rect.bottom() - o)],
                egui::Stroke::new(1.0, grip),
            );
        }
        if hr.dragged() {
            let d = hr.drag_delta();
            let nw = (field_w + d.x).clamp(200.0, 900.0);
            let nh = (field_h + d.y).clamp(120.0, 600.0);
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("field_w".to_string(), Value::from(nw as f64));
                node.params.insert("field_h".to_string(), Value::from(nh as f64));
            }
        }
    }

    rect
}










/// Render the full layout-editing controls row: snap toggle + grid step, the
/// "Add" decoration buttons, and the per-selection inspector strip. Shown at
/// the top of the Advanced sub-patch node body in Layout mode AND below the
/// Easy-mode preset bar, so both surfaces expose identical layout tools.
/// Caller is responsible for only invoking this while layout editing is active.
pub(crate) fn layout_editing_controls(
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    outer_id: NodeId,
) {
    let sel_module = subpatch_selected_module_info(snarl, outer_id);
    if let Some(sp) = snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) {
        layout_editing_controls_core(ui, &mut LayoutStateMut::of_subpatch(sp), sel_module);
    }
}

/// Mutable view over the layout-edit state shared by sub-patch layouts
/// (`UiSubPatch`) and the screen overlay (`OverlayLayout`). Lets the toolbar,
/// inspector, and (overlay) body machinery operate on either container.
pub(crate) struct LayoutStateMut<'a> {
    pub items: &'a mut Vec<LayoutItem>,
    pub snap_enabled: &'a mut bool,
    pub snap_grid_px: &'a mut u32,
    pub selected_item: &'a mut Option<usize>,
    pub selected_items: &'a mut Vec<usize>,
}

impl<'a> LayoutStateMut<'a> {
    pub fn of_subpatch(sp: &'a mut crate::canvas::node::UiSubPatch) -> Self {
        Self {
            items: &mut sp.items,
            snap_enabled: &mut sp.snap_enabled,
            snap_grid_px: &mut sp.snap_grid_px,
            selected_item: &mut sp.selected_item,
            selected_items: &mut sp.selected_items,
        }
    }
    pub fn of_overlay(ov: &'a mut crate::canvas::node::OverlayLayout) -> Self {
        Self {
            items: &mut ov.items,
            snap_enabled: &mut ov.snap_enabled,
            snap_grid_px: &mut ov.snap_grid_px,
            selected_item: &mut ov.selected_item,
            selected_items: &mut ov.selected_items,
        }
    }
}

/// Module info the inspector strip needs for the currently-selected item when
/// it's a module pin: (module_id, graph channel count). Precomputed by the
/// caller because resolving it requires the snarl that CONTAINS the pinned
/// node — which differs between sub-patch layouts (the owning sub-patch's
/// inner snarl) and the overlay (per-pin `source_path`).
pub(crate) type SelectedModuleInfo = Option<(String, usize)>;

/// Container-agnostic core of the layout-edit toolbar (snap controls +
/// decoration adders) and inspector strip. See `layout_editing_controls` for
/// the sub-patch wrapper; the overlay toolbar calls this directly.
pub(crate) fn layout_editing_controls_core(
    ui: &mut egui::Ui,
    state: &mut LayoutStateMut<'_>,
    sel_module: SelectedModuleInfo,
) {
    ui.horizontal(|ui| layout_toolbar_controls_core(ui, state));

    // Inspector strip for the currently selected item (+ bulk-style
    // propagation across a multi-selection).
    layout_inspector_strip_core(ui, state, sel_module);
}

/// The snap + decoration-adder controls, WITHOUT a row wrapper — the caller
/// supplies the layout (a `ui.horizontal` in the sub-patch wrapper; the
/// overlay toolbar keeps these on its main row while the inspector strip drops
/// to a second row below). Split out of `layout_editing_controls_core` so the
/// selected-item properties can always live on their own row.
pub(crate) fn layout_toolbar_controls_core(
    ui: &mut egui::Ui,
    state: &mut LayoutStateMut<'_>,
) {
    let mut add_kind: Option<&'static str> = None;
    ui.checkbox(state.snap_enabled, egui::RichText::new("Snap").small())
        .on_hover_text("Snap pinned-element positions and sizes to a grid in Layout mode");
    ui.add_enabled_ui(*state.snap_enabled, |ui| {
        ui.label(egui::RichText::new("grid").small().weak());
        let mut g = *state.snap_grid_px as i32;
        if ui.add(egui::DragValue::new(&mut g)
            .speed(0.5)
            .range(2i32..=64)
            .suffix("px"))
            .on_hover_text("Grid step in pixels (rounded to multiples of 2)")
            .changed()
        {
            *state.snap_grid_px = ((g.max(2)) / 2 * 2) as u32;
        }
    });
    ui.separator();
    ui.label(egui::RichText::new("Add:").small().weak());
    if ui.small_button("T").on_hover_text("Add Text label").clicked() {
        add_kind = Some("text");
    }
    if ui.small_button("▢").on_hover_text("Add Rectangle").clicked() {
        add_kind = Some("rect");
    }
    if ui.small_button("◯").on_hover_text("Add Ellipse").clicked() {
        add_kind = Some("ellipse");
    }
    if ui.small_button("╱").on_hover_text("Add Line").clicked() {
        add_kind = Some("line");
    }
    if ui.small_button("SVG").on_hover_text("Add SVG").clicked() {
        add_kind = Some("svg");
    }
    if let Some(kind) = add_kind {
        let deco = make_default_decoration(kind);
        state.items.push(LayoutItem::Deco(deco));
        let idx = state.items.len() - 1;
        *state.selected_item = Some(idx);
        *state.selected_items = vec![idx];
    }
}

/// Resolve `SelectedModuleInfo` for a sub-patch layout's current selection:
/// looks the pinned node up in the sub-patch's own inner snarl.
fn subpatch_selected_module_info(
    snarl: &Snarl<NodeData>,
    outer_id: NodeId,
) -> SelectedModuleInfo {
    let sp = snarl.get_node(outer_id).and_then(|n| n.subpatch.as_deref())?;
    let idx = sp.selected_item?;
    let LayoutItem::Module(m) = sp.items.get(idx)? else { return None };
    let inner = sp.snarl.get_node(egui_snarl::NodeId(m.inner_node_id))?;
    Some((inner.module_id.clone(), graph_channels_of_node(inner)))
}

/// Channel count for a graph pin's per-channel color row. Response curves
/// expose `min(inputs, outputs)` channels; scopes one per input; the
/// trigscope's index 0 is the trigger; envelope has a single trail color.
pub(crate) fn graph_channels_of_node(inner: &NodeData) -> usize {
    match inner.module_id.as_str() {
        "module.response_curve"
        | "module.vec_response_curve"
        | "module.twoway_response_curve" =>
            inner.inputs.len().min(inner.outputs.len()).max(1),
        "display.trigscope" => inner.inputs.len().saturating_sub(1).max(1),
        "generator.envelope" => 1,
        _ => inner.inputs.len().max(1),
    }
}

/// Container-agnostic core of `layout_inspector_strip`. `sel_module` carries
/// (module_id, graph_channels) for the selected item when it's a module pin —
/// resolved by the caller against the snarl that contains the pinned node.
pub(crate) fn layout_inspector_strip_core(
    ui: &mut egui::Ui,
    state: &mut LayoutStateMut<'_>,
    sel_module: SelectedModuleInfo,
) {
    let Some(idx) = *state.selected_item else { return };

    let inner_mid = sel_module.as_ref().map(|(mid, _)| mid.as_str());
    let is_text_pin   = inner_mid == Some("module.label");
    let is_switch_pin = inner_mid == Some("module.switch");
    let is_input_viewer_pin = inner_mid == Some("module.input_viewer");
    // The 3D controller viewer shares the Input Viewer's style-override STORAGE
    // but has its own inspector (adds view angle / opacity / highlight fade).
    let is_c3d_pin = inner_mid == Some("display.controller3d");
    // Touch Zones / Virtual Menu pins: pad colour + visibility overrides. The
    // override only affects the "field" element's render; on cards/options
    // pins the controls are inert.
    let is_zone_pad_pin = matches!(
        inner_mid,
        Some("module.touch_zones") | Some("module.menu")
    );
    let is_graph_pin = matches!(
        inner_mid,
        Some("module.response_curve")
            | Some("module.vec_response_curve")
            | Some("module.twoway_response_curve")
            | Some("display.oscilloscope")
            | Some("display.vectorscope")
            | Some("display.trigscope")
            | Some("generator.envelope")
    );
    let graph_channels = sel_module.as_ref().map(|(_, ch)| *ch).unwrap_or(1);

    // Snapshot the primary's style BEFORE the inspector edits it (only when a
    // multi-selection is active — single-select needs no propagation).
    let multi: Vec<usize> = state.selected_items.clone();
    let before: Option<ItemStyle> = if multi.len() > 1 {
        state.items.get(idx).map(item_style_of)
    } else { None };

    if idx < state.items.len() {
        match &mut state.items[idx] {
            LayoutItem::Deco(_) => {
                decoration_inspector_strip_item(ui, state.items, idx);
            }
            LayoutItem::Module(_) if is_text_pin => {
                text_pin_inspector_strip_item(ui, state.items, idx);
            }
            LayoutItem::Module(_) if is_switch_pin => {
                switch_pin_inspector_strip_item(ui, state.items, idx);
            }
            LayoutItem::Module(_) if is_input_viewer_pin => {
                input_viewer_pin_inspector_strip_item(ui, state.items, idx);
            }
            LayoutItem::Module(_) if is_zone_pad_pin => {
                menu_pin_inspector_strip_item(ui, state.items, idx);
            }
            LayoutItem::Module(_) if is_c3d_pin => {
                controller3d_pin_inspector_strip_item(ui, state.items, idx);
            }
            LayoutItem::Module(_) if is_graph_pin => {
                graph_pin_inspector_strip_item(ui, state.items, idx, graph_channels);
            }
            _ => {}
        }
    } else {
        *state.selected_item = None;
    }

    // Diff the primary against its pre-edit snapshot and apply the changed
    // style fields to the rest of the selection.
    if let Some(before) = before {
        if let Some(after) = state.items.get(idx).map(item_style_of) {
            let changed = before.diff(&after);
            if changed.any() {
                for &j in multi.iter() {
                    if j == idx { continue; }
                    if let Some(it) = state.items.get_mut(j) {
                        changed.apply_to(it);
                    }
                }
            }
        }
    }
}

/// ctx-data slot holding a copied layout-item style for paste-across.
pub(crate) fn layout_style_clipboard_key() -> egui::Id {
    egui::Id::new("flexinput::layout_style_clipboard")
}

/// Shared per-item right-click context menu for the layout editor. Used for
/// both the selected (primary) item and non-selected items. Sets the various
/// pending-action flags the caller commits after the loop.
#[allow(clippy::too_many_arguments)]
pub(crate) fn layout_item_context_menu(
    ui: &mut egui::Ui,
    idx: usize,
    is_deco: bool,
    has_style_clip: bool,
    n_selected: usize,
    zaction: &mut Option<(usize, &'static str)>,
    delete_idx: &mut Option<usize>,
    dup_request: &mut bool,
    copy_style_from: &mut Option<usize>,
    paste_style: &mut bool,
    menu_header: &str,
) {
    ui.label(egui::RichText::new(menu_header).small().strong());
    ui.separator();
    if ui.button("Send to surface").clicked()   { *zaction = Some((idx, "top"));    ui.close(); }
    if ui.button("Step Up").clicked()           { *zaction = Some((idx, "up"));     ui.close(); }
    if ui.button("Step Down").clicked()         { *zaction = Some((idx, "down"));   ui.close(); }
    if ui.button("Send to background").clicked(){ *zaction = Some((idx, "bottom")); ui.close(); }
    ui.separator();
    // Style copy/paste. Copy stashes THIS item's style; paste applies the
    // clipboard to the whole selection (or this item if nothing else selected).
    if ui.button("Copy style").clicked() { *copy_style_from = Some(idx); ui.close(); }
    ui.add_enabled_ui(has_style_clip, |ui| {
        let label = if n_selected > 1 {
            format!("Paste style to {} selected", n_selected)
        } else {
            "Paste style".to_string()
        };
        if ui.button(label).clicked() { *paste_style = true; ui.close(); }
    });
    // Duplicate — decorations only (module widgets reference an inner node and
    // can't be cloned as standalone layout items). Duplicates the whole
    // decoration selection when multiple are selected.
    if is_deco {
        ui.separator();
        let dlabel = if n_selected > 1 {
            "Duplicate selection".to_string()
        } else {
            "Duplicate".to_string()
        };
        if ui.button(dlabel).clicked() { *dup_request = true; ui.close(); }
    }
    ui.separator();
    if ui.button("Unpin").clicked() { *delete_idx = Some(idx); ui.close(); }
}

/// Clamp a layout-resize candidate size for a Module pin to its no-crop
/// envelope, derived from the cached natural (scale-1.0, minimum-flex-width)
/// content size: width can't go below what the scale floor needs for the
/// text, and height can't demand a larger text scale than the width can hold.
/// This makes the resize handle itself respect "contents never crop out of
/// frame" — to get a taller row (bigger text) you first have to give it
/// enough width for the enlarged labels plus the flexible parts' minimum.
/// Pins without a cached natural (graphs, whole-module) are unconstrained.
pub(crate) fn clamp_pin_frame_to_content(
    ui: &egui::Ui,
    outer_id: NodeId,
    it: &LayoutItem,
    w: f32,
    h: f32,
) -> (f32, f32) {
    let LayoutItem::Module(m) = it else { return (w, h) };
    let ws_key = egui::Id::new(("pin_ws_nat", outer_id.0, m.inner_node_id, m.element_id.as_str()));
    let Some(nat) = ui.ctx().data(|d| d.get_temp::<egui::Vec2>(ws_key)) else { return (w, h) };
    if nat.x < 1.0 || nat.y < 1.0 { return (w, h); }
    // 0.5 is the scale floor in `apply_widget_scale`: any narrower and the
    // text can no longer shrink to fit the frame.
    let w = w.max(nat.x * 0.5);
    // The largest text scale this width can hold (4.0 = the global ceiling —
    // taller than that only adds empty space).
    let s_max = (w / nat.x).min(4.0);
    let h = h.clamp(nat.y * 0.5, nat.y * s_max);
    (w, h)
}

/// Apply a Z-order action against a `Vec<LayoutItem>` (paint order).
pub(crate) fn apply_zorder_action_items(items: &mut Vec<LayoutItem>, idx: usize, act: &str, n: usize) {
    match act {
        "up"     if idx + 1 < n => { items.swap(idx, idx + 1); }
        "down"   if idx > 0     => { items.swap(idx, idx - 1); }
        "top"    if idx + 1 < n => { let it = items.remove(idx); items.push(it); }
        "bottom" if idx > 0     => { let it = items.remove(idx); items.insert(0, it); }
        _ => {}
    }
}

/// Dispatches to the appropriate per-element renderer for a pinned element,
/// wrapping the dispatch with content-size measurement: after the element
/// renders, its content size (normalized back to scale 1.0) is cached per pin
/// so the next frame's `apply_widget_scale` fits the ACTUAL content to the
/// container instead of a hard-coded estimate. This is what keeps row widgets
/// scaling coherently with their frame and never cropping out of it.
pub(crate) fn render_pinned_element(
    inner_id: egui_snarl::NodeId,
    module_id: &str,
    element_id: &str,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container_size: egui::Vec2,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    panic_shortcut: &crate::app::PanicShortcut,
    automap_parent: Option<&AutomapGlowParent<'_>>,
    outer_snapshot: Option<&Snarl<NodeData>>,
    outer_id: NodeId,
    is_layout_mode: bool,
    graph_override: Option<crate::canvas::node::PinGraphOverride>,
    iv_style_override: Option<crate::canvas::node::IvStyleOverride>,
    menu_style_override: Option<crate::canvas::node::MenuStyleOverride>,
) {
    // Stable identity for this pin's natural-size cache: (outer node, inner
    // node, element). Two pins of the same element share one entry, which is
    // fine — they render identical content.
    let ws_key = egui::Id::new(("pin_ws_nat", outer_id.0, inner_id.0, element_id));
    ui.ctx().data_mut(|d| d.insert_temp(pin_ws_key_scratch(), ws_key));

    render_pinned_element_impl(
        inner_id, module_id, element_id, ui, inner_snarl, container_size,
        live_signals, panic_shortcut, automap_parent, outer_snapshot,
        outer_id, is_layout_mode, graph_override, iv_style_override,
        menu_style_override,
    );

    // `applied` is only present when the renderer routed through
    // `apply_widget_scale` (row-style widgets). Graphs / whole-module pins
    // size themselves to the container and are skipped.
    let applied: Option<f32> = ui.ctx().data(|d| d.get_temp(pin_ws_applied_scratch()));
    let stretch: f32 = ui.ctx().data(|d| d.get_temp(pin_ws_flex_scratch())).unwrap_or(0.0);
    ui.ctx().data_mut(|d| {
        d.remove::<egui::Id>(pin_ws_key_scratch());
        d.remove::<f32>(pin_ws_applied_scratch());
        d.remove::<egui::Vec2>(pin_ws_resolved_scratch());
        d.remove::<f32>(pin_ws_flex_scratch());
    });
    if let Some(scale) = applied {
        let measured = ui.min_rect().size();
        if measured.x > 4.0 && measured.y > 4.0 && scale > 0.0 {
            // Normalize back to scale 1.0, with any flexible-element stretch
            // removed so the cache holds the row's MINIMUM width.
            let nat = egui::vec2((measured.x - stretch).max(1.0), measured.y) / scale;
            let prev: Option<egui::Vec2> = ui.ctx().data(|d| d.get_temp(ws_key));
            // ~1px dead-band: font rasterization rounds a little differently
            // at each scale; without it the fit oscillates while resizing.
            if prev.map_or(true, |p| (p - nat).abs().max_elem() > 1.0) {
                ui.ctx().data_mut(|d| d.insert_temp(ws_key, nat));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_pinned_element_impl(
    inner_id: egui_snarl::NodeId,
    module_id: &str,
    element_id: &str,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container_size: egui::Vec2,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    panic_shortcut: &crate::app::PanicShortcut,
    automap_parent: Option<&AutomapGlowParent<'_>>,
    outer_snapshot: Option<&Snarl<NodeData>>,
    outer_id: NodeId,
    is_layout_mode: bool,
    graph_override: Option<crate::canvas::node::PinGraphOverride>,
    iv_style_override: Option<crate::canvas::node::IvStyleOverride>,
    menu_style_override: Option<crate::canvas::node::MenuStyleOverride>,
) {
    let cap_w = container_size.x.max(20.0);
    // Whole-module pinned renderers manage their own width/clip; don't cap
    // ahead of them.
    let is_whole_module = element_id == "whole_module";
    if !is_whole_module {
        ui.set_max_width(cap_w);
    }

    // ── Per-element renderers ────────────────────────────────────────────────
    // Build a parent frame describing THIS subpatch's boundary so the inner
    // module body can walk through its inlet → outer wire → device source.
    // Only meaningful for whole-module pins (other renderers don't read
    // upstream wiring); built lazily from `outer_snapshot` to avoid carrying
    // unused references when no whole-module pin is present.
    let bridged_parent_holder: Option<AutomapGlowParent<'_>> = match outer_snapshot {
        Some(outer_snarl) => Some(AutomapGlowParent {
            snarl: outer_snarl,
            subpatch_node_id: outer_id,
            prev: automap_parent,
        }),
        None => None,
    };
    let bridged_parent = bridged_parent_holder.as_ref();

    // Per-pin graph color override (Response Curve / Oscilloscope / Vectorscope).
    // Extracted from the already-cloned items vec at the call site and passed
    // directly, so we don't need an outer snarl snapshot for this path.
    let graph_ov_ref = graph_override.as_ref();

    // Record which element of this inner node is currently being rendered, so
    // `publish_nav_field_rects` (called from inside the row renderers, which only
    // receive `inner_id`) can key its rects by (inner, element). Without this,
    // every row of a multi-element module (gyro, curve, …) would publish to the
    // same inner-id key and the focused-field glow would land on whichever row
    // painted last — not the one being edited.
    ui.ctx().data_mut(|d| d.insert_temp(
        egui::Id::new(("gp_nav_cur_element", inner_id.0)), element_id.to_string()));

    match (module_id, element_id) {
        ("module.remapper", "whole_module") => {
            render_remapper_whole_module(
                inner_id, ui, inner_snarl, container_size,
                live_signals, panic_shortcut, bridged_parent, is_layout_mode,
            );
            return;
        }
        ("module.map_action", "whole_module") => {
            render_map_action_whole_module(
                inner_id, ui, inner_snarl, container_size,
                live_signals, panic_shortcut, bridged_parent, is_layout_mode,
            );
            return;
        }
        ("module.automap_combiner", "whole_module") => {
            render_combiner_whole_module(
                inner_id, ui, inner_snarl, container_size,
                live_signals, panic_shortcut, bridged_parent, is_layout_mode,
            );
            return;
        }
        ("module.network_send", "whole_module") => {
            render_net_whole_module(
                true, inner_id, ui, inner_snarl, container_size,
                live_signals, panic_shortcut, bridged_parent, is_layout_mode,
            );
            return;
        }
        ("module.network_recv", "whole_module") => {
            render_net_whole_module(
                false, inner_id, ui, inner_snarl, container_size,
                live_signals, panic_shortcut, bridged_parent, is_layout_mode,
            );
            return;
        }
        // Input Viewer board: whole-container render, letterboxed to the
        // board's fixed aspect. Pure display — no interaction when pinned.
        ("module.input_viewer", "viewer") => {
            let (rect, _) = ui.allocate_exact_size(container_size, egui::Sense::hover());
            let board = super::input_viewer::letterbox(rect);
            let skin_param = inner_snarl.get_node(inner_id)
                .and_then(|n| n.params.get("skin").and_then(|v| v.as_str()))
                .unwrap_or("auto").to_string();
            let skin = remapper_resolve_skin(inner_snarl, inner_id, &skin_param, bridged_parent);
            let dev = remapper_upstream_device_id(inner_snarl, inner_id, 0, bridged_parent);
            let style = super::input_viewer::IvStyle::from_override(iv_style_override.as_ref());
            super::input_viewer::paint_viewer_board(
                ui, board, inner_id.0, dev.as_deref(), skin, &style, live_signals,
            );
            return;
        }
        // 3D controller viewer: whole-container render, cropped/tinted per its
        // style override. Pure display — orientation from its Vec4 input mirror.
        ("display.controller3d", "viewer") => {
            let (rect, _) = ui.allocate_exact_size(container_size, egui::Sense::hover());
            let override_name = inner_snarl.get_node(inner_id)
                .and_then(|n| n.params.get("model").and_then(|v| v.as_str()))
                .unwrap_or("").to_string();
            let traced = inner_snarl.in_pin(InPinId { node: inner_id, input: 0 })
                .remotes.first().copied()
                .and_then(|src| controller3d_physical_device(inner_snarl, src, bridged_parent));
            let (dev_id, deadzone) = match traced {
                Some((d, z)) => (Some(d), z),
                None => (None, 0.1),
            };
            let resolved = if !override_name.is_empty() && override_name != "auto" {
                override_name
            } else if let Some(dev) = dev_id.as_deref() {
                crate::model::model_for_device(dev)
            } else {
                crate::model::available_models().into_iter().next().unwrap_or_default()
            };
            let orientation = inner_snarl.get_node(inner_id)
                .and_then(|n| n.extra.last_signals.get(1).copied().flatten())
                .and_then(|s| match s {
                    Signal::Vec4(v) => Some(glam::Quat::from_xyzw(v.x, v.y, v.z, v.w)),
                    _ => None,
                })
                .filter(|q| q.length_squared() > 1e-6)
                .map(|q| q.normalize())
                .unwrap_or(glam::Quat::IDENTITY);
            // Deferred materials edits from the layout/overlay inspector strip
            // (the strip has no snarl access, so requests ride egui temp
            // memory; we hold the &mut snarl and apply them here).
            let edit_id = egui::Id::new(("c3d_matedit", inner_id.0));
            let reset_id = egui::Id::new(("c3d_matreset", inner_id.0));
            let pending_edit =
                ui.ctx().data_mut(|d| d.remove_temp::<(String, [u8; 3])>(edit_id));
            let pending_reset = ui
                .ctx()
                .data_mut(|d| d.remove_temp::<bool>(reset_id))
                .unwrap_or(false);
            if pending_reset {
                if let Some(node) = inner_snarl.get_node_mut(inner_id) {
                    node.params.remove("materials");
                }
            } else if let Some((key, rgb)) = pending_edit {
                if let Some(node) = inner_snarl.get_node_mut(inner_id) {
                    let mut mats = node
                        .params
                        .get("materials")
                        .and_then(|v| v.as_object().cloned())
                        .unwrap_or_default();
                    mats.insert(key, serde_json::json!([rgb[0], rgb[1], rgb[2]]));
                    node.params
                        .insert("materials".into(), serde_json::Value::Object(mats));
                }
            }
            // Whole-scheme load (inspector Load… button).
            if let Some(map) = ui.ctx().data_mut(|d| {
                d.remove_temp::<serde_json::Map<String, serde_json::Value>>(
                    egui::Id::new(("c3d_matload", inner_id.0)),
                )
            }) {
                if let Some(node) = inner_snarl.get_node_mut(inner_id) {
                    node.params
                        .insert("materials".into(), serde_json::Value::Object(map));
                }
            }
            // Model swap (inspector chooser) — same keep-colours semantics as
            // the module body ("" = auto-detect from the device).
            if let Some(model_sel) = ui.ctx().data_mut(|d| {
                d.remove_temp::<String>(egui::Id::new(("c3d_modeledit", inner_id.0)))
            }) {
                let keep = inner_snarl
                    .get_node(inner_id)
                    .and_then(|n| n.params.get("keep_colors").and_then(|v| v.as_bool()))
                    .unwrap_or(false);
                if let Some(node) = inner_snarl.get_node_mut(inner_id) {
                    node.params
                        .insert("model".to_string(), serde_json::Value::String(model_sel));
                    if !keep {
                        node.params.remove("materials");
                    }
                }
            }
            // Publish the effective colours for the inspector strip to display.
            let cur_rgb = controller3d_scheme_rgb(inner_snarl, inner_id, &resolved);
            ui.ctx().data_mut(|d| {
                d.insert_temp(
                    egui::Id::new(("c3d_pub", inner_id.0)),
                    (resolved.clone(), cur_rgb),
                )
            });

            let (bg, outline, outline_w, accent) = controller3d_style(iv_style_override.as_ref());
            let (scheme, mut alpha, mut cam_pitch) =
                controller3d_scheme(inner_snarl, inner_id, &resolved);
            let mut tailoff = inner_snarl
                .get_node(inner_id)
                .and_then(|n| n.params.get("highlight_tailoff").and_then(|v| v.as_f64()))
                .unwrap_or(0.25) as f32;
            // Per-pin display overrides (view angle / opacity / fade /
            // composite) — each falls back to the module's own params (or
            // fully-opaque for composite) when unset.
            let mut composite = 1.0f32;
            if let Some(o) = iv_style_override.as_ref() {
                if let Some(p) = o.c3d_pitch {
                    cam_pitch = p.to_radians();
                }
                if let Some(a) = o.c3d_alpha {
                    alpha = a.clamp(0.0, 1.0);
                }
                if let Some(f) = o.c3d_fade {
                    tailoff = f;
                }
                if let Some(c) = o.c3d_composite {
                    composite = c.clamp(0.0, 1.0);
                }
            }
            let ctx = ui.ctx().clone();
            let live = controller3d_live(
                live_signals, dev_id.as_deref(), &ctx, inner_id.0, tailoff, accent, deadzone,
            );
            render_controller3d_core(
                ui, rect, &resolved, orientation, bg, outline, outline_w, scheme, alpha, cam_pitch,
                live, composite,
            );
            return;
        }
        // Touch Zones pad(s): whole-container render, move-only dividers + live
        // dots. Ports mode exposes no add/remove (that needs advanced wiring).
        // The Virtual Menu's field/cards share the same param schema and route
        // through the same pinned renderers.
        ("module.touch_zones", "field") | ("module.menu", "field") => {
            render_touch_zones_pinned(
                inner_id, ui, inner_snarl, container_size, live_signals,
                bridged_parent, menu_style_override.as_ref(), is_layout_mode,
            );
            return;
        }
        ("module.menu", "options") => {
            super::menu_body::render_menu_options_pinned(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("module.menu", "cards") => {
            render_touch_zone_cards_whole_module(
                inner_id, ui, inner_snarl, container_size,
                live_signals, panic_shortcut, bridged_parent, is_layout_mode,
            );
            return;
        }
        ("module.touch_zones", "cards") => {
            // Mapping-mode card list as a standalone pinnable widget — routed
            // through the shared whole-module renderer (scale + scroll + clip +
            // interaction) so it behaves exactly like the Remapper's pin.
            render_touch_zone_cards_whole_module(
                inner_id, ui, inner_snarl, container_size,
                live_signals, panic_shortcut, bridged_parent, is_layout_mode,
            );
            return;
        }
        // Knob slider: scaled-up slider taking the full container width.
        ("module.knob", "value") => {
            render_knob_value(inner_id, ui, inner_snarl, container_size);
            return;
        }
        // Constant: just the dragvalue, no other UI clutter.
        ("module.constant", "value") => {
            render_constant_value(inner_id, ui, inner_snarl, container_size);
            return;
        }
        // Dropdown: just the ComboBox, sized to the pinned container.
        ("module.dropdown", "selection") => {
            render_dropdown_selection(inner_id, ui, inner_snarl, container_size);
            return;
        }
        // Switch: just the toggle. Reads per-pin color overrides (fill /
        // outline / text per state) from the outer snapshot if present.
        ("module.switch", "toggle") => {
            render_switch_toggle(inner_id, ui, inner_snarl, container_size, outer_snapshot, outer_id);
            return;
        }
        // Text label: scaled (width) + cropped (height) with scroll, mirroring
        // Remapper's pin behavior. Per-pin color override is read inside the
        // renderer from `outer_snapshot`'s exposed_modules.
        ("module.label", "text") => {
            render_label_text_pinned_scroll(
                inner_id, ui, inner_snarl, container_size,
                outer_snapshot, outer_id, is_layout_mode,
            );
            return;
        }
        ("module.svg", "image") => {
            show_svg_body_sized(inner_id, ui, inner_snarl, container_size);
            return;
        }
        // Gyro 3DOF — pinable rows.
        // Legacy "mode" stays mapped to the new pointer-mode renderer so
        // patches saved before the split keep their pin working.
        ("processing.gyro_3dof", "mode") |
        ("processing.gyro_3dof", "pointer_mode") => {
            render_gyro_mode_row(inner_id, ui, inner_snarl, container_size, "pointer");
            return;
        }
        ("processing.gyro_3dof", "steering_mode") => {
            render_gyro_mode_row(inner_id, ui, inner_snarl, container_size, "steering");
            return;
        }
        ("processing.gyro_3dof", "steering_opts") => {
            render_gyro_steering_opts_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("processing.gyro_3dof", "lean_threshold") => {
            render_gyro_lean_threshold_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("processing.gyro_3dof", "gyro_invert") => {
            render_gyro_invert_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("processing.gyro_3dof", "accel_invert") => {
            render_accel_invert_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("processing.gyro_3dof", "lean_left") => {
            render_gyro_lean_section_pin(
                inner_id, ui, inner_snarl, container_size, "left",
                live_signals, panic_shortcut, bridged_parent, is_layout_mode,
            );
            return;
        }
        ("processing.gyro_3dof", "lean_right") => {
            render_gyro_lean_section_pin(
                inner_id, ui, inner_snarl, container_size, "right",
                live_signals, panic_shortcut, bridged_parent, is_layout_mode,
            );
            return;
        }
        // Response curve graphs (regular and Vec): render only the curve
        // canvas, no surrounding sliders.
        ("module.response_curve", "curve") => {
            render_response_curve_only(inner_id, ui, inner_snarl, container_size, false, graph_ov_ref);
            return;
        }
        ("module.vec_response_curve", "curve") => {
            render_response_curve_only(inner_id, ui, inner_snarl, container_size, true, graph_ov_ref);
            return;
        }
        ("module.response_curve", "scale_row") => {
            render_response_curve_scale_row(inner_id, ui, inner_snarl, container_size, false);
            return;
        }
        ("module.response_curve", "range_row") => {
            render_response_curve_range_row(inner_id, ui, inner_snarl, container_size, false);
            return;
        }
        ("module.response_curve", "grid_row") => {
            render_response_curve_grid_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("module.response_curve", "grid_options_row") => {
            render_response_curve_grid_options_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("module.vec_response_curve", "scale_row") => {
            render_response_curve_scale_row(inner_id, ui, inner_snarl, container_size, true);
            return;
        }
        ("module.vec_response_curve", "range_row") => {
            render_response_curve_range_row(inner_id, ui, inner_snarl, container_size, true);
            return;
        }
        ("module.vec_response_curve", "grid_row") => {
            render_response_curve_grid_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("module.vec_response_curve", "grid_options_row") => {
            render_response_curve_grid_options_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        // Vec Reshaper — each element renders as its own scaled widget.
        ("module.vec_reshape", "pad") => {
            render_vec_reshape_pad(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("module.vec_reshape", "curve") => {
            render_vec_reshape_curve(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("module.vec_reshape", "target_row") => {
            render_vec_reshape_target_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("module.vec_reshape", "options_row") => {
            render_vec_reshape_options_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("module.vec_reshape", "range_row") => {
            render_vec_reshape_range_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("module.vec_reshape", "grid_row") => {
            render_vec_reshape_grid_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("module.vec_reshape", "preset_row") => {
            render_vec_reshape_preset_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        // Two-way Response Curve
        ("module.twoway_response_curve", "curve") => {
            render_twoway_curve_only(inner_id, ui, inner_snarl, container_size, graph_ov_ref);
            return;
        }
        ("module.twoway_response_curve", "scale_row") => {
            render_response_curve_scale_row(inner_id, ui, inner_snarl, container_size, false);
            return;
        }
        ("module.twoway_response_curve", "range_row") => {
            render_response_curve_range_row(inner_id, ui, inner_snarl, container_size, false);
            return;
        }
        ("module.twoway_response_curve", "grid_row") => {
            render_response_curve_grid_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("module.twoway_response_curve", "grid_options_row") => {
            render_response_curve_grid_options_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("module.twoway_response_curve", "hyst_row") => {
            render_twoway_hyst_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("module.twoway_response_curve", "interp_row") => {
            render_twoway_interp_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("module.twoway_response_curve", "lane_toggle") => {
            render_twoway_lane_toggle(inner_id, ui, inner_snarl, container_size);
            return;
        }
        // Average / Delay / DC Filter — bare DragValue rows.
        ("module.average", "samples") => {
            render_dragvalue_param(inner_id, ui, inner_snarl, container_size,
                "Samples", "buf_size", 10.0, 1.0, 1.0..=10_000.0, None);
            return;
        }
        ("module.average", "spike_mad") => {
            render_dragvalue_param(inner_id, ui, inner_snarl, container_size,
                "Spike MAD", "spike_mad", 0.0, 0.1, 0.0..=20.0, Some(1));
            return;
        }
        ("module.delay", "ms") => {
            render_dragvalue_param(inner_id, ui, inner_snarl, container_size,
                "ms", "delay_ms", 100.0, 1.0, 0.0..=60_000.0, None);
            return;
        }
        ("module.dc_filter", "window_ms") => {
            render_dragvalue_param(inner_id, ui, inner_snarl, container_size,
                "Window ms", "window_ms", 500.0, 10.0, 10.0..=60_000.0, None);
            return;
        }
        ("module.dc_filter", "decay_ms") => {
            render_dragvalue_param(inner_id, ui, inner_snarl, container_size,
                "Decay ms", "decay_ms", 200.0, 10.0, 10.0..=60_000.0, None);
            return;
        }
        // Counter — per-row.
        ("logic.counter", "mode")       => { render_counter_mode(inner_id, ui, inner_snarl, container_size); return; }
        ("logic.counter", "range_mode") => { render_counter_range_mode(inner_id, ui, inner_snarl, container_size); return; }
        ("logic.counter", "step")       => { render_counter_step(inner_id, ui, inner_snarl, container_size); return; }
        ("logic.counter", "min_max")    => { render_counter_min_max(inner_id, ui, inner_snarl, container_size); return; }
        // Logic Delay — mode + time.
        ("logic.delay", "mode") => { render_logic_delay_mode(inner_id, ui, inner_snarl, container_size); return; }
        ("logic.delay", "time") => { render_logic_delay_time(inner_id, ui, inner_snarl, container_size); return; }
        // Oscillator — per-row + bare preview.
        ("generator.oscillator", "shape")   => { render_oscillator_shape(inner_id, ui, inner_snarl, container_size); return; }
        ("generator.oscillator", "freq")    => { render_oscillator_freq(inner_id, ui, inner_snarl, container_size);  return; }
        ("generator.oscillator", "phase")   => { render_oscillator_phase(inner_id, ui, inner_snarl, container_size); return; }
        ("generator.oscillator", "preview") => {
            render_oscillator_preview(inner_id, ui, inner_snarl, container_size);
            return;
        }
        // Envelope Generator — per-row.
        ("generator.envelope", "curve") => {
            render_envelope_curve_only(inner_id, ui, inner_snarl, container_size, graph_ov_ref);
            return;
        }
        ("generator.envelope", "time_row") => {
            render_envelope_time_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("generator.envelope", "mode_row") => {
            render_envelope_mode_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("generator.envelope", "sustain_row") => {
            render_envelope_sustain_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("generator.envelope", "grid_row") => {
            render_envelope_grid_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("generator.envelope", "grid_options_row") => {
            render_envelope_grid_options_row(inner_id, ui, inner_snarl, container_size);
            return;
        }
        // Audio Stream Haptics — the scope widget, or any single calibration row.
        ("module.audio_stream_haptics", "asth_scope") => {
            render_asth_pinned_scope(inner_id, ui, inner_snarl, container_size, bridged_parent);
            return;
        }
        ("module.audio_stream_haptics", "asth_mode_row") => {
            render_asth_pinned_mode(inner_id, ui, inner_snarl, container_size);
            return;
        }
        ("module.audio_stream_haptics", eid) if AsthRow::from_element_id(eid).is_some() => {
            render_asth_pinned_row(inner_id, eid, ui, inner_snarl, container_size);
            return;
        }
        // Readout — live value display, scaled to container.
        ("display.readout", "value") => {
            render_readout_value(inner_id, ui, inner_snarl, container_size);
            return;
        }
        // Oscilloscope — bare display + bare controls row.
        ("display.oscilloscope", "display") => {
            render_oscilloscope_display(inner_id, ui, inner_snarl, container_size, graph_ov_ref);
            return;
        }
        ("display.oscilloscope", "controls") => {
            render_oscilloscope_controls(inner_id, ui, inner_snarl, container_size);
            return;
        }
        // Vectorscope — bare display.
        ("display.vectorscope", "display") => {
            render_vectorscope_display(inner_id, ui, inner_snarl, container_size, graph_ov_ref);
            return;
        }
        // Trigger scope — bare display + bare controls row.
        ("display.trigscope", "display") => {
            render_trigscope_display(inner_id, ui, inner_snarl, container_size, graph_ov_ref);
            return;
        }
        ("display.trigscope", "controls") => {
            render_trigscope_controls(inner_id, ui, inner_snarl, container_size);
            return;
        }
        _ => {}
    }

    // ── Legacy / unknown element_id ─────────────────────────────────────────
    // Older patches stored `element_id == "default"` (whole-body pin). The new
    // model is per-element only, so render a placeholder asking the user to
    // re-pin via Layout mode rather than displaying a misleading body crop.
    let _ = container_size;
    let _ = inner_snarl;
    let _ = inner_id;
    let _ = module_id;
    let _ = element_id;
    ui.label(egui::RichText::new("Re-pin via Layout mode").small().weak());
}

// ── Whole-module pinned renderers (Remapper / Map Action) ─────────────────────
//
// Renders the full module body scaled to the user-chosen container width and
// vertically cropped to the container height. Content past the crop is reachable
// by mouse-wheel scrolling. On any change to the capture draft or mappings list
// the view auto-snaps back to the top so newly-detected input is always visible.
//
// Strategy: paint the body unscaled into a fresh layer at body-coords, then
// install a TSTransform on that layer (scale + translate) to project body-space
// onto container-space. Using a real layer transform (not `with_visual_transform`)
// is essential so pointer hits inside the body — Learn/Add/× buttons — map back
// correctly through the inverse transform.
//
// The inner module body reads its first input pin to detect a wired AutoMap
// source; we construct that InPin from the *inner* snarl so the body sees the
// same wiring it would when rendered inside the sub-patch editor.

const REMAP_DESIGN_W: f32 = 380.0;

fn remap_body_inputs_for(
    inner_id: NodeId,
    inner_snarl: &Snarl<NodeData>,
) -> Vec<InPin> {
    let n_in = inner_snarl.get_node(inner_id).map(|n| n.inputs.len()).unwrap_or(0);
    (0..n_in)
        .map(|i| inner_snarl.in_pin(InPinId { node: inner_id, input: i }))
        .collect()
}

/// Per-pinned-widget runtime state stashed in egui ctx data.
///   (scroll_offset, last_draft_hash, last_mappings_hash)
///
/// Hashes (not lengths) so swapping one captured button for another — same
/// count, different content — still triggers the auto-scroll-to-top.
type RemapPinState = (f32, u64, u64);

fn remap_hash_draft(node: &NodeData, with_output: bool) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for s in remapper_read_str_array(node, "draft_input") {
        s.hash(&mut h);
        0u8.hash(&mut h); // separator
    }
    if with_output {
        1u8.hash(&mut h);
        for s in remapper_read_str_array(node, "draft_output") {
            s.hash(&mut h);
            0u8.hash(&mut h);
        }
    }
    h.finish()
}

fn remap_hash_mappings(node: &NodeData) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if let Some(arr) = node.params.get("mappings").and_then(|v| v.as_array()) {
        for m in arr {
            // serde_json::Value already implements Hash via its variants for
            // strings/numbers/bools/null but not for arrays/objects directly.
            // Stringify for a stable fingerprint — patches are small enough
            // that the cost is negligible per frame.
            m.to_string().hash(&mut h);
            0u8.hash(&mut h);
        }
    }
    h.finish()
}

fn remap_pin_state_id(outer_layer: egui::LayerId, inner_id: NodeId, tag: &'static str) -> egui::Id {
    egui::Id::new(("fxi_remap_pin_state", outer_layer, inner_id.0, tag))
}

fn remap_layer_id(outer_layer: egui::LayerId, inner_id: NodeId, tag: &'static str) -> egui::LayerId {
    // Child layer order MUST match parent_ui.layer_id().order — egui's
    // set_sublayer debug_asserts on mismatched orders (panic message:
    // "Trying to set sublayers across layers of different order").
    // The CentralPanel that hosts the snarl canvas lives in
    // Order::Background, so hardcoding Middle here used to fire the
    // assert in debug builds whenever a sub-patch body rendered.
    egui::LayerId::new(
        outer_layer.order,
        egui::Id::new(("fxi_remap_pin_layer", outer_layer, inner_id.0, tag)),
    )
}

fn render_remapper_whole_module(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container_size: egui::Vec2,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    panic_shortcut: &crate::app::PanicShortcut,
    automap_parent: Option<&AutomapGlowParent<'_>>,
    is_layout_mode: bool,
) {
    render_remap_whole_module_impl(
        "remapper", REMAP_DESIGN_W, inner_id, ui, inner_snarl, container_size,
        live_signals, panic_shortcut, automap_parent, is_layout_mode,
        "Remapper",
        |id, ins, ui, sn, sigs, panic, am| {
            show_remapper_body(id, ins, ui, sn, sigs, panic, am);
        },
        remap_hash_mappings,
        |n| remap_hash_draft(n, true),
    );
}

fn render_map_action_whole_module(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container_size: egui::Vec2,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    panic_shortcut: &crate::app::PanicShortcut,
    automap_parent: Option<&AutomapGlowParent<'_>>,
    is_layout_mode: bool,
) {
    render_remap_whole_module_impl(
        "map_action", REMAP_DESIGN_W, inner_id, ui, inner_snarl, container_size,
        live_signals, panic_shortcut, automap_parent, is_layout_mode,
        "Map Action",
        |id, ins, ui, sn, sigs, panic, am| {
            show_map_action_body(id, ins, ui, sn, sigs, panic, am);
        },
        remap_hash_mappings,
        |n| remap_hash_draft(n, false),
    );
}

/// Hash of a Touch Zones node's committed zone mappings — re-bases the pinned
/// card list's scroll when a card is added/removed.
fn tz_cards_hash_map(n: &NodeData) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if let Some(v) = n.params.get("zone_maps") { v.to_string().hash(&mut h); }
    n.params.get("sel_zone").map(|v| v.to_string()).unwrap_or_default().hash(&mut h);
    h.finish()
}
/// Hash of the in-flight Learn capture (phase / trigger / picked output) — snaps
/// the scroll to top when a fresh capture begins, mirroring the Remapper draft.
fn tz_cards_hash_draft(n: &NodeData) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for k in ["_tz_phase", "_tz_trig", "_tz_draft_out"] {
        n.params.get(k).map(|v| v.to_string()).unwrap_or_default().hash(&mut h);
    }
    h.finish()
}

/// Pinned Touch Zones mapping-card list — same whole-module treatment (scale +
/// scroll + clip + interaction) the Remapper/Map Action pins use.
fn render_touch_zone_cards_whole_module(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container_size: egui::Vec2,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    panic_shortcut: &crate::app::PanicShortcut,
    automap_parent: Option<&AutomapGlowParent<'_>>,
    is_layout_mode: bool,
) {
    render_remap_whole_module_impl(
        "tz_cards", REMAP_DESIGN_W, inner_id, ui, inner_snarl, container_size,
        live_signals, panic_shortcut, automap_parent, is_layout_mode,
        "Touch Zones",
        |id, _ins, ui, sn, sigs, _panic, am| {
            let visuals = ui.visuals().clone();
            let accent = visuals.selection.bg_fill;
            ui.vertical(|ui| {
                render_touch_zone_cards(id, ui, sn, &visuals, accent, sigs, am);
            });
        },
        tz_cards_hash_map,
        tz_cards_hash_draft,
    );
}

/// Hash of the Combiner's resolution settings (input count + per-pin policy /
/// port overrides + per-port defaults) so the whole-module layout widget
/// re-bases its scroll when the config changes, mirroring the Remapper's
/// capture-hash behaviour.
fn combiner_hash_config(n: &NodeData) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    n.inputs.len().hash(&mut h);
    for key in ["combiner_pin_policy", "combiner_pin_port", "combiner_port_default"] {
        if let Some(v) = n.params.get(key) {
            v.to_string().hash(&mut h);
        }
    }
    h.finish()
}

fn render_combiner_whole_module(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container_size: egui::Vec2,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    panic_shortcut: &crate::app::PanicShortcut,
    automap_parent: Option<&AutomapGlowParent<'_>>,
    is_layout_mode: bool,
) {
    render_remap_whole_module_impl(
        "combiner", REMAP_DESIGN_W, inner_id, ui, inner_snarl, container_size,
        live_signals, panic_shortcut, automap_parent, is_layout_mode,
        "Combiner",
        |id, ins, ui, sn, sigs, _panic, _am| {
            show_automap_combiner_body(id, ins, ui, sn, sigs);
        },
        combiner_hash_config,
        combiner_hash_config,
    );
}

/// Design width for the Network Send/Receive whole-module pin — matches the
/// node body's `set_min_width(170)` with a little breathing room.
const NET_DESIGN_W: f32 = 184.0;

/// Hash of a Network node's config so the whole-module pin re-bases its scroll
/// when the transport / mode changes (mirrors the Combiner's config hash).
fn net_hash_config(n: &NodeData) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for key in ["net_transport", "net_host", "net_port", "net_peer", "net_keep"] {
        if let Some(v) = n.params.get(key) {
            v.to_string().hash(&mut h);
        }
    }
    h.finish()
}

fn render_net_whole_module(
    is_send: bool,
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container_size: egui::Vec2,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    panic_shortcut: &crate::app::PanicShortcut,
    automap_parent: Option<&AutomapGlowParent<'_>>,
    is_layout_mode: bool,
) {
    let tag = if is_send { "net_send" } else { "net_recv" };
    render_remap_whole_module_impl(
        tag, NET_DESIGN_W, inner_id, ui, inner_snarl, container_size,
        live_signals, panic_shortcut, automap_parent, is_layout_mode,
        if is_send { "Network Send" } else { "Network Receive" },
        move |id, _ins, ui, sn, _sigs, _panic, am| {
            if is_send {
                show_net_send_body(id, ui, sn, am);
            } else {
                show_net_recv_body(id, ui, sn, am);
            }
        },
        net_hash_config,
        net_hash_config,
    );
}

#[allow(clippy::too_many_arguments)]
fn render_remap_whole_module_impl<BodyFn, MapLenFn, DraftLenFn>(
    tag: &'static str,
    design_w: f32,
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container_size: egui::Vec2,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    panic_shortcut: &crate::app::PanicShortcut,
    automap_parent: Option<&AutomapGlowParent<'_>>,
    is_layout_mode: bool,
    placeholder_label: &'static str,
    body_fn: BodyFn,
    map_len_fn: MapLenFn,
    draft_len_fn: DraftLenFn,
)
where
    BodyFn: FnOnce(
        NodeId,
        &[InPin],
        &mut egui::Ui,
        &mut Snarl<NodeData>,
        &std::collections::HashMap<(String, String), Signal>,
        &crate::app::PanicShortcut,
        Option<&AutomapGlowParent<'_>>,
    ),
    MapLenFn:   Fn(&NodeData) -> u64,
    DraftLenFn: Fn(&NodeData) -> u64,
{
    let _ = placeholder_label; // kept on the call sig for future per-module styling

    // ── 1. Reserve the container area in the outer UI ───────────────────────
    // Use no sense — in lock mode the body layer handles interactions; in
    // layout mode the parent UI's drag/resize/right-click handles do.
    let (container_rect, _container_resp) = ui.allocate_exact_size(
        container_size,
        egui::Sense::hover(),
    );

    // Cap min sizes so the scale math stays sane.
    let container_w = container_size.x.max(40.0);
    let container_h = container_size.y.max(20.0);
    let scale = (container_w / design_w).clamp(0.25, 4.0);

    // ── 2. Detect "new capture" — compare current state vs last frame ───────
    // (Skip update in layout mode so the user's chosen scroll position is
    // preserved across layout/lock toggles.)
    //
    // The scroll only re-snaps to the top when *new input is detected* — i.e.
    // the capture draft changes (a freshly pressed gamepad/keyboard chord, or
    // the draft being cleared by Add). It deliberately does NOT re-snap when
    // the user edits an existing mapping (toggling press mode, dragging the
    // time-gap value, flipping hold/turbo) — those mutate the `mappings` array
    // but must leave the user's scroll position where it is. `cur_map_h` is
    // still tracked/persisted for potential future use, but does not gate the
    // re-snap. (For the Combiner, `draft_len_fn == map_len_fn`, so its
    // config-change rebase still fires through the draft path.)
    let state_key = remap_pin_state_id(ui.layer_id(), inner_id, tag);
    let (cur_draft_h, cur_map_h): (u64, u64) = inner_snarl.get_node(inner_id).map(|n| {
        (draft_len_fn(n), map_len_fn(n))
    }).unwrap_or((0, 0));
    let prev: Option<RemapPinState> = ui.ctx().data(|d| d.get_temp(state_key));
    let (prev_offset, prev_draft, _prev_map) = prev.unwrap_or((0.0, cur_draft_h, cur_map_h));
    let any_capture_change = (cur_draft_h != prev_draft) && !is_layout_mode;

    // ── 3. Compute pointer-over check via raw input (the body layer above
    //       intercepts the parent's hover Response, so we go to the source).
    //       Convert global pointer → parent-UI local via inverse layer xform.
    let parent_to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
        .unwrap_or(egui::emath::TSTransform::IDENTITY);
    let from_global = parent_to_global.inverse();
    let pointer_over = ui.ctx().input(|i| i.pointer.hover_pos())
        .map(|g| container_rect.contains(from_global * g))
        .unwrap_or(false);

    // ── 4. Compute scroll offset (in body-space px, before scaling) ─────────
    let mut scroll_offset_body = if any_capture_change { 0.0 } else { prev_offset };
    if pointer_over && !is_layout_mode {
        let wheel = ui.input(|i| i.smooth_scroll_delta.y);
        if wheel != 0.0 {
            scroll_offset_body -= wheel / scale;
        }
    }
    // Gamepad-nav scroll: the nav driver publishes a body-space scroll delta
    // (px) keyed by inner node id while the user is inside this widget
    // (RemapScroll level). Apply it directly — no pointer-over gate, since the
    // driver only targets the selected widget.
    if !is_layout_mode {
        let scroll_id = egui::Id::new(("gp_nav_remap_scroll", inner_id.0));
        let cur_pass = ui.ctx().cumulative_pass_nr();
        if let Some((pass, delta)) = ui.ctx().data(|d| d.get_temp::<(u64, f32)>(scroll_id)) {
            // Accept this frame's or last frame's stamp (driver runs before the
            // body paints, but allow a 1-frame lag for safety).
            if cur_pass.saturating_sub(pass) <= 1 {
                scroll_offset_body += delta;
            }
        }
    }

    // ── 4b. Auto-scroll while a card is being drag-reordered ────────────────
    // The body (rendered below) sets a per-body-layer flag while a mapping card
    // is being dragged. When the drag pointer nears the top/bottom edge of the
    // visible band, nudge the scroll so off-screen rows come into reach. We
    // read last frame's flag (the body renders after this point); the drag
    // spans many frames so the one-frame lag is invisible. A repaint is
    // requested so the scroll keeps advancing even with a stationary pointer.
    if !is_layout_mode {
        let drag_flag_id = egui::Id::new((
            "fxi_reorder_drag_active",
            remap_layer_id(ui.layer_id(), inner_id, tag),
        ));
        let drag_active = ui.ctx().data(|d| d.get_temp::<bool>(drag_flag_id)).unwrap_or(false);
        if drag_active {
            if let Some(g) = ui.ctx().input(|i| i.pointer.hover_pos()) {
                let local = from_global * g; // parent-UI/container coords
                // Edge band: within this many px of the container's top/bottom
                // triggers auto-scroll, ramping to `max_speed` at the very edge.
                let band = 28.0_f32.min(container_h * 0.4);
                let max_speed = 14.0_f32; // body px per frame at the edge
                let mut delta = 0.0;
                let dist_top = local.y - container_rect.top();
                let dist_bot = container_rect.bottom() - local.y;
                if dist_top < band {
                    let t = ((band - dist_top) / band).clamp(0.0, 1.0);
                    delta -= max_speed * t / scale;
                } else if dist_bot < band {
                    let t = ((band - dist_bot) / band).clamp(0.0, 1.0);
                    delta += max_speed * t / scale;
                }
                if delta != 0.0 {
                    scroll_offset_body += delta;
                    request_repaint_throttled(ui.ctx());
                    // Publish the applied scroll delta so the dragged card can
                    // add it to its visual lift and stay glued to the pointer
                    // while the body scrolls under it. (`begin` consumes it.)
                    let comp_id = egui::Id::new((
                        "fxi_reorder_scroll_comp",
                        remap_layer_id(ui.layer_id(), inner_id, tag),
                    ));
                    ui.ctx().data_mut(|d| d.insert_temp(comp_id, delta));
                }
            }
        }
    }

    // ── 5. Render the body — two paths depending on mode ────────────────────
    //
    // LOCK mode (live, interactive):
    //   Paint into a fresh transform layer; install a TSTransform that
    //   scales + scrolls + composes with the parent layer's transform.
    //   This is the only way to get true scaled visuals with working
    //   input routing.
    //
    // LAYOUT mode (preview, non-interactive):
    //   Use `ui.with_visual_transform` to scale visuals only — no layer,
    //   no input claim. Parent UI's drag / resize / right-click handles
    //   stay fully responsive because there is no competing layer above.
    let inputs = remap_body_inputs_for(inner_id, inner_snarl);
    let body_h: f32;

    if is_layout_mode {
        // Visual-only transform: scale around (0,0), then translate so
        // body-origin lands at `container_rect.min`. with_visual_transform
        // re-bases existing shape coords; we still pre-allocate a child
        // UI at body-coord origin (0,0) so widgets compute their rects in
        // a normalized space before the visual transform reapplies them.
        let xform = egui::emath::TSTransform::new(
            container_rect.min.to_vec2()
                - egui::vec2(0.0, scroll_offset_body * scale),
            scale,
        );
        let inner = ui.with_visual_transform(xform, |ui| {
            let body_max_rect = egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(design_w, 100_000.0),
            );
            let mut body_ui = ui.new_child(
                egui::UiBuilder::new().max_rect(body_max_rect),
            );
            // Clip to the visible band in body coords; with_visual_transform
            // will re-base these shapes into the container_rect on paint.
            let visible_band = egui::Rect::from_min_size(
                egui::pos2(0.0, scroll_offset_body),
                egui::vec2(design_w, container_h / scale),
            );
            body_ui.set_clip_rect(visible_band);
            body_ui.add_enabled_ui(false, |body_ui| {
                body_fn(
                    inner_id,
                    &inputs,
                    body_ui,
                    inner_snarl,
                    live_signals,
                    panic_shortcut,
                    automap_parent,
                );
            });
            body_ui.min_rect().height().max(1.0)
        });
        body_h = inner.inner;
    } else {
        let body_layer = remap_layer_id(ui.layer_id(), inner_id, tag);
        let body_max_rect = egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(REMAP_DESIGN_W, 100_000.0),
        );
        let mut body_ui = ui.new_child(
            egui::UiBuilder::new()
                .layer_id(body_layer)
                .max_rect(body_max_rect),
        );
        let visible_band = egui::Rect::from_min_size(
            egui::pos2(0.0, scroll_offset_body),
            egui::vec2(REMAP_DESIGN_W, container_h / scale),
        );
        // Intersect with the parent UI's clip rect mapped into body-local
        // coords. Without this, the body_layer paints into Order::Middle
        // and can escape the canvas viewport (spilling onto tab bars, side
        // panels, etc.) when the host node sits near the canvas edge.
        //
        // `ui.clip_rect()` is in PARENT-layer coords (not screen), so we
        // map it through only `local_xform.inverse()` — NOT through
        // `parent_to_global` — to reach body-local coords. Doing both
        // would over-transform and collapse the clip to an empty rect.
        let parent_clip_local = ui.clip_rect();
        let local_translation_preview = container_rect.min.to_vec2()
            - egui::vec2(0.0, scroll_offset_body * scale);
        let local_xform_preview = egui::emath::TSTransform::new(local_translation_preview, scale);
        let inv_local = local_xform_preview.inverse();
        let parent_clip_body = egui::Rect::from_min_max(
            inv_local * parent_clip_local.min,
            inv_local * parent_clip_local.max,
        );
        let final_clip = visible_band.intersect(parent_clip_body);
        body_ui.set_clip_rect(final_clip);

        body_fn(
            inner_id,
            &inputs,
            &mut body_ui,
            inner_snarl,
            live_signals,
            panic_shortcut,
            automap_parent,
        );
        body_h = body_ui.min_rect().height().max(1.0);

        // Clamp scroll offset using actual body height before painting chrome.
        let max_offset_body = (body_h - container_h / scale).max(0.0);
        if scroll_offset_body < 0.0 { scroll_offset_body = 0.0; }
        if scroll_offset_body > max_offset_body { scroll_offset_body = max_offset_body; }

        // ── Scrollbar — painted INTO the body layer so it shares the layer's
        //    z-order (always above the body widgets, never lost behind a
        //    sublayer). Coordinates are in body-space; we add `scroll_offset_body`
        //    to the Y so the scrollbar stays stationary on screen as the body
        //    scrolls (the body layer's translation includes -scroll_offset_body*scale).
        let mut new_scroll = scroll_offset_body;
        if max_offset_body > 0.5 {
            // Visible band in body coords.
            let band_top = scroll_offset_body;
            let band_h_body = container_h / scale;
            // Scrollbar geometry, all in body-coords. Convert pixel sizes to
            // body-coords by dividing by `scale` so the on-screen size stays
            // constant regardless of the user's zoom on the widget.
            let sb_w_body = 6.0 / scale;
            let sb_inset_body = 1.0 / scale;
            let track_x_min = design_w - sb_w_body - sb_inset_body;
            let track_y_min = band_top + sb_inset_body;
            let track_y_max = band_top + band_h_body - sb_inset_body;
            let track_h = (track_y_max - track_y_min).max(1.0);
            let track_rect = egui::Rect::from_min_max(
                egui::pos2(track_x_min, track_y_min),
                egui::pos2(track_x_min + sb_w_body, track_y_max),
            );

            let visible_frac = (band_h_body / body_h).clamp(0.05, 1.0);
            let min_thumb_body = 14.0 / scale;
            let thumb_h = (track_h * visible_frac).max(min_thumb_body);
            let scroll_frac = (scroll_offset_body / max_offset_body).clamp(0.0, 1.0);
            let thumb_y = track_y_min + (track_h - thumb_h) * scroll_frac;
            let thumb_rect = egui::Rect::from_min_size(
                egui::pos2(track_x_min, thumb_y),
                egui::vec2(sb_w_body, thumb_h),
            );

            // Interaction on the body layer at thumb_rect (body-coords).
            let drag_id = egui::Id::new(("fxi_remap_sb_drag", body_layer, inner_id.0));
            let thumb_resp = body_ui.interact(thumb_rect, drag_id, egui::Sense::click_and_drag());
            if thumb_resp.drag_started() {
                body_ui.ctx().data_mut(|d| d.insert_temp(drag_id, (scroll_offset_body, 0.0f32)));
            }
            if thumb_resp.dragged() {
                let track_travel = (track_h - thumb_h).max(1.0);
                // drag_delta is in body layer coords (already scale-adjusted by
                // the layer's inverse transform). track_travel in same coords.
                let body_per_track_px = max_offset_body / track_travel;
                let (start, acc) = body_ui.ctx().data(|d| d.get_temp::<(f32, f32)>(drag_id))
                    .unwrap_or((scroll_offset_body, 0.0));
                let new_acc = acc + thumb_resp.drag_delta().y;
                body_ui.ctx().data_mut(|d| d.insert_temp(drag_id, (start, new_acc)));
                new_scroll = (start + new_acc * body_per_track_px)
                    .clamp(0.0, max_offset_body);
            }
            if thumb_resp.drag_stopped() {
                body_ui.ctx().data_mut(|d| d.remove_temp::<(f32, f32)>(drag_id));
            }

            let painter = body_ui.painter();
            let track_col = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 14);
            painter.rect_filled(track_rect, 2.0 / scale, track_col);
            let thumb_col = if thumb_resp.dragged() {
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 180)
            } else if thumb_resp.hovered() {
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 140)
            } else {
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 90)
            };
            painter.rect_filled(thumb_rect, 2.0 / scale, thumb_col);
        }
        scroll_offset_body = new_scroll;

        let local_translation = container_rect.min.to_vec2()
            - egui::vec2(0.0, scroll_offset_body * scale);
        let local_xform = egui::emath::TSTransform::new(local_translation, scale);
        ui.ctx().set_transform_layer(body_layer, parent_to_global * local_xform);
        ui.ctx().set_sublayer(ui.layer_id(), body_layer);
    }

    // ── 6. Re-clamp scroll offset (in layout-mode path it isn't set above) ──
    let max_offset_body = (body_h - container_h / scale).max(0.0);
    if scroll_offset_body < 0.0 { scroll_offset_body = 0.0; }
    if scroll_offset_body > max_offset_body { scroll_offset_body = max_offset_body; }

    // ── 9. Persist updated state for next frame ─────────────────────────────
    ui.ctx().data_mut(|d| {
        d.insert_temp::<RemapPinState>(
            state_key,
            (scroll_offset_body, cur_draft_h, cur_map_h),
        );
    });
}


// ── Per-element pinned renderers ──────────────────────────────────────────────
//
// These render a single UI element of a module sized to the user's chosen
// container, with no extra controls / labels around it. They intentionally
// avoid exposing buttons that would mutate the inner module's I/O structure
// (e.g. add/remove pins, Learn, Clear unused).

fn render_knob_value(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (value, bipolar) = inner_snarl.get_node(inner_id).map(|n| {
        let v = n.params.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let b = n.params.get("bipolar").and_then(|v| v.as_bool()).unwrap_or(false);
        (v, b)
    }).unwrap_or((0.0, false));

    let (lo, hi) = if bipolar { (-1.0f32, 1.0f32) } else { (0.0f32, 1.0f32) };
    let mut v = value.clamp(lo, hi);

    let avail = egui::vec2(container.x.max(40.0), container.y.max(20.0));
    let aspect = avail.x / avail.y.max(1.0);
    let (rect, resp) = ui.allocate_exact_size(avail, egui::Sense::click_and_drag());

    let mut value_changed = false;
    if resp.double_clicked() {
        v = 0.0f32.clamp(lo, hi);
        value_changed = true;
    } else if resp.dragged() {
        let delta = resp.drag_delta();
        let range = hi - lo;
        let norm_delta = if aspect >= 2.0 { delta.x / rect.width() } else { -delta.y / rect.height() };
        v = (v + norm_delta * range).clamp(lo, hi);
        value_changed = true;
    }
    if resp.hovered() {
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll != 0.0 {
            v = (v + scroll * 0.005 * (hi - lo)).clamp(lo, hi);
            value_changed = true;
        }
    }

    let t = (v - lo) / (hi - lo);
    let painter = ui.painter_at(rect);
    let active = resp.hovered() || resp.dragged();
    if aspect >= 2.0 {
        draw_knob_h_fader(&painter, rect, t, bipolar, active);
    } else if aspect <= 0.5 {
        draw_knob_v_fader(&painter, rect, t, bipolar, active);
    } else {
        draw_knob_rotary(&painter, rect, t, bipolar, active);
    }

    if value_changed {
        if let Some(node) = inner_snarl.get_node_mut(inner_id) {
            if let Some(n) = Number::from_f64(v as f64) {
                node.params.insert("value".to_string(), Value::Number(n));
            }
        }
    }
}

fn render_constant_value(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let value = inner_snarl.get_node(inner_id)
        .and_then(|n| n.params.get("value").and_then(|v| v.as_f64()))
        .unwrap_or(0.0) as f32;
    let mut v = value;
    // Use the full container for the dragvalue; the box IS the whole pin, so
    // (like the readout) its text scales with the container height rather
    // than staying at theme size inside an ever-larger box.
    ui.set_max_width(container.x);
    let h = container.y.max(18.0);
    let font_scale = (h / 24.0).clamp(0.6, 3.5);
    if (font_scale - 1.0).abs() > 0.02 {
        for (_, font_id) in ui.style_mut().text_styles.iter_mut() {
            font_id.size = (font_id.size * font_scale).max(6.0);
        }
    }
    if ui.add_sized([container.x, h], egui::DragValue::new(&mut v).speed(0.01)).changed() {
        if let Some(node) = inner_snarl.get_node_mut(inner_id) {
            if let Some(n) = Number::from_f64(v as f64) {
                node.params.insert("value".to_string(), Value::Number(n));
            }
        }
    }
}

fn render_switch_toggle(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    outer_snapshot: Option<&Snarl<NodeData>>,
    outer_id: NodeId,
) {
    let active = inner_snarl.get_node(inner_id).map(read_switch_active).unwrap_or(false);
    let state = inner_snarl.get_node(inner_id)
        .map(|n| read_switch_state(n, active))
        .unwrap_or(SwitchState {
            caption: (if active { "ON" } else { "OFF" }).to_string(),
            svg_data: String::new(), svg_rev: 0, pos: CaptionPos::Right,
        });

    // Per-pin color override lookup from the outer sub-patch's items list.
    let override_ = outer_snapshot
        .and_then(|outer| outer.get_node(outer_id))
        .and_then(|n| n.subpatch.as_ref())
        .and_then(|sp| sp.items.iter().find_map(|it| match it {
            LayoutItem::Module(m)
                if m.inner_node_id == inner_id.0 && m.element_id == "toggle" =>
                m.switch_override.clone(),
            _ => None,
        }))
        .unwrap_or_default();

    // Resolve effective fill / outline / text colors. Override fields beat
    // theme defaults; defaults match the canvas-side body styling.
    let theme_fill = if active {
        ui.style().visuals.selection.bg_fill
    } else {
        ui.style().visuals.widgets.inactive.bg_fill
    };
    let theme_stroke = if active {
        ui.style().visuals.selection.stroke.color
    } else {
        ui.style().visuals.widgets.inactive.bg_stroke.color
    };
    let theme_text = if active {
        ui.style().visuals.strong_text_color()
    } else {
        ui.style().visuals.text_color()
    };
    let (ov_fill, ov_outline, ov_text) = if active {
        (override_.fill_on, override_.outline_on, override_.text_on)
    } else {
        (override_.fill_off, override_.outline_off, override_.text_off)
    };
    let fill_col    = ov_fill.map(rgba_to_color32).unwrap_or(theme_fill);
    let outline_col = ov_outline.map(rgba_to_color32).unwrap_or(theme_stroke);
    let text_col    = ov_text.map(rgba_to_color32).unwrap_or(theme_text);
    let outline_px  = override_.outline_px.unwrap_or(1.0);

    let avail = egui::vec2(container.x.max(24.0), container.y.max(16.0));
    let (rect, resp) = ui.allocate_exact_size(avail, egui::Sense::click());

    let painter = ui.painter_at(rect);
    painter.rect(rect, 4.0, fill_col,
        egui::Stroke::new(outline_px, outline_col), egui::StrokeKind::Inside);
    paint_switch_content(ui, rect, inner_id.0, &state, active, text_col);

    if resp.clicked() {
        if let Some(node) = inner_snarl.get_node_mut(inner_id) {
            switch_handle_click(node, active);
        }
    }
}

/// Pinned-Text renderer: scale by width, crop by height with scrollbar,
/// auto-scroll to top when the text content hash changes. Mirrors the
/// Remapper whole-module pin pattern. Per-pin color override is read by
/// finding this `inner_id` in `outer_snapshot`'s `exposed_modules` list.
fn render_label_text_pinned_scroll(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container_size: egui::Vec2,
    outer_snapshot: Option<&Snarl<NodeData>>,
    outer_id: NodeId,
    is_layout_mode: bool,
) {
    use std::hash::{Hash, Hasher};

    // ── 1. Resolve text + module-native styling ─────────────────────────────
    let (text, base_font, base_col) = inner_snarl.get_node(inner_id).map(|n| {
        let t = n.params.get("text").and_then(|v| v.as_str()).unwrap_or("Label").to_string();
        let f = n.params.get("font_size").and_then(|v| v.as_f64()).unwrap_or(14.0) as f32;
        let c = read_label_color(n);
        (t, f, c)
    }).unwrap_or_else(|| ("Label".to_string(), 14.0, egui::Color32::from_rgb(220, 220, 220)));

    // ── 2. Per-pin override lookup ──────────────────────────────────────────
    let override_ = outer_snapshot
        .and_then(|outer| outer.get_node(outer_id))
        .and_then(|n| n.subpatch.as_ref())
        .and_then(|sp| sp.exposed_modules.iter().find(|e|
            e.inner_node_id == inner_id.0 && e.element_id == "text"
        ))
        .and_then(|e| e.text_override.clone())
        .unwrap_or_default();
    let fill_col = override_.fill
        .map(rgba_to_color32)
        .unwrap_or(base_col);
    let outline_col = override_.outline.map(rgba_to_color32).unwrap_or(egui::Color32::TRANSPARENT);
    let outline_px = override_.outline_px.unwrap_or(0.0);

    // ── 3. Container reservation + scale ────────────────────────────────────
    let (container_rect, _) = ui.allocate_exact_size(container_size, egui::Sense::hover());
    let design_w: f32 = 200.0;
    let container_w = container_size.x.max(40.0);
    let container_h = container_size.y.max(16.0);
    let scale = (container_w / design_w).clamp(0.25, 4.0);

    // ── 4. State key (per layer + inner node) ───────────────────────────────
    let state_key = egui::Id::new(("fxi_label_pin_state", ui.layer_id().id, inner_id.0));
    type LabelPinState = (f32, u64); // (scroll_offset_body, text_hash)
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    base_font.to_bits().hash(&mut hasher);
    let cur_text_hash = hasher.finish();
    let prev: Option<LabelPinState> = ui.ctx().data(|d| d.get_temp(state_key));
    let (prev_offset, prev_hash) = prev.unwrap_or((0.0, cur_text_hash));
    let changed = (cur_text_hash != prev_hash) && !is_layout_mode;

    // ── 5. Pointer-over for wheel scroll ────────────────────────────────────
    let parent_to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
        .unwrap_or(egui::emath::TSTransform::IDENTITY);
    let from_global = parent_to_global.inverse();
    let pointer_over = ui.ctx().input(|i| i.pointer.hover_pos())
        .map(|g| container_rect.contains(from_global * g))
        .unwrap_or(false);

    let mut scroll_offset_body = if changed { 0.0 } else { prev_offset };
    if pointer_over && !is_layout_mode {
        let wheel = ui.input(|i| i.smooth_scroll_delta.y);
        if wheel != 0.0 {
            scroll_offset_body -= wheel / scale;
        }
    }

    // ── 6. Render — visual-transform path (layout) vs layer path (lock) ─────
    let render_body = |body_ui: &mut egui::Ui, content_w: f32| -> f32 {
        body_ui.set_max_width(content_w);
        // Optional 8-direction offset outline (cheap halo) via painter.layout_no_wrap-free wrap.
        if outline_col.a() > 0 && outline_px > 0.05 {
            let origin = body_ui.cursor().min;
            let galley = body_ui.painter().layout(
                text.clone(),
                egui::FontId::proportional(base_font),
                outline_col,
                content_w,
            );
            let painter = body_ui.painter().clone();
            for (dx, dy) in [(-1.0,0.0),(1.0,0.0),(0.0,-1.0),(0.0,1.0),
                             (-1.0,-1.0),(1.0,-1.0),(-1.0,1.0),(1.0,1.0)] {
                painter.galley(
                    origin + egui::vec2(dx * outline_px, dy * outline_px),
                    galley.clone(),
                    outline_col,
                );
            }
        }
        let resp = body_ui.label(
            egui::RichText::new(&text).size(base_font).color(fill_col),
        );
        resp.rect.height().max(1.0)
    };

    let body_h: f32;
    if is_layout_mode {
        let xform = egui::emath::TSTransform::new(
            container_rect.min.to_vec2() - egui::vec2(0.0, scroll_offset_body * scale),
            scale,
        );
        let inner = ui.with_visual_transform(xform, |ui| {
            let body_max_rect = egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(design_w, 100_000.0),
            );
            let mut body_ui = ui.new_child(egui::UiBuilder::new().max_rect(body_max_rect));
            let visible_band = egui::Rect::from_min_size(
                egui::pos2(0.0, scroll_offset_body),
                egui::vec2(design_w, container_h / scale),
            );
            body_ui.set_clip_rect(visible_band);
            body_ui.add_enabled_ui(false, |b| render_body(b, design_w))
                .inner
        });
        body_h = inner.inner;
    } else {
        let body_layer = egui::LayerId::new(
            // Match parent layer order — see remap_layer_id for rationale.
            ui.layer_id().order,
            egui::Id::new(("fxi_label_pin_layer", ui.layer_id().id, inner_id.0)),
        );
        let body_max_rect = egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(design_w, 100_000.0),
        );
        let mut body_ui = ui.new_child(
            egui::UiBuilder::new().layer_id(body_layer).max_rect(body_max_rect),
        );
        let visible_band = egui::Rect::from_min_size(
            egui::pos2(0.0, scroll_offset_body),
            egui::vec2(design_w, container_h / scale),
        );
        // Intersect with the parent UI's clip rect (parent-layer coords)
        // mapped into body-local via inverse local transform, so the
        // body_layer cannot spill outside the canvas viewport.
        let parent_clip_local = ui.clip_rect();
        let local_translation_preview = container_rect.min.to_vec2()
            - egui::vec2(0.0, scroll_offset_body * scale);
        let local_xform_preview = egui::emath::TSTransform::new(local_translation_preview, scale);
        let inv_local = local_xform_preview.inverse();
        let parent_clip_body = egui::Rect::from_min_max(
            inv_local * parent_clip_local.min,
            inv_local * parent_clip_local.max,
        );
        let final_clip = visible_band.intersect(parent_clip_body);
        body_ui.set_clip_rect(final_clip);
        body_h = render_body(&mut body_ui, design_w);

        let max_offset_body = (body_h - container_h / scale).max(0.0);
        if scroll_offset_body < 0.0 { scroll_offset_body = 0.0; }
        if scroll_offset_body > max_offset_body { scroll_offset_body = max_offset_body; }

        // Scrollbar painted into body layer with Y offset so it stays on screen.
        let mut new_scroll = scroll_offset_body;
        if max_offset_body > 0.5 {
            let band_top = scroll_offset_body;
            let band_h_body = container_h / scale;
            let sb_w_body = 6.0 / scale;
            let sb_inset_body = 1.0 / scale;
            let track_x_min = design_w - sb_w_body - sb_inset_body;
            let track_y_min = band_top + sb_inset_body;
            let track_y_max = band_top + band_h_body - sb_inset_body;
            let track_h = (track_y_max - track_y_min).max(1.0);
            let track_rect = egui::Rect::from_min_max(
                egui::pos2(track_x_min, track_y_min),
                egui::pos2(track_x_min + sb_w_body, track_y_max),
            );
            let visible_frac = (band_h_body / body_h).clamp(0.05, 1.0);
            let min_thumb_body = 14.0 / scale;
            let thumb_h = (track_h * visible_frac).max(min_thumb_body);
            let scroll_frac = (scroll_offset_body / max_offset_body).clamp(0.0, 1.0);
            let thumb_y = track_y_min + (track_h - thumb_h) * scroll_frac;
            let thumb_rect = egui::Rect::from_min_size(
                egui::pos2(track_x_min, thumb_y),
                egui::vec2(sb_w_body, thumb_h),
            );
            let drag_id = egui::Id::new(("fxi_label_sb_drag", body_layer, inner_id.0));
            let thumb_resp = body_ui.interact(thumb_rect, drag_id, egui::Sense::click_and_drag());
            if thumb_resp.drag_started() {
                body_ui.ctx().data_mut(|d| d.insert_temp(drag_id, (scroll_offset_body, 0.0f32)));
            }
            if thumb_resp.dragged() {
                let track_travel = (track_h - thumb_h).max(1.0);
                let body_per_track_px = max_offset_body / track_travel;
                let (start, acc) = body_ui.ctx().data(|d| d.get_temp::<(f32, f32)>(drag_id))
                    .unwrap_or((scroll_offset_body, 0.0));
                let new_acc = acc + thumb_resp.drag_delta().y;
                body_ui.ctx().data_mut(|d| d.insert_temp(drag_id, (start, new_acc)));
                new_scroll = (start + new_acc * body_per_track_px).clamp(0.0, max_offset_body);
            }
            if thumb_resp.drag_stopped() {
                body_ui.ctx().data_mut(|d| d.remove_temp::<(f32, f32)>(drag_id));
            }
            let painter = body_ui.painter();
            painter.rect_filled(track_rect, 2.0 / scale,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 14));
            let thumb_col = if thumb_resp.dragged() {
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 180)
            } else if thumb_resp.hovered() {
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 140)
            } else {
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 90)
            };
            painter.rect_filled(thumb_rect, 2.0 / scale, thumb_col);
        }
        scroll_offset_body = new_scroll;

        let local_translation = container_rect.min.to_vec2()
            - egui::vec2(0.0, scroll_offset_body * scale);
        let local_xform = egui::emath::TSTransform::new(local_translation, scale);
        ui.ctx().set_transform_layer(body_layer, parent_to_global * local_xform);
        ui.ctx().set_sublayer(ui.layer_id(), body_layer);
    }

    let max_offset_body = (body_h - container_h / scale).max(0.0);
    if scroll_offset_body < 0.0 { scroll_offset_body = 0.0; }
    if scroll_offset_body > max_offset_body { scroll_offset_body = max_offset_body; }

    ui.ctx().data_mut(|d| {
        d.insert_temp::<LabelPinState>(state_key, (scroll_offset_body, cur_text_hash));
    });
}

// ── Gyro 3DOF row renderers ──────────────────────────────────────────────────

fn render_gyro_mode_row(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    target_family: &str,
) {
    let (cur_family, cur_axis) = snarl.get_node(inner_id)
        .map(gyro_read_family_axis)
        .unwrap_or_else(|| ("pointer".into(), "pitch_yaw".into()));
    let mut family = cur_family;
    let mut axis = cur_axis;
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(220.0, 22.0));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (id, lbl) in GYRO_AXIS_OPTIONS {
            let selected = family == target_family && axis == id;
            if ui.selectable_label(selected, egui::RichText::new(lbl)).clicked() {
                family = target_family.to_string();
                axis   = id.to_string();
                changed = true;
            }
        }
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("family".into(), Value::String(family));
            node.params.insert("axis".into(),   Value::String(axis));
            node.params.remove("mode");
        }
    }
}

fn render_gyro_steering_opts_row(
    inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2,
) {
    let snap = snarl.get_node(inner_id);
    let mut exclude_y = snap.and_then(|n| n.params.get("steering_exclude_y").and_then(|v| v.as_bool())).unwrap_or(false);
    let mut strength  = snap.and_then(|n| n.params.get("recenter_strength").and_then(|v| v.as_f64())).unwrap_or(0.0) as f32;
    let mut ease      = snap.and_then(|n| n.params.get("reset_ease_in").and_then(|v| v.as_f64())).unwrap_or(0.25) as f32;
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(220.0, 22.0));
    let mut fr = [egui::Rect::NOTHING; 3];
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let r = ui.checkbox(&mut exclude_y, egui::RichText::new("excl. Y"));
        fr[0] = r.rect; changed |= r.changed();
        ui.label(egui::RichText::new("re-center").weak());
        let r = ui.add(egui::DragValue::new(&mut strength).speed(0.05).range(0.0..=4.0).suffix(" /s"));
        fr[1] = r.rect; changed |= r.changed();
        ui.label(egui::RichText::new("ease").weak());
        let r = ui.add(egui::DragValue::new(&mut ease).speed(0.05).range(0.0..=2.0).suffix(" s"));
        fr[2] = r.rect; changed |= r.changed();
    });
    publish_nav_field_rects(ui, inner_id, &fr);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("steering_exclude_y".into(), Value::Bool(exclude_y));
            node.params.remove("recenter_blend");
            node.params.insert("recenter_strength".into(),
                serde_json::Number::from_f64(strength as f64).map(Value::Number).unwrap_or(Value::Null));
            node.params.insert("reset_ease_in".into(),
                serde_json::Number::from_f64(ease as f64).map(Value::Number).unwrap_or(Value::Null));
        }
    }
}

/// Layout-pin renderer for a single lean section (left or right). Reuses
/// the same transform-layer / scaled-content / custom-scrollbar machinery
/// as Remapper's whole-module pin via `render_remap_whole_module_impl`.
/// The body callback curries `side` and forwards to `show_gyro_lean_mapping_section`.
fn render_gyro_lean_section_pin(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container_size: egui::Vec2,
    side: &'static str,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    panic_shortcut: &crate::app::PanicShortcut,
    bridged_parent: Option<&AutomapGlowParent<'_>>,
    is_layout_mode: bool,
) {
    let tag: &'static str = if side == "left" { "lean_l" } else { "lean_r" };
    let mappings_key = if side == "left" { "lean_left" } else { "lean_right" };
    let draft_key    = if side == "left" { "_lean_left_draft" } else { "_lean_right_draft" };

    let hash_mappings = move |n: &NodeData| -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        if let Some(arr) = n.params.get(mappings_key).and_then(|v| v.as_array()) {
            for m in arr {
                m.to_string().hash(&mut h);
                0u8.hash(&mut h);
            }
        }
        h.finish()
    };
    let hash_draft = move |n: &NodeData| -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for s in remapper_read_str_array(n, draft_key) {
            s.hash(&mut h);
            0u8.hash(&mut h);
        }
        h.finish()
    };

    render_remap_whole_module_impl(
        tag, REMAP_DESIGN_W, inner_id, ui, inner_snarl, container_size,
        live_signals, panic_shortcut, bridged_parent, is_layout_mode,
        "Lean section",
        move |id, ins, ui, sn, sigs, panic, am| {
            show_gyro_lean_mapping_section(id, ui, sn, side, ins, sigs, panic, am);
        },
        hash_mappings,
        hash_draft,
    );
}

fn render_gyro_lean_threshold_row(
    inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2,
) {
    let snap = snarl.get_node(inner_id);
    let mut threshold = snap.and_then(|n| n.params.get("lean_threshold").and_then(|v| v.as_f64())).unwrap_or(0.3) as f32;
    let lean_v = snap.and_then(|n| match n.extra.last_signals.get(3) { Some(Some(Signal::Float(f))) => Some(*f), _ => None }).unwrap_or(0.0);
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(180.0, 22.0));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(egui::RichText::new("Lean").weak());
        changed |= ui.add(egui::DragValue::new(&mut threshold).speed(0.02).range(0.01..=4.0)).changed();
        ui.label(egui::RichText::new(format!("({:+.2})", lean_v)).weak());
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("lean_threshold".into(),
                serde_json::Number::from_f64(threshold as f64).map(Value::Number).unwrap_or(Value::Null));
        }
    }
}

fn render_gyro_invert_row(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let (mut yaw, mut pitch, mut roll) = snarl.get_node(inner_id).map(|n| {
        (
            n.params.get("inv_yaw").and_then(|v| v.as_bool()).unwrap_or(false),
            n.params.get("inv_pitch").and_then(|v| v.as_bool()).unwrap_or(false),
            n.params.get("inv_roll").and_then(|v| v.as_bool()).unwrap_or(false),
        )
    }).unwrap_or_default();
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(180.0, 22.0));
    let mut fr = [egui::Rect::NOTHING; 3];
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.label(egui::RichText::new("Gyr:").weak());
        let r = ui.checkbox(&mut yaw,   egui::RichText::new("yaw"));   fr[0] = r.rect; changed |= r.changed();
        let r = ui.checkbox(&mut pitch, egui::RichText::new("pitch")); fr[1] = r.rect; changed |= r.changed();
        let r = ui.checkbox(&mut roll,  egui::RichText::new("roll"));  fr[2] = r.rect; changed |= r.changed();
    });
    publish_nav_field_rects(ui, inner_id, &fr);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("inv_yaw".into(),   Value::Bool(yaw));
            node.params.insert("inv_pitch".into(), Value::Bool(pitch));
            node.params.insert("inv_roll".into(),  Value::Bool(roll));
        }
    }
}

fn render_accel_invert_row(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let (mut x, mut y, mut z) = snarl.get_node(inner_id).map(|n| {
        (
            n.params.get("inv_accel_x").and_then(|v| v.as_bool()).unwrap_or(false),
            n.params.get("inv_accel_y").and_then(|v| v.as_bool()).unwrap_or(false),
            n.params.get("inv_accel_z").and_then(|v| v.as_bool()).unwrap_or(false),
        )
    }).unwrap_or_default();
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(160.0, 22.0));
    let mut fr = [egui::Rect::NOTHING; 3];
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.label(egui::RichText::new("Acc:").weak());
        let r = ui.checkbox(&mut x, egui::RichText::new("X"));  fr[0] = r.rect; changed |= r.changed();
        let r = ui.checkbox(&mut y, egui::RichText::new("Y"));  fr[1] = r.rect; changed |= r.changed();
        let r = ui.checkbox(&mut z, egui::RichText::new("+Z")); fr[2] = r.rect; changed |= r.changed();
    });
    publish_nav_field_rects(ui, inner_id, &fr);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("inv_accel_x".into(), Value::Bool(x));
            node.params.insert("inv_accel_y".into(), Value::Bool(y));
            node.params.insert("inv_accel_z".into(), Value::Bool(z));
        }
    }
}


// ── Average / Delay / DC Filter ───────────────────────────────────────────────

fn render_dragvalue_param(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    label: &str,
    param: &str,
    default: f32,
    speed: f64,
    range: std::ops::RangeInclusive<f32>,
    max_decimals: Option<usize>,
) {
    let cur = snarl.get_node(inner_id)
        .and_then(|n| n.params.get(param).and_then(|v| v.as_f64()))
        .unwrap_or(default as f64) as f32;
    let mut v = cur;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(120.0, 22.0));
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).weak());
        let mut dv = egui::DragValue::new(&mut v).speed(speed).range(range);
        if let Some(d) = max_decimals { dv = dv.max_decimals(d); }
        // The value box is the row's flexible element: it fills the surplus
        // width while its height (and text) tracks the scaled row metrics —
        // sizing it to the container gave a huge box with tiny text in it.
        let w = pin_flex_width(ui, container, 64.0);
        let h = ui.spacing().interact_size.y;
        if ui.add_sized([w, h], dv).changed() {
            if let (Some(node), Some(n)) = (
                snarl.get_node_mut(inner_id),
                Number::from_f64(v as f64),
            ) {
                node.params.insert(param.into(), Value::Number(n));
            }
        }
    });
}

// ── Counter ───────────────────────────────────────────────────────────────────

fn render_counter_mode(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let mut mode = snarl.get_node(inner_id)
        .and_then(|n| n.params.get("mode").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| "loop".to_string());
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(220.0, 22.0));
    ui.horizontal(|ui| {
        for (lbl, id) in [("Loop", "loop"), ("Limit", "limit"), ("Bounce", "bounce"), ("Unlimited", "unlimited")] {
            changed |= ui.selectable_value(&mut mode, id.to_string(), egui::RichText::new(lbl)).changed();
        }
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("mode".into(), Value::String(mode));
        }
    }
}

fn render_counter_range_mode(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let mut normalized = snarl.get_node(inner_id)
        .and_then(|n| n.params.get("normalized").and_then(|v| v.as_bool()))
        .unwrap_or(false);
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(140.0, 22.0));
    ui.horizontal(|ui| {
        changed |= ui.selectable_value(&mut normalized, false, egui::RichText::new("Raw")).changed();
        changed |= ui.selectable_value(&mut normalized, true,  egui::RichText::new("0..1")).changed();
        if ui.small_button("↺").on_hover_text("Reset counter").clicked() {
            if let Some(node) = snarl.get_node_mut(inner_id) {
                while node.extra.aux_f32.len() < 2 { node.extra.aux_f32.push(0.0); }
                node.extra.aux_f32[0] = 0.0;
                node.extra.aux_f32[1] = 1.0;
                node.extra.aux_f32_dirty = true;
            }
        }
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("normalized".into(), Value::Bool(normalized));
        }
    }
}

fn render_counter_step(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let mut step = snarl.get_node(inner_id)
        .and_then(|n| n.params.get("step_param").and_then(|v| v.as_f64()))
        .unwrap_or(1.0) as f32;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(120.0, 22.0));
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Step").weak());
        if ui.add(egui::DragValue::new(&mut step).speed(0.1).range(0.001..=10000.0)).changed() {
            if let (Some(node), Some(n)) = (snarl.get_node_mut(inner_id), Number::from_f64(step as f64)) {
                node.params.insert("step_param".into(), Value::Number(n));
            }
        }
    });
}

fn render_counter_min_max(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let (mut min_p, mut max_p, mode) = snarl.get_node(inner_id).map(|n| {
        let mn = n.params.get("min_param").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let mx = n.params.get("max_param").and_then(|v| v.as_f64()).unwrap_or(10.0) as f32;
        let md = n.params.get("mode").and_then(|v| v.as_str()).unwrap_or("loop").to_string();
        (mn, mx, md)
    }).unwrap_or((0.0, 10.0, "loop".to_string()));
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(180.0, 22.0));
    let mut fr = [egui::Rect::NOTHING; 2];
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Min").weak());
        let r = ui.add(egui::DragValue::new(&mut min_p).speed(0.1));
        fr[0] = r.rect; changed |= r.changed();
        ui.label(egui::RichText::new("Max").weak());
        let r = ui.add_enabled(mode != "unlimited", egui::DragValue::new(&mut max_p).speed(0.1));
        fr[1] = r.rect; changed |= r.changed();
    });
    publish_nav_field_rects(ui, inner_id, &fr);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            if let Some(n) = Number::from_f64(min_p as f64) { node.params.insert("min_param".into(), Value::Number(n)); }
            if let Some(n) = Number::from_f64(max_p as f64) { node.params.insert("max_param".into(), Value::Number(n)); }
        }
    }
}

// ── Logic Delay ───────────────────────────────────────────────────────────────

fn render_logic_delay_mode(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let mut mode = snarl.get_node(inner_id)
        .and_then(|n| n.params.get("mode").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| "delay_false".to_string());
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(180.0, 22.0));
    ui.horizontal(|ui| {
        changed |= ui.selectable_value(&mut mode, "delay_true".into(),  egui::RichText::new("Delay ON")).changed();
        changed |= ui.selectable_value(&mut mode, "delay_false".into(), egui::RichText::new("Delay OFF")).changed();
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("mode".into(), Value::String(mode));
        }
    }
}

fn render_logic_delay_time(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let (mut time, mut unit) = snarl.get_node(inner_id).map(|n| {
        let t = n.params.get("time").and_then(|v| v.as_f64()).unwrap_or(100.0) as f32;
        let u = n.params.get("unit").and_then(|v| v.as_str()).unwrap_or("ms").to_string();
        (t, u)
    }).unwrap_or((100.0, "ms".to_string()));
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(200.0, 22.0));
    ui.horizontal(|ui| {
        let limit = if unit == "ms" { 60_000.0 } else { 10_000.0 };
        changed |= ui.add(egui::DragValue::new(&mut time).speed(1.0).range(0.0..=limit)).changed();
        changed |= ui.selectable_value(&mut unit, "ms".into(),      egui::RichText::new("ms")).changed();
        changed |= ui.selectable_value(&mut unit, "samples".into(), egui::RichText::new("frames")).changed();
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("unit".into(), Value::String(unit));
            if let Some(n) = Number::from_f64(time as f64) { node.params.insert("time".into(), Value::Number(n)); }
        }
    }
}

// ── Oscillator ────────────────────────────────────────────────────────────────

fn render_oscillator_shape(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let mut shape = snarl.get_node(inner_id)
        .and_then(|n| n.params.get("shape").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| "sine".to_string());
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(180.0, 22.0));
    ui.horizontal(|ui| {
        changed |= ui.selectable_value(&mut shape, "sine".into(),     egui::RichText::new("Sine")).changed();
        changed |= ui.selectable_value(&mut shape, "triangle".into(), egui::RichText::new("Tri")).changed();
        changed |= ui.selectable_value(&mut shape, "saw".into(),      egui::RichText::new("Saw")).changed();
        changed |= ui.selectable_value(&mut shape, "square".into(),   egui::RichText::new("Sqr")).changed();
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("shape".into(), Value::String(shape));
        }
    }
}

fn render_oscillator_freq(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let (mut freq_unit, mut freq_p) = snarl.get_node(inner_id).map(|n| {
        let u = n.params.get("freq_unit").and_then(|v| v.as_str()).unwrap_or("hz").to_string();
        let f = n.params.get("freq_param").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
        (u, f)
    }).unwrap_or(("hz".to_string(), 1.0));
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(180.0, 22.0));
    ui.horizontal(|ui| {
        changed |= ui.selectable_value(&mut freq_unit, "hz".into(), egui::RichText::new("Hz")).changed();
        changed |= ui.selectable_value(&mut freq_unit, "ms".into(), egui::RichText::new("ms")).changed();
        let (lo, hi, spd) = if freq_unit == "hz" { (0.01, 200.0, 0.1) } else { (1.0, 60_000.0, 10.0) };
        changed |= ui.add(egui::DragValue::new(&mut freq_p).speed(spd).range(lo..=hi)).changed();
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("freq_unit".into(), Value::String(freq_unit));
            if let Some(n) = Number::from_f64(freq_p as f64) { node.params.insert("freq_param".into(), Value::Number(n)); }
        }
    }
}

fn render_oscillator_phase(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let (mut phase_p, mut bipolar) = snarl.get_node(inner_id).map(|n| {
        let p = n.params.get("phase_param").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let b = n.params.get("bipolar").and_then(|v| v.as_bool()).unwrap_or(true);
        (p, b)
    }).unwrap_or((0.0, true));
    let mut changed = false;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(220.0, 22.0));
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Phase").weak());
        changed |= ui.add(egui::DragValue::new(&mut phase_p).speed(0.01).range(0.0..=1.0)).changed();
        ui.separator();
        changed |= ui.selectable_value(&mut bipolar, true,  egui::RichText::new("Bi")).changed();
        changed |= ui.selectable_value(&mut bipolar, false, egui::RichText::new("Uni")).changed();
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("bipolar".into(), Value::Bool(bipolar));
            if let Some(n) = Number::from_f64(phase_p as f64) { node.params.insert("phase_param".into(), Value::Number(n)); }
        }
    }
}

fn render_oscillator_preview(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (shape, phase_p, bipolar) = snarl.get_node(inner_id).map(|n| {
        let s = n.params.get("shape").and_then(|v| v.as_str()).unwrap_or("sine").to_string();
        let p = n.params.get("phase_param").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let b = n.params.get("bipolar").and_then(|v| v.as_bool()).unwrap_or(true);
        (s, p, b)
    }).unwrap_or(("sine".to_string(), 0.0, true));

    let avail = egui::vec2(container.x.max(40.0), container.y.max(20.0));
    let (rect, _) = ui.allocate_exact_size(avail, egui::Sense::hover());
    if !ui.is_rect_visible(rect) { return; }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, egui::Color32::from_gray(18));
    let zero_y = if bipolar { rect.center().y } else { rect.bottom() };
    painter.line_segment(
        [egui::pos2(rect.left(), zero_y), egui::pos2(rect.right(), zero_y)],
        egui::Stroke::new(0.5, egui::Color32::from_gray(55)),
    );
    let n = 128usize;
    let pts: Vec<egui::Pos2> = (0..=n).map(|i| {
        let t = i as f32 / n as f32;
        let phase = (t + phase_p).rem_euclid(1.0);
        let v = {
            let raw = flexinput_engine::osc_sample(&shape, phase);
            if bipolar { raw } else { (raw + 1.0) * 0.5 }
        };
        let x = rect.left() + t * rect.width();
        let y = if bipolar {
            rect.center().y - v * rect.height() * 0.45
        } else {
            rect.bottom() - v * rect.height() * 0.9
        };
        egui::pos2(x, y.clamp(rect.top(), rect.bottom()))
    }).collect();
    painter.add(egui::Shape::line(pts, egui::Stroke::new(1.5, egui::Color32::from_rgb(100, 180, 255))));
}





/// Estimate the device-source body width using small-text label measurement,
/// matching the styling that `show_input` / `show_output` use. Padding
/// constants are intentionally **small** so the header chip lands just
/// inside the body's right edge rather than pushing the body wider.
fn estimate_device_body_width(ui: &egui::Ui, node: &NodeData) -> f32 {
    let font = egui::TextStyle::Small.resolve(ui.style());
    let measure = |s: &str| ui.painter()
        .layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE)
        .size().x;
    let in_w  = node.inputs.iter().map(|p| measure(&p.name)).fold(0.0_f32, f32::max);
    let out_w = node.outputs.iter()
        .filter(|p| p.name != "Auto-Map")
        .map(|p| measure(&p.name)).fold(0.0_f32, f32::max);
    // snarl: pin_size = interact_size.y * 0.6 (~11 px), so each side reserves
    // pin_size + label. Inner gap ≈ item_spacing.x. Underestimate by a few
    // pixels so we never push the body wider than it naturally wants.
    let pin_size = ui.spacing().interact_size.y * 0.6;
    let gap      = ui.spacing().item_spacing.x;
    in_w + out_w + pin_size * 2.0 + gap
}







// ── Layout decorations ──────────────────────────────────────────────────────
//
// Decorations are static body items (text labels, SVGs, basic shapes) drawn
// under exposed-module pins. They live on `UiSubPatch::decorations` and are
// edited only in Layout mode via the toolbar + inspector strip.

const DECO_DEFAULT_FILL:    [u8; 4] = [200, 200, 200, 220];
const DECO_DEFAULT_STROKE:  [u8; 4] = [255, 255, 255, 220];

pub(crate) fn make_default_decoration(kind: &str) -> LayoutDecoration {
    match kind {
        "text" => LayoutDecoration::Text {
            pos: [16.0, 16.0],
            size: [160.0, 28.0],
            text: "Text".to_string(),
            font_size: 16.0,
            fill: DECO_DEFAULT_FILL,
            outline: [0, 0, 0, 0],
            outline_px: 0.0,
            align: TextAlign::Left,
            valign: crate::canvas::node::TextVAlign::Top,
        },
        "rect" => LayoutDecoration::Rect {
            pos: [16.0, 16.0],
            size: [120.0, 80.0],
            fill: [60, 60, 60, 180],
            stroke: DECO_DEFAULT_STROKE,
            stroke_px: 1.0,
            corner_radius: 4.0,
        },
        "ellipse" => LayoutDecoration::Ellipse {
            pos: [16.0, 16.0],
            size: [100.0, 100.0],
            fill: [60, 60, 60, 180],
            stroke: DECO_DEFAULT_STROKE,
            stroke_px: 1.0,
        },
        "line" => LayoutDecoration::Line {
            a: [16.0, 16.0],
            b: [136.0, 16.0],
            stroke: DECO_DEFAULT_STROKE,
            stroke_px: 1.5,
        },
        "svg" => LayoutDecoration::Svg {
            pos: [16.0, 16.0],
            size: [120.0, 120.0],
            svg_data: String::new(),
            rev: 0,
            tint: [255, 255, 255, 0],
            tint_mode: "override".to_string(),
            stroke: [0, 0, 0, 0],
            stroke_px: 0.0,
        },
        _ => LayoutDecoration::Rect {
            pos: [16.0, 16.0],
            size: [120.0, 80.0],
            fill: DECO_DEFAULT_FILL,
            stroke: DECO_DEFAULT_STROKE,
            stroke_px: 1.0,
            corner_radius: 4.0,
        },
    }
}

fn rgba_to_color32(c: [u8; 4]) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
}

fn color_button(ui: &mut egui::Ui, label: &str, rgba: &mut [u8; 4]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).small().weak());
        // Overlay decorations blend over a game, so alpha is always meaningful.
        changed = fxi_color_swatch(ui, rgba, label, true);
    });
    changed
}

/// Flattened style snapshot of a layout item, used for bulk-style propagation
/// across a multi-selection. Each field is `Option` so we can both (a) capture
/// only what an item kind actually has, and (b) diff to find what the user
/// changed. `apply_to` writes a changed field onto another item only where that
/// item kind exposes the same field — e.g. changing a Rect's fill while a Line
/// is also selected leaves the Line's (absent) fill untouched.
#[derive(Clone, Default, PartialEq)]
pub(crate) struct ItemStyle {
    fill: Option<[u8; 4]>,
    stroke: Option<[u8; 4]>,
    stroke_px: Option<f32>,
    corner_radius: Option<f32>,
    font_size: Option<f32>,
    // Graph-pin override (Response Curve / Oscilloscope / Vectorscope). Carried
    // whole so copy/paste-style and multi-select propagation transfer the
    // background, outline, and per-channel line/dot colors together. Applied
    // only onto other graph pins (see `apply_to`).
    graph: Option<crate::canvas::node::PinGraphOverride>,
    // Text-pin / switch-pin overrides are deliberately NOT bulk-propagated
    // here (they're per-module-kind and rarely span a mixed selection); the
    // decoration-level fill/stroke covers the common decoration case.
}

impl ItemStyle {
    pub(crate) fn any(&self) -> bool {
        self.fill.is_some() || self.stroke.is_some() || self.stroke_px.is_some()
            || self.corner_radius.is_some() || self.font_size.is_some()
            || self.graph.is_some()
    }
    /// Build the set of fields that differ between `self` (before) and `after`.
    pub(crate) fn diff(&self, after: &ItemStyle) -> ItemStyle {
        fn pick<T: PartialEq + Copy>(b: Option<T>, a: Option<T>) -> Option<T> {
            match (b, a) { (Some(bv), Some(av)) if bv != av => Some(av), _ => None }
        }
        ItemStyle {
            fill: pick(self.fill, after.fill),
            stroke: pick(self.stroke, after.stroke),
            stroke_px: pick(self.stroke_px, after.stroke_px),
            corner_radius: pick(self.corner_radius, after.corner_radius),
            font_size: pick(self.font_size, after.font_size),
            // Clone (not Copy) so picked by reference comparison.
            graph: match (&self.graph, &after.graph) {
                (b, Some(a)) if b.as_ref() != Some(a) => Some(a.clone()),
                _ => None,
            },
        }
    }
    /// Apply each changed field to `it` where that field exists on the item.
    pub(crate) fn apply_to(&self, it: &mut LayoutItem) {
        let d = match it {
            LayoutItem::Deco(d) => d,
            // Graph-pin styling rides on the `graph` payload only; copy/paste
            // and propagation between graph pins go through here.
            LayoutItem::Module(m) => {
                if let Some(g) = &self.graph {
                    let empty = g.background.is_none() && g.outline.is_none()
                        && g.outline_px.is_none() && g.gridline.is_none()
                        && g.channel_colors.iter().all(|c| c.is_none());
                    m.graph_override = if empty { None } else { Some(g.clone()) };
                }
                return;
            }
        };
        match d {
            LayoutDecoration::Text { fill, outline, outline_px, font_size, .. } => {
                if let Some(v) = self.fill { *fill = v; }
                if let Some(v) = self.stroke { *outline = v; }
                if let Some(v) = self.stroke_px { *outline_px = v; }
                if let Some(v) = self.font_size { *font_size = v; }
            }
            LayoutDecoration::Rect { fill, stroke, stroke_px, corner_radius, .. } => {
                if let Some(v) = self.fill { *fill = v; }
                if let Some(v) = self.stroke { *stroke = v; }
                if let Some(v) = self.stroke_px { *stroke_px = v; }
                if let Some(v) = self.corner_radius { *corner_radius = v; }
            }
            LayoutDecoration::Ellipse { fill, stroke, stroke_px, .. } => {
                if let Some(v) = self.fill { *fill = v; }
                if let Some(v) = self.stroke { *stroke = v; }
                if let Some(v) = self.stroke_px { *stroke_px = v; }
            }
            LayoutDecoration::Line { stroke, stroke_px, .. } => {
                if let Some(v) = self.stroke { *stroke = v; }
                if let Some(v) = self.stroke_px { *stroke_px = v; }
            }
            LayoutDecoration::Svg { stroke, stroke_px, .. } => {
                if let Some(v) = self.stroke { *stroke = v; }
                if let Some(v) = self.stroke_px { *stroke_px = v; }
            }
        }
    }
}

/// Snapshot the bulk-propagatable style of a layout item. For Text the
/// `outline`/`outline_px` map onto the generic `stroke`/`stroke_px` slots so a
/// stroke change made on, say, a Rect can carry to a Text outline.
pub(crate) fn item_style_of(it: &LayoutItem) -> ItemStyle {
    match it {
        LayoutItem::Deco(LayoutDecoration::Text { fill, outline, outline_px, font_size, .. }) => ItemStyle {
            fill: Some(*fill), stroke: Some(*outline), stroke_px: Some(*outline_px),
            corner_radius: None, font_size: Some(*font_size),
            ..ItemStyle::default()
        },
        LayoutItem::Deco(LayoutDecoration::Rect { fill, stroke, stroke_px, corner_radius, .. }) => ItemStyle {
            fill: Some(*fill), stroke: Some(*stroke), stroke_px: Some(*stroke_px),
            corner_radius: Some(*corner_radius), font_size: None,
            ..ItemStyle::default()
        },
        LayoutItem::Deco(LayoutDecoration::Ellipse { fill, stroke, stroke_px, .. }) => ItemStyle {
            fill: Some(*fill), stroke: Some(*stroke), stroke_px: Some(*stroke_px),
            corner_radius: None, font_size: None,
            ..ItemStyle::default()
        },
        LayoutItem::Deco(LayoutDecoration::Line { stroke, stroke_px, .. }) => ItemStyle {
            fill: None, stroke: Some(*stroke), stroke_px: Some(*stroke_px),
            corner_radius: None, font_size: None,
            ..ItemStyle::default()
        },
        LayoutItem::Deco(LayoutDecoration::Svg { stroke, stroke_px, .. }) => ItemStyle {
            fill: None, stroke: Some(*stroke), stroke_px: Some(*stroke_px),
            corner_radius: None, font_size: None,
            ..ItemStyle::default()
        },
        // Module pins only carry their graph override into the style snapshot
        // (text/switch pin overrides stay per-kind and aren't bulk-propagated).
        // `graph_override` is only ever populated on graph pins, so this is
        // inert for other module kinds.
        LayoutItem::Module(m) => ItemStyle {
            graph: m.graph_override.clone(),
            ..ItemStyle::default()
        },
    }
}

/// Render the contextual inspector strip for the selected decoration inside
/// a `LayoutItem::Deco`. Bails out cleanly if `idx` is stale or not a deco.
/// Z-order is controlled via the right-click menu, not this strip.
fn decoration_inspector_strip_item(
    ui: &mut egui::Ui,
    items: &mut Vec<LayoutItem>,
    idx: usize,
) {
    if idx >= items.len() { return; }
    let deco = match &mut items[idx] {
        LayoutItem::Deco(d) => d,
        _ => return,
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(deco.type_label()).small().strong());
        ui.separator();
        match deco {
            LayoutDecoration::Text { text, font_size, fill, outline, outline_px, align, valign, .. } => {
                use crate::canvas::node::TextVAlign;
                ui.add(egui::TextEdit::singleline(text).desired_width(140.0).hint_text("text"));
                ui.add(egui::DragValue::new(font_size).speed(0.25).range(6.0f32..=96.0).suffix("px"))
                    .on_hover_text("Font size");
                color_button(ui, "fill", fill);
                color_button(ui, "outline", outline);
                ui.add(egui::DragValue::new(outline_px).speed(0.1).range(0.0f32..=8.0).suffix("px"))
                    .on_hover_text("Outline thickness");
                egui::ComboBox::from_id_salt(("deco_align", idx))
                    .width(70.0)
                    .selected_text(match *align { TextAlign::Left => "Left", TextAlign::Center => "Center", TextAlign::Right => "Right" })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(align, TextAlign::Left, "Left");
                        ui.selectable_value(align, TextAlign::Center, "Center");
                        ui.selectable_value(align, TextAlign::Right, "Right");
                    });
                egui::ComboBox::from_id_salt(("deco_valign", idx))
                    .width(70.0)
                    .selected_text(match *valign { TextVAlign::Top => "Top", TextVAlign::Center => "Middle", TextVAlign::Bottom => "Bottom" })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(valign, TextVAlign::Top, "Top");
                        ui.selectable_value(valign, TextVAlign::Center, "Middle");
                        ui.selectable_value(valign, TextVAlign::Bottom, "Bottom");
                    })
                    .response.on_hover_text("Vertical alignment");
            }
            LayoutDecoration::Rect { fill, stroke, stroke_px, corner_radius, .. } => {
                color_button(ui, "fill", fill);
                color_button(ui, "stroke", stroke);
                ui.add(egui::DragValue::new(stroke_px).speed(0.1).range(0.0f32..=12.0).suffix("px"))
                    .on_hover_text("Stroke thickness");
                ui.add(egui::DragValue::new(corner_radius).speed(0.25).range(0.0f32..=64.0).suffix("r"))
                    .on_hover_text("Corner radius");
            }
            LayoutDecoration::Ellipse { fill, stroke, stroke_px, .. } => {
                color_button(ui, "fill", fill);
                color_button(ui, "stroke", stroke);
                ui.add(egui::DragValue::new(stroke_px).speed(0.1).range(0.0f32..=12.0).suffix("px"))
                    .on_hover_text("Stroke thickness");
            }
            LayoutDecoration::Line { stroke, stroke_px, .. } => {
                color_button(ui, "stroke", stroke);
                ui.add(egui::DragValue::new(stroke_px).speed(0.1).range(0.1f32..=12.0).suffix("px"))
                    .on_hover_text("Line thickness");
            }
            LayoutDecoration::Svg { tint, tint_mode, stroke, stroke_px, svg_data, rev, .. } => {
                if ui.small_button("Load…").on_hover_text("Load SVG file").clicked() {
                    // This inspector strip is reachable from the overlay's edit
                    // toolbar, where the always-on-top overlay would otherwise
                    // sit ON TOP of the (owner-less) native file dialog and hide
                    // it. Drop the overlay's topmost bit around the blocking
                    // pick_file so the dialog is reachable (no-op elsewhere).
                    let picked = crate::overlay::with_overlay_not_topmost(|| {
                        rfd::FileDialog::new().add_filter("SVG", &["svg"]).pick_file()
                    });
                    if let Some(path) = picked {
                        if let Ok(text) = std::fs::read_to_string(&path) {
                            *svg_data = text;
                            *rev = rev.wrapping_add(1);
                        }
                    }
                }
                color_button(ui, "tint", tint);
                egui::ComboBox::from_id_salt(("deco_svg_mode", idx))
                    .width(80.0)
                    .selected_text(tint_mode.as_str())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(tint_mode, "override".to_string(), "override");
                        ui.selectable_value(tint_mode, "additive".to_string(), "additive");
                    });
                color_button(ui, "stroke", stroke);
                ui.add(egui::DragValue::new(stroke_px).speed(0.1).range(0.0f32..=12.0).suffix("px"))
                    .on_hover_text("Frame stroke thickness");
            }
        }
    });
}

/// Inspector strip for the per-pin Text color override (when the selected
/// item is a Text module pin). Operates on `LayoutItem::Module`.
fn text_pin_inspector_strip_item(
    ui: &mut egui::Ui,
    items: &mut Vec<LayoutItem>,
    idx: usize,
) {
    if idx >= items.len() { return; }
    let exp = match &mut items[idx] {
        LayoutItem::Module(m) => m,
        _ => return,
    };
    let mut ov = exp.text_override.clone().unwrap_or_default();
    let mut clear = false;
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("Text pin override").small().strong());
        ui.separator();
        let mut fill_rgba = ov.fill.unwrap_or([220, 220, 220, 255]);
        if color_button(ui, "fill", &mut fill_rgba) { ov.fill = Some(fill_rgba); }
        let mut outline_rgba = ov.outline.unwrap_or([0, 0, 0, 0]);
        if color_button(ui, "outline", &mut outline_rgba) { ov.outline = Some(outline_rgba); }
        let mut opx = ov.outline_px.unwrap_or(0.0);
        if ui.add(egui::DragValue::new(&mut opx).speed(0.1).range(0.0f32..=8.0).suffix("px"))
            .on_hover_text("Outline thickness")
            .changed()
        {
            ov.outline_px = Some(opx);
        }
        if ui.small_button("Reset").on_hover_text("Use Text module's own colors").clicked() {
            clear = true;
        }
    });
    if clear {
        exp.text_override = None;
    } else if ov.fill.is_some() || ov.outline.is_some() || ov.outline_px.is_some() {
        exp.text_override = Some(ov);
    }
}

/// Inspector strip for a pinned Input Viewer board's style (when the selected
/// item is a `module.input_viewer` pin). Delegates to the board module's own
/// control cluster; stores the result on the pin's `iv_style_override`.
/// Inspector-strip controls for a pinned Touch Zones / Virtual Menu field pad:
/// main / highlight colour overrides (each falls back to the module's own
/// colour when unset) plus a visibility mode for live views (always / show on
/// touch / touched zones only). Stored per pin on `menu_style_override`.
fn menu_pin_inspector_strip_item(
    ui: &mut egui::Ui,
    items: &mut Vec<LayoutItem>,
    idx: usize,
) {
    use crate::canvas::node::{MenuStyleOverride, ZoneVisibility};
    if idx >= items.len() { return; }
    let exp = match &mut items[idx] {
        LayoutItem::Module(m) => m,
        _ => return,
    };
    let cleared = MenuStyleOverride { main: None, hi: None, visibility: ZoneVisibility::Always };
    let mut ov = exp.menu_style_override.unwrap_or(cleared);
    let before = ov;
    let mut reset = false;
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("Zone pad style").small().strong());
        ui.separator();
        ui.label(egui::RichText::new("main").small().weak());
        let mut mc = ov.main.unwrap_or(super::menu_body::MENU_MAIN_DEFAULT);
        if fxi_color_swatch(ui, &mut mc,
            "Main colour for THIS pinned pad (alpha = pad opacity).\nUnset = the module's own colour.", true)
        {
            ov.main = Some(mc);
        }
        ui.label(egui::RichText::new("highlight").small().weak());
        let mut hc = ov.hi.unwrap_or(super::menu_body::MENU_HIGHLIGHT_DEFAULT);
        if fxi_color_swatch(ui, &mut hc,
            "Highlight colour for THIS pinned pad (active zone / affordances).\nUnset = the module's own colour.", true)
        {
            ov.hi = Some(hc);
        }
        ui.separator();
        egui::ComboBox::from_id_salt(("menu_pin_vis", exp.inner_node_id, idx))
            .selected_text(match ov.visibility {
                ZoneVisibility::Always => "Always show",
                ZoneVisibility::OnTouch => "Show on touch",
                ZoneVisibility::TouchedZones => "Touched zones only",
            })
            .width(150.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut ov.visibility, ZoneVisibility::Always, "Always show");
                ui.selectable_value(&mut ov.visibility, ZoneVisibility::OnTouch, "Show on touch")
                    .on_hover_text("Hide the pad unless a touch point / zone is active.");
                ui.selectable_value(&mut ov.visibility, ZoneVisibility::TouchedZones, "Touched zones only")
                    .on_hover_text("Paint only the zone(s) currently touched — no pad chrome.\nRadial menus behave like Show on touch.");
            });
        if ui.small_button("Reset")
            .on_hover_text("Module colours, always shown")
            .clicked()
        {
            reset = true;
        }
    });
    if reset {
        exp.menu_style_override = None;
    } else if ov != before || exp.menu_style_override.is_some() {
        exp.menu_style_override = if ov == cleared { None } else { Some(ov) };
    }
}

fn input_viewer_pin_inspector_strip_item(
    ui: &mut egui::Ui,
    items: &mut Vec<LayoutItem>,
    idx: usize,
) {
    if idx >= items.len() { return; }
    let exp = match &mut items[idx] {
        LayoutItem::Module(m) => m,
        _ => return,
    };
    exp.iv_style_override = super::input_viewer::iv_style_inspector(
        ui, exp.inner_node_id, exp.iv_style_override.as_ref());
}

/// Inspector strip for a pinned 3D controller viewer: frame style (bg /
/// highlight accent / outline) plus per-pin display settings — view angle,
/// model opacity, and highlight fade — each overriding the module's own params
/// for THIS pinned instance only. Reset clears the whole override.
fn controller3d_pin_inspector_strip_item(
    ui: &mut egui::Ui,
    items: &mut Vec<LayoutItem>,
    idx: usize,
) {
    use crate::canvas::node::IvStyleOverride;
    if idx >= items.len() { return; }
    let exp = match &mut items[idx] {
        LayoutItem::Module(m) => m,
        _ => return,
    };
    let mut ov = exp.iv_style_override.unwrap_or(IvStyleOverride {
        bg: [18, 18, 22, 255],
        accent: C3D_DEFAULT_ACCENT,
        tint: [255, 255, 255, 255],
        outline: [70, 70, 75, 255],
        outline_px: 0.0,
        c3d_pitch: None,
        c3d_alpha: None,
        c3d_fade: None,
        c3d_composite: None,
    });
    let before = ov;
    let mut reset = false;

    ui.vertical(|ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("3D viewer style").small().strong());
            ui.separator();
            ui.label(egui::RichText::new("bg").small().weak());
            fxi_color_swatch(ui, &mut ov.bg,
                "Frame fill — lower the alpha for a see-through frame over a game.", true);
            ui.label(egui::RichText::new("highlight").small().weak());
            fxi_color_swatch(ui, &mut ov.accent,
                "Active-input highlight: pressed buttons, trigger pull, stick tilt, touch dots, x-ray ghosts.", true);
            ui.label(egui::RichText::new("outline").small().weak());
            fxi_color_swatch(ui, &mut ov.outline, "Frame outline colour.", true);
            ui.add(egui::DragValue::new(&mut ov.outline_px).range(0.0..=6.0).speed(0.05))
                .on_hover_text("Frame outline width (0 = none).");
            if ui.small_button("Reset").on_hover_text("Use default style + the module's own display settings").clicked() {
                reset = true;
            }
        });
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            // Display overrides: initialized from the override, else the module
            // defaults; touching a slider pins it for this instance.
            let mut pitch = ov.c3d_pitch.unwrap_or(C3D_DEFAULT_PITCH_DEG);
            ui.label(egui::RichText::new("view").small().weak());
            if ui.add(egui::Slider::new(&mut pitch, 0.0..=85.0).suffix("°"))
                .on_hover_text("Camera elevation for this pinned instance.")
                .changed() { ov.c3d_pitch = Some(pitch); }
            let mut alpha = ov.c3d_alpha.unwrap_or(1.0);
            ui.label(egui::RichText::new("opacity").small().weak());
            if ui.add(egui::Slider::new(&mut alpha, 0.1..=1.0))
                .on_hover_text("Model see-through for this pinned instance.")
                .changed() { ov.c3d_alpha = Some(alpha); }
            let mut fade = ov.c3d_fade.unwrap_or(0.25);
            ui.label(egui::RichText::new("fade").small().weak());
            if ui.add(egui::Slider::new(&mut fade, 0.05..=2.0).suffix(" s"))
                .on_hover_text("Highlight fade-out time for this pinned instance.")
                .changed() { ov.c3d_fade = Some(fade); }
            let mut comp = ov.c3d_composite.unwrap_or(1.0);
            ui.label(egui::RichText::new("alpha").small().weak());
            if ui.add(egui::Slider::new(&mut comp, 0.05..=1.0))
                .on_hover_text("Widget composite alpha: fades the whole rendered controller as a 2D image (overlay), without the see-through look.")
                .changed() { ov.c3d_composite = Some(comp); }
        });

        // Material colours + model — edit the NODE (shared with the module
        // body and every pinned instance). Current values are published by the
        // pinned renderer each frame; edits ride temp memory back to it.
        let uid = exp.inner_node_id;
        let pub_data = ui.ctx().data(|d| {
            d.get_temp::<(String, crate::model::Scheme)>(egui::Id::new(("c3d_pub", uid)))
        });
        if let Some((model, cur)) = pub_data {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                // Model chooser (Auto + every available model folder).
                ui.label(egui::RichText::new("Model").small().strong());
                let mut sel = model.clone();
                egui::ComboBox::from_id_salt(("c3d_pin_model", uid))
                    .selected_text(sel.clone())
                    .show_ui(ui, |ui| {
                        if ui.selectable_value(&mut sel, "auto".to_string(), "Auto (device)").clicked() {
                            ui.ctx().data_mut(|d| {
                                d.insert_temp(egui::Id::new(("c3d_modeledit", uid)), String::new())
                            });
                        }
                        for m in crate::model::available_models() {
                            if ui.selectable_value(&mut sel, m.clone(), &m).clicked() {
                                ui.ctx().data_mut(|d| {
                                    d.insert_temp(egui::Id::new(("c3d_modeledit", uid)), m.clone())
                                });
                            }
                        }
                    });
                ui.separator();
                ui.label(egui::RichText::new("Colours").small().strong())
                    .on_hover_text("Model colours — shared with the module (not per-pin).");
                for (row_label, groups) in crate::model::material::ROWS {
                    ui.label(egui::RichText::new(*row_label).small().weak());
                    for &g in *groups {
                        let mut col = cur[g as usize];
                        if c3d_color_swatch(ui, &mut col, g.label()) {
                            ui.ctx().data_mut(|d| {
                                d.insert_temp(
                                    egui::Id::new(("c3d_matedit", uid)),
                                    (g.key().to_string(), col),
                                )
                            });
                        }
                    }
                }
                if ui
                    .small_button("Reset colours")
                    .on_hover_text("Reset the model colours to this model's defaults")
                    .clicked()
                {
                    ui.ctx().data_mut(|d| {
                        d.insert_temp(egui::Id::new(("c3d_matreset", uid)), true)
                    });
                }
                if ui
                    .small_button("Save…")
                    .on_hover_text("Save these colours to a .fxcol file")
                    .clicked()
                {
                    controller3d_scheme_save(&cur);
                }
                if ui
                    .small_button("Load…")
                    .on_hover_text("Load colours from a .fxcol file")
                    .clicked()
                {
                    controller3d_scheme_load_async(ui.ctx(), uid);
                }
            });
        }
    });

    if reset {
        exp.iv_style_override = None;
    } else if ov != before || exp.iv_style_override.is_some() {
        exp.iv_style_override = Some(ov);
    }
}

/// Inspector strip for the per-pin Switch color override (when the selected
/// item is a Switch module pin). Exposes fill, outline, and caption colors
/// for both ON and OFF states, plus a shared outline thickness.
fn switch_pin_inspector_strip_item(
    ui: &mut egui::Ui,
    items: &mut Vec<LayoutItem>,
    idx: usize,
) {
    use crate::canvas::node::PinSwitchOverride;
    if idx >= items.len() { return; }
    let exp = match &mut items[idx] {
        LayoutItem::Module(m) => m,
        _ => return,
    };
    let mut ov = exp.switch_override.clone().unwrap_or_default();
    let mut clear = false;

    ui.vertical(|ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Switch ON").small().strong());
            let mut fill = ov.fill_on.unwrap_or([80, 140, 200, 255]);
            if color_button(ui, "fill", &mut fill) { ov.fill_on = Some(fill); }
            let mut outl = ov.outline_on.unwrap_or([180, 200, 220, 255]);
            if color_button(ui, "outline", &mut outl) { ov.outline_on = Some(outl); }
            let mut txt = ov.text_on.unwrap_or([255, 255, 255, 255]);
            if color_button(ui, "text", &mut txt) { ov.text_on = Some(txt); }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Switch OFF").small().strong());
            let mut fill = ov.fill_off.unwrap_or([60, 60, 65, 255]);
            if color_button(ui, "fill", &mut fill) { ov.fill_off = Some(fill); }
            let mut outl = ov.outline_off.unwrap_or([100, 100, 110, 255]);
            if color_button(ui, "outline", &mut outl) { ov.outline_off = Some(outl); }
            let mut txt = ov.text_off.unwrap_or([200, 200, 200, 255]);
            if color_button(ui, "text", &mut txt) { ov.text_off = Some(txt); }
        });
        ui.horizontal_wrapped(|ui| {
            let mut opx = ov.outline_px.unwrap_or(1.0);
            if ui.add(egui::DragValue::new(&mut opx).speed(0.1).range(0.0f32..=8.0).suffix("px"))
                .on_hover_text("Outline thickness (both states)")
                .changed()
            {
                ov.outline_px = Some(opx);
            }
            if ui.small_button("Reset").on_hover_text("Use theme defaults").clicked() {
                clear = true;
            }
        });
    });
    if clear {
        exp.switch_override = None;
    } else {
        let any = ov.fill_on.is_some() || ov.fill_off.is_some()
            || ov.outline_on.is_some() || ov.outline_off.is_some()
            || ov.text_on.is_some() || ov.text_off.is_some()
            || ov.outline_px.is_some();
        exp.switch_override = if any { Some(PinSwitchOverride { ..ov }) } else { None };
    }
}

/// Inspector strip for the per-pin graph color override (Response Curve,
/// Oscilloscope, Vectorscope). Exposes background + outline + outline width,
/// then one color swatch per input channel for the line/dot color. All controls
/// share one wrapped row, so when there isn't enough width (e.g. Easy mode) the
/// channel swatches wrap onto a second row automatically. `n_channels` is the
/// inner module's resolved channel count.
fn graph_pin_inspector_strip_item(
    ui: &mut egui::Ui,
    items: &mut Vec<LayoutItem>,
    idx: usize,
    n_channels: usize,
) {
    use crate::canvas::node::PinGraphOverride;
    if idx >= items.len() { return; }
    let exp = match &mut items[idx] {
        LayoutItem::Module(m) => m,
        _ => return,
    };
    let mut ov = exp.graph_override.clone().unwrap_or_default();
    let mut clear = false;

    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("Graph pin override").small().strong());
        ui.separator();

        // Background: default is the shared 60%-opacity base.
        let mut bg = ov.background.unwrap_or([16, 16, 16, 153]);
        if color_button(ui, "bg", &mut bg) { ov.background = Some(bg); }

        // Outline: off by default (transparent / 0px).
        let mut outl = ov.outline.unwrap_or([180, 180, 180, 255]);
        if color_button(ui, "outline", &mut outl) { ov.outline = Some(outl); }
        let mut opx = ov.outline_px.unwrap_or(0.0);
        if ui.add(egui::DragValue::new(&mut opx).speed(0.1).range(0.0f32..=8.0).suffix("px"))
            .on_hover_text("Outline thickness")
            .changed()
        {
            ov.outline_px = Some(opx);
        }

        // Gridline / axis color. Default matches the brighter label hue
        // (unmultiplied 180,180,180,160 == GRAPH_GRID_DEFAULT premultiplied).
        let mut grid = ov.gridline.unwrap_or([180, 180, 180, 160]);
        if color_button(ui, "grid", &mut grid) { ov.gridline = Some(grid); }

        ui.separator();

        // One line/dot color swatch per channel. Each defaults to the built-in
        // palette entry for that channel.
        ov.channel_colors.resize(n_channels, None);
        for ch in 0..n_channels {
            let default = MULTI_COLORS[ch % MULTI_COLORS.len()];
            let mut col = ov.channel_colors[ch]
                .unwrap_or([default.r(), default.g(), default.b(), 255]);
            if color_button(ui, &format!("Ch{}", ch + 1), &mut col) {
                ov.channel_colors[ch] = Some(col);
            }
        }

        if ui.small_button("Reset").on_hover_text("Use the module's default colors").clicked() {
            clear = true;
        }
    });

    if clear {
        exp.graph_override = None;
    } else {
        let any = ov.background.is_some()
            || ov.outline.is_some()
            || ov.outline_px.is_some()
            || ov.gridline.is_some()
            || ov.channel_colors.iter().any(|c| c.is_some());
        // Drop trailing None channel slots so we don't serialize empty tails.
        while matches!(ov.channel_colors.last(), Some(None)) { ov.channel_colors.pop(); }
        exp.graph_override = if any { Some(PinGraphOverride { ..ov }) } else { None };
    }
}

/// Paint a single decoration into the given body painter. Coordinates are in
/// body-local space; caller already translated `origin` and provides absolute
/// `painter` and `offset` to add to local coords.
pub(crate) fn paint_decoration(painter: &egui::Painter, origin: egui::Pos2, deco: &LayoutDecoration) {
    match deco {
        LayoutDecoration::Rect { pos, size, fill, stroke, stroke_px, corner_radius } => {
            let r = egui::Rect::from_min_size(
                origin + egui::vec2(pos[0], pos[1]),
                egui::vec2(size[0].max(1.0), size[1].max(1.0)),
            );
            let fcol = rgba_to_color32(*fill);
            if fcol.a() > 0 {
                painter.rect_filled(r, *corner_radius, fcol);
            }
            let scol = rgba_to_color32(*stroke);
            if scol.a() > 0 && *stroke_px > 0.05 {
                painter.rect_stroke(r, *corner_radius,
                    egui::Stroke::new(*stroke_px, scol),
                    egui::StrokeKind::Inside);
            }
        }
        LayoutDecoration::Ellipse { pos, size, fill, stroke, stroke_px } => {
            let r = egui::Rect::from_min_size(
                origin + egui::vec2(pos[0], pos[1]),
                egui::vec2(size[0].max(1.0), size[1].max(1.0)),
            );
            let center = r.center();
            let radius = egui::vec2(r.width() * 0.5, r.height() * 0.5);
            // egui has no ellipse primitive; approximate with a polygon of 64 verts.
            let mut pts = Vec::with_capacity(64);
            for i in 0..64 {
                let t = (i as f32) / 64.0 * std::f32::consts::TAU;
                pts.push(egui::pos2(center.x + radius.x * t.cos(), center.y + radius.y * t.sin()));
            }
            let fcol = rgba_to_color32(*fill);
            if fcol.a() > 0 {
                painter.add(egui::Shape::convex_polygon(pts.clone(), fcol, egui::Stroke::NONE));
            }
            let scol = rgba_to_color32(*stroke);
            if scol.a() > 0 && *stroke_px > 0.05 {
                pts.push(pts[0]);
                painter.add(egui::Shape::line(pts, egui::Stroke::new(*stroke_px, scol)));
            }
        }
        LayoutDecoration::Line { a, b, stroke, stroke_px } => {
            let p1 = origin + egui::vec2(a[0], a[1]);
            let p2 = origin + egui::vec2(b[0], b[1]);
            painter.line_segment([p1, p2],
                egui::Stroke::new(*stroke_px, rgba_to_color32(*stroke)));
        }
        LayoutDecoration::Text { pos, size, text, font_size, fill, outline, outline_px, align, valign } => {
            use crate::canvas::node::TextVAlign;
            let r = egui::Rect::from_min_size(
                origin + egui::vec2(pos[0], pos[1]),
                egui::vec2(size[0].max(1.0), size[1].max(1.0)),
            );
            // Horizontal anchor + x from `align`; vertical anchor + y from
            // `valign`. We combine the two into a single Align2 so the glyph
            // run is positioned within the box per both axes.
            let (h_align2_kind, x) = match align {
                TextAlign::Left   => (0u8, r.min.x),
                TextAlign::Center => (1u8, r.center().x),
                TextAlign::Right  => (2u8, r.max.x),
            };
            let (v_kind, y) = match valign {
                TextVAlign::Top    => (0u8, r.min.y),
                TextVAlign::Center => (1u8, r.center().y),
                TextVAlign::Bottom => (2u8, r.max.y),
            };
            let ax = match h_align2_kind { 0 => egui::Align::LEFT, 1 => egui::Align::Center, _ => egui::Align::RIGHT };
            let ay = match v_kind        { 0 => egui::Align::TOP,  1 => egui::Align::Center, _ => egui::Align::BOTTOM };
            let anchor = egui::Align2([ax, ay]);
            let fcol = rgba_to_color32(*fill);
            let ocol = rgba_to_color32(*outline);
            // Cheap text outline: paint 8-direction offset copies first.
            if ocol.a() > 0 && *outline_px > 0.05 {
                for (dx, dy) in [(-1.0,0.0),(1.0,0.0),(0.0,-1.0),(0.0,1.0),(-1.0,-1.0),(1.0,-1.0),(-1.0,1.0),(1.0,1.0)] {
                    painter.text(
                        egui::pos2(x + dx * *outline_px, y + dy * *outline_px),
                        anchor, text,
                        egui::FontId::proportional(*font_size),
                        ocol,
                    );
                }
            }
            painter.text(
                egui::pos2(x, y),
                anchor, text,
                egui::FontId::proportional(*font_size),
                fcol,
            );
        }
        LayoutDecoration::Svg { pos, size, svg_data, rev, tint, tint_mode, stroke, stroke_px } => {
            let r = egui::Rect::from_min_size(
                origin + egui::vec2(pos[0], pos[1]),
                egui::vec2(size[0].max(8.0), size[1].max(8.0)),
            );
            let scol = rgba_to_color32(*stroke);
            if scol.a() > 0 && *stroke_px > 0.05 {
                painter.rect_stroke(r, 0.0,
                    egui::Stroke::new(*stroke_px, scol),
                    egui::StrokeKind::Inside);
            }
            if svg_data.is_empty() {
                painter.rect_stroke(r, 2.0,
                    egui::Stroke::new(1.0, egui::Color32::from_gray(80)),
                    egui::StrokeKind::Inside);
                painter.text(r.center(), egui::Align2::CENTER_CENTER, "SVG",
                    egui::FontId::proportional(11.0), egui::Color32::from_gray(140));
                return;
            }
            let pw = (r.width().round() as u32).max(1);
            let ph = (r.height().round() as u32).max(1);
            let ctx = painter.ctx();
            let cache_key = egui::Id::new((
                "deco_svg_tex", svg_data.as_ptr() as usize, *rev, pw, ph, tint_mode.as_str(),
                tint[0], tint[1], tint[2], tint[3],
            ));
            let cached = ctx.data(|d| d.get_temp::<egui::TextureHandle>(cache_key));
            let tex = match cached {
                Some(t) => Some(t),
                None => {
                    rasterize_svg_recolored(svg_data, pw, ph, tint_mode, rgba_to_color32(*tint))
                        .map(|img| {
                            let h = ctx.load_texture(
                                format!("deco-svg-{}-{}", *rev, pw),
                                img,
                                egui::TextureOptions::LINEAR,
                            );
                            ctx.data_mut(|d| d.insert_temp(cache_key, h.clone()));
                            h
                        })
                }
            };
            if let Some(tex) = tex {
                painter.image(
                    tex.id(), r,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        }
    }
}

// (Old `show_subpatch_decorations` has been folded into `show_subpatch_body`.
//  Decorations + module pins share one Z-order, one selection, and one
//  interaction layer.)
