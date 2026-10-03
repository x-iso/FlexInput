//! Gyro to Stick Rotation body: which stick and which way "turning" is measured,
//! the sensitivity, the inner deadzone and the stabiliser, and a circle showing
//! the stick where the thumb has it and where the gyro has turned it to.
//!
//! Every row and the circle are pinnable on their own. The evaluation is
//! `eval_stick_rotation_node` in the engine; the circle reads what it publishes
//! in `last_out` (`STICK_ROT_OUT_*`).

use super::*;
use flexinput_engine::eval::{
    STICK_ROT_DEADZONE_DEFAULT, STICK_ROT_DEADZONE_PARAM, STICK_ROT_INVERT_PARAM, STICK_ROT_MODE_PARAM,
    STICK_ROT_OUT_ENGAGED, STICK_ROT_OUT_OFFSET, STICK_ROT_OUT_RAW, STICK_ROT_OUT_ROTATED,
    STICK_ROT_RELATIVE_PARAM, STICK_ROT_RETURN_DEFAULT_MS, STICK_ROT_RETURN_PARAM,
    STICK_ROT_SENS_DEFAULT, STICK_ROT_SENS_PARAM, STICK_ROT_SMOOTH_PARAM, STICK_ROT_STICK_PARAM,
};

/// The stick a node turns, as its bus pin.
pub(crate) fn stick_rotation_stick(node: &NodeData) -> &'static str {
    match node.params.get(STICK_ROT_STICK_PARAM).and_then(|v| v.as_str()) {
        Some("left_stick") => "left_stick",
        _ => "right_stick",
    }
}

fn num(node: &NodeData, key: &str, default: f32) -> f32 {
    node.params.get(key).and_then(|v| v.as_f64()).map(|v| v as f32).unwrap_or(default)
}

const STICK_TIP: &str = "The stick to turn. The pad's turn rotates it once it is pushed past the deadzone.";
const MODE_TIP: &str = "Yaw — turning about the pad's own vertical, however it is held.\n\
    World — turning about gravity: held flat it is the yaw, pitched up towards\n\
    you it becomes the roll, and anything in between mixes the two.";
const SENS_TIP: &str = "Degrees the stick turns per degree the pad turns.\n1.0 = the aim turns exactly as far as your hands.";
const INVERT_TIP: &str = "Turn the stick the other way.";
const DEADZONE_TIP: &str = "Inner deadzone, as a fraction of stick travel. While the stick is inside it the\n\
    gyro does nothing and the stick passes through untouched, and the turn the\n\
    gyro added is dropped — the next push starts from your thumb's direction.\n\
    0 = no deadzone: the turn is never dropped, and keeps building and\n\
    applying even while the stick is centred.";
const HOLD_TIP: &str = "How the turn behaves once the pad stops turning.\n\
    Absolute — it holds: the stick stays turned by however far the pad has\n\
    turned since the stick was pushed out.\n\
    Relative — it folds back to your thumb's direction over the Return time.\n\
    A quick flick of the wrist registers nearly in full; a steady turn holds\n\
    the stick turned by its speed times the return time.";
const RETURN_TIP: &str = "Relative: how quickly the stick folds back to your thumb once the pad\n\
    stops turning. After this long about a third of the turn is left, after\n\
    three times as long almost none. Longer feels more like Absolute.";
const SMOOTH_TIP: &str = "Stabilise the stick's direction against a shaky thumb (0 = off).\n\
    Small back-and-forth wobble is smoothed over about this long; a deliberate\n\
    sweep is let through with little delay. Only the thumb's direction is\n\
    smoothed — never the stick's length, and never the gyro's turn.";

pub(crate) fn show_stick_rotation_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) -> bool {
    let mut changed = false;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
        let r = ui.horizontal(|ui| changed |= options_row(node_id, ui, snarl));
        register_exposable_element(ui, node_id, "options", r.response.rect);
        ui.horizontal(|ui| {
            let r = ui.horizontal(|ui| changed |= sens_row(node_id, ui, snarl));
            register_exposable_element(ui, node_id, "sens", r.response.rect);
        });
        let r = ui.horizontal(|ui| changed |= hold_row(node_id, ui, snarl));
        register_exposable_element(ui, node_id, "hold", r.response.rect);
        ui.horizontal(|ui| {
            let r = ui.horizontal(|ui| changed |= number_row(node_id, ui, snarl, NumRow::Deadzone));
            register_exposable_element(ui, node_id, "deadzone", r.response.rect);
            ui.separator();
            let r = ui.horizontal(|ui| changed |= number_row(node_id, ui, snarl, NumRow::Smooth));
            register_exposable_element(ui, node_id, "stabilise", r.response.rect);
        });
        let w = ui.available_width().clamp(120.0, 200.0);
        let rect = render_stick_rotation_field(node_id, ui, snarl, egui::vec2(w, w));
        register_exposable_element(ui, node_id, "field", rect);
    });
    changed
}

/// Stick (L / R) and mode (Yaw / World).
fn options_row(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) -> bool {
    let Some(node) = snarl.get_node(node_id) else { return false };
    let stick = stick_rotation_stick(node);
    let world = node.params.get(STICK_ROT_MODE_PARAM).and_then(|v| v.as_str()) == Some("world");
    let mut set: Option<(&str, &str)> = None;
    ui.label(egui::RichText::new("Stick").small()).on_hover_text(STICK_TIP);
    for (pin, lbl) in [("left_stick", "L"), ("right_stick", "R")] {
        if ui.selectable_label(stick == pin, egui::RichText::new(lbl).small()).on_hover_text(STICK_TIP).clicked() {
            set = Some((STICK_ROT_STICK_PARAM, pin));
        }
    }
    ui.separator();
    for (mode, lbl, on) in [("yaw", "Yaw", !world), ("world", "World", world)] {
        if ui.selectable_label(on, egui::RichText::new(lbl).small()).on_hover_text(MODE_TIP).clicked() {
            set = Some((STICK_ROT_MODE_PARAM, mode));
        }
    }
    let Some((k, v)) = set else { return false };
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.params.insert(k.to_string(), Value::String(v.to_string()));
    }
    true
}

/// Sensitivity and invert.
fn sens_row(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) -> bool {
    let Some(node) = snarl.get_node(node_id) else { return false };
    let mut sens = num(node, STICK_ROT_SENS_PARAM, STICK_ROT_SENS_DEFAULT);
    let mut invert = node.params.get(STICK_ROT_INVERT_PARAM).and_then(|v| v.as_bool()).unwrap_or(false);
    let mut set: Option<(&str, Value)> = None;
    ui.label(egui::RichText::new("Sens").small()).on_hover_text(SENS_TIP);
    if ui.add(egui::DragValue::new(&mut sens).speed(0.01).range(0.0..=10.0).max_decimals(2).suffix("×"))
        .on_hover_text(SENS_TIP).changed()
    {
        set = serde_json::Number::from_f64(sens as f64).map(|n| (STICK_ROT_SENS_PARAM, Value::Number(n)));
    }
    if ui.checkbox(&mut invert, egui::RichText::new("Invert").small()).on_hover_text(INVERT_TIP).changed() {
        set = Some((STICK_ROT_INVERT_PARAM, Value::Bool(invert)));
    }
    let Some((k, v)) = set else { return false };
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.params.insert(k.to_string(), v);
    }
    true
}

/// Absolute / Relative, and Relative's return time.
fn hold_row(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) -> bool {
    let Some(node) = snarl.get_node(node_id) else { return false };
    let relative = node.params.get(STICK_ROT_RELATIVE_PARAM).and_then(|v| v.as_bool()).unwrap_or(false);
    let mut ret = num(node, STICK_ROT_RETURN_PARAM, STICK_ROT_RETURN_DEFAULT_MS);
    let mut set: Option<(&str, Value)> = None;
    for (rel, lbl) in [(false, "Absolute"), (true, "Relative")] {
        if ui.selectable_label(relative == rel, egui::RichText::new(lbl).small()).on_hover_text(HOLD_TIP).clicked() {
            set = Some((STICK_ROT_RELATIVE_PARAM, Value::Bool(rel)));
        }
    }
    if relative {
        ui.label(egui::RichText::new("Return").small()).on_hover_text(RETURN_TIP);
        if ui.add(egui::DragValue::new(&mut ret).speed(2.0).range(0.0..=2000.0).max_decimals(0).suffix(" ms"))
            .on_hover_text(RETURN_TIP).changed()
        {
            set = serde_json::Number::from_f64(ret as f64).map(|n| (STICK_ROT_RETURN_PARAM, Value::Number(n)));
        }
    }
    let Some((k, v)) = set else { return false };
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.params.insert(k.to_string(), v);
    }
    true
}

#[derive(Clone, Copy)]
enum NumRow {
    Deadzone,
    Smooth,
}

/// The deadzone or the stabiliser.
fn number_row(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, which: NumRow) -> bool {
    let Some(node) = snarl.get_node(node_id) else { return false };
    let (label, key, tip) = match which {
        NumRow::Deadzone => ("Deadzone", STICK_ROT_DEADZONE_PARAM, DEADZONE_TIP),
        NumRow::Smooth => ("Stabilise", STICK_ROT_SMOOTH_PARAM, SMOOTH_TIP),
    };
    ui.label(egui::RichText::new(label).small()).on_hover_text(tip);
    let r = match which {
        NumRow::Deadzone => {
            let mut v = num(node, key, STICK_ROT_DEADZONE_DEFAULT);
            let r = ui.add(egui::DragValue::new(&mut v).speed(0.005).range(0.0..=0.95).max_decimals(2));
            (r, v)
        }
        NumRow::Smooth => {
            let mut v = num(node, key, 0.0);
            let r = ui.add(egui::DragValue::new(&mut v).speed(1.0).range(0.0..=500.0).max_decimals(0).suffix(" ms"));
            (r, v)
        }
    };
    let (resp, v) = r;
    if !resp.on_hover_text(tip).changed() {
        return false;
    }
    if let (Some(node), Some(n)) = (snarl.get_node_mut(node_id), serde_json::Number::from_f64(v as f64)) {
        node.params.insert(key.to_string(), Value::Number(n));
    }
    true
}

/// The live circle of a Gyro to Stick Rotation node, sized to `container`,
/// square. See [`paint_stick_rotation`].
pub(crate) fn render_stick_rotation_field(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &Snarl<NodeData>,
    container: egui::Vec2,
) -> egui::Rect {
    let (out, deadzone) = snarl
        .get_node(node_id)
        .map(|n| (n.extra.last_out.clone(), num(n, STICK_ROT_DEADZONE_PARAM, STICK_ROT_DEADZONE_DEFAULT)))
        .unwrap_or_default();
    paint_stick_rotation(ui, container, &out, 0, deadzone)
}

/// The live circle: the stick's range, the deadzone, the stick where the thumb
/// has it (hollow), where the gyro turned it to (filled, with the aim line), and
/// the arc between the two, with the offset written under it. Reads the four
/// live values (`STICK_ROT_OUT_*`) from `out` starting at `base` — 0 for the
/// module, the trailing outputs for a JSM node (`jsm_rotation_out`).
pub(crate) fn paint_stick_rotation(
    ui: &mut egui::Ui,
    container: egui::Vec2,
    out: &[Option<Signal>],
    base: usize,
    deadzone: f32,
) -> egui::Rect {
    let side = container.x.min(container.y).max(48.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    let accent = visuals.selection.bg_fill;
    let weak = visuals.weak_text_color();
    let line = visuals.widgets.noninteractive.bg_stroke.color;

    let label_h = 14.0;
    let c = rect.center() - egui::vec2(0.0, label_h * 0.5);
    let radius = (side - label_h) * 0.5 - 6.0;
    let deadzone = deadzone.clamp(0.0, 1.0);

    painter.circle_stroke(c, radius, egui::Stroke::new(1.0, line));
    painter.circle_filled(c, radius * deadzone, weak.gamma_multiply(0.15));
    painter.circle_stroke(c, radius * deadzone, egui::Stroke::new(1.0, weak.gamma_multiply(0.5)));
    // Up, as the compass the offset is measured from.
    painter.line_segment([c - egui::vec2(0.0, radius), c - egui::vec2(0.0, radius - 5.0)],
        egui::Stroke::new(1.0, line));

    let get = |i: usize| out.get(base + i).copied().flatten();
    let vec = |i: usize| match get(i) {
        Some(Signal::Vec2(v)) => Some(v),
        _ => None,
    };
    let raw = vec(STICK_ROT_OUT_RAW).unwrap_or_default();
    let rotated = vec(STICK_ROT_OUT_ROTATED).unwrap_or(raw);
    let offset = match get(STICK_ROT_OUT_OFFSET) {
        Some(Signal::Float(f)) => f,
        _ => 0.0,
    };
    let engaged = matches!(get(STICK_ROT_OUT_ENGAGED), Some(Signal::Bool(true)));
    // The bus counts y up; the screen counts it down.
    let at = |v: glam::Vec2| c + egui::vec2(v.x, -v.y) * radius;

    if engaged {
        // The arc the gyro turned the stick through, drawn back from where it
        // points now so it ends on the dot — with stabilising on, the thumb's
        // smoothed direction is where it starts, not quite the raw one.
        let len = rotated.length();
        let to = rotated.x.atan2(rotated.y);
        let steps = ((offset.abs() / 4.0).ceil() as usize).clamp(1, 90);
        let arc: Vec<egui::Pos2> = (0..=steps)
            .map(|i| {
                let a = to - offset.to_radians() * (1.0 - i as f32 / steps as f32);
                at(glam::Vec2::new(a.sin(), a.cos()) * len)
            })
            .collect();
        painter.add(egui::Shape::line(arc, egui::Stroke::new(1.5, accent.gamma_multiply(0.6))));
        painter.line_segment([c, at(rotated)], egui::Stroke::new(1.5, accent));
        painter.line_segment([c, at(raw)], egui::Stroke::new(1.0, weak.gamma_multiply(0.6)));
    }
    painter.circle_stroke(at(raw), 4.0, egui::Stroke::new(1.5, weak));
    if engaged {
        painter.circle_filled(at(rotated), 4.5, accent);
    }

    let text = if engaged { format!("{offset:+.1}°") } else { "—".to_string() };
    painter.text(
        egui::pos2(rect.center().x, rect.bottom() - 2.0),
        egui::Align2::CENTER_BOTTOM,
        text,
        egui::FontId::proportional(11.0),
        if engaged { visuals.text_color() } else { weak },
    );
    rect
}

/// A JSM Config node's circle, when its config turns a stick with the gyro
/// (`GYRO_OUTPUT = *_ROTATION`); `None` — and nothing drawn — when it doesn't.
pub(crate) fn render_jsm_rotation(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &Snarl<NodeData>,
    cfg: &flexinput_engine::eval::JsmConfig,
    container: egui::Vec2,
) -> Option<egui::Rect> {
    let node = snarl.get_node(node_id)?;
    let (_, deadzone) = flexinput_engine::eval::jsm_rotation_of(cfg)?;
    let base = flexinput_engine::eval::jsm_rotation_out(node.outputs.len());
    Some(paint_stick_rotation(ui, container, &node.extra.last_out, base, deadzone))
}

// ── pinned copies ───────────────────────────────────────────────────────────

pub(crate) fn render_stick_rotation_options_pinned(
    node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2,
) {
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(170.0, 22.0));
    if ui.horizontal(|ui| options_row(node_id, ui, snarl)).inner {
        mark_overlay_param_write(ui.ctx());
    }
}

pub(crate) fn render_stick_rotation_hold_pinned(
    node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2,
) {
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(200.0, 22.0));
    if ui.horizontal(|ui| hold_row(node_id, ui, snarl)).inner {
        mark_overlay_param_write(ui.ctx());
    }
}

pub(crate) fn render_stick_rotation_sens_pinned(
    node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2,
) {
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(150.0, 22.0));
    if ui.horizontal(|ui| sens_row(node_id, ui, snarl)).inner {
        mark_overlay_param_write(ui.ctx());
    }
}
