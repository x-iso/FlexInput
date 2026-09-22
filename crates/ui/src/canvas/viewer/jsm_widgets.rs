//! The controls the JSM editor grows around itself: a slider per numeric setting,
//! and the sensitivity curve drawn live.
//!
//! Both are **views of the text**. A slider rewrites the number on its own line, so
//! the config stays the single source of truth and a slider is only a nicer way to
//! type; the curve is drawn from the compiled config, which is why the fork's
//! telemetry socket is not needed here — its GUI is a separate process and has to
//! ask over a port, while this one is the same process and can simply look.
//!
//! Every widget registers itself as pinnable, so a tuning session can live in the
//! config overlay with the editor left behind on the canvas.

use super::*;
use crate::canvas::node::PinGraphOverride;
use flexinput_engine::eval::JsmKnob;

/// A pinned JSM widget's colours, or the module's own where a field is `None`.
///
/// This is the same `PinGraphOverride` the Response Curve and the scopes use, so
/// the layout inspector's existing strip drives it with no new UI: `background`,
/// `outline`, `gridline`, then channel 1 = the sensitivity line (and a fader's
/// fill), channel 2 = the output line.
pub(crate) type JsmPaint<'a> = Option<&'a PinGraphOverride>;

fn col(v: Option<[u8; 4]>) -> Option<Color32> {
    v.map(|c| Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
}

/// Channel `n` (0-based) of the override, if set.
fn channel(paint: JsmPaint<'_>, n: usize) -> Option<Color32> {
    col(paint.and_then(|p| p.channel_colors.get(n).copied().flatten()))
}

/// Paint the pin's background and frame behind a widget, where they were set.
fn backdrop(painter: &egui::Painter, rect: egui::Rect, paint: JsmPaint<'_>, fallback: Color32) {
    painter.rect_filled(rect, 3.0, col(paint.and_then(|p| p.background)).unwrap_or(fallback));
    let px = paint.and_then(|p| p.outline_px).unwrap_or(0.0);
    if px > 0.0 {
        if let Some(c) = col(paint.and_then(|p| p.outline)) {
            painter.rect_stroke(rect, 3.0, egui::Stroke::new(px, c), egui::StrokeKind::Inside);
        }
    }
}

/// Reserve the shape slots a body's background and frame will fill.
///
/// A node body's rect isn't known until it has been laid out, and the plate has
/// to sit *under* what it contains — so the slots are claimed first, in painting
/// order, and set afterwards. `None` when there is nothing to paint, so an
/// un-styled body adds no shapes at all.
pub(crate) fn reserve_backdrop(
    ui: &mut egui::Ui,
    paint: JsmPaint<'_>,
) -> Option<(egui::layers::ShapeIdx, egui::layers::ShapeIdx)> {
    let wanted = paint.is_some_and(|p| {
        p.background.is_some() || (p.outline_px.unwrap_or(0.0) > 0.0 && p.outline.is_some())
    });
    wanted.then(|| {
        let p = ui.painter();
        (p.add(egui::Shape::Noop), p.add(egui::Shape::Noop))
    })
}

/// Fill in what [`reserve_backdrop`] claimed, now that the body has a size.
pub(crate) fn fill_backdrop(
    ui: &mut egui::Ui,
    slots: Option<(egui::layers::ShapeIdx, egui::layers::ShapeIdx)>,
    paint: JsmPaint<'_>,
) {
    let Some((bg_at, outline_at)) = slots else { return };
    // A little air around the content, so the frame reads as a frame rather than
    // as something touching the text.
    let rect = ui.min_rect().expand(3.0);
    let painter = ui.painter();
    if let Some(bg) = col(paint.and_then(|p| p.background)) {
        painter.set(bg_at, egui::Shape::rect_filled(rect, 4.0, bg));
    }
    let px = paint.and_then(|p| p.outline_px).unwrap_or(0.0);
    if let (true, Some(c)) = (px > 0.0, col(paint.and_then(|p| p.outline))) {
        painter.set(
            outline_at,
            egui::Shape::rect_stroke(rect, 4.0, egui::Stroke::new(px, c), egui::StrokeKind::Inside),
        );
    }
}

/// Point the text editor's own frame at the pin's colours.
///
/// The editor is a `TextEdit`, and it fills and frames itself from the style —
/// on top of whatever was painted behind it. So a background laid under the body
/// never showed through where it mattered most. `extreme_bg_color` is the fill a
/// code editor uses; the three widget strokes are its border, set together so the
/// frame doesn't change colour just because the pointer crossed it.
pub(crate) fn style_text_editor(ui: &mut egui::Ui, paint: JsmPaint<'_>) {
    if let Some(bg) = col(paint.and_then(|p| p.background)) {
        ui.visuals_mut().extreme_bg_color = bg;
    }
    let px = paint.and_then(|p| p.outline_px).unwrap_or(0.0);
    if let (true, Some(c)) = (px > 0.0, col(paint.and_then(|p| p.outline))) {
        let stroke = egui::Stroke::new(px, c);
        let w = &mut ui.visuals_mut().widgets;
        w.inactive.bg_stroke = stroke;
        w.hovered.bg_stroke = stroke;
        w.active.bg_stroke = stroke;
    }
}

/// The fader's colours from the same override: channel 1 tints the fill.
fn knob_paint(paint: JsmPaint<'_>) -> super::simple_bodies::KnobPaint {
    super::simple_bodies::KnobPaint {
        track: col(paint.and_then(|p| p.gridline)),
        accent: channel(paint, 0),
        handle: channel(paint, 1),
    }
}

/// Height of one slider's track row, under its label.
const FADER_H: f32 = 16.0;
/// How tall the curve preview is drawn.
const GRAPH_H: f32 = 110.0;
/// How much of the body the diagnostics may take before they start scrolling.
const NOTES_MAX_H: f32 = 96.0;

/// One setting's slider: its name and value above, the fader below, full width.
///
/// Returns the new value while it is being dragged or scrolled.
pub(crate) fn fader(
    ui: &mut egui::Ui,
    width: f32,
    knob: &JsmKnob,
    paint: JsmPaint<'_>,
) -> Option<f32> {
    let mut out = None;
    // The value is laid out first and keeps its full width; the NAME is what
    // gives way, with an ellipsis. A trimmed number is useless — it can read as a
    // different number — whereas a trimmed name is still recognisable, and the
    // tooltip carries it in full either way.
    caption_row(ui, width, &knob.name, &shown_value(knob));

    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, FADER_H),
        egui::Sense::click_and_drag(),
    );
    let t = knob.t();
    if resp.dragged() {
        // Drag anywhere on the track: the handle follows the pointer, which is what
        // a fader this short wants — chasing a 5px handle is no fun.
        if let Some(p) = resp.interact_pointer_pos() {
            let nt = ((p.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
            out = Some(knob.at(nt));
        }
    }
    // Deliberately no wheel adjustment here: the strip these sit in scrolls, and
    // on the canvas the wheel pans the Scene. A fader that quietly changed a
    // sensitivity while you were scrolling past it would be a nasty surprise.
    // Pinned on its own there is nothing to fight, so `pinned_fader` keeps it.
    let active = resp.hovered() || resp.dragged();
    // The same fader the Knob module draws, laid on its side. Bipolar when the
    // setting can go negative, so the centre line means something.
    super::simple_bodies::draw_knob_h_fader_styled(
        &ui.painter_at(rect),
        rect,
        t,
        knob.lo < 0.0,
        active,
        knob_paint(paint),
    );
    if active {
        resp.on_hover_text(format!(
            "{} — {} to {}{}",
            knob.name,
            trim(knob.lo),
            trim(knob.hi),
            if knob.value < knob.lo || knob.value > knob.hi {
                "\n(the config's value is outside the slider's range, so the handle sits at the end)"
            } else {
                ""
            }
        ));
    }
    out
}

/// The same setting pinned on its own, filling the container it was given.
///
/// It takes the Knob module's shape: wide and it is a horizontal fader, tall and
/// it is a vertical one, square and it is a rotary — so a tuning panel can be
/// laid out however it reads best, and a pin dragged narrow stops being a
/// stretched bar. Drags are relative, as the Knob's are.
///
/// Returns the new value while it is being dragged or scrolled.
pub(crate) fn pinned_fader(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    knob: &JsmKnob,
    paint: JsmPaint<'_>,
) -> Option<f32> {
    let avail = egui::vec2(size.x.max(24.0), size.y.max(18.0));
    let (rect, resp) = ui.allocate_exact_size(avail, egui::Sense::click_and_drag());

    // Which shape this is decides where the text goes. A wide fader has a label
    // row across its top, as it does in the strip. A vertical fader or a rotary
    // has no width to spare for a side-by-side row, so the name sits above the
    // control and the value below it — and both are allowed to spill past the
    // widget's own bounds rather than be clipped, because a pin narrowed until
    // its caption no longer fits should still tell you what it is.
    let wide = avail.x / avail.y.max(1.0) >= 2.0;
    let label_h = if wide && avail.y >= 40.0 { (avail.y * 0.22).clamp(12.0, 26.0) } else { 0.0 };
    let track = egui::Rect::from_min_max(rect.min + egui::vec2(0.0, label_h), rect.max);
    let aspect = track.width() / track.height().max(1.0);

    let mut out = None;
    let t = knob.t();
    if resp.dragged() {
        let d = resp.drag_delta();
        // Along whichever axis the control runs, a drag across it is the range.
        let step = if aspect >= 2.0 {
            d.x / track.width().max(1.0)
        } else {
            -d.y / track.height().max(1.0)
        };
        if step != 0.0 {
            out = Some(knob.at(t + step));
        }
    } else if resp.hovered() {
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll != 0.0 {
            out = Some(knob.at(t + scroll * 0.002));
        }
    }

    let painter = ui.painter_at(rect);
    let active = resp.hovered() || resp.dragged();
    let bipolar = knob.lo < 0.0;
    // A pinned widget can be given a background and a frame of its own; on the
    // canvas the node body already provides both, so there it stays transparent.
    if paint.is_some_and(|p| p.background.is_some() || p.outline_px.unwrap_or(0.0) > 0.0) {
        backdrop(&painter, rect, paint, Color32::TRANSPARENT);
    }
    let kp = knob_paint(paint);
    if aspect >= 2.0 {
        super::simple_bodies::draw_knob_h_fader_styled(&painter, track, t, bipolar, active, kp);
    } else if aspect <= 0.5 {
        super::simple_bodies::draw_knob_v_fader_styled(&painter, track, t, bipolar, active, kp);
    } else {
        super::simple_bodies::draw_knob_rotary_styled(&painter, track, t, bipolar, active, kp);
    }

    // The text grows with the pin, so a widget scaled up on a config overlay is
    // readable from wherever the overlay is being used.
    //
    // The clip rect is widened deliberately. A pinned widget is clipped to its
    // own container, so a caption drawn above a knob — or one wider than a narrow
    // pin — was clipped away ENTIRELY, leaving a knob with no label at all. Rather
    // than shrink the control to make room, let the text overhang: a caption you
    // can read next to a knob you can grab beats both of them being too small.
    //
    // `set_clip_rect` REPLACES, where `with_clip_rect` intersects — and
    // intersecting with the container is exactly what we are trying to escape, so
    // the obvious-looking builder call here is silently a no-op.
    let mut text = ui.painter().clone();
    text.set_clip_rect(rect.expand2(egui::vec2(rect.width(), rect.height() * 0.6 + 14.0)));
    let vis = ui.visuals();
    if label_h > 0.0 {
        let font = egui::FontId::proportional((label_h * 0.62).clamp(8.0, 20.0));
        text.text(
            egui::pos2(rect.left() + 2.0, rect.top() + label_h * 0.5),
            egui::Align2::LEFT_CENTER,
            &knob.name,
            font.clone(),
            vis.weak_text_color(),
        );
        text.text(
            egui::pos2(rect.right() - 2.0, rect.top() + label_h * 0.5),
            egui::Align2::RIGHT_CENTER,
            shown_value(knob),
            font,
            vis.strong_text_color(),
        );
    } else if !wide {
        let font = egui::FontId::proportional((avail.y * 0.13).clamp(8.0, 18.0));
        text.text(
            egui::pos2(rect.center().x, rect.top() - 1.0),
            egui::Align2::CENTER_BOTTOM,
            &knob.name,
            font.clone(),
            vis.weak_text_color(),
        );
        text.text(
            egui::pos2(rect.center().x, rect.bottom() + 1.0),
            egui::Align2::CENTER_TOP,
            shown_value(knob),
            font,
            vis.strong_text_color(),
        );
    }
    if active {
        resp.on_hover_text(format!(
            "{} = {} — {} to {}",
            knob.name,
            shown_value(knob),
            trim(knob.lo),
            trim(knob.hi)
        ));
    }
    out
}

/// Height of the graph's Log/Exp row.
pub(crate) const WARP_H: f32 = 15.0;
/// How far the axis can be stretched either way: the exponent runs 1/5 .. 5.
const WARP_RANGE: f32 = 5.0;

/// The graph's speed axis is drawn with an exponent; this is the slider's
/// position for it, -1 (Log) through 0 (linear) to +1 (Exp).
pub(crate) fn warp_to_t(warp: f32) -> f32 {
    (warp.max(1e-3).ln() / WARP_RANGE.ln()).clamp(-1.0, 1.0)
}

pub(crate) fn t_to_warp(t: f32) -> f32 {
    WARP_RANGE.powf(t.clamp(-1.0, 1.0))
}

/// The Log ⟷ Exp control under the curve.
///
/// A gyro deadzone is a couple of degrees per second wide and the noise floor a
/// resting pad sits at is smaller still; against a 500°/s axis both are inside
/// the first pixel. Pulling this to Log stretches the slow end until they are
/// something you can actually look at. Returns the new exponent while it moves.
pub(crate) fn warp_slider(
    ui: &mut egui::Ui,
    width: f32,
    warp: f32,
    paint: JsmPaint<'_>,
) -> Option<f32> {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(width, WARP_H), egui::Sense::click_and_drag());
    let small = egui::FontId::proportional(9.0);
    let vis = ui.visuals();
    let painter = ui.painter_at(rect);
    // End labels, with the track between them — one row, because this is a view
    // control and must not cost the curve the height it needs to be read.
    let ends = 22.0_f32.min(width * 0.2);
    painter.text(
        egui::pos2(rect.left(), rect.center().y),
        egui::Align2::LEFT_CENTER,
        "Log",
        small.clone(),
        vis.weak_text_color(),
    );
    painter.text(
        egui::pos2(rect.right(), rect.center().y),
        egui::Align2::RIGHT_CENTER,
        "Exp",
        small,
        vis.weak_text_color(),
    );
    let track = egui::Rect::from_min_max(
        egui::pos2(rect.left() + ends, rect.top()),
        egui::pos2(rect.right() - ends, rect.bottom()),
    );

    let mut out = None;
    let t = warp_to_t(warp);
    if resp.double_clicked() {
        // Back to a plain linear axis, which is where you want to end up after
        // looking at the small end.
        out = Some(1.0);
    } else if resp.dragged() {
        if let Some(p) = resp.interact_pointer_pos() {
            let nt = ((p.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
            out = Some(t_to_warp(nt * 2.0 - 1.0));
        }
    }
    let active = resp.hovered() || resp.dragged();
    super::simple_bodies::draw_knob_h_fader_styled(
        &painter,
        track,
        (t + 1.0) * 0.5,
        true,
        active,
        knob_paint(paint),
    );
    if active {
        resp.on_hover_text(
            "Stretch the speed axis. Log opens up the slow end — where a gyro \
             deadzone and a resting pad's noise floor live, both of them inside \
             the first pixel of a linear 500°/s axis. Double-click for linear.",
        );
    }
    out
}

/// A setting's name on the left and its value on the right, in one row of the
/// given width — the value complete, the name truncated if it has to be.
///
/// The width a fader strip gets can be narrow (beside the editor, on a small
/// node), and something has to give. A trimmed number can read as a *different*
/// number, so it never gives; a trimmed name is still recognisable.
fn caption_row(ui: &mut egui::Ui, width: f32, name: &str, value: &str) {
    let font = egui::TextStyle::Small.resolve(ui.style());
    let (weak, strong) = (ui.visuals().weak_text_color(), ui.visuals().text_color());
    let val = ui.ctx().fonts_mut(|f| {
        f.layout_no_wrap(value.to_string(), egui::FontId::monospace(font.size), strong)
    });
    let gap = 6.0;
    let name_w = (width - val.size().x - gap).max(8.0);
    let mut job = egui::text::LayoutJob::simple_singleline(name.to_string(), font, weak);
    job.wrap.max_width = name_w;
    job.wrap.max_rows = 1;
    job.wrap.overflow_character = Some('…');
    let nm = ui.ctx().fonts_mut(|f| f.layout_job(job));

    let h = nm.size().y.max(val.size().y);
    let (row, _) = ui.allocate_exact_size(egui::vec2(width, h), egui::Sense::hover());
    let painter = ui.painter();
    painter.galley(
        egui::pos2(row.left(), row.center().y - nm.size().y * 0.5),
        nm,
        weak,
    );
    painter.galley(
        egui::pos2(row.right() - val.size().x, row.center().y - val.size().y * 0.5),
        val,
        strong,
    );
}

/// A setting's value as the config would spell it.
fn shown_value(knob: &JsmKnob) -> String {
    if knob.integral {
        format!("{}", knob.value.round() as i64)
    } else {
        format!("{:.2}", knob.value)
    }
}

fn trim(v: f32) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Colour of the sensitivity curve, and of the output curve drawn against it.
const SENS_COLOR: Color32 = Color32::from_rgb(96, 200, 128);
const OUTPUT_COLOR: Color32 = Color32::from_rgb(91, 199, 215);

/// The sensitivity curve the config describes, drawn the way the custom-curve
/// fork's GUI draws it: turn speed across, sensitivity up, both axes anchored at
/// zero with a labelled grid, so two configs can be compared by eye and a number
/// can be read off.
///
/// Two curves, because sensitivity alone doesn't tell you how the aim feels:
///
/// * solid — the sensitivity the config gives at that turn speed;
/// * dashed — the resulting camera speed (turn speed × sensitivity), normalised
///   to the same axis. A flat sensitivity makes this a straight ramp. Where it
///   sags or kinks, turning the pad faster moves the camera *less*, which is the
///   fault worth catching and the reason the fork draws it.
///
/// Live dots mark where the pad is right now, and hovering reads off any speed.
pub(crate) fn curve_graph(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    points: &[flexinput_engine::eval::JsmCurvePoint],
    live_dps: Option<f32>,
    paint: JsmPaint<'_>,
    warp: f32,
) -> egui::Rect {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let vis = ui.visuals();
    let sens_color = channel(paint, 0).unwrap_or(SENS_COLOR);
    let output_color = channel(paint, 1).unwrap_or(OUTPUT_COLOR);
    backdrop(&painter, rect, paint, vis.extreme_bg_color);

    let small = egui::FontId::proportional(9.0);
    if points.len() < 2 {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "set a gyro sensitivity to preview its curve",
            small,
            vis.weak_text_color(),
        );
        return rect;
    }

    // Both axes start at zero, as the fork's do — a curve read against a floating
    // band tells you the shape but never the numbers, and the numbers are what a
    // sensitivity is.
    let axis_x = points.last().map(|p| p.dps).unwrap_or(1.0).max(1e-3);
    let axis_y = points.iter().fold(0.0f32, |a, p| a.max(p.sens)).max(2.0);

    // The graph is drawn at anything from a node body's 110px to a full pinned
    // panel, so the axis labels appear only where they would fit — cramped, they
    // would cover the curve they are there to explain.
    let labelled = width >= 200.0 && height >= 80.0;
    let (pad_l, pad_b, pad_tr) = if labelled { (30.0, 13.0, 6.0) } else { (3.0, 3.0, 3.0) };
    let plot = egui::Rect::from_min_max(
        rect.min + egui::vec2(pad_l, pad_tr),
        rect.max - egui::vec2(pad_tr, pad_b),
    );
    if plot.width() < 8.0 || plot.height() < 8.0 {
        return rect;
    }
    // The speed axis can be stretched at either end: below 1 the slow end opens
    // up, which is the only way to see a deadzone a couple of degrees wide or the
    // noise floor a resting pad sits at, against a 500°/s axis.
    let warp = warp.clamp(0.05, 20.0);
    let at_speed = |dps: f32| (dps / axis_x).clamp(0.0, 1.0).powf(warp);
    let speed_at = |p: f32| axis_x * p.clamp(0.0, 1.0).powf(1.0 / warp);
    let to_pos = |dps: f32, sens: f32| {
        egui::pos2(
            plot.left() + at_speed(dps) * plot.width(),
            plot.bottom() - (sens / axis_y).clamp(0.0, 1.0) * plot.height(),
        )
    };

    // ── grid ─────────────────────────────────────────────────────────────────
    let grid = egui::Stroke::new(
        1.0,
        col(paint.and_then(|p| p.gridline))
            .unwrap_or_else(|| vis.weak_text_color().gamma_multiply(0.28)),
    );
    for i in 0..=6 {
        let y = plot.bottom() - plot.height() * i as f32 / 6.0;
        painter.line_segment([egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)], grid);
        if labelled {
            painter.text(
                egui::pos2(plot.left() - 4.0, y),
                egui::Align2::RIGHT_CENTER,
                format!("{:.2}", axis_y * i as f32 / 6.0),
                small.clone(),
                vis.weak_text_color(),
            );
        }
    }
    for i in 0..=10 {
        let x = plot.left() + plot.width() * i as f32 / 10.0;
        painter.line_segment([egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())], grid);
        // Every other one: ten numbers along a node-width axis is a smear.
        if labelled && i % 2 == 0 {
            // The gridlines are evenly spaced on SCREEN, so their values are
            // whatever the warp puts there — read them off, don't assume.
            let v = speed_at(i as f32 / 10.0);
            painter.text(
                egui::pos2(x, plot.bottom() + 2.0),
                egui::Align2::CENTER_TOP,
                if v < 10.0 { format!("{v:.1}") } else { format!("{}", v.round() as i64) },
                small.clone(),
                vis.weak_text_color(),
            );
        }
    }

    // ── the two curves ───────────────────────────────────────────────────────
    let max_out = points.iter().fold(1e-6f32, |a, p| a.max(p.dps * p.sens));
    let output_at = |p: &flexinput_engine::eval::JsmCurvePoint| p.dps * p.sens / max_out * axis_y;
    let out_line: Vec<egui::Pos2> = points.iter().map(|p| to_pos(p.dps, output_at(p))).collect();
    painter.extend(egui::Shape::dashed_line(
        &out_line,
        egui::Stroke::new(1.3, output_color),
        5.0,
        5.0,
    ));
    let line: Vec<egui::Pos2> = points.iter().map(|p| to_pos(p.dps, p.sens)).collect();
    painter.add(egui::Shape::line(line, egui::Stroke::new(1.8, sens_color)));

    // ── where the pad is right now ───────────────────────────────────────────
    // The dots are the point of drawing this live rather than as a static
    // picture: you turn the pad and watch which part of the curve you actually
    // spend your time on.
    let sample_at = |dps: f32| {
        points
            .iter()
            .min_by(|a, b| (a.dps - dps).abs().total_cmp(&(b.dps - dps).abs()))
            .copied()
    };
    // No lower gate: a resting pad's noise floor is exactly what a stretched slow
    // end is for, and hiding it under 0.5°/s would defeat the zoom.
    if let Some(dps) = live_dps {
        if let Some(p) = sample_at(dps) {
            let x = dps.min(axis_x);
            painter.line_segment(
                [egui::pos2(to_pos(x, 0.0).x, plot.top()), egui::pos2(to_pos(x, 0.0).x, plot.bottom())],
                egui::Stroke::new(1.0, Color32::from_white_alpha(36)),
            );
            painter.circle_filled(to_pos(x, p.sens), 3.5, sens_color);
            painter.circle_filled(to_pos(x, output_at(&p)), 3.0, output_color);
        }
    }

    // ── read off any speed ───────────────────────────────────────────────────
    if let Some(pos) = resp.hover_pos().filter(|p| plot.contains(*p)) {
        let dps = speed_at((pos.x - plot.left()) / plot.width());
        if let Some(p) = sample_at(dps) {
            painter.line_segment(
                [egui::pos2(pos.x, plot.top()), egui::pos2(pos.x, plot.bottom())],
                egui::Stroke::new(1.0, vis.weak_text_color()),
            );
            painter.circle_filled(to_pos(p.dps, p.sens), 2.5, sens_color);
            let text = format!("{:.0}°/s → {}", p.dps, trim((p.sens * 100.0).round() / 100.0));
            // Flip the label to whichever side of the line has room for it.
            let (anchor, dx) = if pos.x > plot.center().x {
                (egui::Align2::RIGHT_TOP, -4.0)
            } else {
                (egui::Align2::LEFT_TOP, 4.0)
            };
            painter.text(
                egui::pos2(pos.x + dx, plot.top() + 1.0),
                anchor,
                text,
                small.clone(),
                vis.strong_text_color(),
            );
        }
    }

    // ── what the axes are ────────────────────────────────────────────────────
    if labelled {
        painter.text(
            egui::pos2(plot.center().x, rect.bottom() - 1.0),
            egui::Align2::CENTER_BOTTOM,
            "turn speed °/s",
            small.clone(),
            vis.weak_text_color(),
        );
    } else {
        painter.text(
            plot.right_bottom() + egui::vec2(-1.0, -1.0),
            egui::Align2::RIGHT_BOTTOM,
            format!("{}°/s", axis_x.round() as i64),
            small.clone(),
            vis.weak_text_color(),
        );
        painter.text(
            plot.left_top() + egui::vec2(1.0, 1.0),
            egui::Align2::LEFT_TOP,
            format!("sens {}", trim((axis_y * 100.0).round() / 100.0)),
            small,
            vis.weak_text_color(),
        );
    }
    rect
}

/// How fast the pad feeding this node is turning, in degrees per second, or `None`
/// when it reports no gyro.
///
/// This is the magnitude of the two axes JSM's defaults aim with — pitch and yaw.
/// A config that remaps `MOUSE_X_FROM_GYRO_AXIS` is aiming with something else, so
/// the marker is then indicative rather than exact; the curve itself is unaffected.
pub(crate) fn live_turn_speed(
    live: &std::collections::HashMap<(String, String), Signal>,
    dev: &str,
) -> Option<f32> {
    if dev.is_empty() {
        return None;
    }
    let f = |pin: &str| {
        live.get(&(dev.to_string(), pin.to_string()))
            .map(|s| s.as_float())
    };
    let (pitch, yaw) = (f("gyro_y"), f("gyro_z"));
    match (pitch, yaw) {
        (None, None) => None,
        (p, y) => {
            // The bus carries a rate as a fraction of this many degrees per second
            // (`flexinput_devices::gyro::GYRO_REF_DPS`).
            const REF_DPS: f32 = 2000.0;
            let (p, y) = (p.unwrap_or(0.0), y.unwrap_or(0.0));
            Some((p * p + y * y).sqrt() * REF_DPS)
        }
    }
}

/// The diagnostics, scrolling once they outgrow the room they are given.
///
/// The wheel has to be lifted off the canvas by hand or the Scene pans instead —
/// the same thing the editor needs, for the same reason (`canvas/wheel.rs`).
pub(crate) fn scrolling_notes(
    ui: &mut egui::Ui,
    node_id: NodeId,
    width: f32,
    max_height: f32,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let claimant = ui.id().with(("jsm_notes", node_id.0));
    crate::canvas::wheel::scrolling_body(
        ui,
        claimant,
        egui::ScrollArea::vertical()
            .max_height(max_height.min(NOTES_MAX_H))
            .max_width(width)
            .auto_shrink([false, true]),
        |ui| {
            ui.set_max_width(width);
            add_contents(ui);
        },
    );
}

/// The fader strip, scrolling within the height a pinned container gives it.
///
/// Separate claimant from the diagnostics above it, or the two would fight over
/// the same lifted wheel delta and one of them would never scroll.
pub(crate) fn scrolling_faders<R>(
    ui: &mut egui::Ui,
    node_id: NodeId,
    width: f32,
    max_height: f32,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let claimant = ui.id().with(("jsm_faders", node_id.0));
    crate::canvas::wheel::scrolling_body(
        ui,
        claimant,
        egui::ScrollArea::vertical()
            .id_salt(("jsm_faders", node_id.0))
            .max_height(max_height)
            .max_width(width)
            .auto_shrink([false, true]),
        |ui| {
            ui.set_max_width(width);
            add_contents(ui)
        },
    )
    .inner
}

/// Room a slider takes, label included — the body needs it to budget its height.
pub(crate) fn fader_height(ui: &egui::Ui) -> f32 {
    ui.text_style_height(&egui::TextStyle::Small) + FADER_H + ui.spacing().item_spacing.y
}

pub(crate) fn graph_height() -> f32 {
    GRAPH_H
}

/// The pinnable element id for one setting's slider, and the name back out of it.
///
/// One pair rather than a `format!` in one place and a `&k[5..]` in another: the
/// slice was off by one waiting to happen, and a wrong name here shows an empty
/// pin rather than an error.
pub(crate) fn knob_element_id(name: &str) -> String {
    format!("knob:{name}")
}

pub(crate) fn knob_name_of(element: &str) -> Option<&str> {
    element.strip_prefix("knob:").filter(|n| !n.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{knob_element_id, knob_name_of};

    // The slider's position and the axis exponent have to round-trip, or the
    // handle would drift a little every time the graph was redrawn.
    #[test]
    fn the_axis_slider_round_trips_its_exponent() {
        use super::{t_to_warp, warp_to_t};
        for t in [-1.0f32, -0.5, 0.0, 0.37, 1.0] {
            let back = warp_to_t(t_to_warp(t));
            assert!((back - t).abs() < 1e-4, "t {t} came back {back}");
        }
        assert!((t_to_warp(0.0) - 1.0).abs() < 1e-6, "the middle is a linear axis");
        assert!(t_to_warp(-1.0) < 1.0, "Log stretches the slow end");
        assert!(t_to_warp(1.0) > 1.0, "Exp stretches the fast end");
        // Out-of-range stored values clamp rather than flinging the handle off.
        assert_eq!(warp_to_t(1000.0), 1.0);
        assert_eq!(warp_to_t(0.0), -1.0);
    }

    #[test]
    fn a_sliders_pin_id_round_trips_its_setting_name() {
        for name in ["GYRO_SENS", "ACCEL_SIGMOID_WIDTH", "LEFT_STICK_UNDEADZONE_INNER"] {
            let id = knob_element_id(name);
            assert_eq!(knob_name_of(&id), Some(name), "{name}");
        }
        // Anything that is not a slider id is not mistaken for one.
        assert_eq!(knob_name_of("editor"), None);
        assert_eq!(knob_name_of("curve"), None);
        assert_eq!(knob_name_of("knob:"), None, "an empty name is not a setting");
    }
}

// ── the command list ─────────────────────────────────────────────────────────

/// Per-node state for the "Commands…" list: whether it is open, what has been
/// typed to filter it, and which row is highlighted.
#[derive(Clone, Default)]
pub(crate) struct CommandList {
    pub open: bool,
    pub filter: String,
    pub index: usize,
    /// Rows the pad has asked to move, not yet applied.
    ///
    /// The pad says "one down" rather than an index because the list is the
    /// only place that knows what the filter left in it, or where a group
    /// begins — a second copy of that in the nav driver would be two answers to
    /// the same question, and they would drift.
    pub step: i32,
    /// Groups the pad has asked to jump. 150 names is a long walk otherwise.
    pub group_step: i32,
    /// The pad has chosen the highlighted row.
    pub confirm: bool,
    /// Scroll the highlighted row into view: set whenever the pad moves it,
    /// since a pad has no pointer and cannot scroll the list itself.
    pub follow: bool,
    /// Show the binding vocabulary — every key, mouse button, pad output and
    /// JSM action — rather than what fits where the cursor is.
    ///
    /// West asks "what can go here"; North asks "show me the keys". Two
    /// questions worth asking separately, because the cursor is often on the
    /// name of a line whose VALUE is the part you came to write.
    pub values: bool,
}

/// Room the command list needs: the filter row, the rows themselves, and the
/// explanation under them.
pub(crate) const LIST_H: f32 = 210.0;

fn list_id(node_id: NodeId) -> egui::Id {
    egui::Id::new(("jsm_cmd_list", node_id.0))
}

pub(crate) fn command_list_state(ctx: &egui::Context, node_id: NodeId) -> CommandList {
    ctx.data(|d| d.get_temp::<CommandList>(list_id(node_id))).unwrap_or_default()
}

pub(crate) fn set_command_list_state(ctx: &egui::Context, node_id: NodeId, st: CommandList) {
    ctx.data_mut(|d| d.insert_temp(list_id(node_id), st));
}

/// The first row of the group `step` groups away from the one `index` sits in.
///
/// Walking 150 names a row at a time is not navigation, and the list is already
/// grouped for reading — so the triggers jump by heading.
fn jump_group(rows: &[&flexinput_engine::eval::JsmItem], index: usize, step: i32) -> usize {
    let mut starts: Vec<usize> = Vec::new();
    let mut group = "";
    for (i, r) in rows.iter().enumerate() {
        if r.group != group {
            group = r.group;
            starts.push(i);
        }
    }
    if starts.is_empty() {
        return index;
    }
    // Which group the highlight is in now, then that many headings along. From
    // the middle of a group, "back" is the previous heading rather than the top
    // of this one: one press, one heading, whichever way you are going.
    let here = starts.iter().rposition(|&s| s <= index).unwrap_or(0);
    let want = (here as i64 + step as i64).clamp(0, starts.len() as i64 - 1) as usize;
    starts[want]
}

/// Rows matching the filter, in catalogue order.
///
/// Matching is on the name only and is case-insensitive and substring-based:
/// people look for "GYRO" or "stick", and a fuzzy match would put surprising
/// things at the top of a list whose whole job is to be predictable.
pub(crate) fn filtered<'a>(
    items: &'a [flexinput_engine::eval::JsmItem],
    filter: &str,
) -> Vec<&'a flexinput_engine::eval::JsmItem> {
    let needle = filter.trim().to_ascii_uppercase();
    items
        .iter()
        .filter(|i| needle.is_empty() || i.name.to_ascii_uppercase().contains(&needle))
        .collect()
}

/// What picking a row types into the config.
///
/// A setting comes with its `=` because a setting without one is an error, and
/// the next thing you want is to type the value. A command and a button stand on
/// their own — a button is the left side of a binding, so it gets the `=` too.
pub(crate) fn insertion_for(name: &str, kind: flexinput_engine::eval::JsmKind) -> String {
    use flexinput_engine::eval::JsmKind as K;
    match kind {
        K::Setting | K::Trigger => format!("{name} = "),
        // A command is a whole line. A binding is only ever the right-hand side
        // of one, so on its own it lands as an error saying so — which is the
        // truth about a key written where a name belongs, and only reachable by
        // clicking a row in the pad's own key list with the mouse.
        K::Command | K::Binding => name.to_string(),
    }
}

/// The command list itself, drawn under the editor while it is open.
///
/// Returns the chosen row's name and kind — the caller decides where it lands,
/// because that depends on whether a pad with a cursor asked for it or a mouse
/// with none did.
///
/// `kinds` is what can legally stand where the insertion will go. An empty
/// slice means nothing can: the list says where values come from instead of
/// offering names that would be an error there.
///
/// Every row says what the module actually does with that name — live, not yet,
/// or ignored with the reason — because a list that offered all 131 settings as
/// equals would be quietly promising things this module doesn't run.
pub(crate) fn command_list(
    ui: &mut egui::Ui,
    node_id: NodeId,
    width: f32,
    pad_pins: &std::collections::HashSet<String>,
    budget: Option<f32>,
    kinds: &[flexinput_engine::eval::JsmKind],
) -> Option<(String, flexinput_engine::eval::JsmKind)> {
    use flexinput_engine::eval::JsmSupportState as S;
    let mut st = command_list_state(ui.ctx(), node_id);
    if !st.open {
        return None;
    }
    // Two vocabularies, never mixed: the names that start a line, and the
    // values that finish one. `kinds` says which of them can stand where this
    // insertion will land.
    let mut items: Vec<_> = flexinput_engine::eval::jsm_catalogue(pad_pins)
        .into_iter()
        .filter(|i| kinds.contains(&i.kind))
        .collect();
    if kinds.contains(&flexinput_engine::eval::JsmKind::Binding) {
        items.extend(flexinput_engine::eval::jsm_bindings());
    }
    let mut picked = None;
    let mut close = false;

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Commands").small().strong());
        let resp = ui.add(
            egui::TextEdit::singleline(&mut st.filter)
                .desired_width((width - 90.0).max(60.0))
                .font(egui::TextStyle::Small)
                .hint_text("filter"),
        );
        // Typing narrows the list, so the highlight has to come back to the top
        // or it would point at a row that scrolled out from under it.
        if resp.changed() {
            st.index = 0;
        }
        if ui.small_button("✕").on_hover_text("Close the list").clicked() {
            close = true;
        }
    });

    // Pinned, the container's height is all there is and the list has to live
    // inside what the editor gave up for it; on the canvas the body grows.
    let rows_h = match budget {
        Some(h) => (h - 56.0).max(48.0),
        None => 160.0,
    };
    let rows = filtered(&items, &st.filter);
    if rows.is_empty() {
        // Two different nothings, and telling them apart is the difference
        // between a list that looks broken and one that teaches the other
        // half of the controls.
        let why = if kinds.is_empty() {
            "A value goes here, and the list only knows names — hold South and push the stick for a number, or North for a key or button."
        } else {
            "nothing by that name"
        };
        ui.label(egui::RichText::new(why).small().weak());
    }
    st.index = st.index.min(rows.len().saturating_sub(1));
    // What the pad asked for, resolved against the list the pad can actually
    // see. Anything it moves, it also wants scrolled into view.
    if st.step != 0 || st.group_step != 0 {
        st.follow = true;
        if !rows.is_empty() {
            if st.step != 0 {
                let n = rows.len() as i64;
                st.index = (st.index as i64 + st.step as i64).clamp(0, n - 1) as usize;
            }
            if st.group_step != 0 {
                st.index = jump_group(&rows, st.index, st.group_step);
            }
        }
        st.step = 0;
        st.group_step = 0;
    }
    if st.confirm {
        st.confirm = false;
        if let Some(r) = rows.get(st.index) {
            picked = Some((r.name.clone(), r.kind));
        }
    }

    let claimant = ui.id().with(("jsm_cmds", node_id.0));
    crate::canvas::wheel::scrolling_body(
        ui,
        claimant,
        egui::ScrollArea::vertical()
            .id_salt(("jsm_cmds", node_id.0))
            .max_height(rows_h)
            .max_width(width)
            .auto_shrink([false, true]),
        |ui| {
            ui.set_max_width(width);
            let mut group = "";
            for (i, item) in rows.iter().enumerate() {
                if item.group != group {
                    group = item.group;
                    ui.label(egui::RichText::new(group).small().weak().italics());
                }
                let (tint, why) = match item.state {
                    S::Live => (ui.visuals().text_color(), None),
                    S::Pending(w) => (Color32::from_rgb(214, 168, 74), Some(w)),
                    S::Ignored(w) => (ui.visuals().weak_text_color(), Some(w)),
                };
                let label = egui::RichText::new(&item.name).small().monospace().color(tint);
                let resp = ui.selectable_label(i == st.index, label);
                // A pad has no pointer, so the list has to bring the highlight
                // to it — walking off the bottom of a box you can't scroll is
                // the same as the highlight vanishing.
                if i == st.index && st.follow {
                    resp.scroll_to_me(None);
                }
                // JSM's own words for what it does, plus our reason when we
                // don't run it. Both, because "not live yet" without knowing
                // what the setting IS tells you nothing.
                let tip = match (item.help, why) {
                    (Some(h), Some(w)) => Some(format!("{h}

{w}")),
                    (Some(h), None) => Some(h.to_string()),
                    (None, Some(w)) => Some(w.to_string()),
                    (None, None) => None,
                };
                if let Some(t) = tip {
                    resp.clone().on_hover_text(t);
                }
                if resp.clicked() {
                    st.index = i;
                    picked = Some((item.name.clone(), item.kind));
                }
            }
        },
    );

    if let Some(row) = rows.get(st.index) {
        // The highlighted row explained under the list, so it is readable
        // without hovering — and so a pad, which has no pointer to hover with,
        // gets the same explanation when it walks the list.
        if let Some(h) = row.help {
            ui.label(egui::RichText::new(h).small());
        }
        if let S::Pending(w) | S::Ignored(w) = row.state {
            ui.label(egui::RichText::new(w).small().weak().italics());
        }
    }
    st.follow = false;
    if picked.is_some() || close {
        st.open = false;
    }
    set_command_list_state(ui.ctx(), node_id, st);
    picked
}

#[cfg(test)]
mod command_list_tests {
    use super::{filtered, insertion_for, jump_group};
    use flexinput_engine::eval::{jsm_catalogue, JsmKind};
    use std::collections::HashSet;

    /// The triggers jump by heading. 150 names a row at a time is not
    /// navigation, and the list is already grouped for reading.
    #[test]
    fn the_triggers_jump_by_heading_rather_than_by_row() {
        let items = jsm_catalogue(&HashSet::new());
        let rows = filtered(&items, "");
        let mut starts = Vec::new();
        let mut group = "";
        for (i, r) in rows.iter().enumerate() {
            if r.group != group {
                group = r.group;
                starts.push(i);
            }
        }
        assert!(starts.len() > 3, "the list is grouped to begin with");
        assert_eq!(jump_group(&rows, 0, 1), starts[1]);
        assert_eq!(jump_group(&rows, starts[1], -1), starts[0]);
        // From inside a group, one press is one heading whichever way you go —
        // not "back to the top of this one" for half of them.
        let mid = starts[1] + 1;
        assert!(mid < starts[2], "a group with more than one row in it");
        assert_eq!(jump_group(&rows, mid, 1), starts[2]);
        assert_eq!(jump_group(&rows, mid, -1), starts[0]);
        // The ends clamp rather than wrap: landing back at the top with no
        // sign you had reached the bottom is how a list loses you.
        assert_eq!(jump_group(&rows, 0, -1), starts[0]);
        let last = *starts.last().unwrap();
        assert_eq!(jump_group(&rows, last, 1), last);
        // An empty list has no headings and no opinion.
        assert_eq!(jump_group(&[], 0, 1), 0);
    }

    #[test]
    fn the_filter_finds_names_by_any_part_of_them() {
        let items = jsm_catalogue(&HashSet::new());
        let names = |f: &str| -> Vec<String> {
            filtered(&items, f).iter().map(|i| i.name.clone()).collect()
        };
        // Case-insensitive substring, because people type "gyro" not "GYRO_".
        let gyro = names("gyro");
        assert!(gyro.contains(&"GYRO_SENS".to_string()));
        assert!(gyro.contains(&"MIN_GYRO_SENS".to_string()), "matches in the middle too");
        assert!(!gyro.contains(&"STICK_POWER".to_string()));
        assert_eq!(names("GYRO"), gyro, "case makes no difference");

        // An empty filter is the whole catalogue, not nothing.
        assert_eq!(filtered(&items, "").len(), items.len());
        assert_eq!(filtered(&items, "   ").len(), items.len(), "whitespace is not a filter");
        assert!(names("zzzznope").is_empty());
    }

    #[test]
    fn a_picked_row_is_inserted_as_a_line_that_parses() {
        let items = jsm_catalogue(&HashSet::new());
        let find = |n: &str| items.iter().find(|i| i.name == n).expect("listed");

        // A setting and a button both want their `=`: without one, the line the
        // list just wrote would be an error the moment it landed.
        let insert = |n: &str| insertion_for(n, find(n).kind);
        assert_eq!(insert("GYRO_SENS"), "GYRO_SENS = ");
        assert_eq!(find("S").kind, JsmKind::Trigger);
        assert_eq!(insert("S"), "S = ");
        // A command stands alone, and an `=` would make it one.
        assert_eq!(insert("RESET_MAPPINGS"), "RESET_MAPPINGS");

        // The whole point: what the list writes is a line the parser accepts.
        for name in ["RESET_MAPPINGS", "ONE_EURO_FILTER"] {
            let line = insert(name);
            let cfg = flexinput_engine::eval::jsm_compile(&line, &[]);
            assert!(
                !matches!(cfg.lines[0].status, flexinput_engine::eval::JsmLineStatus::Error(_)),
                "`{line}` should not land as an error: {:?}",
                cfg.lines[0].status
            );
        }
    }
}
