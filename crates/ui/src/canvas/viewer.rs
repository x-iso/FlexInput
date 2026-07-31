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
mod conflicts;
mod controller3d;
mod expose;
mod glow;
mod gyro_body;
mod layout_edit;
mod midi;
mod net;
mod osc_envelope;
mod pins;
mod pinned;
mod remapper_bodies;
mod remapper_card;
mod remapper_filter;
mod remapper_reorder;
mod remapper_support;
mod reshape;
mod rws;
mod scale;
mod simple_bodies;
mod subpatch_body;
mod touch_zones;
pub(crate) use asth::*;
pub(crate) use curve_bodies::*;
pub(crate) use curve_support::*;
pub(crate) use scopes::*;
pub(crate) use automap_bodies::*;
pub(crate) use chrome::*;
pub(crate) use conflicts::*;
pub(crate) use controller3d::*;
pub(crate) use expose::*;
pub(crate) use glow::*;
pub(crate) use gyro_body::*;
pub(crate) use layout_edit::*;
pub(crate) use midi::*;
pub(crate) use net::*;
pub(crate) use osc_envelope::*;
pub(crate) use pins::*;
pub(crate) use pinned::*;
pub(crate) use remapper_bodies::*;
pub(crate) use remapper_card::*;
pub(crate) use remapper_filter::*;
pub(crate) use remapper_reorder::*;
pub(crate) use remapper_support::*;
pub(crate) use reshape::*;
pub(crate) use rws::*;
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
        let is_rws            = snarl.get_node(node).map(|n| n.module_id == "processing.rws").unwrap_or(false);

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

                // RWS Aim: input-mode dropdown + Calibrate Start/Stop live in the
                // header so the body stays clean for the knobs + ruler. Both are
                // still registered as pinnable elements ("input"/"cal").
                if is_rws {
                    let input_mode = snarl.get_node(node)
                        .and_then(|n| n.params.get("input_mode").and_then(|v| v.as_str()))
                        .unwrap_or("gyro").to_string();
                    ui.label(egui::RichText::new("Input mode").small().weak())
                        .on_hover_text("How the Rotation input is read:\n• Gyro — a true angular rate (±1 = ±2000 °/s), for 1:1 calibration.\n• Stick (rate) — a stick deflection driven as a turn rate up to Max °/s.");
                    let combo = egui::ComboBox::from_id_salt((node, "rws_hdr_input"))
                        .selected_text(if input_mode == "stick_rate" { "Stick" } else { "Gyro" })
                        .width(72.0)
                        .show_ui(ui, |ui| {
                            for (val, lbl) in [("gyro", "Gyro"), ("stick_rate", "Stick (rate)")] {
                                if ui.selectable_label(input_mode == val, lbl).clicked() {
                                    if let Some(n) = snarl.get_node_mut(node) {
                                        n.params.insert("input_mode".into(), Value::String(val.to_string()));
                                    }
                                }
                            }
                        });
                    crate::canvas::viewer::register_exposable_element(ui, node, "input", combo.response.rect);
                    // Turn rate at full stick deflection — only meaningful in stick mode.
                    if input_mode == "stick_rate" {
                        let mut mr = snarl.get_node(node)
                            .and_then(|n| n.params.get("max_rate_dps").and_then(|v| v.as_f64()))
                            .unwrap_or(360.0) as f32;
                        if ui.add(egui::DragValue::new(&mut mr).speed(5.0).range(1.0..=100_000.0).suffix(" °/s"))
                            .on_hover_text("Turn rate at full stick deflection.")
                            .changed()
                        {
                            if let (Some(n), Some(num)) = (snarl.get_node_mut(node), Number::from_f64(mr as f64)) {
                                n.params.insert("max_rate_dps".into(), Value::Number(num));
                            }
                        }
                    }
                    // Calibrate is DISABLED on the module for safety: it drives
                    // your real mouse, so it must be pinned to the Config Overlay
                    // and run from there (where a gamepad, not the busy mouse,
                    // controls it). A ⚠ explains why.
                    ui.label(egui::RichText::new("⚠").color(Color32::from_rgb(230, 180, 60)))
                        .on_hover_text("Calibration takes over your real mouse.\nPin this button (or the ruler) to the Config Overlay and run it from there with a gamepad.");
                    let cal_btn = ui.add_enabled(
                        false,
                        egui::Button::new(egui::RichText::new("▶ Calibrate").color(Color32::from_gray(140))),
                    ).on_disabled_hover_text("Pin to the Config Overlay to calibrate — it takes over your mouse.");
                    crate::canvas::viewer::register_exposable_element(ui, node, "cal", cal_btn.rect);
                    // Spin speed is a plain number — safe to set here.
                    let mut cs = snarl.get_node(node)
                        .and_then(|n| n.params.get("cal_speed").and_then(|v| v.as_f64()))
                        .unwrap_or(0.5) as f32;
                    if ui.add(egui::DragValue::new(&mut cs).speed(0.01).range(0.05..=10.0).suffix(" rev/s"))
                        .on_hover_text("Calibration spin speed (revolutions per second).")
                        .changed()
                    {
                        if let (Some(n), Some(num)) = (snarl.get_node_mut(node), Number::from_f64(cs as f64)) {
                            n.params.insert("cal_speed".into(), Value::Number(num));
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
            // Both gilrs AND sdl physical pads (phys_pad_slug handles both
            // prefixes) — an SDL-surfaced pad needs the same digital-trigger
            // option as its gilrs twin (was gilrs-only: the toggle vanished when
            // dedup kept the SDL node of a DInput pad).
            if is_device_source
                && crate::canvas::remapper_icons::phys_pad_slug(dev_id_str).is_some()
            {
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
                | "math.add" | "math.subtract" | "math.multiply" | "math.divide" | "math.negate"
                | "math.min_max" | "math.quantize" | "module.vec_to_deflection"
                | "module.selector" | "module.split" | "module.dropdown" | "module.macro"
                | "logic.greater_than" | "logic.less_than" | "logic.delay" | "logic.counter"
                | "generator.oscillator" | "generator.envelope" | "processing.gyro_3dof"
                | "processing.rws"
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
            "display.controller3d"  => show_controller3d_body(node_id, ui, snarl, self.live_signals, self.automap_parent.as_ref()),
            "processing.rws"   => show_rws_body(node_id, ui, snarl),
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
            "math.add" | "math.subtract" | "math.multiply" | "math.divide" | "math.min_max" => {
                show_math_variadic_body(node_id, inputs, ui, snarl);
            }
            "math.negate" => show_inverse_body(node_id, ui, snarl),
            "math.quantize" => show_quantize_body(node_id, inputs, ui, snarl),
            "module.vec_to_deflection" => show_vec_to_deflection_body(node_id, ui, snarl),
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





//  interaction layer.)
