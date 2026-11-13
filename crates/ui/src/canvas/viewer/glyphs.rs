//! Small vector marks for switches whose meaning is a shape rather than a word: a magnet for
//! snapping to a grid, a plateau for holding at the sustain level, a turnaround for bouncing, a
//! ring for looping. They are drawn, not typed: the fonts a build happens to carry have no
//! reliable magnet, and a word at the size a compact row allows is what holds a module wide.
//!
//! Every mark lives in a square at the centre of whatever it is handed, sized off the shorter
//! side, so it reads the same at one block tall as at three. The word stays where it belongs -
//! in the tooltip, next to what the switch actually does.

/// A switch mark. Named for what the shape stands for, not for what it looks like.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Glyph {
    /// A horseshoe magnet: dragged points settle on what is near them.
    Magnet,
    /// Up, flat, along: run to the sustain level and wait there.
    Hold,
    /// Out one way and back the other: forward while held, reverse on release.
    Bounce,
    /// A ring with an arrowhead on it: repeat the segment.
    Loop,
}

/// How thick a mark's strokes are, whatever size it is drawn at. Thin enough not to fill a
/// 20-point square, thick enough to survive the canvas being zoomed out.
const STROKE: f32 = 1.6;

/// Draw `glyph` in the middle of `rect`. `colour` is the mark itself; the square it sits in is
/// the button's, already painted by egui.
pub(crate) fn paint(painter: &egui::Painter, rect: egui::Rect, glyph: Glyph, colour: egui::Color32) {
    // Leave a little air at the edges so the mark doesn't touch the button's frame, and never
    // grow past a size where the strokes start reading as a blob.
    let side = rect.height().min(rect.width()).min(20.0) - 2.0;
    if side < 6.0 {
        // Too little room for anything to be recognisable: draw nothing rather than a smudge.
        return;
    }
    let c = rect.center();
    let r = side * 0.5;
    let stroke = egui::Stroke::new(STROKE, colour);
    match glyph {
        Glyph::Magnet => {
            // The horseshoe: legs down the sides joined over the top, with the poles marked by a
            // heavier tick at each foot - the part that does the pulling.
            let legs = r * 0.62;
            let top = c.y - r * 0.18;
            let foot = c.y + r * 0.82;
            painter.add(egui::Shape::line(
                ring_points(egui::pos2(c.x, top), legs, std::f32::consts::PI, 0.0),
                stroke,
            ));
            painter.line_segment([egui::pos2(c.x - legs, top), egui::pos2(c.x - legs, foot)], stroke);
            painter.line_segment([egui::pos2(c.x + legs, top), egui::pos2(c.x + legs, foot)], stroke);
            let pole = egui::Stroke::new(STROKE + 0.9, colour);
            painter.line_segment([egui::pos2(c.x - legs - 1.6, foot), egui::pos2(c.x - legs + 1.6, foot)], pole);
            painter.line_segment([egui::pos2(c.x + legs - 1.6, foot), egui::pos2(c.x + legs + 1.6, foot)], pole);
        }
        Glyph::Hold => {
            // Attack, the flat top the level waits on, release: the shape the curve makes when
            // this switch is on, which is why it needs no word beside it.
            let (left, right) = (c.x - r, c.x + r);
            let (bottom, plateau) = (c.y + r * 0.6, c.y - r * 0.5);
            painter.add(egui::Shape::line(
                vec![
                    egui::pos2(left, bottom),
                    egui::pos2(left + 0.22 * side, plateau),
                    egui::pos2(right - 0.34 * side, plateau),
                    egui::pos2(right, bottom),
                ],
                stroke,
            ));
        }
        Glyph::Bounce => {
            // There and back: one arrow each way on two lines. Reads as ping-pong at any size and
            // can't be mistaken for the loop ring.
            let (left, right) = (c.x - r * 0.78, c.x + r * 0.78);
            let out = c.y - r * 0.32;
            let back = c.y + r * 0.42;
            painter.line_segment([egui::pos2(left, out), egui::pos2(right, out)], stroke);
            head(painter, egui::pos2(right, out), egui::vec2(-1.0, 0.0), r * 0.4, stroke);
            painter.line_segment([egui::pos2(right, back), egui::pos2(left, back)], stroke);
            head(painter, egui::pos2(left, back), egui::vec2(1.0, 0.0), r * 0.4, stroke);
        }
        Glyph::Loop => {
            // Almost a ring, with the gap where the head is so it reads as going round again
            // rather than as a circle sitting there.
            let rad = r * 0.72;
            let gap = 0.6;
            let from = -std::f32::consts::FRAC_PI_2 + gap;
            let to = -std::f32::consts::FRAC_PI_2 + 2.0 * std::f32::consts::PI - gap;
            let ring = ring_points(c, rad, from, to);
            painter.add(egui::Shape::line(ring.clone(), stroke));
            // The head sits at the end of the ring and points the way the ring is travelling,
            // which is what separates this mark from a circle.
            let tip = ring[ring.len() - 1];
            let step = tip - ring[ring.len() - 2];
            let dir = if step.length_sq() > 1.0e-6 { step.normalized() } else { egui::vec2(1.0, 0.0) };
            head(painter, tip, dir, rad * 0.5, stroke);
        }
    }
}

/// Two strokes making a head at `tip`, opening back along `dir` (the direction the line is
/// travelling). A filled head would need a polygon shape; at these sizes a chevron reads the same.
fn head(painter: &egui::Painter, tip: egui::Pos2, dir: egui::Vec2, len: f32, stroke: egui::Stroke) {
    let back = -dir;
    let perp = egui::vec2(-back.y, back.x);
    for s in [-1.0f32, 1.0] {
        painter.line_segment([tip, tip + back * len + perp * len * 0.72 * s], stroke);
    }
}

/// Points along an arc, from `out_angle` to `in_angle`, as a polyline: no dependency on whether
/// the egui build in use has an arc shape, and at these radii the facets don't show.
fn ring_points(center: egui::Pos2, radius: f32, out_angle: f32, in_angle: f32) -> Vec<egui::Pos2> {
    let steps = 18;
    (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            let a = out_angle + (in_angle - out_angle) * t;
            center + egui::vec2(a.cos(), a.sin()) * radius
        })
        .collect()
}
