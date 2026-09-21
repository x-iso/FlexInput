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
use flexinput_engine::eval::JsmKnob;

/// Height of one slider's track row, under its label.
const FADER_H: f32 = 16.0;
/// How tall the curve preview is drawn.
const GRAPH_H: f32 = 110.0;
/// How much of the body the diagnostics may take before they start scrolling.
const NOTES_MAX_H: f32 = 96.0;

/// One setting's slider: its name and value above, the fader below, full width.
///
/// Returns the new value while it is being dragged or scrolled.
pub(crate) fn fader(ui: &mut egui::Ui, width: f32, knob: &JsmKnob) -> Option<f32> {
    let mut out = None;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(&knob.name).small().weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let shown = if knob.integral {
                format!("{}", knob.value.round() as i64)
            } else {
                format!("{:.2}", knob.value)
            };
            ui.label(egui::RichText::new(shown).small().monospace());
        });
    });

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
    } else if resp.hovered() {
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll != 0.0 {
            out = Some(knob.at(t + scroll * 0.002));
        }
    }
    let active = resp.hovered() || resp.dragged();
    // The same fader the Knob module draws, laid on its side. Bipolar when the
    // setting can go negative, so the centre line means something.
    super::simple_bodies::draw_knob_h_fader(
        &ui.painter_at(rect),
        rect,
        t,
        knob.lo < 0.0,
        active,
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

fn trim(v: f32) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// The sensitivity curve the config describes: turn speed across, sensitivity up,
/// with a marker showing how fast the pad is turning right now.
pub(crate) fn curve_graph(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    points: &[flexinput_engine::eval::JsmCurvePoint],
    live_dps: Option<f32>,
) -> egui::Rect {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let vis = ui.visuals();
    painter.rect_filled(rect, 3.0, vis.extreme_bg_color);

    if points.len() < 2 {
        return rect;
    }
    let max_dps = points.last().map(|p| p.dps).unwrap_or(1.0).max(1e-3);
    let hi = points.iter().fold(0.0f32, |a, p| a.max(p.sens));
    let lo = points.iter().fold(f32::MAX, |a, p| a.min(p.sens));
    // A flat curve (both sensitivities equal, or both zero) still needs a band to
    // draw in, or everything lands on one line and reads as broken.
    let (lo, hi) = if hi - lo < 1e-4 { (lo - 0.5, hi + 0.5) } else { (lo, hi) };
    let pad = 4.0;
    let plot = rect.shrink(pad);
    let to_pos = |dps: f32, sens: f32| {
        egui::pos2(
            plot.left() + (dps / max_dps).clamp(0.0, 1.0) * plot.width(),
            plot.bottom() - ((sens - lo) / (hi - lo)).clamp(0.0, 1.0) * plot.height(),
        )
    };

    // A line at each end of the sensitivity range, so the curve has something to be
    // read against.
    for s in [lo, hi] {
        let y = to_pos(0.0, s).y;
        painter.line_segment(
            [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
            egui::Stroke::new(1.0, vis.weak_text_color().gamma_multiply(0.35)),
        );
    }

    let line: Vec<egui::Pos2> = points.iter().map(|p| to_pos(p.dps, p.sens)).collect();
    painter.add(egui::Shape::line(
        line,
        egui::Stroke::new(1.6, Color32::from_rgb(80, 200, 120)),
    ));

    // Where the pad is right now. The dot is the point of drawing this live rather
    // than as a static picture: you turn the pad and watch where on the curve you
    // actually spend your time.
    if let Some(dps) = live_dps {
        if dps > 0.5 {
            let at = points
                .iter()
                .min_by(|a, b| {
                    (a.dps - dps).abs().total_cmp(&(b.dps - dps).abs())
                })
                .copied();
            if let Some(p) = at {
                let pos = to_pos(dps.min(max_dps), p.sens);
                painter.circle_filled(pos, 3.5, Color32::WHITE);
                painter.line_segment(
                    [egui::pos2(pos.x, plot.top()), egui::pos2(pos.x, plot.bottom())],
                    egui::Stroke::new(1.0, Color32::from_white_alpha(40)),
                );
            }
        }
    }

    // Corner labels: what the axes mean, without an axis apparatus that would not
    // fit in a node body.
    let small = egui::FontId::proportional(9.0);
    painter.text(
        plot.left_bottom() + egui::vec2(1.0, -1.0),
        egui::Align2::LEFT_BOTTOM,
        "0",
        small.clone(),
        vis.weak_text_color(),
    );
    painter.text(
        plot.right_bottom() + egui::vec2(-1.0, -1.0),
        egui::Align2::RIGHT_BOTTOM,
        format!("{}°/s", max_dps.round() as i64),
        small.clone(),
        vis.weak_text_color(),
    );
    painter.text(
        plot.left_top() + egui::vec2(1.0, 1.0),
        egui::Align2::LEFT_TOP,
        format!("sens {}", trim((hi * 100.0).round() / 100.0)),
        small,
        vis.weak_text_color(),
    );
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
