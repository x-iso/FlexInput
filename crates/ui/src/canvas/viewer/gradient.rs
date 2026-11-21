//! Gradient fills for layout shapes and SVG tints: the colour at a point of an object, the
//! picture a shape is painted with, the fill editor in the inspector, and the handles that
//! edit a gradient on the object itself.

use eframe::egui;

use crate::canvas::node::{Gradient, GradientKind, GradientStop};

impl Gradient {
    /// A gradient of `kind` from `from` to `to`, on its usual line (`default_line`).
    pub(crate) fn new(kind: GradientKind, from: [u8; 4], to: [u8; 4]) -> Self {
        let (start, end) = Self::default_line(kind);
        Self {
            kind,
            start,
            end,
            bias: 0.5,
            stops: vec![GradientStop { at: 0.0, color: from }, GradientStop { at: 1.0, color: to }],
        }
    }

    /// Where a new gradient's line runs, as shares of the box: across the middle, left to
    /// right; or for a radial one, from the centre out to the middle of the right edge.
    pub(crate) fn default_line(kind: GradientKind) -> ([f32; 2], [f32; 2]) {
        match kind {
            GradientKind::Linear => ([0.0, 0.5], [1.0, 0.5]),
            GradientKind::Radial => ([0.5, 0.5], [1.0, 0.5]),
        }
    }

    /// How far along the line point `p` of a box `size` big lies: 0 at the start, 1 at the
    /// end, held there past either. Across the line for a linear gradient, out from the start
    /// for a radial one - measured in the box's own units, so a circle stays round in a box
    /// that isn't square.
    pub(crate) fn travel(&self, p: egui::Vec2, size: egui::Vec2) -> f32 {
        let s = egui::vec2(self.start[0] * size.x, self.start[1] * size.y);
        let e = egui::vec2(self.end[0] * size.x, self.end[1] * size.y);
        let d = e - s;
        let len2 = d.length_sq().max(1e-6);
        let t = match self.kind {
            GradientKind::Linear => (p - s).dot(d) / len2,
            GradientKind::Radial => (p - s).length() / len2.sqrt(),
        };
        t.clamp(0.0, 1.0)
    }

    /// Where `t` along the line falls on the colours: the bias pulls their middle (0.5) to it.
    pub(crate) fn biased(&self, t: f32) -> f32 {
        let b = self.bias.clamp(0.02, 0.98);
        if (b - 0.5).abs() < 1e-4 {
            t
        } else {
            t.powf(0.5_f32.ln() / b.ln())
        }
    }

    /// The colour `u` of the way along the stops: between the two either side, blended with
    /// their alphas premultiplied, so a see-through stop fades the colour out rather than
    /// darkening it. Premultiplied, as egui keeps colours.
    pub(crate) fn color_at(&self, u: f32) -> egui::Color32 {
        let pm = |c: [u8; 4]| {
            let a = c[3] as f32 / 255.0;
            [c[0] as f32 * a, c[1] as f32 * a, c[2] as f32 * a, c[3] as f32]
        };
        let out = |c: [f32; 4]| {
            egui::Color32::from_rgba_premultiplied(
                c[0].round() as u8,
                c[1].round() as u8,
                c[2].round() as u8,
                c[3].round() as u8,
            )
        };
        let mut stops: Vec<&GradientStop> = self.stops.iter().collect();
        stops.sort_by(|a, b| a.at.total_cmp(&b.at));
        let (Some(first), Some(last)) = (stops.first(), stops.last()) else {
            return egui::Color32::TRANSPARENT;
        };
        if u <= first.at {
            return out(pm(first.color));
        }
        if u >= last.at {
            return out(pm(last.color));
        }
        for w in stops.windows(2) {
            let (a, b) = (w[0], w[1]);
            if u <= b.at {
                let f = ((u - a.at) / (b.at - a.at).max(1e-6)).clamp(0.0, 1.0);
                let (ca, cb) = (pm(a.color), pm(b.color));
                return out(std::array::from_fn(|i| ca[i] + (cb[i] - ca[i]) * f));
            }
        }
        out(pm(last.color))
    }

    /// The colour at point `p` of a box `size` big.
    pub(crate) fn sample(&self, p: egui::Vec2, size: egui::Vec2) -> egui::Color32 {
        self.color_at(self.biased(self.travel(p, size)))
    }

    /// A number that changes whenever the gradient does: what its pictures are cached under.
    pub(crate) fn cache_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (self.kind as u8).hash(&mut h);
        for v in [self.start[0], self.start[1], self.end[0], self.end[1], self.bias] {
            v.to_bits().hash(&mut h);
        }
        for s in &self.stops {
            s.at.to_bits().hash(&mut h);
            s.color.hash(&mut h);
        }
        h.finish()
    }

    /// Keep the stops in order along the line, as editing them can leave them out of it.
    pub(crate) fn sort_stops(&mut self) {
        self.stops.sort_by(|a, b| a.at.total_cmp(&b.at));
    }

    /// Another colour, in the widest gap between two, the colour already there.
    pub(crate) fn add_stop_in_widest_gap(&mut self) {
        self.sort_stops();
        let gap = self
            .stops
            .windows(2)
            .enumerate()
            .max_by(|a, b| (a.1[1].at - a.1[0].at).total_cmp(&(b.1[1].at - b.1[0].at)))
            .map(|(i, w)| (i, (w[0].at + w[1].at) * 0.5));
        if let Some((_, at)) = gap {
            self.add_stop_at(at);
        }
    }

    /// Another colour at `at` along the line, the colour the gradient has there now.
    pub(crate) fn add_stop_at(&mut self, at: f32) {
        let color = self.color_at(at).to_srgba_unmultiplied();
        self.stops.push(GradientStop { at: at.clamp(0.0, 1.0), color });
        self.sort_stops();
    }
}

/// The gradient as a picture `w` by `h` pixels covering an object `size` big: each pixel the
/// colour at its centre, measured in the object's units, so a radial gradient stays round
/// however the picture is squeezed to fit the cache's size.
pub(crate) fn gradient_image(g: &Gradient, w: usize, h: usize, size: egui::Vec2) -> egui::ColorImage {
    let (w, h) = (w.max(1), h.max(1));
    let pixels = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| {
            let p = egui::vec2((x as f32 + 0.5) / w as f32 * size.x, (y as f32 + 0.5) / h as f32 * size.y);
            g.sample(p, size)
        })
        .collect();
    egui::ColorImage { size: [w, h], pixels, source_size: egui::vec2(w as f32, h as f32) }
}

/// The picture of `g` across an object `size` points big, at `pixels_per_point`, from the
/// shared texture cache: at most 256 pixels a side, since a gradient has nothing finer in it
/// for the filtering to lose.
pub(crate) fn gradient_texture(ctx: &egui::Context, g: &Gradient, size: egui::Vec2, pixels_per_point: f32) -> egui::TextureHandle {
    let w = ((size.x * pixels_per_point).round() as usize).clamp(2, 256);
    let h = ((size.y * pixels_per_point).round() as usize).clamp(2, 256);
    let key = egui::Id::new(("gradient_tex", g.cache_key(), w, h, size.x.round() as i32, size.y.round() as i32));
    if let Some(tex) = crate::widgets::tex_cache::get(ctx, key) {
        return tex;
    }
    let tex = ctx.load_texture(format!("gradient-{w}x{h}"), gradient_image(g, w, h, size), egui::TextureOptions::LINEAR);
    crate::widgets::tex_cache::put(ctx, key, tex.clone());
    tex
}

/// Paint `shape` - a rect or a polygon, its own colour ignored - filled with `g` across the
/// object's box `rect`: the shape's mesh, edges smoothed as egui smooths them, wearing the
/// gradient's picture.
pub(crate) fn paint_gradient_fill(painter: &egui::Painter, shape: egui::Shape, rect: egui::Rect, g: &Gradient) {
    let size = egui::vec2(rect.width().max(1e-3), rect.height().max(1e-3));
    let ppp = painter.pixels_per_point();
    let tex = gradient_texture(painter.ctx(), g, size, ppp);
    let mut tess = egui::epaint::Tessellator::new(ppp, Default::default(), [1, 1], Vec::new());
    let mut mesh = egui::epaint::Mesh::default();
    tess.tessellate_shape(shape, &mut mesh);
    for v in &mut mesh.vertices {
        v.uv = egui::pos2((v.pos.x - rect.min.x) / size.x, (v.pos.y - rect.min.y) / size.y);
    }
    mesh.texture_id = tex.id();
    painter.add(egui::Shape::mesh(mesh));
}

/// The gradient a layout object is painted with, if it has one: a shape's fill, an SVG's tint.
pub(crate) fn item_gradient(it: &crate::canvas::node::LayoutItem) -> Option<&Gradient> {
    use crate::canvas::node::{LayoutDecoration, LayoutItem};
    match it {
        LayoutItem::Deco(LayoutDecoration::Rect { fill_gradient, .. })
        | LayoutItem::Deco(LayoutDecoration::Ellipse { fill_gradient, .. }) => fill_gradient.as_ref(),
        LayoutItem::Deco(LayoutDecoration::Svg { tint_gradient, .. }) => tint_gradient.as_ref(),
        _ => None,
    }
}

/// Put an edited gradient back on the object it came from (`item_gradient`).
pub(crate) fn set_item_gradient(it: &mut crate::canvas::node::LayoutItem, g: Gradient) {
    use crate::canvas::node::{LayoutDecoration, LayoutItem};
    match it {
        LayoutItem::Deco(LayoutDecoration::Rect { fill_gradient, .. })
        | LayoutItem::Deco(LayoutDecoration::Ellipse { fill_gradient, .. }) => *fill_gradient = Some(g),
        LayoutItem::Deco(LayoutDecoration::Svg { tint_gradient, .. }) => *tint_gradient = Some(g),
        _ => {}
    }
}



/// What a gradient's handles catch on while one is dragged: edges (Shift) or centres (Ctrl),
/// as a moved object does, within `reach` - the object's own box and the `others`.
pub(crate) struct HandleSnap<'a> {
    pub mode: super::ObjectSnap,
    pub reach: f32,
    pub others: &'a [egui::Rect],
}

/// The lines a dragged gradient handle can catch on: across (x) and down (y), from `rect` -
/// the gradient's own object - and `snap`'s others.
fn snap_lines(rect: egui::Rect, snap: &HandleSnap<'_>) -> (Vec<f32>, Vec<f32>) {
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for r in std::iter::once(&rect).chain(snap.others) {
        match snap.mode {
            super::ObjectSnap::Edges => {
                xs.extend([r.left(), r.right()]);
                ys.extend([r.top(), r.bottom()]);
            }
            super::ObjectSnap::Centres => {
                xs.push(r.center().x);
                ys.push(r.center().y);
            }
        }
    }
    (xs, ys)
}

/// The nearest of `lines` to `v` within `reach`.
fn nearest(lines: &[f32], v: f32, reach: f32) -> Option<f32> {
    lines.iter().copied().filter(|l| (l - v).abs() <= reach).min_by(|a, b| (a - v).abs().total_cmp(&(b - v).abs()))
}

/// Edit `g` on its object, at `rect`: drag the two ends of its line (a radial gradient's
/// centre and edge), the bias diamond along it, and the colours between; double-click the
/// line for another colour there, right-click one to take it off. With `snap`, a dragged
/// end catches on edges or centres the way a moved object does, and a point along the line
/// where the line crosses one; the line it caught on is drawn. Drawn and hit-tested as the
/// caller places it - after every object, so an object above this one never takes them.
/// Returns the gradient as edited, when it was, and whether the pointer is on a handle (a
/// press there is the handle's, not a click selecting what's under it).
pub(crate) fn gradient_handles(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    g: &Gradient,
    id: egui::Id,
    snap: Option<HandleSnap<'_>>,
) -> (Option<Gradient>, bool) {
    let size = egui::vec2(rect.width().max(1.0), rect.height().max(1.0));
    let on_screen = |p: [f32; 2]| rect.min + egui::vec2(p[0] * size.x, p[1] * size.y);
    let (s, e) = (on_screen(g.start), on_screen(g.end));
    let d = e - s;
    let len2 = d.length_sq().max(1e-6);
    let along = |p: egui::Pos2| (p - s).dot(d) / len2;
    let painter = ui.painter().clone();
    let dark = egui::Color32::from_black_alpha(170);
    painter.line_segment([s, e], egui::Stroke::new(3.0_f32, dark));
    painter.line_segment([s, e], egui::Stroke::new(1.0_f32, egui::Color32::WHITE));
    let mut out = g.clone();
    let mut changed = false;
    let mut hot = false;
    let lines = snap.as_ref().map(|sn| (snap_lines(rect, sn), sn.reach));
    let mut guides: (Vec<f32>, Vec<f32>) = (Vec::new(), Vec::new());
    // A point `t` along the line, moved to where the line crosses the nearest edge or centre
    // within reach, if one is.
    let catch_along = |t: f32, guides: &mut (Vec<f32>, Vec<f32>)| -> f32 {
        let Some(((xs, ys), reach)) = lines.as_ref() else { return t };
        let p = s + d * t;
        let mut best: Option<(f32, f32, bool, f32)> = None; // (distance, t, across, line)
        for (across, ls) in [(true, xs), (false, ys)] {
            let (pv, dv, sv) = if across { (p.x, d.x, s.x) } else { (p.y, d.y, s.y) };
            if dv.abs() < 1e-3 {
                continue;
            }
            if let Some(l) = nearest(ls, pv, *reach) {
                let dist = (l - pv).abs();
                if best.is_none_or(|b| dist < b.0) {
                    best = Some((dist, (l - sv) / dv, across, l));
                }
            }
        }
        match best {
            Some((_, t, across, l)) => {
                if across { guides.0.push(l) } else { guides.1.push(l) }
                t
            }
            None => t,
        }
    };

    // The line: double-click on it for another colour there.
    let line = ui.interact(egui::Rect::from_two_pos(s, e).expand(6.0), id.with("line"), egui::Sense::click());
    // On the line itself - not just in the box round it - a press is the line's too: a
    // double-click there adds a colour rather than selecting what's above.
    if let Some(p) = line.hover_pos() {
        hot |= (s + d * along(p).clamp(0.0, 1.0)).distance(p) <= 6.0;
    }
    if line.double_clicked() {
        if let Some(p) = line.interact_pointer_pos() {
            let t = along(p).clamp(0.0, 1.0);
            if (s + d * t).distance(p) <= 6.0 {
                out.add_stop_at(t);
                changed = true;
            }
        }
    }

    // The colours between the ends, dragged along the line; right-click takes one off.
    let mut remove = None;
    for i in 1..g.stops.len().saturating_sub(1) {
        let stop = g.stops[i];
        let c = s + d * stop.at.clamp(0.0, 1.0);
        let r = egui::Rect::from_center_size(c, egui::vec2(10.0, 10.0));
        let col = super::rgba_to_color32(stop.color);
        painter.rect_filled(r, 2.0, col);
        painter.rect_stroke(r, 2.0, egui::Stroke::new(1.5_f32, egui::Color32::WHITE), egui::StrokeKind::Outside);
        let resp = ui.interact(r.expand(2.0), id.with(("stop", i)), egui::Sense::click_and_drag())
            .on_hover_text("Drag along the line (Shift: onto edges, Ctrl: onto centres); right-click to take this colour off");
        hot |= resp.hovered() || resp.dragged();
        if resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                out.stops[i].at = catch_along(along(p), &mut guides).clamp(0.001, 0.999);
                changed = true;
            }
        }
        if resp.secondary_clicked() && g.stops.len() > 2 {
            remove = Some(i);
        }
    }
    if let Some(i) = remove {
        out.stops.remove(i);
        changed = true;
    }

    // The bias: a diamond where the middle of the colours falls.
    let b = s + d * g.bias.clamp(0.02, 0.98);
    let diamond = vec![b + egui::vec2(0.0, -6.0), b + egui::vec2(6.0, 0.0), b + egui::vec2(0.0, 6.0), b + egui::vec2(-6.0, 0.0)];
    painter.add(egui::Shape::convex_polygon(diamond, dark, egui::Stroke::new(1.5_f32, egui::Color32::WHITE)));
    let resp = ui.interact(egui::Rect::from_center_size(b, egui::vec2(12.0, 12.0)), id.with("bias"), egui::Sense::drag())
        .on_hover_text("Where the middle of the colours falls (Shift: onto edges, Ctrl: onto centres)");
    hot |= resp.hovered() || resp.dragged();
    if resp.dragged() {
        if let Some(p) = resp.interact_pointer_pos() {
            out.bias = catch_along(along(p), &mut guides).clamp(0.02, 0.98);
            changed = true;
        }
    }

    // The two ends, last so they win where they overlap the rest. Each follows the pointer
    // from where it was grabbed, so a catch on a line lets go again as the pointer moves on.
    for (end, at, word) in [(false, s, "start"), (true, e, "end")] {
        let stop = if end { g.stops.last() } else { g.stops.first() };
        let col = stop.map_or(egui::Color32::WHITE, |s| super::rgba_to_color32(s.color));
        painter.circle(at, 6.5, col, egui::Stroke::new(1.5_f32, egui::Color32::WHITE));
        painter.circle_stroke(at, 8.0, egui::Stroke::new(1.0_f32, dark));
        let tip = match (g.kind, end) {
            (GradientKind::Radial, false) => "Drag the gradient's centre (Shift: onto edges, Ctrl: onto centres)",
            (GradientKind::Radial, true) => "Drag the gradient's edge (Shift: onto edges, Ctrl: onto centres)",
            (_, false) => "Drag where the gradient starts (Shift: onto edges, Ctrl: onto centres)",
            (_, true) => "Drag where the gradient ends (Shift: onto edges, Ctrl: onto centres)",
        };
        let resp = ui.interact(egui::Rect::from_center_size(at, egui::vec2(16.0, 16.0)), id.with(word), egui::Sense::drag())
            .on_hover_text(tip);
        hot |= resp.hovered() || resp.dragged();
        let grab_id = id.with((word, "grab"));
        if resp.drag_started() {
            if let Some(p) = resp.interact_pointer_pos() {
                ui.ctx().data_mut(|dd| dd.insert_temp(grab_id, at - p));
            }
        }
        if resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                let grab: egui::Vec2 = ui.ctx().data(|dd| dd.get_temp(grab_id)).unwrap_or_default();
                let mut q = p + grab;
                if let Some(((xs, ys), reach)) = lines.as_ref() {
                    if let Some(x) = nearest(xs, q.x, *reach) {
                        q.x = x;
                        guides.0.push(x);
                    }
                    if let Some(y) = nearest(ys, q.y, *reach) {
                        q.y = y;
                        guides.1.push(y);
                    }
                }
                let share = [(q.x - rect.min.x) / size.x, (q.y - rect.min.y) / size.y];
                if end {
                    out.end = share;
                } else {
                    out.start = share;
                }
                changed = true;
            }
        }
    }
    super::paint_snap_guides(&painter, egui::Pos2::ZERO, ui.clip_rect(), &guides.0, &guides.1);
    (changed.then_some(out), hot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(kind: GradientKind) -> Gradient {
        Gradient::new(kind, [255, 0, 0, 255], [0, 0, 255, 255])
    }

    /// Along a linear gradient's line the colour runs from the first stop to the last, the
    /// middle halfway between - and the bias moves where that middle is.
    #[test]
    fn a_linear_gradient_runs_along_its_line() {
        let mut lin = g(GradientKind::Linear);
        let size = egui::vec2(200.0, 100.0);
        assert_eq!(lin.sample(egui::vec2(0.0, 50.0), size), egui::Color32::from_rgb(255, 0, 0));
        assert_eq!(lin.sample(egui::vec2(200.0, 0.0), size), egui::Color32::from_rgb(0, 0, 255));
        let mid = lin.sample(egui::vec2(100.0, 80.0), size);
        assert!((mid.r() as i32 - 128).abs() <= 1 && (mid.b() as i32 - 128).abs() <= 1, "{mid:?}");
        // Bias at a quarter: the middle colour is a quarter of the way along.
        lin.bias = 0.25;
        let quarter = lin.sample(egui::vec2(50.0, 50.0), size);
        assert!((quarter.r() as i32 - 128).abs() <= 2, "{quarter:?}");
    }

    /// A radial gradient is round however wide its box: as far out to the side as up from
    /// the centre is the same colour.
    #[test]
    fn a_radial_gradient_is_round_in_a_wide_box() {
        let mut rad = g(GradientKind::Radial);
        rad.end = [0.5, 0.0]; // the edge straight up, 50 points in a 200 by 100 box
        let size = egui::vec2(200.0, 100.0);
        let up = rad.sample(egui::vec2(100.0, 25.0), size);
        let side = rad.sample(egui::vec2(125.0, 50.0), size);
        assert_eq!(up, side);
    }

    /// A stop that's see-through fades the colour out rather than darkening it on the way.
    #[test]
    fn a_see_through_stop_fades_without_darkening() {
        let fade = Gradient::new(GradientKind::Linear, [255, 255, 255, 255], [0, 0, 0, 0]);
        let mid = fade.color_at(0.5).to_srgba_unmultiplied();
        assert!(mid[0] >= 250 && (mid[3] as i32 - 128).abs() <= 1, "white at half alpha: {mid:?}");
    }
}
