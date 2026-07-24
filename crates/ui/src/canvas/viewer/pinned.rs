//! Pinned-element rendering: the per-element dispatch, whole-module
//! wrappers, and the small per-element row renderers.

use super::*;

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
pub(crate) fn render_pinned_element_impl(
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
            let board = crate::canvas::input_viewer::letterbox(rect);
            let skin_param = inner_snarl.get_node(inner_id)
                .and_then(|n| n.params.get("skin").and_then(|v| v.as_str()))
                .unwrap_or("auto").to_string();
            let skin = remapper_resolve_skin(inner_snarl, inner_id, &skin_param, bridged_parent);
            let dev = remapper_upstream_device_id(inner_snarl, inner_id, 0, bridged_parent);
            let style = crate::canvas::input_viewer::IvStyle::from_override(iv_style_override.as_ref());
            crate::canvas::input_viewer::paint_viewer_board(
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
            // Model: the pin's own choice wins (`Some("")` = force auto-detect),
            // else follow the module's param, else auto-detect.
            let pin_model = iv_style_override.as_ref().and_then(|o| o.c3d_model.clone());
            let auto_resolve = |dev_id: Option<&str>| {
                if let Some(dev) = dev_id {
                    crate::model::model_for_device(dev)
                } else {
                    crate::model::available_models().into_iter().next().unwrap_or_default()
                }
            };
            let resolved = match pin_model {
                Some(m) if !m.is_empty() => m,
                Some(_) => auto_resolve(dev_id.as_deref()),
                None if !override_name.is_empty() && override_name != "auto" => override_name,
                None => auto_resolve(dev_id.as_deref()),
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
            // Colours/model are the PIN's own style override (edited directly
            // by the inspector strip — no snarl writes, no temp channels; a
            // pinned instance can never hijack the module's own state, and the
            // module's shared scheme shows through wherever the pin doesn't
            // override).
            let pin_mats = iv_style_override.as_ref().and_then(|o| o.c3d_materials.clone());
            // Publish the NODE-base colours (shared scheme, no pin override)
            // for the inspector strip — each strip applies its OWN pin's
            // `c3d_materials` on top, so two pins of the same node never fight
            // over this key.
            let cur_rgb = controller3d_scheme_rgb(inner_snarl, inner_id, &resolved);
            ui.ctx().data_mut(|d| {
                d.insert_temp(
                    egui::Id::new(("c3d_pub", inner_id.0)),
                    (resolved.clone(), cur_rgb),
                )
            });

            let (bg, outline, outline_w, accent) = controller3d_style(iv_style_override.as_ref());
            let (scheme, mut alpha, mut cam_pitch) =
                controller3d_scheme_with(inner_snarl, inner_id, &resolved, pin_mats.as_ref());
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
            crate::canvas::menu_body::render_menu_options_pinned(inner_id, ui, inner_snarl, container_size);
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

/// Whether a pinned `(module_id, element_id)` is an INTERACTIVE parameter
/// control — a slider / curve / toggle / dropdown / numeric row the user can
/// tweak, as opposed to a pure display (viewers, scopes, readouts, previews,
/// labels, images) or a whole-module body. The config overlay (M3) consults
/// this to curate its tweak-pin pick flow: only editable elements may be
/// pinned there, so a config pin is always something you can actually adjust
/// live over a game.
///
/// This is an explicit allow-list keyed to the interactive arms of
/// [`render_pinned_element_impl`]'s dispatch. Anything not listed — display
/// elements, `whole_module` bodies, and unknown/legacy ids — is treated as
/// non-editable. The table is expanded in later M3 phases as more element
/// types are verified to render + write correctly standalone.
pub(crate) fn is_editable_element(module_id: &str, element_id: &str) -> bool {
    // Audio Stream Haptics: the mode row and any calibration row are editable;
    // the scope is display-only.
    if module_id == "module.audio_stream_haptics" {
        return element_id == "asth_mode_row"
            || AsthRow::from_element_id(element_id).is_some();
    }
    matches!(
        (module_id, element_id),
        // Bare scalar / choice widgets.
        ("module.knob", "value")
            | ("module.constant", "value")
            | ("module.dropdown", "selection")
            | ("module.switch", "toggle")
            // Gyro 3DOF rows.
            | ("processing.gyro_3dof", "mode")
            | ("processing.gyro_3dof", "pointer_mode")
            | ("processing.gyro_3dof", "steering_mode")
            | ("processing.gyro_3dof", "steering_opts")
            | ("processing.gyro_3dof", "lean_threshold")
            | ("processing.gyro_3dof", "gyro_invert")
            | ("processing.gyro_3dof", "accel_invert")
            // Response curves (regular / vec / two-way): the curve canvas + rows.
            | ("module.response_curve", "curve")
            | ("module.response_curve", "scale_row")
            | ("module.response_curve", "range_row")
            | ("module.response_curve", "grid_row")
            | ("module.response_curve", "grid_options_row")
            | ("module.vec_response_curve", "curve")
            | ("module.vec_response_curve", "scale_row")
            | ("module.vec_response_curve", "range_row")
            | ("module.vec_response_curve", "grid_row")
            | ("module.vec_response_curve", "grid_options_row")
            | ("module.twoway_response_curve", "curve")
            | ("module.twoway_response_curve", "scale_row")
            | ("module.twoway_response_curve", "range_row")
            | ("module.twoway_response_curve", "grid_row")
            | ("module.twoway_response_curve", "grid_options_row")
            | ("module.twoway_response_curve", "hyst_row")
            | ("module.twoway_response_curve", "interp_row")
            | ("module.twoway_response_curve", "lane_toggle")
            // Vec Reshaper.
            | ("module.vec_reshape", "pad")
            | ("module.vec_reshape", "curve")
            | ("module.vec_reshape", "target_row")
            | ("module.vec_reshape", "options_row")
            | ("module.vec_reshape", "range_row")
            | ("module.vec_reshape", "grid_row")
            | ("module.vec_reshape", "preset_row")
            // Average / Delay / DC Filter DragValue rows.
            | ("module.average", "samples")
            | ("module.average", "spike_mad")
            | ("module.delay", "ms")
            | ("module.dc_filter", "window_ms")
            | ("module.dc_filter", "decay_ms")
            // Counter / Logic Delay rows.
            | ("logic.counter", "mode")
            | ("logic.counter", "range_mode")
            | ("logic.counter", "step")
            | ("logic.counter", "min_max")
            | ("logic.delay", "mode")
            | ("logic.delay", "time")
            // Oscillator (preview is display-only).
            | ("generator.oscillator", "shape")
            | ("generator.oscillator", "freq")
            | ("generator.oscillator", "phase")
            // Envelope Generator rows.
            | ("generator.envelope", "curve")
            | ("generator.envelope", "time_row")
            | ("generator.envelope", "mode_row")
            | ("generator.envelope", "sustain_row")
            | ("generator.envelope", "grid_row")
            | ("generator.envelope", "grid_options_row")
            // Scope control rows (the display halves are display-only).
            | ("display.oscilloscope", "controls")
            | ("display.trigscope", "controls")
    )
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

pub(crate) const REMAP_DESIGN_W: f32 = 380.0;

pub(crate) fn remap_body_inputs_for(
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
pub(crate) type RemapPinState = (f32, u64, u64);

pub(crate) fn remap_hash_draft(node: &NodeData, with_output: bool) -> u64 {
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

pub(crate) fn remap_hash_mappings(node: &NodeData) -> u64 {
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

pub(crate) fn remap_pin_state_id(outer_layer: egui::LayerId, inner_id: NodeId, tag: &'static str) -> egui::Id {
    egui::Id::new(("fxi_remap_pin_state", outer_layer, inner_id.0, tag))
}

pub(crate) fn remap_layer_id(outer_layer: egui::LayerId, inner_id: NodeId, tag: &'static str) -> egui::LayerId {
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

pub(crate) fn render_remapper_whole_module(
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

pub(crate) fn render_map_action_whole_module(
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
pub(crate) fn tz_cards_hash_map(n: &NodeData) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if let Some(v) = n.params.get("zone_maps") { v.to_string().hash(&mut h); }
    n.params.get("sel_zone").map(|v| v.to_string()).unwrap_or_default().hash(&mut h);
    h.finish()
}
/// Hash of the in-flight Learn capture (phase / trigger / picked output) — snaps
/// the scroll to top when a fresh capture begins, mirroring the Remapper draft.
pub(crate) fn tz_cards_hash_draft(n: &NodeData) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for k in ["_tz_phase", "_tz_trig", "_tz_draft_out"] {
        n.params.get(k).map(|v| v.to_string()).unwrap_or_default().hash(&mut h);
    }
    h.finish()
}

/// Pinned Touch Zones mapping-card list — same whole-module treatment (scale +
/// scroll + clip + interaction) the Remapper/Map Action pins use.
pub(crate) fn render_touch_zone_cards_whole_module(
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
pub(crate) fn combiner_hash_config(n: &NodeData) -> u64 {
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

pub(crate) fn render_combiner_whole_module(
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
pub(crate) const NET_DESIGN_W: f32 = 184.0;

/// Hash of a Network node's config so the whole-module pin re-bases its scroll
/// when the transport / mode changes (mirrors the Combiner's config hash).
pub(crate) fn net_hash_config(n: &NodeData) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for key in ["net_transport", "net_host", "net_port", "net_peer", "net_keep"] {
        if let Some(v) = n.params.get(key) {
            v.to_string().hash(&mut h);
        }
    }
    h.finish()
}

pub(crate) fn render_net_whole_module(
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
pub(crate) fn render_remap_whole_module_impl<BodyFn, MapLenFn, DraftLenFn>(
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

pub(crate) fn render_knob_value(
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

pub(crate) fn render_constant_value(
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

pub(crate) fn render_switch_toggle(
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
pub(crate) fn render_label_text_pinned_scroll(
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

pub(crate) fn render_gyro_mode_row(
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

pub(crate) fn render_gyro_steering_opts_row(
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
pub(crate) fn render_gyro_lean_section_pin(
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

pub(crate) fn render_gyro_lean_threshold_row(
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

pub(crate) fn render_gyro_invert_row(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
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

pub(crate) fn render_accel_invert_row(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
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

pub(crate) fn render_dragvalue_param(
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

pub(crate) fn render_counter_mode(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
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

pub(crate) fn render_counter_range_mode(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
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

pub(crate) fn render_counter_step(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
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

pub(crate) fn render_counter_min_max(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
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

pub(crate) fn render_logic_delay_mode(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
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

pub(crate) fn render_logic_delay_time(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
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

pub(crate) fn render_oscillator_shape(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
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

pub(crate) fn render_oscillator_freq(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
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

pub(crate) fn render_oscillator_phase(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
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

pub(crate) fn render_oscillator_preview(
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
pub(crate) fn estimate_device_body_width(ui: &egui::Ui, node: &NodeData) -> f32 {
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

#[cfg(test)]
mod editable_element_tests {
    use super::is_editable_element;

    #[test]
    fn interactive_controls_are_editable() {
        // A representative slice of the adjustable-control allow-list.
        for (m, e) in [
            ("module.knob", "value"),
            ("module.switch", "toggle"),
            ("module.dropdown", "selection"),
            ("module.response_curve", "curve"),
            ("module.twoway_response_curve", "lane_toggle"),
            ("module.vec_reshape", "pad"),
            ("processing.gyro_3dof", "pointer_mode"),
            ("logic.counter", "step"),
            ("generator.oscillator", "freq"),
            ("module.audio_stream_haptics", "asth_mode_row"),
        ] {
            assert!(is_editable_element(m, e), "{m}/{e} should be editable");
        }
    }

    #[test]
    fn display_and_whole_module_elements_are_not_editable() {
        for (m, e) in [
            // Pure displays.
            ("module.input_viewer", "viewer"),
            ("display.controller3d", "viewer"),
            ("display.readout", "value"),
            ("display.oscilloscope", "display"),
            ("module.svg", "image"),
            ("generator.oscillator", "preview"),
            ("module.audio_stream_haptics", "asth_scope"),
            // Whole-module bodies (too big to be a single tweak).
            ("module.remapper", "whole_module"),
            ("module.map_action", "whole_module"),
            // Unknown / legacy.
            ("module.knob", "default"),
            ("module.nonexistent", "value"),
        ] {
            assert!(!is_editable_element(m, e), "{m}/{e} should NOT be editable");
        }
    }
}




