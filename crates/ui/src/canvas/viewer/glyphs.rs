//! Small marks for switches whose meaning is a shape rather than a word: a magnet for
//! snapping to a grid, a plateau for holding at the sustain level, a there-and-back for
//! bouncing, a ring for looping. The word stays in the tooltip.
//!
//! The marks are drawings - `app/assets/{hold_env,bounce,loop,magnet}.svg` - in the idiom of
//! the module glyphs (see [`super::module_icons`]): a 3-unit stroke, round caps, no fill. Each
//! is rasterized white, once per size, and tinted where it is painted, so a switch changing
//! colour costs no new texture.

use egui::{Color32, Rect};

const HOLD: &str = include_str!("../../../../../app/assets/hold_env.svg");
const BOUNCE: &str = include_str!("../../../../../app/assets/bounce.svg");
const LOOP: &str = include_str!("../../../../../app/assets/loop.svg");
const MAGNET: &str = include_str!("../../../../../app/assets/magnet.svg");
const STOPWATCH: &str = include_str!("../../../../../app/assets/stopwatch.svg");
const SAVE: &str = include_str!("../../../../../app/assets/save.svg");
const LOAD: &str = include_str!("../../../../../app/assets/load.svg");
const RESET: &str = include_str!("../../../../../app/assets/reset.svg");
const ABS: &str = include_str!("../../../../../app/assets/modules/ABS.svg");
const ADD_TEXT: &str = include_str!("../../../../../app/assets/layout/add_text.svg");
const ADD_RECT: &str = include_str!("../../../../../app/assets/layout/add_rect.svg");
const ADD_ELLIPSE: &str = include_str!("../../../../../app/assets/layout/add_ellipse.svg");
const ADD_LINE: &str = include_str!("../../../../../app/assets/layout/add_line.svg");
const ADD_PICTURE: &str = include_str!("../../../../../app/assets/layout/add_picture.svg");
const ANCHORS: &str = include_str!("../../../../../app/assets/layout/anchors.svg");

/// A switch mark. Named for what the shape stands for, not for what it looks like.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub(crate) enum Glyph {
    /// Dragged points settle on what is near them.
    Magnet,
    /// Run to the sustain level and wait there.
    Hold,
    /// Forward while held, back on release.
    Bounce,
    /// Repeat the segment.
    Loop,
    /// Time held back: the Delay module, where its name doesn't fit.
    Stopwatch,
    /// Write this to a file / read it from one / put it back as it shipped: header buttons
    /// that were words, where three words left the module's name no room.
    Save,
    Load,
    Reset,
    /// Magnitude: a curve's Abs switch wears the Abs module's own mark.
    Abs,
    /// What the layout editor adds: a text, a rectangle, an ellipse, a line, a picture.
    AddText,
    AddRect,
    AddEllipse,
    AddLine,
    AddPicture,
    /// What each element of an overlay holds on to.
    Anchors,
}

impl Glyph {
    fn svg(self) -> &'static str {
        match self {
            Glyph::Magnet => MAGNET,
            Glyph::Hold => HOLD,
            Glyph::Bounce => BOUNCE,
            Glyph::Loop => LOOP,
            Glyph::Stopwatch => STOPWATCH,
            Glyph::Save => SAVE,
            Glyph::Load => LOAD,
            Glyph::Reset => RESET,
            Glyph::Abs => ABS,
            Glyph::AddText => ADD_TEXT,
            Glyph::AddRect => ADD_RECT,
            Glyph::AddEllipse => ADD_ELLIPSE,
            Glyph::AddLine => ADD_LINE,
            Glyph::AddPicture => ADD_PICTURE,
            Glyph::Anchors => ANCHORS,
        }
    }

    /// The side of the square cut out of the drawing, in the drawing's own units, centred on
    /// what is drawn rather than on the page: the pages are mostly margin (a 25-unit mark on a
    /// 59-unit sheet), and a mark shown page and all comes out the size of a full stop.
    ///
    /// Hold, Bounce and Loop sit side by side, so they share one window - the same scale, the
    /// same stroke weight. The magnet is drawn on its own, finer sheet.
    fn window(self) -> f32 {
        match self {
            Glyph::Magnet => 48.0,
            Glyph::Hold | Glyph::Bounce | Glyph::Loop | Glyph::Stopwatch => 34.0,
            Glyph::Save | Glyph::Load | Glyph::Reset | Glyph::Abs => 34.0,
            Glyph::AddText | Glyph::AddRect | Glyph::AddEllipse | Glyph::AddLine | Glyph::AddPicture => 34.0,
            Glyph::Anchors => 34.0,
        }
    }
}

/// Air between a mark and the edge of the button it is on.
const MARGIN: f32 = 2.0;
/// The largest a mark is drawn, and the smallest worth drawing.
const MAX_SIDE: f32 = 24.0;
const MIN_SIDE: f32 = 8.0;

/// Draw `glyph` in the middle of `rect`, in `colour`. The button under it is the caller's.
pub(crate) fn paint(painter: &egui::Painter, rect: Rect, glyph: Glyph, colour: Color32) {
    let side = (rect.height().min(rect.width()) - 2.0 * MARGIN).min(MAX_SIDE);
    if side < MIN_SIDE {
        return;
    }
    let ctx = painter.ctx();
    // At the pixels it covers, the canvas's zoom included (`raster_ppp`). Cached per size step.
    let px = ((side * crate::widgets::raster_ppp(ctx, painter.layer_id())).round() as u32).clamp(16, 512);
    let key = egui::Id::new(("switch_mark", glyph, px));
    let tex = crate::widgets::tex_cache::get(ctx, key).or_else(|| {
        let handle = ctx.load_texture(
            format!("switch_mark_{glyph:?}_{px}"),
            rasterize(glyph, px)?,
            egui::TextureOptions::LINEAR,
        );
        crate::widgets::tex_cache::put(ctx, key, handle.clone());
        Some(handle)
    });
    let Some(tex) = tex else { return };
    painter.image(
        tex.id(),
        Rect::from_center_size(rect.center(), egui::Vec2::splat(side)),
        Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        colour,
    );
}

/// A device icon (one of the device cards' / node headers' SVGs), white, cropped to what is
/// drawn, and how wide it is for its height - filled as drawn, or (`outline`) as just the
/// edge of its silhouette, a line three texels in from it. For drawing a pad flat on a plane
/// (the 3DOF preview lays it in the pad's own frame). Cached per icon and look.
pub(crate) fn svg_icon(ctx: &egui::Context, svg: &'static [u8], outline: bool) -> Option<(egui::TextureId, f32)> {
    let key = egui::Id::new(("fxi_device_icon", svg.as_ptr() as usize, outline));
    let aspect_key = key.with("aspect");
    if let (Some(tex), Some(aspect)) = (crate::widgets::tex_cache::get(ctx, key), ctx.data(|d| d.get_temp::<f32>(aspect_key))) {
        return Some((tex.id(), aspect));
    }
    let tree = usvg::Tree::from_data(svg, &usvg::Options::default()).ok()?;
    let (cx, cy, w, h) = drawn(&tree);
    // Drawn small - a pad in a preview a few hundred points across - and there is no mipmap
    // on every backend, so the texture is sized near what it is drawn at: an edge a few
    // texels wide at 256 shrank to a fraction of a pixel and broke up into dots.
    let scale = 128.0 / w.max(h).max(1.0);
    let (pw, ph) = ((w * scale).ceil().max(1.0) as u32, (h * scale).ceil().max(1.0) as u32);
    let transform = tiny_skia::Transform::from_translate(w * 0.5 - cx, h * 0.5 - cy).post_scale(scale, scale);
    let mut pixmap = tiny_skia::Pixmap::new(pw, ph)?;
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let (pw, ph) = (pw as usize, ph as usize);
    let alpha: Vec<u8> = pixmap.data().chunks_exact(4).map(|p| p[3]).collect();
    let rgba: Vec<u8> = if outline {
        // White where the silhouette is and something within three texels isn't: the edge
        // of the shape, everything inside it left out.
        const R: isize = 3;
        let inside = |x: isize, y: isize| {
            x >= 0 && y >= 0 && (x as usize) < pw && (y as usize) < ph && alpha[y as usize * pw + x as usize] > 96
        };
        let mut out = vec![0u8; pw * ph * 4];
        for y in 0..ph as isize {
            for x in 0..pw as isize {
                if !inside(x, y) {
                    continue;
                }
                let edge = (-R..=R).any(|dy| (-R..=R).any(|dx| dx * dx + dy * dy <= R * R && !inside(x + dx, y + dy)));
                if edge {
                    let i = (y as usize * pw + x as usize) * 4;
                    out[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                }
            }
        }
        out
    } else {
        // The art keeps its shape; drawn white so the caller's tint decides its colour.
        alpha.iter().flat_map(|&a| [a, a, a, a]).collect()
    };
    let image = egui::ColorImage::from_rgba_premultiplied([pw, ph], &rgba);
    let handle = ctx.load_texture(format!("fxi_device_icon_{}", outline), image, egui::TextureOptions::LINEAR);
    let id = handle.id();
    crate::widgets::tex_cache::put(ctx, key, handle);
    let aspect = w / h.max(1e-3);
    ctx.data_mut(|d| d.insert_temp(aspect_key, aspect));
    Some((id, aspect))
}

/// What is drawn on the sheet, strokes included, as (centre x, centre y, width, height).
fn drawn(tree: &usvg::Tree) -> (f32, f32, f32, f32) {
    let b = tree.root().abs_stroke_bounding_box();
    (b.x() + b.width() * 0.5, b.y() + b.height() * 0.5, b.width(), b.height())
}

/// The mark at `px` pixels square, as drawn (white), premultiplied.
fn rasterize(glyph: Glyph, px: u32) -> Option<egui::ColorImage> {
    let tree = usvg::Tree::from_str(glyph.svg(), &usvg::Options::default()).ok()?;
    let (cx, cy, _, _) = drawn(&tree);
    let window = glyph.window();
    let scale = px as f32 / window;
    let transform = tiny_skia::Transform::from_translate(window * 0.5 - cx, window * 0.5 - cy).post_scale(scale, scale);
    let mut pixmap = tiny_skia::Pixmap::new(px, px)?;
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Some(egui::ColorImage::from_rgba_premultiplied([px as usize; 2], pixmap.data()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Glyph; 15] = [
        Glyph::Abs,
        Glyph::Magnet,
        Glyph::Hold,
        Glyph::Bounce,
        Glyph::Loop,
        Glyph::Stopwatch,
        Glyph::Save,
        Glyph::Load,
        Glyph::Reset,
        Glyph::AddText,
        Glyph::AddRect,
        Glyph::AddEllipse,
        Glyph::AddLine,
        Glyph::AddPicture,
        Glyph::Anchors,
    ];

    // A drawing edited to be bigger than its window would be cut off at the button's edge,
    // and nothing else would say so.
    #[test]
    fn every_mark_fits_the_window_it_is_cut_from() {
        for g in ALL {
            let tree = usvg::Tree::from_str(g.svg(), &usvg::Options::default()).expect("the drawing parses");
            let (_, _, w, h) = drawn(&tree);
            assert!(w > 8.0 && h > 8.0, "{g:?}: nothing much is drawn ({w} x {h})");
            assert!(
                w <= g.window() && h <= g.window(),
                "{g:?}: the drawing is {w} x {h}, its window {}",
                g.window()
            );
        }
    }

    #[test]
    fn every_mark_rasterizes_centred_and_clear_of_the_edge() {
        for g in ALL {
            let px = 64usize;
            let img = rasterize(g, px as u32).expect("the mark rasterizes");
            let (mut x0, mut y0, mut x1, mut y1) = (px, px, 0usize, 0usize);
            for (i, p) in img.pixels.iter().enumerate() {
                if p.a() > 40 {
                    let (x, y) = (i % px, i / px);
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
            assert!(x1 > x0 && y1 > y0, "{g:?}: nothing was drawn");
            assert!(x0 > 0 && y0 > 0 && x1 < px - 1 && y1 < px - 1, "{g:?}: cut off at the edge");
            let off_x = (x0 + x1) as f32 * 0.5 - (px - 1) as f32 * 0.5;
            let off_y = (y0 + y1) as f32 * 0.5 - (px - 1) as f32 * 0.5;
            assert!(off_x.abs() <= 1.5 && off_y.abs() <= 1.5, "{g:?}: off centre by ({off_x}, {off_y})");
        }
    }
}
