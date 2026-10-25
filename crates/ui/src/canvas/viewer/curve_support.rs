//! Shared response-curve machinery: the core graph painter, pinned row
//! renderers, .fxc save/load, curve clipboard + right-click menu, scale
//! helpers.

use super::*;

/// Renders the response curve graph as a bare widget filling `container`.
/// No surrounding sliders, buttons, channel +/-, etc. — just the graph,
/// fully interactive (drag points, alt-bias, dbl-click add, right-click remove).
/// Sized exactly to the user-allocated rect from the sub-patch layout.
pub(crate) fn render_response_curve_only(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    is_vec: bool,
    graph_ov: Option<&crate::canvas::node::PinGraphOverride>,
) {
    // No vsync bypass — same rationale as show_response_curve_body.
    let avail = egui::vec2(container.x.max(20.0), container.y.max(20.0));
    let (rect, bg_resp) = ui.allocate_exact_size(avail, egui::Sense::click());
    let bg_for_menu = bg_resp.clone();
    paint_response_curve_graph(inner_id, ui, inner_snarl, rect, bg_resp, is_vec, graph_ov);
    // Right-click context menu — same actions as the canvas-editor body so the
    // layout-pinned widget is fully usable on its own.
    let _ = is_vec; // graph-only menu doesn't distinguish; kept for signature compat.
    bg_for_menu.context_menu(|ui| {
        curve_context_menu(ui, inner_id, inner_snarl, None);
    });
}

/// Bare Log-Exp slider + Abs (only for non-vec) + Snap.
pub(crate) fn render_response_curve_scale_row(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    is_vec: bool,
) {
    let (mut sc_t, mut absolute, mut snap_on) = snarl.get_node(inner_id).map(|n| {
        let s = n.params.get("scale_t").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let a = if is_vec { true } else { n.params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true) };
        let sn = n.params.get("snap").and_then(|v| v.as_bool()).unwrap_or(false);
        (s, a, sn)
    }).unwrap_or((0.0, true, false));

    ui.set_max_width(container.x);
    let s = apply_widget_scale(ui, container, egui::vec2(220.0, 22.0));
    let mut changed = false;
    let mut fr: Vec<egui::Rect> = Vec::with_capacity(3);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Log").weak());
        // ASTH row model: the slider is the row's flexible element — it takes
        // its minimum scaled width plus ALL surplus container width, so the
        // labels/checkboxes scale with the frame height while widening the
        // frame lengthens the slider.
        let slider_w = pin_flex_width(ui, container, 60.0);
        let slider_h = (16.0 * s).max(10.0);
        let slider_id = ui.make_persistent_id(("log_exp_pin", inner_id));
        let (slider_rect, c) = log_exp_slider(ui, slider_id, egui::vec2(slider_w, slider_h), &mut sc_t);
        fr.push(slider_rect);
        changed |= c;
        ui.label(egui::RichText::new("Exp").weak());
        ui.separator();
        if !is_vec {
            let was = absolute;
            let r = ui.checkbox(&mut absolute, egui::RichText::new("Abs"));
            fr.push(r.rect);
            changed |= absolute != was;
        }
        let was = snap_on;
        let r = ui.checkbox(&mut snap_on, egui::RichText::new("Snap"));
        fr.push(r.rect);
        changed |= snap_on != was;
    });
    publish_nav_field_rects(ui, inner_id, &fr);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            if let Some(n) = Number::from_f64(sc_t as f64) { node.params.insert("scale_t".into(), Value::Number(n)); }
            if !is_vec { node.params.insert("absolute".into(), Value::Bool(absolute)); }
            node.params.insert("snap".into(), Value::Bool(snap_on));
        }
    }
}

/// Bare In/Out range row. For non-vec curves: in_min, in_max, out_min, out_max.
/// For vec curves: in_max, out_max (vec curves are always [0,1]).
pub(crate) fn render_response_curve_range_row(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    is_vec: bool,
) {
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(220.0, 22.0));
    let absolute = is_vec || snarl.get_node(inner_id).is_some_and(|n| {
        twoway_vec_mode(n) || n.params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true)
    });
    if let Some(unit) = snarl.get_node(inner_id).and_then(MotionUnit::of) {
        let (fr, _) = motion_view_range_fields(ui, inner_id, snarl, unit, absolute);
        publish_nav_field_rects(ui, inner_id, &fr);
        return;
    }
    let mut changed = false;
    if is_vec {
        let (mut i_max, mut o_max) = snarl.get_node(inner_id).map(|n| {
            let i = n.params.get("in_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let o = n.params.get("out_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            (i, o)
        }).unwrap_or((1.0, 1.0));
        let mut fr = [egui::Rect::NOTHING; 2];
        let linked = snarl.get_node(inner_id).map_or(true, range_linked);
        let before = [0.0, i_max, 0.0, o_max];
        let mut link_clicked = false;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("In max").weak());
            let r = ui.add(egui::DragValue::new(&mut i_max).speed(0.01).max_decimals(2));
            fr[0] = r.rect; changed |= r.changed();
            link_clicked = range_link_button(ui, linked);
            ui.label(egui::RichText::new("Out max").weak());
            let r = ui.add(egui::DragValue::new(&mut o_max).speed(0.01).max_decimals(2));
            fr[1] = r.rect; changed |= r.changed();
        });
        publish_nav_field_rects(ui, inner_id, &fr);
        if link_clicked { toggle_range_link(snarl, inner_id); }
        if changed && linked {
            let mut v = [0.0, i_max, 0.0, o_max];
            follow_range_link(before, &mut v);
            (i_max, o_max) = (v[1], v[3]);
        }
        if changed {
            if let Some(node) = snarl.get_node_mut(inner_id) {
                if let Some(n) = Number::from_f64(i_max as f64) { node.params.insert("in_max".into(),  Value::Number(n)); }
                if let Some(n) = Number::from_f64(o_max as f64) { node.params.insert("out_max".into(), Value::Number(n)); }
            }
        }
    } else {
        let (mut i0, mut i1, mut o0, mut o1) = snarl.get_node(inner_id).map(|n| {
            let i0 = n.params.get("in_min").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
            let i1 = n.params.get("in_max").and_then(|v| v.as_f64()).unwrap_or( 1.0) as f32;
            let o0 = n.params.get("out_min").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
            let o1 = n.params.get("out_max").and_then(|v| v.as_f64()).unwrap_or( 1.0) as f32;
            (i0, i1, o0, o1)
        }).unwrap_or((-1.0, 1.0, -1.0, 1.0));
        let mut fr = [egui::Rect::NOTHING; 4];
        let linked = snarl.get_node(inner_id).map_or(true, range_linked);
        let before = [i0, i1, o0, o1];
        let mut link_clicked = false;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("In").weak());
            let r = ui.add_enabled(!absolute, egui::DragValue::new(&mut i0).speed(0.01).prefix("↓").max_decimals(2))
                .on_disabled_hover_text(ABS_MIN_HOVER);
            fr[0] = r.rect; changed |= r.changed();
            let r = ui.add(egui::DragValue::new(&mut i1).speed(0.01).prefix("↑").max_decimals(2));
            fr[1] = r.rect; changed |= r.changed();
            link_clicked = range_link_button(ui, linked);
            ui.label(egui::RichText::new("Out").weak());
            let r = ui.add_enabled(!absolute, egui::DragValue::new(&mut o0).speed(0.01).prefix("↓").max_decimals(2))
                .on_disabled_hover_text(ABS_MIN_HOVER);
            fr[2] = r.rect; changed |= r.changed();
            let r = ui.add(egui::DragValue::new(&mut o1).speed(0.01).prefix("↑").max_decimals(2));
            fr[3] = r.rect; changed |= r.changed();
        });
        publish_nav_field_rects(ui, inner_id, &fr);
        if link_clicked { toggle_range_link(snarl, inner_id); }
        if changed && linked {
            let mut v = [i0, i1, o0, o1];
            follow_range_link(before, &mut v);
            [i0, i1, o0, o1] = v;
        }
        if changed {
            if let Some(node) = snarl.get_node_mut(inner_id) {
                for (k, v) in [
                    ("in_min", i0 as f64), ("in_max", i1 as f64),
                    ("out_min", o0 as f64), ("out_max", o1 as f64),
                ] {
                    if let Some(n) = Number::from_f64(v) { node.params.insert(k.into(), Value::Number(n)); }
                }
            }
        }
    }
}

/// The Log/Exp setting as text: which way, how far, and the power it puts on
/// the input.
pub(crate) fn log_exp_text(t: f32) -> String {
    if t.abs() < 0.005 {
        return "Linear".to_string();
    }
    let side = if t < 0.0 { "Log" } else { "Exp" };
    format!("{side} {:.2}  ·  x^{:.2}", t.abs(), 2f32.powf(t * 3.0))
}

/// The Log ⟷ Exp scale slider: drag to set, double-click for linear, Ctrl+click
/// to type a value (-1..1). Shows the value while it is changed and on hover.
/// Returns the slider's rect and whether the value changed.
pub(crate) fn log_exp_slider(ui: &mut egui::Ui, id: egui::Id, size: egui::Vec2, sc_t: &mut f32) -> (egui::Rect, bool) {
    // Typing: 1 = just asked for (focus the field), 2 = typing.
    let typing_id = id.with("typing");
    let typing = ui.data(|d| d.get_temp::<u8>(typing_id)).unwrap_or(0);
    if typing > 0 {
        let r = ui.add_sized(size, egui::DragValue::new(sc_t).range(-1.0..=1.0).speed(0.01).max_decimals(3));
        if typing == 1 {
            r.request_focus();
            ui.data_mut(|d| d.insert_temp(typing_id, 2u8));
        } else if !r.has_focus() {
            ui.data_mut(|d| d.insert_temp(typing_id, 0u8));
        }
        return (r.rect, r.changed());
    }
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let mut changed = false;
    if resp.clicked() && ui.input(|i| i.modifiers.command) {
        ui.data_mut(|d| d.insert_temp(typing_id, 1u8));
    } else if resp.double_clicked() {
        *sc_t = 0.0;
        changed = true;
    } else if resp.dragged() {
        *sc_t = (*sc_t + resp.drag_delta().x / rect.width() * 2.0).clamp(-1.0, 1.0);
        changed = true;
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, Color32::from_gray(35));
    let cx = rect.center().x;
    painter.line_segment(
        [egui::pos2(cx, rect.top() + 2.0), egui::pos2(cx, rect.bottom() - 2.0)],
        egui::Stroke::new(1.0, Color32::from_gray(70)),
    );
    let knob_x = rect.left() + (*sc_t + 1.0) * 0.5 * rect.width();
    painter.circle_filled(
        egui::pos2(knob_x, rect.center().y),
        (rect.height() * 0.35).max(3.0),
        if resp.hovered() || resp.dragged() { Color32::WHITE } else { Color32::from_gray(190) },
    );
    if resp.dragged() {
        resp.show_tooltip_text(log_exp_text(*sc_t));
    } else {
        resp.on_hover_text(format!(
            "{}\nDrag: Log ⟷ Exp. Double-click: linear. Ctrl+click: type a value.",
            log_exp_text(*sc_t),
        ));
    }
    (rect, changed)
}

/// A chain-link toggle, painted rather than taken from a font (the emoji
/// font's 🔗 renders as something else): two interlocked links while on,
/// pulled apart while off. Returns the response; the caller acts on clicks.
pub(crate) fn link_toggle(ui: &mut egui::Ui, linked: bool, hover: &str) -> egui::Response {
    let h = ui.text_style_height(&egui::TextStyle::Small).max(10.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(h * 1.3, h), egui::Sense::click());
    let painter = ui.painter_at(rect.expand(1.0));
    let vis = ui.style().interact_selectable(&resp, linked);
    if linked || resp.hovered() {
        painter.rect_filled(rect.expand(1.0), 3.0, vis.bg_fill);
    }
    let col = if linked { vis.fg_stroke.color } else { ui.visuals().weak_text_color() };
    let stroke = egui::Stroke::new(1.3, col);
    let (w, lh) = (rect.width() * 0.5, rect.height() * 0.5);
    let gap = if linked { -rect.width() * 0.14 } else { 0.0 };
    let c = rect.center();
    for side in [-1.0f32, 1.0] {
        let center = egui::pos2(c.x + side * (w + gap) * 0.5, c.y);
        let link = egui::Rect::from_center_size(center, egui::vec2(w, lh));
        painter.rect_stroke(link, lh * 0.5, stroke, egui::StrokeKind::Middle);
    }
    resp.on_hover_text(hover)
}

/// Where a curve's dots may snap, published by its graph each frame for the
/// gamepad driver: the grid lines in graph coordinates, and whether the 1:1
/// line counts too. Absent while Snap is off.
#[derive(Clone, Default)]
pub(crate) struct NavCurveSnap {
    pub xs: Vec<f32>,
    pub ys: Vec<f32>,
    pub line: bool,
}

pub(crate) fn nav_curve_snap_id(node_id: usize) -> egui::Id {
    egui::Id::new(("gp_nav_curve_snap", node_id))
}

/// Publish (or, with Snap off, withdraw) a curve graph's snap targets. The grid
/// positions are fractions across the window `win` = (x_lo, x_range, y_lo, y_range).
pub(crate) fn publish_nav_curve_snap(
    ctx: &egui::Context, node_id: NodeId, on: bool,
    nodes_x: &[f32], nodes_y: &[f32], win: (f32, f32, f32, f32), line: bool,
) {
    let id = nav_curve_snap_id(node_id.0);
    if !on {
        ctx.data_mut(|d| d.remove_temp::<NavCurveSnap>(id));
        return;
    }
    let snap = NavCurveSnap {
        xs: nodes_x.iter().map(|u| win.0 + u * win.1).collect(),
        ys: nodes_y.iter().map(|v| win.2 + v * win.3).collect(),
        line,
    };
    ctx.data_mut(|d| d.insert_temp(id, snap));
}

impl NavCurveSnap {
    /// The next snap target from `cur` in direction `dir` (±1) along one axis:
    /// a grid line, or the 1:1 line's crossing (at `other`, the dot's other
    /// coordinate). `None` when nothing lies that way.
    pub(crate) fn next(&self, along_x: bool, cur: f32, other: f32, dir: f32) -> Option<f32> {
        let lines = if along_x { &self.xs } else { &self.ys };
        lines.iter().copied()
            .chain(self.line.then_some(other))
            .filter(|&c| (c - cur) * dir > 1e-4)
            .min_by(|a, b| (a - cur).abs().total_cmp(&(b - cur).abs()))
    }
}

/// Param that unlinks a curve's In and Out ranges. Stored inverted, like
/// [`GRID_UNLINKED`], so curves come up linked.
pub(crate) const RANGE_UNLINKED: &str = "range_unlinked";

/// Whether a curve's In and Out ranges scale together.
pub(crate) fn range_linked(node: &NodeData) -> bool {
    !node.params.get(RANGE_UNLINKED).and_then(|v| v.as_bool()).unwrap_or(false)
}

/// The 🔗 between a range row's In and Out fields. Returns whether it was
/// clicked; [`toggle_range_link`] then flips the stored link.
pub(crate) fn range_link_button(ui: &mut egui::Ui, linked: bool) -> bool {
    link_toggle(ui, linked, if linked {
        "Linked: scaling In scales Out by as much, and the other way round. Click to set them apart."
    } else {
        "Link In and Out: scale them together"
    })
    .clicked()
}

pub(crate) fn toggle_range_link(snarl: &mut Snarl<NodeData>, node_id: NodeId) {
    if let Some(n) = snarl.get_node_mut(node_id) {
        let linked = range_linked(n);
        n.params.insert(RANGE_UNLINKED.into(), Value::Bool(linked));
    }
}

/// Linked ranges `[in ↓, in ↑, out ↓, out ↑]`: an end that changed since
/// `before` scales its counterpart (In ↓ with Out ↓, In ↑ with Out ↑) by the
/// same ratio — or, from zero, moves it by as much.
pub(crate) fn follow_range_link(before: [f32; 4], now: &mut [f32; 4]) {
    let scaled = |old: f32, new: f32, other: f32| {
        if old.abs() > 1e-6 { other * new / old } else { other + (new - old) }
    };
    for (a, b) in [(0, 2), (1, 3)] {
        if now[a] != before[a] {
            now[b] = scaled(before[a], now[a], before[b]);
        } else if now[b] != before[b] {
            now[a] = scaled(before[b], now[b], before[a]);
        }
    }
}

/// The 1:1 line — output equal to input — within a graph window, as the
/// graph-coordinate span it covers on both axes. On a plain curve it is the
/// corner diagonal; on a motion view showing In and Out over different spans
/// it runs at an angle, which is what a straight curve then looks like.
pub(crate) fn identity_span(x_lo: f32, x_hi: f32, y_lo: f32, y_hi: f32) -> Option<(f32, f32)> {
    let (a, b) = (x_lo.max(y_lo), x_hi.min(y_hi));
    (b > a).then_some((a, b))
}

/// Where a dragged point snaps: the nearest grid intersection (`grid`), or —
/// with `line` — a point where a grid line crosses the 1:1 line, whichever is
/// closer on screen. `nodes_x` / `nodes_y` are the grid positions across the
/// window as 0..1 fractions; `win` is (x_lo, x_range, y_lo, y_range).
pub(crate) fn snap_point(
    x: f32, y: f32,
    grid: (f32, f32),
    nodes_x: &[f32], nodes_y: &[f32],
    win: (f32, f32, f32, f32),
    line: bool,
) -> (f32, f32) {
    if !line {
        return grid;
    }
    let (x_lo, xr, y_lo, yr) = win;
    let dist = |p: (f32, f32)| ((p.0 - x) / xr).powi(2) + ((p.1 - y) / yr).powi(2);
    let within = |v: f32, lo: f32, r: f32| v >= lo - 1e-6 && v <= lo + r + 1e-6;
    let crossings = nodes_x.iter().map(|u| x_lo + u * xr).filter(|&g| within(g, y_lo, yr))
        .chain(nodes_y.iter().map(|v| y_lo + v * yr).filter(|&g| within(g, x_lo, xr)))
        .map(|g| (g, g));
    std::iter::once(grid).chain(crossings)
        .min_by(|a, b| dist(*a).total_cmp(&dist(*b)))
        .unwrap_or(grid)
}

/// A curve dot's output value, next to it: up and to the right, or to the left
/// where that would run off the graph, on a dark backing.
pub(crate) fn paint_point_value(painter: &egui::Painter, rect: egui::Rect, at: egui::Pos2, text: String) {
    let col = Color32::from_rgba_unmultiplied(225, 225, 225, 230);
    let g = painter.layout_no_wrap(text, egui::FontId::proportional(9.0), col);
    let size = g.size() + egui::vec2(4.0, 2.0);
    let right = at.x + 7.0 + size.x <= rect.right();
    let min = egui::pos2(if right { at.x + 7.0 } else { at.x - 7.0 - size.x }, (at.y - 7.0 - size.y).max(rect.top()));
    let bg = egui::Rect::from_min_size(min, size);
    painter.rect_filled(bg, 2.0, Color32::from_rgba_unmultiplied(0, 0, 0, 170));
    painter.galley(bg.min + egui::vec2(2.0, 1.0), g, col);
}

/// ctx-data key for where the gamepad's virtual cursor is this frame, as
/// (viewport it is drawn in, screen position). Absent while it is hidden.
pub(crate) const NAV_CURSOR_KEY: &str = "fxi_gp_nav_cursor_pos";

/// Where a pointer reads a graph at `rect`: the mouse hovering its background
/// (`hover`, from the graph's response), else the gamepad's cursor over it.
pub(crate) fn graph_reading_pos(ui: &egui::Ui, rect: egui::Rect, hover: Option<egui::Pos2>) -> Option<egui::Pos2> {
    if hover.is_some() {
        return hover;
    }
    let (vp, pos): (egui::ViewportId, egui::Pos2) = ui.ctx().data(|d| d.get_temp(egui::Id::new(NAV_CURSOR_KEY)))?;
    if vp != ui.ctx().viewport_id() {
        return None;
    }
    // The cursor is in screen space; the graph may sit on a scaled layer.
    let to_global = ui.ctx().layer_transform_to_global(ui.layer_id()).unwrap_or(egui::emath::TSTransform::IDENTITY);
    let local = to_global.inverse() * pos;
    rect.contains(local).then_some(local)
}

/// A live reading where the cursor is over a curve graph: a faint line at its
/// input, a ring where that meets the curve, and `text(graph x, graph y)` —
/// the input and the output it gives — in a box beside the cursor. `win` is
/// (x_lo, x_hi, y_lo, y_hi); `sample` reads the curve at a graph X.
pub(crate) fn paint_cursor_reading(
    painter: &egui::Painter,
    rect: egui::Rect,
    at: egui::Pos2,
    win: (f32, f32, f32, f32),
    sample: impl Fn(f32) -> f32,
    text: impl Fn(f32, f32) -> String,
) {
    let (x_lo, x_hi, y_lo, y_hi) = win;
    let gx = x_lo + (at.x - rect.left()) / rect.width() * (x_hi - x_lo);
    let gy = sample(gx);
    let on_curve = egui::pos2(at.x, rect.bottom() - (gy.clamp(y_lo, y_hi) - y_lo) / (y_hi - y_lo) * rect.height());
    let faint = Color32::from_rgba_unmultiplied(220, 220, 220, 60);
    painter.line_segment([egui::pos2(at.x, rect.top()), egui::pos2(at.x, rect.bottom())], egui::Stroke::new(1.0, faint));
    painter.circle_stroke(on_curve, 4.0, egui::Stroke::new(1.5, Color32::from_rgba_unmultiplied(255, 255, 255, 200)));

    let col = Color32::from_rgba_unmultiplied(235, 235, 235, 240);
    let g = painter.layout_no_wrap(text(gx, gy), egui::FontId::proportional(10.0), col);
    let size = g.size() + egui::vec2(6.0, 3.0);
    let x = if at.x + 12.0 + size.x <= rect.right() { at.x + 12.0 } else { at.x - 12.0 - size.x };
    let y = (at.y + 14.0).min(rect.bottom() - size.y).max(rect.top());
    let bg = egui::Rect::from_min_size(egui::pos2(x, y), size);
    painter.rect_filled(bg, 3.0, Color32::from_rgba_unmultiplied(0, 0, 0, 190));
    painter.galley(bg.min + egui::vec2(3.0, 1.5), g, col);
}

/// The longest a curve's live trail may be, in ms.
pub(crate) const TRAIL_MS_MAX: i64 = 3000;

/// Add a live-trail sample. A long trail keeps at most `MAX` samples: while the
/// newest is within one slot (`dur / MAX`) of the one before, it is replaced
/// rather than joined, so a 3 s trail costs no more to draw than a short one.
pub(crate) fn push_trail_sample(
    trail: &mut std::collections::VecDeque<(f32, std::time::Instant)>,
    x: f32,
    now: std::time::Instant,
    dur: std::time::Duration,
) {
    const MAX: u32 = 120;
    let slot = dur / MAX;
    let n = trail.len();
    if n >= 2 && now.duration_since(trail[n - 2].1) < slot {
        trail[n - 1] = (x, now);
    } else {
        trail.push_back((x, now));
    }
}

/// Param that unlinks a curve's grid H / V counts. Stored inverted so a curve
/// saved before the link existed comes up linked, the default.
pub(crate) const GRID_UNLINKED: &str = "grid_unlinked";

/// Hover text on a range ↓ field that Abs has set aside.
pub(crate) const ABS_MIN_HOVER: &str = "Abs reads a magnitude, so only the ↑ end of the range applies.";

/// Grid H / V count fields with a 🔗 toggle between them. Linked (the default),
/// changing either count moves the other by as much, so the grid keeps its
/// shape. The link is written straight to the node; returns the H and V field
/// rects and whether a count changed.
pub(crate) fn grid_count_fields(
    ui: &mut egui::Ui,
    node_id: NodeId,
    snarl: &mut Snarl<NodeData>,
    gx: &mut f64,
    gy: &mut f64,
    max: f64,
) -> ([egui::Rect; 2], bool) {
    let unlinked = snarl.get_node(node_id)
        .and_then(|n| n.params.get(GRID_UNLINKED))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let (bx, by) = (*gx, *gy);
    let rh = ui.add(egui::DragValue::new(gx).speed(0.25).range(1.0..=max).max_decimals(0).prefix("H "));
    let link = link_toggle(ui, !unlinked,
        if unlinked { "Link H and V: change both together" } else { "Linked: H and V change together. Click to set them apart." });
    let rv = ui.add(egui::DragValue::new(gy).speed(0.25).range(1.0..=max).max_decimals(0).prefix("V "));
    if link.clicked() {
        if let Some(n) = snarl.get_node_mut(node_id) {
            n.params.insert(GRID_UNLINKED.into(), Value::Bool(!unlinked));
        }
    }
    if !unlinked {
        if rh.changed() {
            *gy = (*gy + (*gx - bx)).clamp(1.0, max);
        } else if rv.changed() {
            *gx = (*gx + (*gy - by)).clamp(1.0, max);
        }
    }
    ([rh.rect, rv.rect], rh.changed() || rv.changed())
}

/// Bare Grid + Trail row.
pub(crate) fn render_response_curve_grid_row(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (mut gx, mut gy, mut tm) = snarl.get_node(inner_id).map(|n| {
        let gx = n.params.get("grid_x").and_then(|v| v.as_i64()).unwrap_or(4) as f64;
        let gy = n.params.get("grid_y").and_then(|v| v.as_i64()).unwrap_or(4) as f64;
        let tm = n.params.get("trail_ms").and_then(|v| v.as_i64()).unwrap_or(300);
        (gx, gy, tm)
    }).unwrap_or((4.0, 4.0, 300));
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(220.0, 22.0));
    let mut changed = false;
    let mut field_rects = [egui::Rect::NOTHING; 3];
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Grid").weak());
        let ([rh, rv], c) = grid_count_fields(ui, inner_id, snarl, &mut gx, &mut gy, 20.0);
        field_rects[0] = rh;
        field_rects[1] = rv;
        changed |= c;
        ui.separator();
        ui.label(egui::RichText::new("Trail").weak());
        let rt = ui.add(egui::DragValue::new(&mut tm).speed(5.0)
            .range(0i64..=TRAIL_MS_MAX).suffix("ms"));
        field_rects[2] = rt.rect; changed |= rt.changed();
    });
    publish_nav_field_rects(ui, inner_id, &field_rects);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("grid_x".into(),   serde_json::json!(gx as i64));
            node.params.insert("grid_y".into(),   serde_json::json!(gy as i64));
            node.params.insert("trail_ms".into(), serde_json::json!(tm));
        }
    }
}

/// Bare Scale grid + Labels checkboxes row.
pub(crate) fn render_response_curve_grid_options_row(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (mut ssg, mut sgl) = snarl.get_node(inner_id).map(|n| {
        let ssg = n.params.get("show_scaled_grid").and_then(|v| v.as_bool()).unwrap_or(false);
        let sgl = n.params.get("show_grid_labels").and_then(|v| v.as_bool()).unwrap_or(false);
        (ssg, sgl)
    }).unwrap_or((false, false));
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(220.0, 22.0));
    let mut changed = false;
    let mut fr = [egui::Rect::NOTHING; 2];
    ui.horizontal(|ui| {
        let was = ssg;
        fr[0] = ui.checkbox(&mut ssg, egui::RichText::new("Scale grid"))
            .on_hover_text("Adapt grid lines to the current Log/Exp scaling").rect;
        changed |= ssg != was;
        let was = sgl;
        fr[1] = ui.checkbox(&mut sgl, egui::RichText::new("Labels"))
            .on_hover_text("Show value labels on grid lines").rect;
        changed |= sgl != was;
    });
    publish_nav_field_rects(ui, inner_id, &fr);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("show_scaled_grid".into(), Value::Bool(ssg));
            node.params.insert("show_grid_labels".into(), Value::Bool(sgl));
        }
    }
}

/// Paints just the response-curve graph (background + grid + curve + control
/// points + bias handles + live-input trails) into `rect`, and writes back any
/// param changes made via interaction. Shared between the in-editor body
/// renderer and the bare layout-pinned renderer on the sub-patch face.
pub(crate) fn paint_response_curve_graph(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    rect: egui::Rect,
    bg_resp: egui::Response,
    is_vec: bool,
    graph_ov: Option<&crate::canvas::node::PinGraphOverride>,
) {
    // Initialise params on first use (kept consistent with the body renderers).
    let needs_init = snarl.get_node(node_id).map(|n| !n.params.contains_key("points")).unwrap_or(false);
    if needs_init {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("points".into(), serde_json::json!([[0.0, 0.0], [1.0, 1.0]]));
            node.params.insert("biases".into(), serde_json::json!([0.0]));
            if !is_vec {
                node.params.insert("absolute".into(), Value::Bool(true));
                node.params.insert("in_min".into(),   serde_json::json!(-1.0));
                node.params.insert("out_min".into(),  serde_json::json!(-1.0));
            }
            node.params.insert("in_max".into(),   serde_json::json!(1.0f64));
            node.params.insert("out_max".into(),  serde_json::json!(1.0f64));
            node.params.insert("grid_x".into(),   serde_json::json!(4i64));
            node.params.insert("grid_y".into(),   serde_json::json!(4i64));
            node.params.insert("snap".into(),     Value::Bool(false));
            node.params.insert("scale_t".into(),  serde_json::json!(0.0f64));
            node.params.insert("trail_ms".into(), serde_json::json!(300i64));
        }
    }

    // Read params.
    let (points, biases, absolute, in_min, in_max, out_min, out_max, grid_x, grid_y, snap, scale_t, trail_ms, show_scaled_grid, show_grid_labels) = snarl
        .get_node(node_id)
        .map(|n| {
            let pts: Vec<[f32; 2]> = n.params.get("points")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|p| {
                    let a = p.as_array()?;
                    Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
                }).collect())
                .unwrap_or_else(|| vec![[0.0, 0.0], [1.0, 1.0]]);
            let bss: Vec<f32> = n.params.get("biases")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|b| b.as_f64().map(|f| f as f32)).collect())
                .unwrap_or_default();
            let abs  = if is_vec { true } else { n.params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true) };
            let i0   = if is_vec { 0.0 } else { n.params.get("in_min") .and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32 };
            let i1   = n.params.get("in_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let o0   = if is_vec { 0.0 } else { n.params.get("out_min").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32 };
            let o1   = n.params.get("out_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let gx   = n.params.get("grid_x").and_then(|v| v.as_i64()).unwrap_or(4).max(1) as usize;
            let gy   = n.params.get("grid_y").and_then(|v| v.as_i64()).unwrap_or(4).max(1) as usize;
            let sn   = n.params.get("snap").and_then(|v| v.as_bool()).unwrap_or(false);
            let sc   = n.params.get("scale_t").and_then(|v| v.as_f64()).map(|f| f as f32).unwrap_or(0.0);
            let tm   = n.params.get("trail_ms").and_then(|v| v.as_i64()).unwrap_or(300).clamp(0, TRAIL_MS_MAX);
            let ssg  = n.params.get("show_scaled_grid").and_then(|v| v.as_bool()).unwrap_or(false);
            let sgl  = n.params.get("show_grid_labels").and_then(|v| v.as_bool()).unwrap_or(MotionUnit::of(n).is_some());
            (pts, bss, abs, i0, i1, o0, o1, gx, gy, sn, sc, tm, ssg, sgl)
        })
        .unwrap_or_else(|| (vec![[0.0, 0.0], [1.0, 1.0]], vec![], true, -1.0, 1.0, -1.0, 1.0, 4, 4, false, 0.0f32, 300, false, false));

    let n_channels = snarl.get_node(node_id).map(curve_channels).unwrap_or(1);
    let legend = snarl.get_node(node_id).and_then(curve_channel_labels);
    let live_inputs: Vec<Option<f32>> = (0..n_channels)
        .map(|ch| snarl.get_node(node_id)
            .and_then(|n| n.extra.last_signals.get(ch)?.as_ref())
            .map(sig_f32))
        .collect();

    // A motion pick runs on the whole sensor and shows only its view of it.
    let view = CurveView::resolve(snarl.get_node(node_id), absolute, scale_t, [in_min, in_max, out_min, out_max]);
    let (x_lo, x_hi) = (view.x_lo, view.x_hi);
    let (y_lo, y_hi) = (view.y_lo, view.y_hi);
    let x_range = x_hi - x_lo;
    let y_range = y_hi - y_lo;

    let mut new_points  = points.clone();
    let mut new_biases  = biases.clone();
    let mut pts_changed  = false;
    let mut bias_changed = false;

    let painter = ui.painter_at(rect);

    let c2s = |x: f32, y: f32| egui::pos2(
        rect.left() + (x - x_lo) / x_range * rect.width(),
        rect.bottom() - (y - y_lo) / y_range * rect.height(),
    );
    let s2c = |pos: egui::Pos2| -> [f32; 2] {[
        x_lo + (pos.x - rect.left()) / rect.width() * x_range,
        y_lo + (rect.bottom() - pos.y) / rect.height() * y_range,
    ]};
    // Shared grid-position builder — same logic as the body renderers.
    let redist = |mut nodes: Vec<f32>, n: usize| -> Vec<f32> {
        let min_gap = 1.0f32 / n as f32 * 0.5;
        for _ in 0..n {
            nodes.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let crowded = (1..nodes.len().saturating_sub(1))
                .filter(|&i| (nodes[i]-nodes[i-1]).min(nodes[i+1]-nodes[i]) < min_gap)
                .min_by(|&a, &b| {
                    let ga = (nodes[a]-nodes[a-1]).min(nodes[a+1]-nodes[a]);
                    let gb = (nodes[b]-nodes[b-1]).min(nodes[b+1]-nodes[b]);
                    ga.partial_cmp(&gb).unwrap()
                });
            let Some(ci) = crowded else { break; };
            nodes.remove(ci);
            let (li, _) = nodes.windows(2).enumerate()
                .max_by(|(_, a), (_, b)| (a[1]-a[0]).partial_cmp(&(b[1]-b[0])).unwrap())
                .unwrap();
            nodes.insert(li+1, (nodes[li]+nodes[li+1])*0.5);
        }
        nodes.sort_by(|a, b| a.partial_cmp(b).unwrap());
        nodes
    };
    let build_grid_nodes = |n: usize| -> Vec<f32> {
        if n == 0 { return vec![0.0, 1.0]; }
        if !show_scaled_grid {
            return (0..=n).map(|i| i as f32 / n as f32).collect();
        }
        if absolute || is_vec {
            let nodes = (0..=n).map(|i| {
                let t = i as f32 / n as f32;
                1.0 - curve_scale_inv(1.0 - t, scale_t)
            }).collect();
            redist(nodes, n)
        } else {
            // Bidirectional: both halves expand outward from centre.
            // t=0 is centre, t=1 is edge; same scale formula as abs case.
            let half_lo = n / 2;
            let half_hi = n - half_lo;
            let lo: Vec<f32> = (0..=half_lo).map(|i| {
                let t = i as f32 / half_lo as f32;
                let s = 1.0 - curve_scale_inv(1.0 - t, scale_t);
                0.5 - s * 0.5
            }).collect();
            let hi: Vec<f32> = (0..=half_hi).map(|i| {
                let t = i as f32 / half_hi as f32;
                let s = 1.0 - curve_scale_inv(1.0 - t, scale_t);
                0.5 + s * 0.5
            }).collect();
            let mut merged = redist(lo, half_lo);
            for v in redist(hi, half_hi).iter().skip(1) { merged.push(*v); }
            merged.sort_by(|a, b| a.partial_cmp(b).unwrap());
            merged
        }
    };
    let snap_nodes_x = build_grid_nodes(grid_x);
    let snap_nodes_y = build_grid_nodes(grid_y);
    let grid_x_positions: Vec<f32> = (1..grid_x).map(|i| x_lo + snap_nodes_x[i] * x_range).collect();
    let grid_y_positions: Vec<f32> = (1..grid_y).map(|i| y_lo + snap_nodes_y[i] * y_range).collect();

    let do_snap = |x: f32, y: f32| -> (f32, f32) {
        if !snap { return (x, y); }
        let u = ((x - x_lo) / x_range).clamp(0.0, 1.0);
        let v = ((y - y_lo) / y_range).clamp(0.0, 1.0);
        let su = snap_nodes_x.iter().copied()
            .min_by(|a, b| (a-u).abs().partial_cmp(&(b-u).abs()).unwrap()).unwrap_or(u);
        let sv = snap_nodes_y.iter().copied()
            .min_by(|a, b| (a-v).abs().partial_cmp(&(b-v).abs()).unwrap()).unwrap_or(v);
        snap_point(x, y, (x_lo + su * x_range, y_lo + sv * y_range),
            &snap_nodes_x, &snap_nodes_y, (x_lo, x_range, y_lo, y_range), true)
    };
    publish_nav_curve_snap(ui.ctx(), node_id, snap, &snap_nodes_x, &snap_nodes_y, (x_lo, x_range, y_lo, y_range), true);

    let (graph_bg, graph_outline) = graph_chrome(graph_ov);
    painter.rect_filled(rect, 2.0, graph_bg);

    let (grid_faint, grid_axis) = graph_grid_colors(graph_ov);
    let gs = egui::Stroke::new(0.5, grid_faint);
    for &x in &grid_x_positions { painter.line_segment([c2s(x, y_lo), c2s(x, y_hi)], gs); }
    for &y in &grid_y_positions { painter.line_segment([c2s(x_lo, y), c2s(x_hi, y)], gs); }
    if let Some((a, b)) = identity_span(x_lo, x_hi, y_lo, y_hi) {
        painter.line_segment([c2s(a, a), c2s(b, b)], egui::Stroke::new(0.5, grid_axis));
    }

    let mut label_inset = egui::Vec2::ZERO;
    if show_grid_labels {
        let xs = grid_x_positions.iter().map(|&x| c2s(x, y_hi).x).zip(view.labels_in(&grid_x_positions));
        let ys = grid_y_positions.iter().map(|&y| c2s(x_lo, y).y).zip(view.labels_out(&grid_y_positions));
        label_inset = paint_axis_labels(&painter, rect, xs.collect(), ys.collect());
    }

    if new_points.len() >= 2 {
        let steps = 120usize;
        let curve_pts: Vec<egui::Pos2> = (0..=steps)
            .map(|i| {
                let x = x_lo + x_range * i as f32 / steps as f32;
                let y = sample_curve(&new_points, x, &new_biases).clamp(y_lo, y_hi);
                c2s(x, y)
            })
            .collect();
        for w in curve_pts.windows(2) {
            painter.line_segment([w[0], w[1]],
                egui::Stroke::new(1.5, Color32::from_gray(200)));
        }
    }

    let bias_id_tag = if is_vec { "vbias_h_only" } else { "bias_h_only" };
    let pt_id_tag   = if is_vec { "vcpt_only" }    else { "cpt_only" };
    let trail_id_tag = if is_vec { "vtrail_only" } else { "trail_only" };

    // Bias handles show on mouse Alt OR gamepad bias mode (hold-North).
    let nav_bias = ui.ctx().data(|d|
        d.get_temp::<u64>(egui::Id::new(("gp_nav_curve_bias", node_id.0))))
        .map_or(false, |p| crate::widgets::nav_pass_matches(ui.ctx(), p));
    let alt_held = ui.input(|i| i.modifiers.alt) || nav_bias;
    if alt_held && new_points.len() >= 2 {
        while new_biases.len() < new_points.len() - 1 { new_biases.push(0.0); }
        for seg in 0..(new_points.len() - 1) {
            let mid_x = (new_points[seg][0] + new_points[seg + 1][0]) * 0.5;
            let mid_y = sample_curve(&new_points, mid_x, &new_biases).clamp(y_lo, y_hi);
            let hpos  = c2s(mid_x, mid_y);
            // A zoomed graph (a motion pick's view) leaves points outside it: no
            // hidden handle may catch clicks meant for whatever is drawn there.
            if !rect.expand(8.0).contains(hpos) { continue; }
            let hid   = ui.id().with((bias_id_tag, node_id, seg));
            let hresp = ui.interact(
                egui::Rect::from_center_size(hpos, egui::Vec2::splat(14.0)),
                hid, egui::Sense::click_and_drag());
            if hresp.double_clicked() {
                new_biases[seg] = 0.0;
                bias_changed = true;
            } else if hresp.dragged() {
                let dy = -hresp.drag_delta().y / rect.height() * y_range;
                new_biases[seg] = (new_biases[seg] + dy).clamp(-2.0, 2.0);
                bias_changed = true;
            }
            let hcol = if hresp.hovered() || hresp.dragged() {
                Color32::from_rgb(255, 220, 50)
            } else { Color32::from_rgb(180, 140, 20) };
            painter.circle_filled(hpos, 4.0, hcol);
            painter.circle_stroke(hpos, 4.0, egui::Stroke::new(1.0, Color32::from_gray(100)));
        }
    }

    let mut remove_idx: Option<usize> = None;
    for i in 0..new_points.len() {
        let [px, py] = new_points[i];
        let screen   = c2s(px, py);
        // A zoomed graph (a motion pick's view) leaves points outside it: no
        // hidden handle may catch clicks meant for whatever is drawn there.
        if !rect.expand(8.0).contains(screen) { continue; }
        let pt_id    = ui.id().with((pt_id_tag, node_id, i));
        let pt_resp  = ui.interact(
            egui::Rect::from_center_size(screen, egui::Vec2::splat(12.0)),
            pt_id, egui::Sense::click_and_drag());

        // Origin-anchored drag: stash the point's [x, y] at drag start and the
        // running pixel offset; each frame, target = origin + accumulated_px
        // mapped to curve coords, then snapped once. Without this, snapping
        // accumulates per-frame rounding and feels "drunk".
        let origin_id = ui.id().with(("crv_pt_origin", pt_id_tag, node_id, i));
        if pt_resp.drag_started() && !alt_held {
            ui.ctx().data_mut(|d| d.insert_temp(origin_id, [px, py, 0.0f32, 0.0f32]));
        }
        if pt_resp.dragged() && !alt_held {
            let prev = ui.ctx().data(|d| d.get_temp::<[f32;4]>(origin_id))
                .unwrap_or([px, py, 0.0, 0.0]);
            let dd  = pt_resp.drag_delta();
            let acc_x_px = prev[2] + dd.x;
            let acc_y_px = prev[3] + dd.y;
            ui.ctx().data_mut(|d| d.insert_temp(origin_id, [prev[0], prev[1], acc_x_px, acc_y_px]));
            let nx_raw = prev[0] + acc_x_px * x_range / rect.width();
            let ny_raw = prev[1] - acc_y_px * y_range / rect.height();
            let lo_x   = new_points.get(i.wrapping_sub(1)).map(|p| p[0] + 0.001).unwrap_or(x_lo);
            let hi_x   = new_points.get(i + 1).map(|p| p[0] - 0.001).unwrap_or(x_hi);
            let (sx, sy) = do_snap(nx_raw, ny_raw);
            new_points[i] = [sx.clamp(lo_x, hi_x), sy.clamp(y_lo, y_hi)];
            pts_changed = true;
        }
        if pt_resp.drag_stopped() {
            ui.ctx().data_mut(|d| d.remove_temp::<[f32;4]>(origin_id));
        }
        // Double-click: onto the 1:1 line — output equal to its input.
        if pt_resp.double_clicked() {
            let lo = if absolute { 0.0 } else { -1.0 };
            new_points[i][1] = new_points[i][0].clamp(lo, 1.0);
            pts_changed = true;
        }
        if pt_resp.secondary_clicked() && new_points.len() > 2 {
            remove_idx = Some(i);
            pts_changed = true;
        }
        let col = if pt_resp.hovered() || pt_resp.dragged() { Color32::WHITE } else { Color32::from_gray(190) };
        painter.circle_filled(screen, 5.0, col);
        painter.circle_stroke(screen, 5.0, egui::Stroke::new(1.0, Color32::from_gray(80)));
        if show_grid_labels || pt_resp.hovered() || pt_resp.dragged() {
            paint_point_value(&painter, rect, screen, view.value_out(py));
        }

        // Gamepad-nav: highlight the dot the driver selected. Driver publishes
        // (pass, selected_idx, editing_dot) under ("gp_nav_curve_sel", node).
        let sel: Option<(u64, usize, bool)> = ui.ctx().data(|d|
            d.get_temp(egui::Id::new(("gp_nav_curve_sel", node_id.0))));
        if let Some((pass, sel_i, editing_dot)) = sel {
            if crate::widgets::nav_pass_matches(ui.ctx(), pass) && sel_i == i {
                let accent = ui.visuals().selection.stroke.color;
                let [r8, g8, b8, _] = accent.to_array();
                for k in 0..5 {
                    let t = (k as f32 + 1.0) / 5.0;
                    let rr = (if editing_dot { 16.0 } else { 12.0 }) * t;
                    let a = ((if editing_dot { 170.0 } else { 120.0 }) * (1.0 - t)) as u8;
                    if a == 0 { continue; }
                    painter.circle_stroke(screen, rr,
                        egui::Stroke::new(2.0, Color32::from_rgba_unmultiplied(r8, g8, b8, a)));
                }
                painter.circle_filled(screen, if editing_dot { 6.0 } else { 5.0 }, accent);
                painter.circle_stroke(screen, if editing_dot { 6.0 } else { 5.0 },
                    egui::Stroke::new(1.5, Color32::WHITE));
            }
        }
    }

    // Gamepad-nav: publish curve geometry (graph rect + axis bounds) so the
    // driver can map graph↔screen for dot stepping, cursor hit-test, and moves.
    // Transform the rect to GLOBAL (screen) space — in Easy mode this body
    // renders on a scaled/scrolled sub-layer, so the raw rect is body-local and
    // would never match the screen-space gamepad cursor.
    {
        let pass = ui.ctx().cumulative_pass_nr();
        let to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
            .unwrap_or(egui::emath::TSTransform::IDENTITY);
        let screen_rect = to_global * rect;
        ui.ctx().data_mut(|d| d.insert_temp(
            egui::Id::new(("gp_nav_curve_geom", node_id.0)),
            (pass, screen_rect, x_lo, x_hi, y_lo, y_hi)));
    }

    // Add/remove keep each segment's bias on its own segment (plain Vec
    // insert/remove would slide every later bend onto its neighbour).
    if let Some(idx) = remove_idx { curve_remove_point(&mut new_points, &mut new_biases, idx); }
    if bg_resp.double_clicked() {
        if let Some(pos) = bg_resp.interact_pointer_pos() {
            let [gx_raw, gy_raw] = s2c(pos);
            let (gx_sn, gy_sn)   = do_snap(gx_raw, gy_raw);
            let gx = gx_sn.clamp(x_lo, x_hi);
            let gy = gy_sn.clamp(y_lo, y_hi);
            let idx = new_points.partition_point(|p| p[0] < gx);
            curve_insert_point(&mut new_points, &mut new_biases, idx, [gx, gy]);
            pts_changed = true;
        }
    }

    // Live-position trails.
    let abs_max   = view.in_max.abs().max(f32::EPSILON);
    let trail_dur = std::time::Duration::from_millis(trail_ms as u64);
    let now       = std::time::Instant::now();
    let mut has_active = false;
    for (ch, raw_opt) in live_inputs.iter().enumerate() {
        let Some(raw) = raw_opt else { continue; };
        has_active = true;
        let graph_x = if absolute {
            curve_scale((raw.abs() / abs_max).clamp(0.0, 1.0), scale_t)
        } else {
            let in_range = (view.in_max - view.in_min).abs().max(f32::EPSILON);
            let norm     = ((raw - view.in_min) / in_range * 2.0 - 1.0).clamp(-1.0, 1.0);
            let sign     = if norm < 0.0 { -1.0f32 } else { 1.0 };
            sign * curve_scale(norm.abs(), scale_t)
        };
        type Trail = std::collections::VecDeque<(f32, std::time::Instant)>;
        let trail_id = ui.id().with((trail_id_tag, node_id, ch as u32));
        let mut trail: Trail = ui.data(|d| d.get_temp::<Trail>(trail_id).clone().unwrap_or_default());
        if trail_ms > 0 {
            push_trail_sample(&mut trail, graph_x, now, trail_dur);
            while trail.front().map(|&(_, t)| now.duration_since(t) > trail_dur).unwrap_or(false) {
                trail.pop_front();
            }
        } else { trail.clear(); }
        let trail_pts: Vec<(f32, std::time::Instant)> = trail.iter().cloned().collect();
        ui.data_mut(|d| d.insert_temp(trail_id, trail));
        let ch_col = graph_channel_color(graph_ov, ch);
        for w in trail_pts.windows(2) {
            let (x0, _)  = w[0];
            let (x1, t1) = w[1];
            let age   = now.duration_since(t1).as_secs_f32() / trail_dur.as_secs_f32();
            let alpha = ((1.0 - age.clamp(0.0, 1.0)) * 220.0) as u8;
            let col   = Color32::from_rgba_unmultiplied(ch_col.r(), ch_col.g(), ch_col.b(), alpha);
            let steps = (((x1 - x0).abs() / x_range * 80.0) as usize).max(1);
            let x0_y  = sample_curve(&new_points, x0, &new_biases).clamp(y_lo, y_hi);
            let mut prev = c2s(x0, x0_y);
            for s in 1..=steps {
                let t  = s as f32 / steps as f32;
                let ix = x0 + (x1 - x0) * t;
                let iy = sample_curve(&new_points, ix, &new_biases).clamp(y_lo, y_hi);
                let next = c2s(ix, iy);
                painter.line_segment([prev, next], egui::Stroke::new(1.5, col));
                prev = next;
            }
        }
        let graph_y = sample_curve(&new_points, graph_x, &new_biases).clamp(y_lo, y_hi);
        let head_col = Color32::from_rgba_unmultiplied(ch_col.r(), ch_col.g(), ch_col.b(), 220);
        painter.circle_filled(c2s(graph_x, graph_y), 3.5, head_col);
    }
    if has_active { request_repaint_throttled(ui.ctx()); }
    if let Some(labels) = legend {
        paint_curve_legend(&painter, rect, labels, label_inset, |ch| graph_channel_color(graph_ov, ch));
    }
    // Live reading at the mouse or gamepad cursor, with the labels.
    if show_grid_labels && !ui.input(|i| i.pointer.any_down()) {
        if let Some(at) = graph_reading_pos(ui, rect, bg_resp.hover_pos()) {
            paint_cursor_reading(&painter, rect, at, (x_lo, x_hi, y_lo, y_hi),
                |x| sample_curve(&new_points, x, &new_biases),
                |gx, gy| format!("{}  →  {}", view.value_in(gx), view.value_out(gy)));
        }
    }

    // Optional override frame, painted last so it sits above the graph content.
    if let Some(stroke) = graph_outline {
        painter.rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
    }

    // Write back curve points / biases.
    if pts_changed || bias_changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if pts_changed {
                new_biases.resize(new_points.len().saturating_sub(1), 0.0);
                let json: Vec<Value> = new_points.iter().map(|p| serde_json::json!([p[0], p[1]])).collect();
                node.params.insert("points".into(), Value::Array(json));
            }
            let bj: Vec<Value> = new_biases.iter()
                .filter_map(|&b| Number::from_f64(b as f64).map(Value::Number))
                .collect();
            node.params.insert("biases".into(), Value::Array(bj));
        }
    }
}


// ── Curve save/load/reset file format ────────────────────────────────────────
// .fxc is a JSON object. Float and Vec curves share the same fields; the Vec
// variant simply never stores "absolute", "in_min", "out_min".  Loading into
// either type ignores fields it doesn't use, so files are cross-compatible.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct CurveFile {
    #[serde(default)]
    pub(crate) points:          Vec<[f64; 2]>,
    #[serde(default)]
    pub(crate) biases:          Vec<f64>,
    #[serde(default = "default_true")]
    pub(crate) absolute:        bool,
    #[serde(default = "default_neg1")]
    pub(crate) in_min:          f64,
    #[serde(default = "default_1")]
    pub(crate) in_max:          f64,
    #[serde(default = "default_neg1")]
    pub(crate) out_min:         f64,
    #[serde(default = "default_1")]
    pub(crate) out_max:         f64,
    #[serde(default = "default_4")]
    pub(crate) grid_x:          i64,
    #[serde(default = "default_4")]
    pub(crate) grid_y:          i64,
    #[serde(default)]
    pub(crate) snap:            bool,
    #[serde(default)]
    pub(crate) scale_t:         f64,
    #[serde(default = "default_300")]
    pub(crate) trail_ms:        i64,
    #[serde(default)]
    pub(crate) show_scaled_grid: bool,
    #[serde(default)]
    pub(crate) show_grid_labels: bool,
    /// A Two-way Curve's whole setup — both lanes, the Hyst graph and its
    /// settings — as its params (`TWOWAY_PRESET_KEYS`). `points` / `biases`
    /// above still hold the lane being edited, so the file loads into a plain
    /// curve too; files without it load into a Two-way Curve's edited lane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) twoway: Option<serde_json::Map<String, Value>>,
}

/// The Two-way Curve params a `.fxc` preset carries beyond the shared ones.
pub(crate) const TWOWAY_PRESET_KEYS: &[&str] = &[
    "points", "biases", "points_dn", "biases_dn", "hyst_points", "hyst_biases",
    "hysteresis_pct", "hyst_dir", "hyst_start_up", "interp_ms", "peak_hold_ms", "peak_hold_dir",
];

/// A Two-way Curve's preset section, or None for any other curve.
pub(crate) fn twoway_preset_section(n: &NodeData) -> Option<serde_json::Map<String, Value>> {
    (curve_ui_module_id(n) == "module.twoway_response_curve").then(|| {
        TWOWAY_PRESET_KEYS.iter()
            .filter_map(|k| n.params.get(*k).map(|v| (k.to_string(), v.clone())))
            .collect()
    })
}

/// Load a preset's two-way section into a Two-way Curve. A key the file lacks
/// is cleared back to its default, so nothing from the old setup lingers
/// (a stale Hyst graph would otherwise override the loaded Hyst value).
pub(crate) fn apply_twoway_preset(n: &mut NodeData, section: &serde_json::Map<String, Value>) {
    for k in TWOWAY_PRESET_KEYS {
        match section.get(*k) {
            Some(v) => { n.params.insert(k.to_string(), v.clone()); }
            None => { n.params.remove(*k); }
        }
    }
}
pub(crate) fn default_true()  -> bool { true  }
pub(crate) fn default_neg1()  -> f64  { -1.0  }
pub(crate) fn default_1()     -> f64  {  1.0  }
pub(crate) fn default_4()     -> i64  {  4    }
pub(crate) fn default_300()   -> i64  { 300   }

/// Is this one of the curves that reshape a signal picked off an AutoMap bus?
pub(crate) fn is_automap_curve(module_id: &str) -> bool {
    matches!(module_id, "module.automap_response_curve" | "module.automap_twoway_response_curve")
}

/// The bus pin an AutoMap curve node reshapes ("" until one is picked).
pub(crate) fn automap_curve_pin(node: &NodeData) -> &str {
    node.params.get(flexinput_engine::eval::AUTOMAP_CURVE_PIN_PARAM)
        .and_then(|v| v.as_str())
        .unwrap_or("")
}

/// Whether an AutoMap curve's picked pin is a Vec2 (a stick or the D-pad).
pub(crate) fn automap_curve_pin_is_vec(node: &NodeData) -> bool {
    let pin = automap_curve_pin(node);
    am_canon::ALL_PINS.iter().any(|p| p.id == pin && p.signal_type == SignalType::Vec2)
}

/// Whether a two-way curve runs in Vec mode. An AutoMap one is exactly while a
/// Vec2 is picked — the engine decides it the same way, so the stored `vec_mode`
/// param is never consulted (or changed) for it; a plain one follows its toggle.
pub(crate) fn twoway_vec_mode(node: &NodeData) -> bool {
    if is_automap_curve(&node.module_id) {
        automap_curve_pin_is_vec(node)
    } else {
        node.params.get("vec_mode").and_then(|v| v.as_bool()).unwrap_or(false)
    }
}

/// How many channels a curve node draws: an AutoMap curve one per picked axis
/// (three for a gyro / accel pick, two for a touch point, else one), any other
/// curve its parallel
/// In/Out pairs.
pub(crate) fn curve_channels(node: &NodeData) -> usize {
    if is_automap_curve(&node.module_id) {
        flexinput_engine::eval::automap_curve_group(automap_curve_pin(node)).map_or(1, |g| g.len())
    } else {
        node.inputs.len().min(node.outputs.len()).max(1)
    }
}

/// Names for a curve's channels, shown in their colours as a legend on the
/// graph. Set where several axes share one graph without pins to tell them
/// apart: an AutoMap curve's gyro / accel / touch-point pick.
pub(crate) fn curve_channel_labels(node: &NodeData) -> Option<&'static [&'static str]> {
    if !is_automap_curve(&node.module_id) {
        return None;
    }
    match automap_curve_pin(node) {
        "gyro"  => Some(&["X roll", "Y pitch", "Z yaw"]),
        "accel" => Some(&["X", "Y", "Z"]),
        "touch1" | "touch2" => Some(&["X", "Y"]),
        _ => None,
    }
}

/// Grid labels for `values`, each followed by `suffix`: as many decimals as the
/// spacing between neighbouring lines needs to tell them apart (the finer the
/// scale, the more), and always a fraction on a value under 10.
pub(crate) fn axis_labels(values: &[f32], suffix: &str) -> Vec<String> {
    let step = values.windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .filter(|d| *d > 1e-6)
        .fold(f32::INFINITY, f32::min);
    // The fewest decimals that write the step itself exactly, capped one past
    // its own magnitude — 2.5 needs 1, 125 needs 0, a third settles for 2.
    let base = if step.is_finite() {
        let cap = ((-step.log10()).ceil() + 1.0).clamp(0.0, 4.0) as usize;
        (0..=cap).find(|&d| {
            let x = step * 10f32.powi(d as i32);
            (x - x.round()).abs() < 0.02
        }).unwrap_or(cap)
    } else {
        0
    };
    values.iter().map(|&v| {
        if v.abs() < 1e-6 {
            return format!("0{suffix}");
        }
        let d = if v.abs() < 10.0 { base.max(1) } else { base };
        format!("{v:.d$}{suffix}")
    }).collect()
}

/// One value as a label, to 3 significant figures (at most 4 decimals), so a
/// dot reads as finely at 2 °/s as at 2000.
pub(crate) fn value_label(v: f32, suffix: &str) -> String {
    if v.abs() < 1e-6 {
        return format!("0{suffix}");
    }
    let d = (2 - v.abs().log10().floor() as i32).clamp(0, 4) as usize;
    format!("{v:.d$}{suffix}")
}

/// Paint grid labels: input labels along the top, given as (screen x, text),
/// staggered over two rows so neighbours never overlap (a label that fits in
/// neither row is dropped); output labels down the left, as (screen y, text).
/// Returns the corner the labels occupy (left column width, top rows height),
/// so a legend can sit clear of them.
pub(crate) fn paint_axis_labels(
    painter: &egui::Painter,
    rect: egui::Rect,
    xs: Vec<(f32, String)>,
    ys: Vec<(f32, String)>,
) -> egui::Vec2 {
    const ROW_H: f32 = 10.0;
    const GAP: f32 = 3.0;
    let col = Color32::from_rgba_unmultiplied(180, 180, 180, 160);
    let font = egui::FontId::proportional(9.0);
    let mut row_end = [f32::NEG_INFINITY; 2];
    let mut rows_used = 0usize;
    for (sx, text) in xs {
        let g = painter.layout_no_wrap(text, font.clone(), col);
        let left = sx + 1.0;
        let Some(row) = (0..2).find(|&r| left >= row_end[r] + GAP) else { continue };
        row_end[row] = left + g.size().x;
        rows_used = rows_used.max(row + 1);
        painter.galley(egui::pos2(left, rect.top() + 1.0 + ROW_H * row as f32), g, col);
    }
    let mut last_y = f32::INFINITY;
    let mut col_w = 0.0f32;
    for (sy, text) in ys {
        if last_y - sy < ROW_H + 1.0 { continue; }
        last_y = sy;
        let g = painter.layout_no_wrap(text, font.clone(), col);
        col_w = col_w.max(g.size().x);
        painter.galley(egui::pos2(rect.left() + 1.0, sy - 9.0), g, col);
    }
    egui::vec2(if col_w > 0.0 { col_w + 6.0 } else { 0.0 }, ROW_H * rows_used as f32 + 4.0)
}

/// Paint a channel legend near the graph's top-left corner, clear of the grid
/// labels (`inset`, from [`paint_axis_labels`]): each label in its channel's
/// colour, stacked, on a dark backing so it reads over the grid.
pub(crate) fn paint_curve_legend(
    painter: &egui::Painter,
    rect: egui::Rect,
    labels: &[&str],
    inset: egui::Vec2,
    color_of: impl Fn(usize) -> Color32,
) {
    if labels.is_empty() {
        return;
    }
    let font = egui::FontId::proportional(10.0);
    let galleys: Vec<_> = labels.iter().enumerate()
        .map(|(ch, l)| painter.layout_no_wrap(l.to_string(), font.clone(), color_of(ch)))
        .collect();
    let pad = 3.0;
    let row_h = galleys.iter().map(|g| g.size().y).fold(0.0, f32::max);
    let w = galleys.iter().map(|g| g.size().x).fold(0.0, f32::max) + pad * 2.0;
    let h = row_h * galleys.len() as f32 + pad * 2.0;
    let bg = egui::Rect::from_min_size(rect.left_top() + inset.max(egui::vec2(4.0, 4.0)), egui::vec2(w, h));
    painter.rect_filled(bg, 3.0, Color32::from_rgba_unmultiplied(0, 0, 0, 150));
    for (i, g) in galleys.into_iter().enumerate() {
        let pos = bg.left_top() + egui::vec2(pad, pad + row_h * i as f32);
        painter.galley(pos, g, Color32::WHITE);
    }
}

// ── Motion-sensor curves (AutoMap gyro / accel picks) ─────────────────────────

/// The motion sensor behind a curve: an AutoMap curve's gyro / accel pick
/// (whole or one axis), or what a plain curve's wires carry (see
/// [`refresh_motion_units`]). Such a curve runs on the sensor's full range, and its In/Out fields
/// only choose how much of that range the graph shows — in the sensor's own
/// units, with nothing rescaled. The shown range is stored per sensor
/// (`gyro_view_*` in °/s, `accel_view_*` in G), apart from the In/Out ranges a
/// trigger or stick pick scales by, so zooming the graph on one never changes
/// the curve on the other.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MotionUnit { Gyro, Accel }

impl MotionUnit {
    pub(crate) fn of(node: &NodeData) -> Option<Self> {
        if is_automap_curve(&node.module_id) {
            Self::of_pin(automap_curve_pin(node))
        } else if is_plain_curve(&node.module_id) {
            node.extra.motion_unit
        } else {
            None
        }
    }

    /// The sensor a bus pin (or an AutoMap curve's group pick) belongs to.
    pub(crate) fn of_pin(pin: &str) -> Option<Self> {
        if pin == "gyro" || pin.starts_with("gyro_") {
            Some(Self::Gyro)
        } else if pin == "accel" || pin.starts_with("accel_") {
            Some(Self::Accel)
        } else {
            None
        }
    }

    /// The physical value a bus 1.0 stands for (the devices normalise to it).
    pub(crate) fn full_scale(self) -> f32 {
        match self {
            Self::Gyro => flexinput_devices::gyro::GYRO_REF_DPS,
            Self::Accel => flexinput_devices::gyro::ACCEL_REF_G,
        }
    }

    pub(crate) fn suffix(self) -> &'static str {
        match self { Self::Gyro => "°/s", Self::Accel => " G" }
    }

    /// A physical value as an axis label.
    pub(crate) fn format(self, v: f32) -> String {
        match self {
            Self::Gyro => format!("{v:.0}°/s"),
            Self::Accel if v.abs() < 1.0 => format!("{v:.2} G"),
            Self::Accel => format!("{v:.1} G"),
        }
    }

    /// The shown-range param keys: [in ↓, in ↑, out ↓, out ↑].
    pub(crate) fn view_keys(self) -> [&'static str; 4] {
        match self {
            Self::Gyro => ["gyro_view_in_min", "gyro_view_in_max", "gyro_view_out_min", "gyro_view_out_max"],
            Self::Accel => ["accel_view_in_min", "accel_view_in_max", "accel_view_out_min", "accel_view_out_max"],
        }
    }

    /// How far the graph shows until set: what a hand-held turn or tilt
    /// mostly spans, not the sensor's whole range.
    pub(crate) fn default_extent(self) -> f32 {
        match self { Self::Gyro => 500.0, Self::Accel => 2.0 }
    }

    /// Step for dragging / nudging a shown-range value.
    pub(crate) fn step(self) -> f32 {
        match self { Self::Gyro => 10.0, Self::Accel => 0.05 }
    }

    /// The shown range [in ↓, in ↑, out ↓, out ↑], physical units.
    pub(crate) fn view(self, node: &NodeData) -> [f32; 4] {
        let d = self.default_extent();
        let defaults = [-d, d, -d, d];
        let keys = self.view_keys();
        std::array::from_fn(|i| {
            node.params.get(keys[i]).and_then(|v| v.as_f64()).map_or(defaults[i], |v| v as f32)
        })
    }

    pub(crate) fn set_view(self, node: &mut NodeData, view: [f32; 4]) {
        let fs = self.full_scale();
        for (k, v) in self.view_keys().into_iter().zip(view) {
            if let Some(n) = Number::from_f64(v.clamp(-fs, fs) as f64) {
                node.params.insert(k.into(), Value::Number(n));
            }
        }
    }
}

/// A curve module that takes its signal on a wire (not off an AutoMap bus).
pub(crate) fn is_plain_curve(module_id: &str) -> bool {
    matches!(module_id,
        "module.response_curve" | "module.vec_response_curve" | "module.twoway_response_curve")
}

/// What one oscilloscope channel's wire carries: its motion sensor, if it is
/// one (so the scope reads in °/s or G), and a short name for its legend.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ScopeChannel {
    pub(crate) unit: Option<MotionUnit>,
    pub(crate) name: String,
}

/// Trace every plain curve's wires back to their source and record on the node
/// (`extra.motion_unit`) whether they carry a motion sensor, so the curve can
/// read in °/s or G and zoom instead of rescale, like an AutoMap curve's pick.
/// The trace follows a device's or Splitter's pin, a 3DOF to 2D's Out / X / Y
/// in Pointer mode (still angular velocity, in the gyro's units — Steering
/// integrates it into something else), curves in between (a motion curve hands
/// on the same units), and sub-patch boundaries both ways. Every wired input
/// must agree. Oscilloscopes and Trigger Scopes get the same trace per input
/// (`extra.scope_channels`), each on its own. Runs once a frame, before
/// the graph is built.
pub(crate) fn refresh_motion_units(snarl: &mut Snarl<NodeData>) {
    let mut found = Vec::new();
    let mut scopes = Vec::new();
    collect_motion_units(snarl, &mut Vec::new(), &mut Vec::new(), &mut found, &mut scopes);
    for (path, unit) in found {
        if let Some(n) = node_at_path_mut(snarl, &path) {
            n.extra.motion_unit = unit;
        }
    }
    for (path, channels) in scopes {
        if let Some(n) = node_at_path_mut(snarl, &path) {
            if n.extra.scope_channels != channels {
                n.extra.scope_channels = channels;
            }
        }
    }
}

type Frames<'a> = Vec<(&'a Snarl<NodeData>, NodeId)>;

fn collect_motion_units<'a>(
    snarl: &'a Snarl<NodeData>,
    path: &mut Vec<NodeId>,
    frames: &mut Frames<'a>,
    out: &mut Vec<(Vec<NodeId>, Option<MotionUnit>)>,
    scopes: &mut Vec<(Vec<NodeId>, Vec<ScopeChannel>)>,
) {
    for (id, n) in snarl.nodes_ids_data() {
        let node = &n.value;
        if is_plain_curve(&node.module_id) {
            path.push(id);
            out.push((path.clone(), curve_inputs_motion_unit(snarl, id, frames)));
            path.pop();
        }
        if matches!(node.module_id.as_str(), "display.oscilloscope" | "display.trigscope") {
            let channels = (0..node.inputs.len())
                .map(|i| match snarl.in_pin(InPinId { node: id, input: i }).remotes.first() {
                    Some(&src) => ScopeChannel {
                        unit: source_motion_unit(snarl, src, frames, 0),
                        name: source_name(snarl, src, frames),
                    },
                    None => ScopeChannel::default(),
                })
                .collect();
            path.push(id);
            scopes.push((path.clone(), channels));
            path.pop();
        }
        if let Some(sp) = node.subpatch.as_ref() {
            path.push(id);
            frames.push((snarl, id));
            collect_motion_units(&sp.snarl, path, frames, out, scopes);
            frames.pop();
            path.pop();
        }
    }
}

/// A short name for what an output pin carries, for a scope's legend: a
/// device's or Splitter's pin by its own name ("Gyro X (roll)"), a sub-patch
/// inlet by what feeds it outside, anything else by its node — plus the pin
/// when the node has several.
fn source_name(snarl: &Snarl<NodeData>, src: OutPinId, frames: &[(&Snarl<NodeData>, NodeId)]) -> String {
    let Some(node) = snarl.get_node(src.node) else { return String::new() };
    let pin = node.outputs.get(src.output).map(|p| p.name.as_str()).unwrap_or("");
    match node.module_id.as_str() {
        "device.source" | "module.automap_split" => pin.to_string(),
        "subpatch.inlet" => frames.last()
            .and_then(|&(outer, sp_id)| {
                let up = *outer.in_pin(InPinId { node: sp_id, input: pin_index_of(node)? }).remotes.first()?;
                Some(source_name(outer, up, &frames[..frames.len() - 1]))
            })
            .unwrap_or_else(|| node.display_name.clone()),
        _ if node.outputs.len() > 1 => format!("{} · {pin}", node.display_name),
        _ => node.display_name.clone(),
    }
}

fn node_at_path_mut<'a>(snarl: &'a mut Snarl<NodeData>, path: &[NodeId]) -> Option<&'a mut NodeData> {
    let (&last, outer) = path.split_last()?;
    let mut cur = snarl;
    for &id in outer {
        cur = &mut cur.get_node_mut(id)?.subpatch.as_mut()?.snarl;
    }
    cur.get_node_mut(last)
}

/// The sensor every wired input of curve `id` carries, if they all agree.
fn curve_inputs_motion_unit(snarl: &Snarl<NodeData>, id: NodeId, frames: &[(&Snarl<NodeData>, NodeId)]) -> Option<MotionUnit> {
    let n_inputs = snarl.get_node(id)?.inputs.len();
    let mut unit = None;
    for i in 0..n_inputs {
        let Some(&src) = snarl.in_pin(InPinId { node: id, input: i }).remotes.first() else { continue };
        let u = source_motion_unit(snarl, src, frames, 0)?;
        if unit.is_some_and(|prev| prev != u) {
            return None;
        }
        unit = Some(u);
    }
    unit
}

/// A 3DOF to 2D in Pointer mode hands its rates straight on; Steering
/// integrates them. (`family`, or the legacy `mode` where "laser" was steering.)
fn gyro_3dof_is_pointer(node: &NodeData) -> bool {
    match node.params.get("family").and_then(|v| v.as_str()) {
        Some(f) => f != "steering",
        None => node.params.get("mode").and_then(|v| v.as_str()) != Some("laser"),
    }
}

fn pin_index_of(node: &NodeData) -> Option<usize> {
    node.params.get("pin_index").and_then(|v| v.as_u64()).map(|v| v as usize)
}

/// The sensor an output pin carries, following it upstream (see
/// [`refresh_motion_units`]).
fn source_motion_unit(
    snarl: &Snarl<NodeData>,
    src: OutPinId,
    frames: &[(&Snarl<NodeData>, NodeId)],
    depth: u32,
) -> Option<MotionUnit> {
    if depth > 16 {
        return None;
    }
    let node = snarl.get_node(src.node)?;
    match node.module_id.as_str() {
        "device.source" | "module.automap_split" => {
            let pin = node.params.get("output_pin_ids")?.as_array()?.get(src.output)?.as_str()?;
            MotionUnit::of_pin(pin)
        }
        // Out (Vec2), X, Y — not Lean, Lean!, Orientation or the bus.
        "processing.gyro_3dof" => (src.output <= 2 && gyro_3dof_is_pointer(node)).then_some(MotionUnit::Gyro),
        m if is_plain_curve(m) => {
            let up = *snarl.in_pin(InPinId { node: src.node, input: src.output }).remotes.first()?;
            source_motion_unit(snarl, up, frames, depth + 1)
        }
        "subpatch.inlet" => {
            let (outer, sp_id) = *frames.last()?;
            let up = *outer.in_pin(InPinId { node: sp_id, input: pin_index_of(node)? }).remotes.first()?;
            source_motion_unit(outer, up, &frames[..frames.len() - 1], depth + 1)
        }
        "subpatch" => {
            let sp = node.subpatch.as_ref()?;
            let outlet = sp.snarl.nodes_ids_data()
                .find(|(_, n)| n.value.module_id == "subpatch.outlet" && pin_index_of(&n.value) == Some(src.output))
                .map(|(id, _)| id)?;
            let up = *sp.snarl.in_pin(InPinId { node: outlet, input: 0 }).remotes.first()?;
            let mut inner = frames.to_vec();
            inner.push((snarl, src.node));
            source_motion_unit(&sp.snarl, up, &inner, depth + 1)
        }
        _ => None,
    }
}

/// For a motion pick, the In/Out ranges the curve runs on are the sensor's
/// whole range; the engine is handed these instead of the stored ones (which
/// stay for when a trigger or stick is picked). Applied by the graph builder.
pub(crate) fn apply_motion_run_ranges(node: &NodeData, params: &mut std::collections::HashMap<String, Value>) {
    if MotionUnit::of(node).is_none() {
        return;
    }
    for (k, v) in [("in_min", -1.0), ("in_max", 1.0), ("out_min", -1.0), ("out_max", 1.0)] {
        params.insert(k.into(), serde_json::json!(v));
    }
}

/// A curve node's params as the engine runs them (see [`apply_motion_run_ranges`]),
/// for UI readers that model the engine — the two-way hysteresis band.
pub(crate) fn curve_run_params(node: &NodeData) -> std::borrow::Cow<'_, std::collections::HashMap<String, Value>> {
    if MotionUnit::of(node).is_none() {
        return std::borrow::Cow::Borrowed(&node.params);
    }
    let mut p = node.params.clone();
    apply_motion_run_ranges(node, &mut p);
    std::borrow::Cow::Owned(p)
}

/// What a curve graph shows and how its axes read: the window in graph
/// coordinates, and the value ranges the curve runs on behind it. A plain
/// curve shows its whole 0..1 (or -1..1) graph over its stored In/Out ranges; a
/// motion pick runs on the whole sensor and shows only its view of it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CurveView {
    pub x_lo: f32, pub x_hi: f32, pub y_lo: f32, pub y_hi: f32,
    /// The ranges values are normalised by (bus units) — for the live dots.
    pub in_min: f32, pub in_max: f32, pub out_min: f32, pub out_max: f32,
    pub unit: Option<MotionUnit>,
    absolute: bool,
    scale_t: f32,
}

impl CurveView {
    /// `absolute` is the curve's effective Abs (forced on by a two-way Vec mode).
    pub(crate) fn of(node: &NodeData, absolute: bool, scale_t: f32) -> Self {
        let f = |k: &str, d: f64| node.params.get(k).and_then(|v| v.as_f64()).unwrap_or(d) as f32;
        let plain = Self::plain(absolute, scale_t, f("in_min", -1.0), f("in_max", 1.0), f("out_min", -1.0), f("out_max", 1.0));
        let Some(unit) = MotionUnit::of(node) else { return plain };
        let fs = unit.full_scale();
        let [il, ih, ol, oh] = unit.view(node).map(|v| (v / fs).clamp(-1.0, 1.0));
        // A value's graph coordinate: the Log/Exp scale, mirrored for a sign.
        let warp = |v: f32| v.signum() * curve_scale(v.abs(), scale_t);
        let full = if absolute { (0.0, 1.0) } else { (-1.0, 1.0) };
        let axis = |lo: f32, hi: f32| -> (f32, f32) {
            let (a, b) = if absolute { (0.0, warp(hi.abs())) } else { (warp(lo), warp(hi)) };
            if b - a > 1e-4 { (a, b) } else { full }
        };
        let (x_lo, x_hi) = axis(il, ih);
        let (y_lo, y_hi) = axis(ol, oh);
        Self {
            x_lo, x_hi, y_lo, y_hi,
            in_min: -1.0, in_max: 1.0, out_min: -1.0, out_max: 1.0,
            unit: Some(unit), absolute, scale_t,
        }
    }

    /// The view for `node` when it has a motion pick, else a plain view over
    /// the ranges the caller already read (`[in ↓, in ↑, out ↓, out ↑]`).
    pub(crate) fn resolve(node: Option<&NodeData>, absolute: bool, scale_t: f32, ranges: [f32; 4]) -> Self {
        match node {
            Some(n) if MotionUnit::of(n).is_some() => Self::of(n, absolute, scale_t),
            _ => Self::plain(absolute, scale_t, ranges[0], ranges[1], ranges[2], ranges[3]),
        }
    }

    pub(crate) fn plain(absolute: bool, scale_t: f32, in_min: f32, in_max: f32, out_min: f32, out_max: f32) -> Self {
        let (lo, hi) = if absolute { (0.0, 1.0) } else { (-1.0, 1.0) };
        Self {
            x_lo: lo, x_hi: hi, y_lo: lo, y_hi: hi,
            in_min, in_max, out_min, out_max,
            unit: None, absolute, scale_t,
        }
    }

    /// The value at graph coordinate `g`, as a fraction of `range_max`.
    fn real(&self, g: f32, range_max: f32) -> f32 {
        let g = if self.absolute { g.max(0.0) } else { g };
        g.signum() * curve_scale_inv(g.abs(), self.scale_t) * range_max
    }

    /// Labels for grid lines at graph coordinates `gs` on an axis ranging
    /// `lo..hi`: the sensor's units for a motion pick, else a percentage of a
    /// unit range, or plain values for a wider one. Abs reads only `hi`.
    fn labels(&self, gs: &[f32], lo: f32, hi: f32) -> Vec<String> {
        let (scale, suffix) = self.scale_suffix(lo, hi);
        let values: Vec<f32> = gs.iter().map(|&g| self.real(g, scale)).collect();
        axis_labels(&values, suffix)
    }

    /// What a graph unit is worth on an axis ranging `lo..hi`, and its suffix.
    fn scale_suffix(&self, lo: f32, hi: f32) -> (f32, &'static str) {
        match self.unit {
            Some(u) => (u.full_scale(), u.suffix()),
            None => {
                let m = if self.absolute { hi.abs() } else { lo.abs().max(hi.abs()) };
                if m <= 1.01 { (m * 100.0, "%") } else { (m, "") }
            }
        }
    }

    /// The input value at graph X `g`, for the cursor reading.
    pub(crate) fn value_in(&self, g: f32) -> String {
        let (scale, suffix) = self.scale_suffix(self.in_min, self.in_max);
        value_label(self.real(g, scale), suffix)
    }

    /// The output value at graph Y `g`, for a dot's label.
    pub(crate) fn value_out(&self, g: f32) -> String {
        let (scale, suffix) = self.scale_suffix(self.out_min, self.out_max);
        value_label(self.real(g, scale), suffix)
    }

    /// Labels for input grid lines at graph X positions `gs`.
    pub(crate) fn labels_in(&self, gs: &[f32]) -> Vec<String> { self.labels(gs, self.in_min, self.in_max) }

    /// Labels for output grid lines at graph Y positions `gs`.
    pub(crate) fn labels_out(&self, gs: &[f32]) -> Vec<String> { self.labels(gs, self.out_min, self.out_max) }
}

/// The In/Out fields of a motion pick: how much of the sensor the graph shows,
/// in its units. Draws the four values in a row and writes them back to the
/// view keys; returns the field rects (for gamepad nav) and whether one changed.
/// Under Abs (`absolute`) the ↓ ends don't apply and are greyed out.
pub(crate) fn motion_view_range_fields(
    ui: &mut egui::Ui,
    node_id: NodeId,
    snarl: &mut Snarl<NodeData>,
    unit: MotionUnit,
    absolute: bool,
) -> ([egui::Rect; 4], bool) {
    let mut view = snarl.get_node(node_id).map(|n| unit.view(n)).unwrap_or([0.0; 4]);
    let before = view;
    let linked = snarl.get_node(node_id).map_or(true, range_linked);
    let mut link_clicked = false;
    let fs = unit.full_scale();
    let decimals = if unit == MotionUnit::Gyro { 0 } else { 2 };
    let mut rects = [egui::Rect::NOTHING; 4];
    let mut changed = false;
    let hover = format!(
        "How much of the sensor the graph shows ({} full scale). Only the view: \
         the curve always runs over the whole range, and nothing is rescaled.",
        unit.format(fs),
    );
    ui.horizontal(|ui| {
        for (i, (label, arrow)) in [("In", "↓"), ("", "↑"), ("Out", "↓"), ("", "↑")].into_iter().enumerate() {
            if i == 2 { link_clicked = range_link_button(ui, linked); }
            if !label.is_empty() { ui.label(egui::RichText::new(label).small().weak()); }
            let is_min = i % 2 == 0;
            let r = ui.add_enabled(!(absolute && is_min), egui::DragValue::new(&mut view[i])
                .speed(unit.step())
                .range(-fs..=fs)
                .prefix(arrow)
                .suffix(unit.suffix())
                .max_decimals(decimals))
                .on_hover_text(hover.as_str())
                .on_disabled_hover_text(ABS_MIN_HOVER);
            rects[i] = r.rect;
            changed |= r.changed();
        }
    });
    if link_clicked {
        toggle_range_link(snarl, node_id);
    }
    if changed {
        if linked {
            follow_range_link(before, &mut view);
        }
        if let Some(n) = snarl.get_node_mut(node_id) {
            unit.set_view(n, view);
        }
    }
    (rects, changed)
}

/// The module an AutoMap curve node renders, pins and navigates as: its body IS
/// the plain curve's body, run on one picked bus signal, so every place that
/// dispatches on a curve's module id asks this instead. The AutoMap Response
/// Curve is the Vec Response Curve while a Vec2 is picked, the Response Curve
/// otherwise. Any other node is itself.
pub(crate) fn curve_ui_module_id(node: &NodeData) -> &str {
    match node.module_id.as_str() {
        "module.automap_twoway_response_curve" => "module.twoway_response_curve",
        "module.automap_response_curve" if automap_curve_pin_is_vec(node) => "module.vec_response_curve",
        "module.automap_response_curve" => "module.response_curve",
        other => other,
    }
}

/// Resolves the (points, biases) param keys to operate on for a given node.
/// For two-way curves this respects `active_lane` so the user only touches
/// the lane they're currently editing — keeps the file format identical to
/// regular curves and avoids needing a two-lane file variant.
pub(crate) fn curve_param_keys(node: &NodeData) -> (&'static str, &'static str) {
    if curve_ui_module_id(node) == "module.twoway_response_curve" {
        let lane = node.params.get("active_lane").and_then(|v| v.as_str()).unwrap_or("up");
        match lane { "dn" => ("points_dn", "biases_dn"), "hy" => ("hyst_points", "hyst_biases"), _ => ("points", "biases") }
    } else {
        ("points", "biases")
    }
}

pub(crate) fn curve_header_save(node_id: NodeId, snarl: &Snarl<NodeData>) {
    let Some(n) = snarl.get_node(node_id) else { return };
    let (pts_key, bias_key) = curve_param_keys(n);
    let pts: Vec<[f64; 2]> = n.params.get(pts_key)
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|p| {
            let a = p.as_array()?;
            Some([a.get(0)?.as_f64()?, a.get(1)?.as_f64()?])
        }).collect())
        .unwrap_or_default();
    let bss: Vec<f64> = n.params.get(bias_key)
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|b| b.as_f64()).collect())
        .unwrap_or_default();
    let cf = CurveFile {
        points:           pts,
        biases:           bss,
        absolute:         n.params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true),
        in_min:           n.params.get("in_min") .and_then(|v| v.as_f64()).unwrap_or(-1.0),
        in_max:           n.params.get("in_max") .and_then(|v| v.as_f64()).unwrap_or( 1.0),
        out_min:          n.params.get("out_min").and_then(|v| v.as_f64()).unwrap_or(-1.0),
        out_max:          n.params.get("out_max").and_then(|v| v.as_f64()).unwrap_or( 1.0),
        grid_x:           n.params.get("grid_x") .and_then(|v| v.as_i64()).unwrap_or(4),
        grid_y:           n.params.get("grid_y") .and_then(|v| v.as_i64()).unwrap_or(4),
        snap:             n.params.get("snap")   .and_then(|v| v.as_bool()).unwrap_or(false),
        scale_t:          n.params.get("scale_t").and_then(|v| v.as_f64()).unwrap_or(0.0),
        trail_ms:         n.params.get("trail_ms").and_then(|v| v.as_i64()).unwrap_or(300),
        show_scaled_grid: n.params.get("show_scaled_grid").and_then(|v| v.as_bool()).unwrap_or(false),
        show_grid_labels: n.params.get("show_grid_labels").and_then(|v| v.as_bool()).unwrap_or(false),
        twoway:           twoway_preset_section(n),
    };
    if let Some(path) = crate::overlay::with_overlay_not_topmost(|| {
        rfd::FileDialog::new()
            .add_filter("FlexInput Curve", &["fxc"])
            .set_file_name("curve.fxc")
            .save_file()
    }) {
        if let Ok(json) = serde_json::to_string_pretty(&cf) {
            let _ = std::fs::write(path, json);
        }
    }
}

pub(crate) fn curve_header_load(node_id: NodeId, is_float: bool, snarl: &mut Snarl<NodeData>) {
    let Some(path) = crate::overlay::with_overlay_not_topmost(|| {
        rfd::FileDialog::new()
            .add_filter("FlexInput Curve", &["fxc"])
            .pick_file()
    }) else { return };
    let Ok(json) = std::fs::read_to_string(path) else { return };
    let Ok(cf)   = serde_json::from_str::<CurveFile>(&json) else { return };
    let Some(node) = snarl.get_node_mut(node_id) else { return };
    let (pts_key, bias_key) = curve_param_keys(node);
    let pts_json: Vec<Value> = cf.points.iter()
        .map(|p| serde_json::json!([p[0], p[1]]))
        .collect();
    let bss_json: Vec<Value> = cf.biases.iter()
        .filter_map(|&b| Number::from_f64(b).map(Value::Number))
        .collect();
    node.params.insert(pts_key.into(),  Value::Array(pts_json));
    node.params.insert(bias_key.into(), Value::Array(bss_json));
    node.params.insert("grid_x".into(),  serde_json::json!(cf.grid_x));
    node.params.insert("grid_y".into(),  serde_json::json!(cf.grid_y));
    node.params.insert("snap".into(),    Value::Bool(cf.snap));
    if let Some(n) = Number::from_f64(cf.scale_t) {
        node.params.insert("scale_t".into(), Value::Number(n));
    }
    node.params.insert("trail_ms".into(), serde_json::json!(cf.trail_ms));
    node.params.insert("show_scaled_grid".into(), Value::Bool(cf.show_scaled_grid));
    node.params.insert("show_grid_labels".into(), Value::Bool(cf.show_grid_labels));
    if is_float {
        node.params.insert("absolute".into(), Value::Bool(cf.absolute));
        if let Some(n) = Number::from_f64(cf.in_min)  { node.params.insert("in_min".into(),  Value::Number(n)); }
        if let Some(n) = Number::from_f64(cf.in_max)  { node.params.insert("in_max".into(),  Value::Number(n)); }
        if let Some(n) = Number::from_f64(cf.out_min) { node.params.insert("out_min".into(), Value::Number(n)); }
        if let Some(n) = Number::from_f64(cf.out_max) { node.params.insert("out_max".into(), Value::Number(n)); }
    } else {
        if let Some(n) = Number::from_f64(cf.in_max)  { node.params.insert("in_max".into(),  Value::Number(n)); }
        if let Some(n) = Number::from_f64(cf.out_max) { node.params.insert("out_max".into(), Value::Number(n)); }
    }
    // A two-way preset into a Two-way Curve: both lanes and every setting.
    if let Some(section) = cf.twoway.as_ref().filter(|_| twoway_preset_section(node).is_some()) {
        apply_twoway_preset(node, section);
    }
}

/// Graph-only load: replaces just the active lane's `points` + `biases`
/// from the chosen `.fxc` file. Range / grid / scale / trail / labels are
/// left untouched so the right-click menu (which is also available from
/// sub-patch layouts where module settings may not be visible) never
/// surprises the user by changing hidden state.
pub(crate) fn curve_graph_load(node_id: NodeId, snarl: &mut Snarl<NodeData>) {
    let Some(path) = crate::overlay::with_overlay_not_topmost(|| {
        rfd::FileDialog::new()
            .add_filter("FlexInput Curve", &["fxc"])
            .pick_file()
    }) else { return };
    let Ok(json) = std::fs::read_to_string(path) else { return };
    let Ok(cf)   = serde_json::from_str::<CurveFile>(&json) else { return };
    let Some(node) = snarl.get_node_mut(node_id) else { return };
    let (pts_key, bias_key) = curve_param_keys(node);
    let pts_json: Vec<Value> = cf.points.iter()
        .map(|p| serde_json::json!([p[0], p[1]]))
        .collect();
    let bss_json: Vec<Value> = cf.biases.iter()
        .filter_map(|&b| Number::from_f64(b).map(Value::Number))
        .collect();
    node.params.insert(pts_key.into(),  Value::Array(pts_json));
    node.params.insert(bias_key.into(), Value::Array(bss_json));
}

/// Graph-only reset: snaps just the active lane back to the default
/// `[(0,0), (1,1)]` identity curve. Like `curve_graph_load`, leaves other
/// module settings (range, grid, scale, etc.) untouched.
pub(crate) fn curve_graph_reset(node_id: NodeId, snarl: &mut Snarl<NodeData>) {
    let Some(node) = snarl.get_node_mut(node_id) else { return };
    let (pts_key, bias_key) = curve_param_keys(node);
    // A two-way curve's Hyst graph resets to one dot — the flat Hyst value.
    let (pts, bss) = if pts_key == "hyst_points" { (serde_json::json!([]), serde_json::json!([])) }
        else { (serde_json::json!([[0.0, 0.0], [1.0, 1.0]]), serde_json::json!([0.0])) };
    node.params.insert(pts_key.into(),  pts);
    node.params.insert(bias_key.into(), bss);
}

pub(crate) fn curve_header_reset(node_id: NodeId, is_float: bool, snarl: &mut Snarl<NodeData>) {
    let Some(node) = snarl.get_node_mut(node_id) else { return };
    // Two-way: reset only the currently-selected lane's points/biases. Other
    // settings (grid, scale, range, etc.) are shared across both lanes and
    // still reset together. For regular/vec curves `curve_param_keys` returns
    // `("points", "biases")` so the behaviour is unchanged.
    let (pts_key, bias_key) = curve_param_keys(node);
    // A two-way curve's Hyst graph resets to one dot — the flat Hyst value.
    let (pts, bss) = if pts_key == "hyst_points" { (serde_json::json!([]), serde_json::json!([])) }
        else { (serde_json::json!([[0.0, 0.0], [1.0, 1.0]]), serde_json::json!([0.0])) };
    node.params.insert(pts_key.into(),             pts);
    node.params.insert(bias_key.into(),            bss);
    node.params.insert("grid_x".into(),            serde_json::json!(4i64));
    node.params.insert("grid_y".into(),            serde_json::json!(4i64));
    node.params.insert("snap".into(),              Value::Bool(false));
    node.params.insert("scale_t".into(),           serde_json::json!(0.0f64));
    node.params.insert("trail_ms".into(),          serde_json::json!(300i64));
    node.params.insert("show_scaled_grid".into(),  Value::Bool(false));
    node.params.insert("show_grid_labels".into(),  Value::Bool(false));
    if is_float {
        node.params.insert("absolute".into(),  Value::Bool(true));
        node.params.insert("in_min".into(),    serde_json::json!(-1.0f64));
        node.params.insert("in_max".into(),    serde_json::json!( 1.0f64));
        node.params.insert("out_min".into(),   serde_json::json!(-1.0f64));
        node.params.insert("out_max".into(),   serde_json::json!( 1.0f64));
    } else {
        node.params.insert("in_max".into(),    serde_json::json!(1.0f64));
        node.params.insert("out_max".into(),   serde_json::json!(1.0f64));
    }
}

/// Maps x ∈ [0,1] → [0,1] continuously. t=0 → linear; t<0 → log-like; t>0 → exp-like.
/// Power law p = 2^(t*3): at t=±1, p=8 or 1/8 — far more extreme than the old log/exp modes.
pub(crate) fn curve_scale(x: f32, t: f32) -> f32 {
    if t.abs() < 1e-4 { return x; }
    x.clamp(0.0, 1.0).powf(2.0f32.powf(t * 3.0))
}

/// Inverse of curve_scale: given a scaled output y ∈ [0,1], find x such that curve_scale(x,t)=y.
/// Used to place grid lines at perceptually even intervals under the current scaling.
pub(crate) fn curve_scale_inv(y: f32, t: f32) -> f32 {
    if t.abs() < 1e-4 { return y; }
    let p = 2.0f32.powf(t * 3.0);
    y.clamp(0.0, 1.0).powf(1.0 / p)
}

// ── Response curve right-click menu ──────────────────────────────────────────
//
// Save/Load/Reset call into the existing `curve_header_*` helpers, which use
// the canonical `.fxc` file format (`CurveFile` struct) and — via
// `curve_param_keys` — operate on the currently-selected lane for two-way
// curves. Copy/Paste live in egui memory only (no file format involved).

pub(crate) const CURVE_CLIP_KEY: &str = "fxi_curve_clipboard";

#[derive(Clone, Debug)]
pub(crate) struct CurveClip {
    pub(crate) points: Vec<[f32; 2]>,
    pub(crate) biases: Vec<f32>,
}

/// Snapshot the active (points, biases) pair from a node, using the same
/// per-lane resolution as save/load/reset (so two-way Copy grabs the lane
/// the user is currently editing).
pub(crate) fn curve_clipboard_copy_from(node: &NodeData) -> CurveClip {
    let (pts_key, bias_key) = curve_param_keys(node);
    let points: Vec<[f32; 2]> = node.params.get(pts_key)
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|p| {
            let a = p.as_array()?;
            Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
        }).collect())
        .unwrap_or_else(|| vec![[0.0, 0.0], [1.0, 1.0]]);
    let biases: Vec<f32> = node.params.get(bias_key)
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|b| b.as_f64().map(|f| f as f32)).collect())
        .unwrap_or_default();
    CurveClip { points, biases }
}

/// Write a clipboard payload into the node's active lane (or the only lane
/// for regular/vec curves). Biases are resized to `points.len() - 1` so the
/// sampler invariant is maintained.
pub(crate) fn curve_clipboard_paste_into(node: &mut NodeData, mut clip: CurveClip) {
    let (pts_key, bias_key) = curve_param_keys(node);
    let need = clip.points.len().saturating_sub(1);
    clip.biases.resize(need, 0.0);
    let pts: Vec<Value> = clip.points.iter()
        .map(|p| serde_json::json!([p[0], p[1]])).collect();
    let bss: Vec<Value> = clip.biases.iter()
        .filter_map(|&b| Number::from_f64(b as f64).map(Value::Number))
        .collect();
    node.params.insert(pts_key.into(), Value::Array(pts));
    node.params.insert(bias_key.into(), Value::Array(bss));
}

pub(crate) fn curve_clipboard_get(ctx: &egui::Context) -> Option<CurveClip> {
    ctx.data(|d| d.get_temp::<CurveClip>(egui::Id::new(CURVE_CLIP_KEY)))
}

pub(crate) fn curve_clipboard_set(ctx: &egui::Context, data: CurveClip) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(CURVE_CLIP_KEY), data));
}

/// Shared right-click menu emitted by every response-curve widget (body and
/// pinned variants). Reset and Load only touch the curve's points/biases —
/// not range, grid, scale, or other module settings — so invoking the menu
/// from a sub-patch layout (where module sliders may be hidden) never alters
/// surrounding state the patch author tuned. Save still writes the full
/// `.fxc` so files remain interchangeable with the header Save button.
/// `lane_label` is "Up" or "Down" for two-way curves; `None` for regular/vec.
pub(crate) fn curve_context_menu(
    ui: &mut egui::Ui,
    node_id: NodeId,
    snarl: &mut Snarl<NodeData>,
    lane_label: Option<&str>,
) -> bool {
    let mut mutated = false;
    let prefix = lane_label.map(|p| format!("{p} curve: ")).unwrap_or_default();

    if ui.button(format!("{prefix}Reset")).on_hover_text("Reset only the curve (range / grid / scale stay as-is)").clicked() {
        curve_graph_reset(node_id, snarl);
        mutated = true;
        ui.close();
    }
    ui.separator();
    if ui.button(format!("{prefix}Copy")).clicked() {
        if let Some(node) = snarl.get_node(node_id) {
            let clip = curve_clipboard_copy_from(node);
            curve_clipboard_set(ui.ctx(), clip);
        }
        ui.close();
    }
    let has_clip = curve_clipboard_get(ui.ctx()).is_some();
    if ui.add_enabled(has_clip, egui::Button::new(format!("{prefix}Paste"))).clicked() {
        if let Some(clip) = curve_clipboard_get(ui.ctx()) {
            if let Some(node) = snarl.get_node_mut(node_id) {
                curve_clipboard_paste_into(node, clip);
                mutated = true;
            }
        }
        ui.close();
    }
    ui.separator();
    if ui.button(format!("{prefix}Save…")).clicked() {
        curve_header_save(node_id, snarl);
        ui.close();
    }
    if ui.button(format!("{prefix}Load…")).on_hover_text("Load only the curve from .fxc (range / grid / scale stay as-is)").clicked() {
        curve_graph_load(node_id, snarl);
        mutated = true;
        ui.close();
    }
    mutated
}

// ── Signal helpers ────────────────────────────────────────────────────────────

pub(crate) fn sig_f32(s: &Signal) -> f32 {
    match s {
        Signal::Float(f) => *f,
        Signal::Bool(b)  => if *b { 1.0 } else { 0.0 },
        Signal::Int(i)   => *i as f32,
        Signal::Vec2(v)  => v.length(),
        Signal::Vec4(v)  => v.length(),
    }
}

#[cfg(test)]
mod refinement_tests {
    use super::*;

    // Linked In/Out: the changed end scales its counterpart by the same ratio,
    // from either side, and from zero it moves by as much.
    #[test]
    fn linked_ranges_scale_together() {
        let mut v = [-1.0, 2.0, -1.0, 1.0];
        follow_range_link([-1.0, 1.0, -1.0, 1.0], &mut v);
        assert_eq!(v, [-1.0, 2.0, -1.0, 2.0]);
        let mut v = [0.0, 500.0, 0.0, 1500.0];
        follow_range_link([0.0, 500.0, 0.0, 1000.0], &mut v);
        assert_eq!(v, [0.0, 750.0, 0.0, 1500.0]);
        let mut v = [0.5, 1.0, 0.25, 1.0];
        follow_range_link([0.0, 1.0, 0.25, 1.0], &mut v);
        assert_eq!(v[2], 0.75, "from zero: moved by as much");
    }

    // The 1:1 line: the corner diagonal on a square window, at an angle when
    // a motion view shows In and Out over different spans.
    #[test]
    fn identity_line_follows_the_view() {
        assert_eq!(identity_span(0.0, 1.0, 0.0, 1.0), Some((0.0, 1.0)));
        assert_eq!(identity_span(-1.0, 1.0, -1.0, 1.0), Some((-1.0, 1.0)));
        // In shows 500 °/s, Out 1000 °/s, of a 2000 °/s sensor.
        assert_eq!(identity_span(0.0, 0.25, 0.0, 0.5), Some((0.0, 0.25)));
    }

    // Snap takes the 1:1 line's crossings with the grid when they are nearer
    // than any intersection — and only the grid where there is no line.
    #[test]
    fn snap_reaches_the_identity_line() {
        let nodes = [0.0, 0.5, 1.0];
        // Window x 0..0.5, y 0..1: the vertical line x=0.25 crosses 1:1 at (0.25, 0.25).
        let win = (0.0, 0.5, 0.0, 1.0);
        let grid = (0.25, 0.5);
        assert_eq!(snap_point(0.25, 0.27, grid, &nodes, &nodes, win, true), (0.25, 0.25));
        assert_eq!(snap_point(0.25, 0.27, grid, &nodes, &nodes, win, false), grid);
        assert_eq!(snap_point(0.24, 0.48, grid, &nodes, &nodes, win, true), grid, "the intersection is nearer");
    }

    // A dot's value keeps 3 significant figures, however small.
    #[test]
    fn dot_values_keep_three_significant_figures() {
        assert_eq!(value_label(500.0, "°/s"), "500°/s");
        assert_eq!(value_label(12.34, "°/s"), "12.3°/s");
        assert_eq!(value_label(2.468, " G"), "2.47 G");
        assert_eq!(value_label(0.02468, " G"), "0.0247 G");
        assert_eq!(value_label(-0.5, "%"), "-0.500%");
        assert_eq!(value_label(0.0, "%"), "0%");
    }

    // The pad's d-pad steps a dot to the next grid line or 1:1 crossing.
    #[test]
    fn gamepad_snap_steps_to_the_next_target() {
        let snap = NavCurveSnap { xs: vec![0.0, 0.25, 0.5, 0.75, 1.0], ys: vec![0.0, 0.5, 1.0], line: true };
        assert_eq!(snap.next(true, 0.3, 0.9, 1.0), Some(0.5));
        assert_eq!(snap.next(true, 0.3, 0.4, 1.0), Some(0.4), "the 1:1 crossing at y = 0.4 comes first");
        assert_eq!(snap.next(false, 0.6, 0.3, -1.0), Some(0.5), "the nearer grid line first");
        assert_eq!(snap.next(false, 0.45, 0.3, -1.0), Some(0.3), "then down onto the 1:1 line at x = 0.3");
        assert_eq!(snap.next(true, 1.0, 0.0, 1.0), None);
        let grid_only = NavCurveSnap { line: false, ..snap };
        assert_eq!(grid_only.next(true, 0.3, 0.4, 1.0), Some(0.5));
    }

    #[test]
    fn log_exp_text_names_the_power() {
        assert_eq!(log_exp_text(0.0), "Linear");
        assert_eq!(log_exp_text(1.0 / 3.0), "Exp 0.33  ·  x^2.00");
        assert_eq!(log_exp_text(-1.0 / 3.0), "Log 0.33  ·  x^0.50");
    }

    // A 3 s trail at 120 fps keeps no more samples than the cap, newest last.
    #[test]
    fn long_trails_are_sample_capped() {
        let dur = std::time::Duration::from_millis(3000);
        let t0 = std::time::Instant::now();
        let mut trail = std::collections::VecDeque::new();
        for i in 0..360u32 {
            let now = t0 + std::time::Duration::from_micros(i as u64 * 8_333);
            push_trail_sample(&mut trail, i as f32, now, dur);
        }
        assert!(trail.len() <= 122, "{}", trail.len());
        assert_eq!(trail.back().unwrap().0, 359.0, "the head is always the newest sample");
    }
}
