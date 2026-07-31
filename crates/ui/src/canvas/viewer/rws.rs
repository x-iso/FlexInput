//! RWS Aim module body (Real-World Sensitivity).
//!
//! Layout: the input-mode dropdown and the Calibrate Start/Stop button live in
//! the node HEADER (see `show_header`); the body holds the numeric knobs (Scale
//! / RWS), the calibration ruler viewport (`field`), and its style row. Every
//! element is registered as a pinnable + gamepad-editable element so it can be
//! dropped onto the config overlay for live calibration — the ruler `field`
//! itself carries the full calibration control set (Scale / Calibrate / Speed /
//! RWS) so it can be run AND stopped from the gamepad with nothing else pinned,
//! which matters because the mouse output is busy driving the game meanwhile.
//! Real evaluation is `compute_rws` in the engine.

use super::*;

/// Signal-graph gyro normalization: ±1.0 == ±this many deg/s. Mirrors the engine
/// `compute_rws`'s constant (kept local to avoid a cross-crate path for the live
/// preview's rate → deg/s conversion; keep the two in sync).
const GYRO_REF_DPS: f32 = 2000.0;

/// Read the ruler style params (transparent-BG by default).
fn rws_field_style(snarl: &Snarl<NodeData>, node_id: NodeId) -> (f32, f32, bool) {
    snarl
        .get_node(node_id)
        .map(|n| {
            let a = n.params.get("field_bg_alpha").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            let t = n.params.get("field_tick_deg").and_then(|v| v.as_f64()).unwrap_or(15.0) as f32;
            let l = n.params.get("field_labels").and_then(|v| v.as_bool()).unwrap_or(true);
            (a.clamp(0.0, 1.0), t.clamp(5.0, 90.0), l)
        })
        .unwrap_or((0.0, 15.0, true))
}

pub(crate) fn show_rws_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (scale, rws) = snarl
        .get_node(node_id)
        .map(|n| {
            let scale = n.params.get("scale").and_then(|v| v.as_f64()).unwrap_or(100.0) as f32;
            let rws = n.params.get("rws").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            (scale, rws)
        })
        .unwrap_or((100.0, 1.0));
    let (bg_alpha, tick_deg, labels) = rws_field_style(snarl, node_id);

    let mut set: Vec<(&str, Value)> = Vec::new();

    // The snarl body ui is left-to-right by default; wrap so the rows stack.
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);

    // Scale — mouse counts per degree; the calibrated ground truth (the value box
    // the calibration viewport edits in the config overlay).
    let r_scale = ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Scale").small())
            .on_hover_text("Mouse counts per degree — the calibrated 1:1 ground truth.\nCalibrate this (RWS 1 then feels like a 1:1 physical rotation).");
        let mut v = scale;
        if ui.add(egui::DragValue::new(&mut v).speed(0.05).range(0.0..=100_000.0)).changed() {
            if let Some(n) = Number::from_f64(v as f64) { set.push(("scale", Value::Number(n))); }
        }
    });
    register_exposable_element(ui, node_id, "scale", r_scale.response.rect);

    // RWS multiplier relative to the calibrated ground truth.
    let r_rws = ui.horizontal(|ui| {
        ui.label(egui::RichText::new("RWS").small())
            .on_hover_text("Sensitivity as a multiple of the calibrated 1:1 ground truth.\n1.0 = matches your physical rotation; 2.0 = twice as fast.");
        let mut v = rws;
        if ui.add(egui::DragValue::new(&mut v).speed(0.01).range(0.01..=50.0)).changed() {
            if let Some(n) = Number::from_f64(v as f64) { set.push(("rws", Value::Number(n))); }
        }
    });
    register_exposable_element(ui, node_id, "rws", r_rws.response.rect);

    // Calibration viewport (own row). Writes `scale` directly via its centre box,
    // so it must run before the `set` flush below (disjoint params — no conflict).
    let fw = ui.available_width().clamp(120.0, 260.0);
    let frect = render_rws_field(node_id, ui, snarl, egui::vec2(fw, 100.0), false);
    register_exposable_element(ui, node_id, "field", frect);

    // View + style row.
    let mode = snarl.get_node(node_id)
        .and_then(|n| n.params.get("field_mode").and_then(|v| v.as_str()))
        .unwrap_or("ruler").to_string();
    let fov = snarl.get_node(node_id)
        .and_then(|n| n.params.get("field_fov").and_then(|v| v.as_f64()))
        .unwrap_or(90.0) as f32;
    let r_style = ui.horizontal(|ui| {
        for (val, lbl) in [("ruler", "Ruler"), ("room", "Room"), ("both", "Both")] {
            if ui.selectable_label(mode == val, egui::RichText::new(lbl).small()).clicked() {
                set.push(("field_mode", Value::String(val.to_string())));
            }
        }
        ui.separator();
        let mut a = bg_alpha;
        if ui.add(egui::DragValue::new(&mut a).speed(0.01).range(0.0..=1.0).prefix("BG "))
            .on_hover_text("Viewport background opacity (0 = transparent).")
            .changed()
        {
            if let Some(n) = Number::from_f64(a as f64) { set.push(("field_bg_alpha", Value::Number(n))); }
        }
        if mode != "ruler" {
            let mut fv = fov;
            if ui.add(egui::DragValue::new(&mut fv).speed(1.0).range(30.0..=140.0).prefix("FOV ").suffix("°"))
                .on_hover_text("Horizontal field of view — set this to match your in-game FOV so the turn rate reads 1:1.")
                .changed()
            {
                if let Some(n) = Number::from_f64(fv as f64) { set.push(("field_fov", Value::Number(n))); }
            }
        }
        if mode != "room" {
            let mut td = tick_deg;
            if ui.add(egui::DragValue::new(&mut td).speed(1.0).range(5.0..=90.0).suffix("°"))
                .on_hover_text("Minor tick spacing.")
                .changed()
            {
                if let Some(n) = Number::from_f64(td as f64) { set.push(("field_tick_deg", Value::Number(n))); }
            }
            let mut lb = labels;
            if ui.checkbox(&mut lb, egui::RichText::new("labels").small()).changed() {
                set.push(("field_labels", Value::Bool(lb)));
            }
        }
    });
    register_exposable_element(ui, node_id, "style", r_style.response.rect);

    ui.label(egui::RichText::new("→ wire Mouse to KB/M “Mouse XY (move)”").small().weak())
        .on_hover_text("RWS drives the mouse via the displacement pin, which ignores the\nKB/M card's mouse sensitivity — so the calibration is portable.");
    }); // ui.vertical

    if !set.is_empty() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            for (k, v) in set {
                node.params.insert(k.to_string(), v);
            }
        }
    }
}

/// Input-mode combo (+ max °/s when in stick-rate mode), as a standalone
/// pinnable element. Mirrors the header control sized to the pinned container.
pub(crate) fn render_rws_input(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let input_mode = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("input_mode").and_then(|v| v.as_str()))
        .unwrap_or("gyro")
        .to_string();
    let mut set: Vec<(&str, Value)> = Vec::new();
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(150.0, 22.0));
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt((node_id, "rws_pin_input"))
            .selected_text(if input_mode == "stick_rate" { "Stick" } else { "Gyro" })
            .width(72.0)
            .show_ui(ui, |ui| {
                for (val, lbl) in [("gyro", "Gyro"), ("stick_rate", "Stick (rate)")] {
                    if ui.selectable_label(input_mode == val, lbl).clicked() {
                        set.push(("input_mode", Value::String(val.to_string())));
                    }
                }
            });
        if input_mode == "stick_rate" {
            let mut mr = snarl
                .get_node(node_id)
                .and_then(|n| n.params.get("max_rate_dps").and_then(|v| v.as_f64()))
                .unwrap_or(360.0) as f32;
            if ui.add(egui::DragValue::new(&mut mr).speed(5.0).range(1.0..=100_000.0).suffix(" °/s")).changed() {
                if let Some(n) = Number::from_f64(mr as f64) { set.push(("max_rate_dps", Value::Number(n))); }
            }
        }
    });
    if !set.is_empty() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            for (k, v) in set { node.params.insert(k.to_string(), v); }
        }
    }
}

/// Calibration Start/Stop + spin-speed row, as a standalone pinnable element.
pub(crate) fn render_rws_cal(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (calibrating, cal_speed) = snarl
        .get_node(node_id)
        .map(|n| {
            (
                n.params.get("calibrating").and_then(|v| v.as_bool()).unwrap_or(false),
                n.params.get("cal_speed").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32,
            )
        })
        .unwrap_or((false, 0.5));

    let mut set: Vec<(&str, Value)> = Vec::new();
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(170.0, 24.0));
    ui.horizontal(|ui| {
        let (txt, col) = if calibrating {
            ("■ Stop", egui::Color32::from_rgb(230, 120, 110))
        } else {
            ("▶ Calibrate", egui::Color32::from_rgb(120, 200, 140))
        };
        if ui.add(egui::Button::new(egui::RichText::new(txt).color(col).strong())).clicked() {
            set.push(("calibrating", Value::Bool(!calibrating)));
        }
        let mut v = cal_speed;
        let w = pin_flex_width(ui, container, 72.0);
        let h = ui.spacing().interact_size.y;
        if ui
            .add_sized([w, h], egui::DragValue::new(&mut v).speed(0.01).range(0.05..=10.0).suffix(" rev/s"))
            .changed()
        {
            if let Some(n) = Number::from_f64(v as f64) { set.push(("cal_speed", Value::Number(n))); }
        }
    });
    if !set.is_empty() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            for (k, v) in set { node.params.insert(k.to_string(), v); }
        }
    }
}

/// Ruler style row (BG opacity / tick spacing / labels), as a standalone
/// pinnable element.
pub(crate) fn render_rws_style(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (bg_alpha, tick_deg, labels) = rws_field_style(snarl, node_id);
    let (mode, fov) = snarl.get_node(node_id).map(|n| {
        (
            n.params.get("field_mode").and_then(|v| v.as_str()).unwrap_or("ruler").to_string(),
            n.params.get("field_fov").and_then(|v| v.as_f64()).unwrap_or(90.0) as f32,
        )
    }).unwrap_or(("ruler".to_string(), 90.0));
    let mut set: Vec<(&str, Value)> = Vec::new();
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(200.0, 22.0));
    ui.horizontal(|ui| {
        for (val, lbl) in [("ruler", "Ruler"), ("room", "Room"), ("both", "Both")] {
            if ui.selectable_label(mode == val, egui::RichText::new(lbl).small()).clicked() {
                set.push(("field_mode", Value::String(val.to_string())));
            }
        }
        let mut a = bg_alpha;
        if ui.add(egui::DragValue::new(&mut a).speed(0.01).range(0.0..=1.0).prefix("BG ")).changed() {
            if let Some(n) = Number::from_f64(a as f64) { set.push(("field_bg_alpha", Value::Number(n))); }
        }
        if mode != "ruler" {
            let mut fv = fov;
            if ui.add(egui::DragValue::new(&mut fv).speed(1.0).range(30.0..=140.0).prefix("FOV ").suffix("°")).changed() {
                if let Some(n) = Number::from_f64(fv as f64) { set.push(("field_fov", Value::Number(n))); }
            }
        }
        if mode != "room" {
            let mut td = tick_deg;
            if ui.add(egui::DragValue::new(&mut td).speed(1.0).range(5.0..=90.0).suffix("°")).changed() {
                if let Some(n) = Number::from_f64(td as f64) { set.push(("field_tick_deg", Value::Number(n))); }
            }
            let mut lb = labels;
            if ui.checkbox(&mut lb, egui::RichText::new("labels").small()).changed() {
                set.push(("field_labels", Value::Bool(lb)));
            }
        }
    });
    if !set.is_empty() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            for (k, v) in set { node.params.insert(k.to_string(), v); }
        }
    }
}

/// Draw the calibration viewport: a scrolling degree ruler with a fixed centre
/// reference marker plus an editable Scale box. While `calibrating`, the ruler
/// scrolls at the known `cal_speed` (rev/s) — the reference the user matches the
/// game to; otherwise it follows the module's live yaw output (recovered from the
/// per-tick displacement) so the user can re-check that the lock still holds.
/// Background is transparent by default (`field_bg_alpha` = 0).
/// Returns the allocated rect so the body can register it as pinnable.
pub(crate) fn render_rws_field(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    is_pinned: bool,
) -> egui::Rect {
    let (calibrating, cal_speed, scale, rws) = snarl
        .get_node(node_id)
        .map(|n| {
            (
                n.params.get("calibrating").and_then(|v| v.as_bool()).unwrap_or(false),
                n.params.get("cal_speed").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32,
                n.params.get("scale").and_then(|v| v.as_f64()).unwrap_or(100.0) as f32,
                n.params.get("rws").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
            )
        })
        .unwrap_or((false, 0.5, 100.0, 1.0));
    let (bg_alpha, tick_deg, labels) = rws_field_style(snarl, node_id);
    let (mode, fov, input_mode, max_rate) = snarl
        .get_node(node_id)
        .map(|n| {
            (
                n.params.get("field_mode").and_then(|v| v.as_str()).unwrap_or("ruler").to_string(),
                n.params.get("field_fov").and_then(|v| v.as_f64()).unwrap_or(90.0) as f32,
                n.params.get("input_mode").and_then(|v| v.as_str()).unwrap_or("gyro").to_string(),
                n.params.get("max_rate_dps").and_then(|v| v.as_f64()).unwrap_or(360.0) as f32,
            )
        })
        .unwrap_or(("ruler".to_string(), 90.0, "gyro".to_string(), 360.0));

    let size = egui::vec2(container.x.max(80.0), container.y.max(48.0));
    let (rect, _resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    let vis_rect = rect.intersect(ui.clip_rect());
    if vis_rect.width() < 2.0 || vis_rect.height() < 2.0 {
        return rect;
    }
    let painter = ui.painter_at(vis_rect);

    // Background — transparent by default; opaque only if the user dials it up.
    if bg_alpha > 0.001 {
        let a = (bg_alpha * 255.0).round().clamp(0.0, 255.0) as u8;
        painter.rect_filled(rect, 4.0, egui::Color32::from_rgba_unmultiplied(16, 18, 24, a));
    }

    // Phase accumulation (degrees), persisted per layer + node. Calibrating →
    // integrate the known spin; live → recover physical degrees from the yaw
    // displacement the module last emitted (out 1 = X counts).
    let dt = ui.input(|i| i.stable_dt).clamp(0.0, 0.1);
    let key = egui::Id::new(("rws_field_phase", ui.layer_id().id, node_id.0));
    let prevcal_key = egui::Id::new(("rws_field_prevcal", ui.layer_id().id, node_id.0));
    let mut phase = ui.ctx().data(|d| d.get_temp::<f32>(key)).unwrap_or(0.0);
    // Reset to the home heading (0°) the moment calibration STOPS.
    let prev_cal: bool = ui.ctx().data(|d| d.get_temp::<bool>(prevcal_key)).unwrap_or(false);
    if prev_cal && !calibrating {
        phase = 0.0;
    }
    ui.ctx().data_mut(|d| d.insert_temp(prevcal_key, calibrating));

    if calibrating {
        // Calibration is observed on the PINNED overlay reference. The canvas
        // module body (and any other non-pinned copy) stays parked at home so it
        // doesn't spin distractingly while you calibrate elsewhere.
        if is_pinned {
            phase += cal_speed * 360.0 * dt;
        } else {
            phase = 0.0;
        }
    } else {
        // Live: rotate the reference at the SAME rate the aim OUTPUT produces —
        // RWS applied — so you can check the room still tracks the game. Read the
        // input rotation RATE from the wire source (a rate, so integrating by the
        // UI dt is cadence-independent, unlike the per-tick output displacement)
        // and interpret it exactly as compute_rws does.
        let rate = snarl
            .in_pin(InPinId { node: node_id, input: 0 })
            .remotes
            .first()
            .copied()
            .and_then(|src| snarl.get_node(src.node).and_then(|n| n.extra.last_out.get(src.output).copied().flatten()))
            .map(|s| match s {
                Signal::Vec2(v) => v.x,
                Signal::Float(f) => f,
                _ => 0.0,
            })
            .unwrap_or(0.0);
        let k = if input_mode == "stick_rate" { max_rate } else { GYRO_REF_DPS };
        phase += rate * k * rws * dt;
    }
    if !phase.is_finite() {
        phase = 0.0;
    }
    phase = phase.rem_euclid(360.0);
    ui.ctx().data_mut(|d| d.insert_temp(key, phase));

    let cx = rect.center().x;
    let show_room = mode == "room" || mode == "both";
    let show_ruler = mode == "ruler" || mode == "both";
    if show_room {
        // 3D cube-room interior: a stronger rotation reference than the flat
        // ruler. Rotates at exactly `phase` (scale-independent), FOV-matched to
        // the game so the visual turn rate matches when Scale is calibrated.
        paint_rws_room(&painter, rect, phase, fov, bg_alpha);
    }
    if show_ruler {
        // In "both" mode the ruler is a centred band across the room (aligned
        // with the centre reference marker); otherwise it fills the field.
        let ruler_rect = if mode == "both" {
            let h = (rect.height() * 0.4).clamp(24.0, 60.0);
            egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), h))
        } else {
            rect
        };
        paint_rws_ruler(&painter, ruler_rect, phase, tick_deg, labels, mode == "both");
    }

    // Fixed centre reference marker (where the camera points "now").
    painter.line_segment(
        [egui::pos2(cx, rect.top() + 2.0), egui::pos2(cx, rect.bottom() - 2.0)],
        egui::Stroke::new(1.5, egui::Color32::from_rgb(120, 200, 255)),
    );

    // Status label (bottom-left).
    let (status, scol) = if calibrating {
        (format!("● CAL {cal_speed:.2} rev/s"), egui::Color32::from_rgb(120, 220, 140))
    } else {
        ("live".to_string(), egui::Color32::from_gray(120))
    };
    painter.text(
        egui::pos2(rect.left() + 4.0, rect.bottom() - 3.0),
        egui::Align2::LEFT_BOTTOM,
        status,
        egui::FontId::proportional(9.0),
        scol,
    );

    // Editable Scale box, overlaid top-centre (the "value box in the middle").
    // Only on the pinned overlay reference: the module body already has its own
    // Scale row, and `ui.put`'s absolute placement would disrupt the body's
    // vertical layout (pushing the following rows over the viewport).
    if is_pinned {
        let box_w = (rect.width() * 0.5).clamp(56.0, 120.0);
        let box_h = (rect.height() * 0.3).clamp(18.0, 24.0);
        let box_rect = egui::Rect::from_center_size(
            egui::pos2(cx, rect.top() + box_h * 0.5 + 3.0),
            egui::vec2(box_w, box_h),
        );
        let mut sv = scale;
        let dv = egui::DragValue::new(&mut sv)
            .speed(0.05)
            .max_decimals(3)
            .range(0.0..=100_000.0);
        if ui
            .put(box_rect, dv)
            .on_hover_text("Calibrated Scale (mouse counts / degree).\nDrag until the game matches this reference's spin.")
            .changed()
        {
            if let (Some(n), Some(num)) = (snarl.get_node_mut(node_id), Number::from_f64(sv as f64)) {
                n.params.insert("scale".into(), Value::Number(num));
            }
        }
    }

    ui.ctx().request_repaint();
    rect
}

/// Draw the flat degree ruler into `rect`: a horizontal scale scrolling under
/// the field centre. `compact` shrinks the ticks for the "both"-mode strip.
fn paint_rws_ruler(
    painter: &egui::Painter,
    rect: egui::Rect,
    phase: f32,
    tick_deg: f32,
    labels: bool,
    compact: bool,
) {
    let cx = rect.center().x;
    let cy = rect.center().y;
    let span_deg = 180.0_f32;
    let ppd = rect.width() / span_deg;
    let half = span_deg / 2.0;
    let (minor_h, major_h) = if compact { (4.0, 8.0) } else { (6.0, 12.0) };
    painter.line_segment(
        [egui::pos2(rect.left(), cy), egui::pos2(rect.right(), cy)],
        egui::Stroke::new(1.0, egui::Color32::from_gray(90)),
    );

    // Minor ticks at the chosen spacing.
    let minor_stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(115));
    let mut d = ((phase - half) / tick_deg).ceil() * tick_deg;
    while d <= phase + half {
        let x = cx + (d - phase) * ppd;
        painter.line_segment([egui::pos2(x, cy - minor_h), egui::pos2(x, cy + minor_h)], minor_stroke);
        d += tick_deg;
    }

    // Major ticks + labels at each 90° (independent of the minor spacing).
    let major_stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(170));
    let font = egui::FontId::proportional((rect.height() * 0.16).clamp(7.0, 11.0));
    let mut d = ((phase - half) / 90.0).ceil() * 90.0;
    while d <= phase + half {
        let x = cx + (d - phase) * ppd;
        painter.line_segment([egui::pos2(x, cy - major_h), egui::pos2(x, cy + major_h)], major_stroke);
        if labels && !compact {
            let deg = (d.rem_euclid(360.0)).round() as i32 % 360;
            painter.text(
                egui::pos2(x, cy + major_h + 1.0),
                egui::Align2::CENTER_TOP,
                format!("{deg}°"),
                font.clone(),
                egui::Color32::from_gray(200),
            );
        }
        d += 90.0;
    }
}

/// Sutherland–Hodgman clip of a convex polygon (camera space) against the near
/// plane (forward = −z ≥ `near`). Keeps the room's face fills finite when the
/// camera sits inside and a face wraps behind it.
fn clip_poly_near(verts: &[glam::Vec3], near: f32) -> Vec<glam::Vec3> {
    let mut out: Vec<glam::Vec3> = Vec::with_capacity(verts.len() + 2);
    let n = verts.len();
    for i in 0..n {
        let cur = verts[i];
        let nxt = verts[(i + 1) % n];
        let cf = -cur.z; // forward distance
        let nf = -nxt.z;
        let cin = cf >= near;
        let nin = nf >= near;
        if cin {
            out.push(cur);
        }
        if cin != nin {
            let t = (near - cf) / (nf - cf);
            out.push(cur + (nxt - cur) * t);
        }
    }
    out
}

/// Paint a cube-room interior seen from a camera at its centre, yawed by
/// `yaw_deg`, with a horizontal field-of-view of `fov_deg`. Each wall/floor/
/// ceiling is a distinctly-coloured grid so rotation direction is unmistakable;
/// `shade` (0..1, the BG-opacity slider) adds per-cell "headlight"-shaded solid
/// fills (bright head-on, dark at grazing corners) for real depth across each
/// wall instead of one flat tone. The camera sits inside, so every face straddles
/// the near plane — segments and face polygons are clipped to it before
/// projection. Pure painter (no wgpu): a rotation reference the user matches the
/// game camera to (FOV-matched → equal on-screen turn rate at 1:1).
fn paint_rws_room(painter: &egui::Painter, rect: egui::Rect, yaw_deg: f32, fov_deg: f32, shade: f32) {
    use glam::{Quat, Vec3};
    let r = 1.0_f32;
    let cx = rect.center().x;
    let cy = rect.center().y;
    let near = 0.02_f32;
    let fov = fov_deg.clamp(20.0, 160.0).to_radians();
    // Focal length in pixels: half-width / tan(halfFOV). At 90° the front wall
    // exactly fills the viewport width — matching a game at the same FOV.
    let f = (rect.width() * 0.5) / (fov * 0.5).tan();
    // World→camera. A positive yaw (mouse/aim to the RIGHT) must scroll the room
    // to the LEFT, so the world rotates by +yaw about Y here (a point dead ahead
    // moves to −x in camera space).
    let inv = Quat::from_rotation_y(yaw_deg.to_radians());
    let project = |p: Vec3| -> egui::Pos2 {
        let z = -p.z;
        egui::pos2(cx + f * p.x / z, cy - f * p.y / z)
    };

    // Clip world segment a→b to the near plane, then project both ends. Returns
    // `None` when the whole segment is behind the camera.
    let clip_project = |aw: Vec3, bw: Vec3| -> Option<(egui::Pos2, egui::Pos2)> {
        let mut a = inv * aw;
        let mut b = inv * bw;
        let za = -a.z; // forward distance (camera looks down −Z)
        let zb = -b.z;
        if za <= near && zb <= near {
            return None;
        }
        if za <= near || zb <= near {
            let t = (near - za) / (zb - za);
            let mid = a + (b - a) * t;
            if za <= near {
                a = mid;
            } else {
                b = mid;
            }
        }
        Some((project(a), project(b)))
    };
    let line = |aw: Vec3, bw: Vec3, stroke: egui::Stroke| {
        if let Some((pa, pb)) = clip_project(aw, bw) {
            painter.line_segment([pa, pb], stroke);
        }
    };

    // Distinct colours per face so orientation reads at a glance.
    let front = egui::Color32::from_rgb(210, 90, 80); // z = −r (ahead at yaw 0)
    let back = egui::Color32::from_rgb(80, 130, 210); // z = +r
    let left = egui::Color32::from_rgb(90, 190, 110); // x = −r
    let right = egui::Color32::from_rgb(220, 160, 70); // x = +r
    let floor = egui::Color32::from_gray(70);
    let ceil = egui::Color32::from_gray(150);

    // Shaded solid fills (opacity = the BG slider). Each face is a grid of small
    // cells, and every cell is shaded by a "headlight" term |view·normal| — the
    // camera sits at the room centre, so a wall's middle faces the camera head-on
    // (bright) while its corners are grazing (dark). That per-cell gradient gives
    // real depth instead of one flat tone per wall. Faces drawn far→near so the
    // nearer wall layers on top. Each face: origin `p0`, span axes `ax`,`ay`,
    // colour, world normal.
    if shade > 0.02 {
        let d = 2.0 * r;
        let faces: [(Vec3, Vec3, Vec3, egui::Color32, Vec3); 6] = [
            (Vec3::new(-r, -r, -r), Vec3::new(d, 0.0, 0.0), Vec3::new(0.0, 0.0, d), floor, Vec3::Y),
            (Vec3::new(-r, r, -r), Vec3::new(d, 0.0, 0.0), Vec3::new(0.0, 0.0, d), ceil, Vec3::Y),
            (Vec3::new(-r, -r, -r), Vec3::new(d, 0.0, 0.0), Vec3::new(0.0, d, 0.0), front, Vec3::Z),
            (Vec3::new(-r, -r, r), Vec3::new(d, 0.0, 0.0), Vec3::new(0.0, d, 0.0), back, Vec3::Z),
            (Vec3::new(-r, -r, -r), Vec3::new(0.0, 0.0, d), Vec3::new(0.0, d, 0.0), left, Vec3::X),
            (Vec3::new(r, -r, -r), Vec3::new(0.0, 0.0, d), Vec3::new(0.0, d, 0.0), right, Vec3::X),
        ];
        let a8 = (shade * 205.0).round().clamp(0.0, 255.0) as u8;
        let m = 4; // shading cells per axis
        let mut order: Vec<usize> = (0..6).collect();
        let fdepth = |i: usize| -> f32 {
            let (p0, ax, ay, ..) = faces[i];
            -(inv * (p0 + ax * 0.5 + ay * 0.5)).z
        };
        order.sort_by(|&a, &b| fdepth(b).partial_cmp(&fdepth(a)).unwrap_or(std::cmp::Ordering::Equal));
        for idx in order {
            let (p0, ax, ay, base, normal) = faces[idx];
            for gi in 0..m {
                for gj in 0..m {
                    let (u0, u1) = (gi as f32 / m as f32, (gi + 1) as f32 / m as f32);
                    let (v0, v1) = (gj as f32 / m as f32, (gj + 1) as f32 / m as f32);
                    let corners = [
                        p0 + ax * u0 + ay * v0,
                        p0 + ax * u1 + ay * v0,
                        p0 + ax * u1 + ay * v1,
                        p0 + ax * u0 + ay * v1,
                    ];
                    let center = p0 + ax * ((u0 + u1) * 0.5) + ay * ((v0 + v1) * 0.5);
                    // Camera at origin → view ray to the cell is just its direction.
                    let facing = center.normalize_or_zero().dot(normal).abs();
                    let b = 0.18 + 0.82 * facing;
                    let cam: Vec<Vec3> = corners.iter().map(|c| inv * *c).collect();
                    let poly = clip_poly_near(&cam, near);
                    if poly.len() < 3 {
                        continue;
                    }
                    let pts: Vec<egui::Pos2> = poly.iter().map(|p| project(*p)).collect();
                    let col = egui::Color32::from_rgba_unmultiplied(
                        (base.r() as f32 * b) as u8,
                        (base.g() as f32 * b) as u8,
                        (base.b() as f32 * b) as u8,
                        a8,
                    );
                    painter.add(egui::Shape::convex_polygon(pts, col, egui::Stroke::NONE));
                }
            }
        }
    }

    // Wireframe grid over every face.
    let n = 4; // grid divisions per face
    let step = 2.0 * r / n as f32;
    let s = |c: egui::Color32| egui::Stroke::new(1.0, c);
    for i in 0..=n {
        let t = -r + i as f32 * step;
        // Floor (y = −r) and ceiling (y = +r): grid over x,z.
        for (yf, c) in [(-r, floor), (r, ceil)] {
            line(Vec3::new(t, yf, -r), Vec3::new(t, yf, r), s(c));
            line(Vec3::new(-r, yf, t), Vec3::new(r, yf, t), s(c));
        }
        // Front/back walls (z = ∓r): grid over x,y.
        for (zf, c) in [(-r, front), (r, back)] {
            line(Vec3::new(t, -r, zf), Vec3::new(t, r, zf), s(c));
            line(Vec3::new(-r, t, zf), Vec3::new(r, t, zf), s(c));
        }
        // Left/right walls (x = ∓r): grid over y,z.
        for (xf, c) in [(-r, left), (r, right)] {
            line(Vec3::new(xf, -r, t), Vec3::new(xf, r, t), s(c));
            line(Vec3::new(xf, t, -r), Vec3::new(xf, t, r), s(c));
        }
    }
}
