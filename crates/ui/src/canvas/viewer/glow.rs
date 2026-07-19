//! Signal glow: radial halos, wire brightening, pin glow smoothing, and
//! AutoMap glow resolution across module boundaries.

use super::*;

/// Brighten a wire color based on signal intensity. The `base` is the dim
/// rest color from `pin_info`; under signal we lerp toward the type-color
/// outline (recovered as `base * 20/9`) and then add a small ~20% lerp
/// toward white at peak so flowing wires read as actively lit.
pub(crate) fn brighten_wire_color(base: Color32, intensity: f32) -> Color32 {
    let t = intensity.clamp(0.0, 1.0);
    // Reconstruct the full-saturation outline color from the dimmed base.
    let undim = |c: u8| ((c as u16 * 20 / 9).min(255)) as u8;
    let outline = Color32::from_rgb(undim(base.r()), undim(base.g()), undim(base.b()));
    // Lerp base → outline by t, then outline → white by t*0.2.
    let lerp = |a: u8, b: u8, k: f32| (a as f32 + (b as f32 - a as f32) * k) as u8;
    let bright = Color32::from_rgb(
        lerp(base.r(), outline.r(), t),
        lerp(base.g(), outline.g(), t),
        lerp(base.b(), outline.b(), t),
    );
    let white_t = t * 0.2;
    Color32::from_rgb(
        lerp(bright.r(), 255, white_t),
        lerp(bright.g(), 255, white_t),
        lerp(bright.b(), 255, white_t),
    )
}

/// Paint a soft circular halo via a triangle fan with per-vertex colors.
/// Center color = `hot` premultiplied by intensity; edge color = transparent.
/// Uses `Color32::TRANSPARENT` for the edge so the gradient is premultiplied-
/// correct (no white-fringe artifacts when interpolating).
pub(crate) fn paint_radial_glow(painter: &egui::Painter, center: egui::Pos2, radius: f32, hot: Color32, intensity: f32) {
    use egui::epaint::{Mesh, Vertex};
    const SEGMENTS: usize = 24;
    let mut mesh = Mesh::default();
    let i = intensity.clamp(0.0, 1.0);
    // Premultiplied: scale all channels by alpha = intensity.
    let center_color = Color32::from_rgba_premultiplied(
        (hot.r() as f32 * i) as u8,
        (hot.g() as f32 * i) as u8,
        (hot.b() as f32 * i) as u8,
        (255.0 * i) as u8,
    );
    let edge_color = Color32::TRANSPARENT;
    let uv = egui::Pos2::ZERO;
    mesh.vertices.push(Vertex { pos: center, uv, color: center_color });
    for k in 0..SEGMENTS {
        let theta = (k as f32) / (SEGMENTS as f32) * std::f32::consts::TAU;
        let p = center + egui::vec2(theta.cos(), theta.sin()) * radius;
        mesh.vertices.push(Vertex { pos: p, uv, color: edge_color });
    }
    for k in 0..SEGMENTS {
        let a = (k + 1) as u32;
        let b = ((k + 1) % SEGMENTS + 1) as u32;
        mesh.indices.extend_from_slice(&[0, a, b]);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// Half-disc radial glow. `a0..a1` defines the angular sweep (radians);
/// the fan is built as a triangle fan around `center` with vertices on
/// the arc, so the glow stays on the convex (outward) side of the pin
/// and doesn't bleed into the node body.
pub(crate) fn paint_radial_glow_half(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    hot: Color32,
    intensity: f32,
    a0: f32,
    a1: f32,
) {
    use egui::epaint::{Mesh, Vertex};
    const SEGMENTS: usize = 16;
    let mut mesh = Mesh::default();
    let i = intensity.clamp(0.0, 1.0);
    let center_color = Color32::from_rgba_premultiplied(
        (hot.r() as f32 * i) as u8,
        (hot.g() as f32 * i) as u8,
        (hot.b() as f32 * i) as u8,
        (255.0 * i) as u8,
    );
    let edge_color = Color32::TRANSPARENT;
    let uv = egui::Pos2::ZERO;
    mesh.vertices.push(Vertex { pos: center, uv, color: center_color });
    for k in 0..=SEGMENTS {
        let t = (k as f32) / (SEGMENTS as f32);
        let theta = a0 + (a1 - a0) * t;
        let p = center + egui::vec2(theta.cos(), theta.sin()) * radius;
        mesh.vertices.push(Vertex { pos: p, uv, color: edge_color });
    }
    for k in 0..SEGMENTS {
        let a = (k + 1) as u32;
        let b = (k + 2) as u32;
        mesh.indices.extend_from_slice(&[0, a, b]);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// Convert a `Signal` to a 0..1 glow intensity. Bool→on/off, Float→|v|,
/// Vec2→length, Int→nonzero.
pub(crate) fn signal_intensity(sig: &Signal) -> f32 {
    match sig {
        Signal::Float(f) => f.abs().min(1.0),
        Signal::Bool(b)  => if *b { 1.0 } else { 0.0 },
        Signal::Vec2(v)  => v.length().min(1.0),
        Signal::Vec4(v)  => v.length().min(1.0),
        Signal::Int(i)   => if *i != 0 { 1.0 } else { 0.0 },
    }
}

/// Read prior glow intensity for this pin (memory-cached), lerp toward `target`
/// at a fixed rate, store back, and return the smoothed value. Smoothing
/// prevents the polling-rate raw signal from strobing the visual.
pub(crate) fn pin_glow_smoothed(ctx: &egui::Context, node: egui_snarl::NodeId, pin_idx: usize, is_input: bool, target: f32) -> f32 {
    let key = egui::Id::new(("pin_glow", node.0, pin_idx, is_input));
    let prev = ctx.data(|d| d.get_temp::<f32>(key)).unwrap_or(0.0);
    let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1);
    // Up-fast / down-slow: jumps to bright instantly, decays over ~250 ms.
    let rate = if target > prev { 30.0 } else { 6.0 };
    let smoothed = prev + (target - prev) * (1.0 - (-rate * dt).exp());
    ctx.data_mut(|d| d.insert_temp(key, smoothed));
    smoothed
}

/// Parent-snarl frame for AutoMap glow chain-walking. When the inner editor's
/// canvas wants to resolve glow on a `subpatch.inlet` AutoMap output, it needs
/// to hop into the outer snarl and follow the outer subpatch's matching input
/// wire. `parent` links to grandparent frames for deeply nested sub-patches.
#[derive(Clone, Copy)]
pub struct AutomapGlowParent<'a> {
    pub snarl: &'a Snarl<NodeData>,
    pub subpatch_node_id: NodeId,
    pub prev: Option<&'a AutomapGlowParent<'a>>,
}

/// Compute glow intensity for an AutoMap output bus by walking the chain back
/// to the originating device.source and max-pooling its live signals. Returns
/// `None` when the walk can't resolve a real device (e.g. dangling chain).
pub(crate) fn resolve_automap_glow_output(
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    snarl: &Snarl<NodeData>,
    src: OutPinId,
    parent: Option<&AutomapGlowParent<'_>>,
) -> Option<f32> {
    let node = snarl.get_node(src.node)?;
    match node.module_id.as_str() {
        "device.source" => {
            let dev_id = node.params.get("device_id").and_then(|v| v.as_str())?;
            let pin_ids = node.params.get("output_pin_ids")
                .and_then(|v| v.as_array())?;
            let mut max_i = 0.0_f32;
            for pid in pin_ids.iter().filter_map(|v| v.as_str()) {
                // Battery is a steady-state status readout (always ~full on
                // virtuals, a slowly-changing level on physicals), not input
                // activity — pooling it would pin the whole AutoMap bus glow on
                // permanently. Its own pin glow is suppressed too (output_pin_glow).
                if pid == "battery" {
                    continue;
                }
                // Exclude raw touch X/Y axes from AutoMap bus glow unless the
                // corresponding Touch Active flag is present and true. These
                // axes are only meaningful when Touch Active is asserted;
                // individual touch X/Y pins and their wires remain unaffected.
                if pid == "touch1_x" || pid == "touch1_y" {
                    match live_signals.get(&(dev_id.to_string(), "touch1_active".to_string())) {
                        Some(Signal::Bool(b)) if *b => {}
                        _ => continue,
                    }
                }
                if pid == "touch2_x" || pid == "touch2_y" {
                    match live_signals.get(&(dev_id.to_string(), "touch2_active".to_string())) {
                        Some(Signal::Bool(b)) if *b => {}
                        _ => continue,
                    }
                }
                if let Some(sig) = live_signals.get(&(dev_id.to_string(), pid.to_string())) {
                    max_i = max_i.max(signal_intensity(sig));
                }
            }
            Some(max_i)
        }
        // Pass-through processors: AutoMap activity = activity on their single
        // AutoMap input wire. Audio Stream Haptics passes the bus straight through
        // (it only injects feedback), so it glows from its input like the others.
        // Touch Zones and the Virtual Menu likewise pass the bus through on their
        // AutoMap output (their zone behaviour goes onto the bus / typed ports);
        // without this they fell to the `_` arm, whose `last_out[0]` is None for
        // the passthrough slot — so the port never lit despite live signals.
        "module.automap_split" | "module.automap_collect" | "module.remapper"
        | "module.audio_stream_haptics" | "module.touch_zones" => {
            let am_idx = node.inputs.iter().position(|p| p.signal_type == SignalType::AutoMap)?;
            walk_automap_input(live_signals, snarl, src.node, am_idx, parent)
        }
        // The Virtual Menu is a pass-through too, but while it is OPEN and
        // SUPPRESSING it strips its enabled pointer sources (stick / touch / gyro)
        // from the bus it forwards. Reflect that in the output glow so the port
        // stops lighting from inputs the game never sees — the user's visual cue
        // that suppression is working. Closed / not-suppressing → plain input walk.
        "module.menu" => {
            let am_idx = node.inputs.iter().position(|p| p.signal_type == SignalType::AutoMap)?;
            let open = node.extra.last_out.get(1).and_then(|s| *s)
                .map(|s| s.as_bool()).unwrap_or(false);
            let suppress = node.params.get("suppress_while_open")
                .and_then(|v| v.as_bool()).unwrap_or(true);
            if open && suppress {
                if let Some(g) = menu_output_glow_excluding_suppressed(
                    live_signals, snarl, src.node, am_idx, node, parent)
                {
                    return Some(g);
                }
            }
            walk_automap_input(live_signals, snarl, src.node, am_idx, parent)
        }
        // 3DOF AutoMap output: the bus passes straight through (the module
        // only injects lean dispatch on top), so it glows from its input like
        // the other passthroughs, max-pooled with the absolute Lean value
        // (output index 3) so its own injection shows even on a quiet bus.
        "processing.gyro_3dof" if node.outputs.get(src.output).map(|p| p.signal_type) == Some(SignalType::AutoMap) => {
            let lean = match node.extra.last_signals.get(3) {
                Some(Some(Signal::Float(f))) => f.abs(),
                _ => 0.0,
            };
            let bus = node.inputs.iter().position(|p| p.signal_type == SignalType::AutoMap)
                .and_then(|am_idx| walk_automap_input(live_signals, snarl, src.node, am_idx, parent))
                .unwrap_or(0.0);
            Some(bus.max(lean).clamp(0.0, 1.0))
        }
        "module.automap_fork" => {
            // Either output mirrors the input bus's activity (gating is logical).
            let am_idx = node.inputs.iter().position(|p| p.signal_type == SignalType::AutoMap)?;
            walk_automap_input(live_signals, snarl, src.node, am_idx, parent)
        }
        "module.automap_selector" => {
            // Glow the union of all AutoMap inputs; per-input visibility falls out
            // of each input wire's own walk via input_pin_glow.
            let mut max_i = 0.0_f32;
            for (i, pin) in node.inputs.iter().enumerate() {
                if pin.signal_type != SignalType::AutoMap { continue; }
                if let Some(v) = walk_automap_input(live_signals, snarl, src.node, i, parent) {
                    max_i = max_i.max(v);
                }
            }
            Some(max_i)
        }
        "module.automap_combiner" => {
            let mut max_i = 0.0_f32;
            for (i, pin) in node.inputs.iter().enumerate() {
                if pin.signal_type != SignalType::AutoMap { continue; }
                if let Some(v) = walk_automap_input(live_signals, snarl, src.node, i, parent) {
                    max_i = max_i.max(v);
                }
            }
            Some(max_i)
        }
        "subpatch.inlet" => {
            // Pop out of the inner snarl: find the outer subpatch's matching
            // input pin and walk its wire upstream in the parent frame.
            let pin_idx = node.params.get("pin_index").and_then(|v| v.as_u64())? as usize;
            let p = parent?;
            let outer_in = p.snarl.in_pin(InPinId { node: p.subpatch_node_id, input: pin_idx });
            let upstream = *outer_in.remotes.first()?;
            resolve_automap_glow_output(live_signals, p.snarl, upstream, p.prev)
        }
        "subpatch" => {
            // Descend into the inner snarl: find the matching outlet by pin_index
            // and walk its single input upstream within the inner snarl. Push a
            // parent frame so an inner inlet hop can pop back to *this* snarl.
            let sp = node.subpatch.as_ref()?;
            let outlet_id: NodeId = sp.snarl.nodes_ids_data()
                .find(|(_, n)| n.value.module_id == "subpatch.outlet"
                    && n.value.params.get("pin_index").and_then(|v| v.as_u64())
                        == Some(src.output as u64))
                .map(|(id, _)| id)?;
            let outlet_in = sp.snarl.in_pin(InPinId { node: outlet_id, input: 0 });
            let upstream = *outlet_in.remotes.first()?;
            let frame = AutomapGlowParent {
                snarl,
                subpatch_node_id: src.node,
                prev: parent,
            };
            resolve_automap_glow_output(live_signals, &sp.snarl, upstream, Some(&frame))
        }
        _ => {
            // Generic node with an AutoMap output but no special routing:
            // try last_out, otherwise treat as unknown (no glow).
            let sig = node.extra.last_out.get(src.output).and_then(|s| s.as_ref())?;
            Some(signal_intensity(sig))
        }
    }
}

/// Walk an AutoMap-typed input pin one hop upstream and resolve the bus's
/// originating-device activity.
pub(crate) fn walk_automap_input(
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    snarl: &Snarl<NodeData>,
    node_id: NodeId,
    in_idx: usize,
    parent: Option<&AutomapGlowParent<'_>>,
) -> Option<f32> {
    let pin = snarl.in_pin(InPinId { node: node_id, input: in_idx });
    let src = *pin.remotes.first()?;
    resolve_automap_glow_output(live_signals, snarl, src, parent)
}

/// Output-bus glow for a Virtual Menu while it is open and suppressing: pool the
/// upstream device's live activity but SKIP the pins the menu strips from the bus
/// (its enabled pointer sources), so the AutoMap port reflects the suppressed
/// OUTPUT rather than the raw input. Returns `None` — caller falls back to the
/// plain input walk — when the upstream resolves to a synthetic injector key
/// rather than a raw device (those values aren't in `live_signals`) or can't be
/// resolved at all.
pub(crate) fn menu_output_glow_excluding_suppressed(
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    snarl: &Snarl<NodeData>,
    node_id: NodeId,
    am_idx: usize,
    node: &NodeData,
    parent: Option<&AutomapGlowParent<'_>>,
) -> Option<f32> {
    let src = *snarl.in_pin(InPinId { node: node_id, input: am_idx }).remotes.first()?;
    let dev_id = crate::app::find_automap_device_id_for_viewer(snarl, src, parent)?;
    // Synthetic injector keys (an upstream Remapper / Collector / …) aren't in
    // live_signals — fall back to the plain walk for those chains.
    if ["collector:", "remap:", "combiner:", "forksel:", "touchmap:", "menumap:", "lean:"]
        .iter().any(|p| dev_id.starts_with(p))
    {
        return None;
    }
    let excluded = menu_suppressed_pin_set(node);
    let mut max_i = 0.0_f32;
    for ap in flexinput_core::automap::ALL_PINS {
        if excluded.contains(ap.id) { continue; }
        // Touch X/Y only count while the matching Active flag is asserted
        // (mirrors the device.source pool).
        if (ap.id == "touch1_x" || ap.id == "touch1_y")
            && !matches!(live_signals.get(&(dev_id.clone(), "touch1_active".to_string())),
                         Some(Signal::Bool(true)))
        { continue; }
        if (ap.id == "touch2_x" || ap.id == "touch2_y")
            && !matches!(live_signals.get(&(dev_id.clone(), "touch2_active".to_string())),
                         Some(Signal::Bool(true)))
        { continue; }
        if let Some(sig) = live_signals.get(&(dev_id.clone(), ap.id.to_string())) {
            max_i = max_i.max(signal_intensity(sig));
        }
    }
    Some(max_i)
}

/// The canonical pins a Virtual Menu strips from its forwarded bus while open +
/// suppressing — its ENABLED pointer sources. Mirrors the suppression set in
/// `flexinput_engine::eval::eval_menu_node`.
pub(crate) fn menu_suppressed_pin_set(node: &NodeData) -> std::collections::HashSet<&'static str> {
    let mut ex: std::collections::HashSet<&'static str> = std::collections::HashSet::new();
    let pb = |k: &str, d: bool| node.params.get(k).and_then(|v| v.as_bool()).unwrap_or(d);
    let legacy = node.params.get("pointer_source").and_then(|v| v.as_str()).unwrap_or("left_stick");
    if pb("ptr_ls", legacy == "left_stick") {
        ex.extend(["left_stick", "left_stick_x", "left_stick_y"]);
    }
    if pb("ptr_rs", legacy == "right_stick") {
        ex.extend(["right_stick", "right_stick_x", "right_stick_y"]);
    }
    if pb("ptr_touch", legacy == "touch1" || legacy == "touch2") {
        let which = node.params.get("ptr_touch_which").and_then(|v| v.as_str())
            .unwrap_or(if legacy == "touch2" { "touch2" } else { "touch1" });
        if which == "touch2" {
            ex.extend(["touch2_x", "touch2_y", "touch2_active", "btn_touchpad"]);
        } else {
            ex.extend(["touch1_x", "touch1_y", "touch1_active", "btn_touchpad"]);
        }
    }
    if pb("ptr_gyro", false) {
        ex.extend(["gyro_x", "gyro_y", "gyro_z"]);
    }
    ex
}

/// Look up live activity for any output pin: device sources read from
/// `live_signals`; other modules read from `NodeExtra.last_out`.
/// Returns `None` when no value is available yet.
pub(crate) fn output_pin_glow(
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    snarl: &Snarl<NodeData>,
    node_id: NodeId,
    out_idx: usize,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) -> Option<(Color32, f32)> {
    let node = snarl.get_node(node_id)?;
    let desc = node.outputs.get(out_idx)?;
    // AutoMap pins: no scalar value flows; intensity comes from walking the
    // chain back to a device.source. Handles every AutoMap-emitting module
    // (Splitter / Collector / Fork / Selector / Combiner / Remapper / Inlet /
    // Subpatch outer node) plus the original device.source max-pool.
    if desc.signal_type == SignalType::AutoMap {
        let src = OutPinId { node: node_id, output: out_idx };
        let intensity = resolve_automap_glow_output(live_signals, snarl, src, automap_parent)
            .unwrap_or(0.0);
        let [r, g, b] = SignalType::AutoMap.color_rgb();
        return Some((Color32::from_rgb(r, g, b), intensity));
    }
    // Device source's other outputs: read from live_signals.
    if node.module_id == "device.source" {
        let dev_id = node.params.get("device_id").and_then(|v| v.as_str())?;
        let pin_id = node.params.get("output_pin_ids")
            .and_then(|v| v.as_array())
            .and_then(|a| a.get(out_idx))
            .and_then(|v| v.as_str())?;
        let sig = live_signals.get(&(dev_id.to_string(), pin_id.to_string()))?;
        // Battery is a steady-state status readout, not activity — keep its own
        // pin/wire glow dark (matches its exclusion from the AutoMap bus pool).
        // The value still renders; only the activity glow is suppressed.
        let intensity = if pin_id == "battery" { 0.0 } else { signal_intensity(sig) };
        let [r, g, b] = sig.signal_type().color_rgb();
        return Some((Color32::from_rgb(r, g, b), intensity));
    }
    // Module nodes: latest evaluated output from NodeExtra.last_out.
    let sig = node.extra.last_out.get(out_idx).and_then(|s| s.as_ref())?;
    let intensity = signal_intensity(sig);
    let [r, g, b] = sig.signal_type().color_rgb();
    Some((Color32::from_rgb(r, g, b), intensity))
}

/// Look up live activity for any input pin by walking back to the upstream
/// output's value. Falls back to `NodeExtra.last_signals` when the wire
/// source can't be resolved (rare; legacy nodes).
pub(crate) fn input_pin_glow(
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    snarl: &Snarl<NodeData>,
    node: &NodeData,
    node_id: NodeId,
    in_idx: usize,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) -> Option<(Color32, f32)> {
    let desc = node.inputs.get(in_idx)?;
    // AutoMap inputs: light up when the upstream chain has device activity.
    if desc.signal_type == SignalType::AutoMap {
        let pin_id = InPinId { node: node_id, input: in_idx };
        let src = snarl.in_pin(pin_id).remotes.first().copied()?;
        let i = resolve_automap_glow_output(live_signals, snarl, src, automap_parent)
            .unwrap_or(0.0);
        let [r, g, b] = SignalType::AutoMap.color_rgb();
        return Some((Color32::from_rgb(r, g, b), i));
    }
    // Walk upstream: take the live value from whatever feeds this input.
    let pin_id = InPinId { node: node_id, input: in_idx };
    let pin = snarl.in_pin(pin_id);
    if let Some(src) = pin.remotes.first().copied() {
        if let Some(glow) = output_pin_glow(live_signals, snarl, src.node, src.output, automap_parent) {
            return Some(glow);
        }
    }
    // Fallback: most-recently evaluated input value stashed on the node itself.
    let sig = node.extra.last_signals.get(in_idx).and_then(|s| s.as_ref())?;
    let intensity = signal_intensity(sig);
    let [r, g, b] = sig.signal_type().color_rgb();
    Some((Color32::from_rgb(r, g, b), intensity))
}

/// Derive the screen-space Y at which a header-relocated AutoMap pin should
/// be drawn, given the `pin_ui` passed into `show_input` / `show_output` for
/// the AutoMap column slot.
///
/// `pin_ui.clip_rect().top()` equals `node_rect.min.y` (snarl shrinks the
/// payload clip to start at the node's top edge), so the chevron's Y center
/// is `node_top + header_frame_margin.top + chevron_height/2`. Using this
/// snarl-stable, per-frame screen Y avoids cross-frame caching entirely —
/// the pin tracks the node's Y exactly during drag and never jumps on
/// collapse/expand transitions.
pub(crate) fn automap_chevron_y(pin_ui: &egui::Ui) -> f32 {
    // Header frame margin top — must match Canvas::new's header_frame config
    // (egui::Margin::symmetric(6, 4)).
    const HEADER_MARGIN_TOP: f32 = 4.0;
    let chevron_half = pin_ui.spacing().icon_width * 0.5;
    pin_ui.clip_rect().top() + HEADER_MARGIN_TOP + chevron_half
}

/// Per-node cache key holding the absolute screen Y of the AutoMap header
/// label center, stashed by `show_header`. Read one frame stale by the
/// pin callbacks. See module docs above the pin functions for the delta
/// recovery scheme that eliminates the resulting drag lag.
pub(crate) fn automap_label_abs_y_key(node: egui_snarl::NodeId) -> egui::Id {
    egui::Id::new(("device_sink_automap_label_abs_y_v2", node.0))
}

/// Companion of `automap_label_abs_y_key`: the AutoMap pin-row Y as seen
/// by `show_input` this frame, stashed for next-frame delta calculation.
pub(crate) fn automap_pin_row_y_key(node: egui_snarl::NodeId) -> egui::Id {
    egui::Id::new(("device_sink_automap_pin_row_y", node.0))
}
