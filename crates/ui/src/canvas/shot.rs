//! A picture of what a headless frame painted, for the tests' rig.
//!
//! The layout tests measure rects, and a rect can be exactly where it should be while the
//! module it belongs to looks wrong: a mark drawn too thin, a plate missing, two outlines on the
//! same row. Nothing here launches the app or needs a GPU - egui's own tessellator turns the
//! frame's shapes into triangles, and this fills them the way its shader would (vertex colour
//! times texture, both premultiplied, blended in gamma space).
//!
//! Good for looking at; not a reference renderer. Edges shared by two translucent triangles
//! can blend twice, and paint callbacks (the 3D controller view) are skipped.

use std::collections::HashMap;

use egui::epaint::{ClippedShape, ImageData, Primitive};
use egui::{Color32, Rect, TextureId};

/// The textures egui has handed out so far: its font atlas and anything loaded through
/// `Context::load_texture`. A frame only carries what changed, so the rig feeds every frame's
/// delta in here.
#[derive(Default)]
pub(crate) struct Shot {
    textures: HashMap<TextureId, Tex>,
}

struct Tex {
    w: usize,
    h: usize,
    px: Vec<Color32>,
}

impl Shot {
    pub(crate) fn absorb(&mut self, delta: &egui::TexturesDelta) {
        for (id, d) in &delta.set {
            let ImageData::Color(img) = &d.image;
            let [w, h] = img.size;
            match d.pos {
                None => {
                    self.textures.insert(*id, Tex { w, h, px: img.pixels.clone() });
                }
                Some([x0, y0]) => {
                    let Some(t) = self.textures.get_mut(id) else { continue };
                    for y in 0..h {
                        for x in 0..w {
                            if x0 + x < t.w && y0 + y < t.h {
                                t.px[(y0 + y) * t.w + x0 + x] = img.pixels[y * w + x];
                            }
                        }
                    }
                }
            }
        }
        for id in &delta.free {
            self.textures.remove(id);
        }
    }

    /// Paint `shapes` and save the part of the screen inside `area` (screen points) as a PNG,
    /// at `scale` pixels to the point.
    pub(crate) fn save(
        &self,
        ctx: &egui::Context,
        shapes: &[ClippedShape],
        area: Rect,
        scale: f32,
        path: &std::path::Path,
    ) -> Result<(), String> {
        let w = (area.width() * scale).ceil().max(1.0) as usize;
        let h = (area.height() * scale).ceil().max(1.0) as usize;
        // Premultiplied, 0..1, gamma space - what the shader blends in.
        let mut buf = vec![[0.0f32; 4]; w * h];
        for prim in ctx.tessellate(shapes.to_vec(), scale) {
            let Primitive::Mesh(mesh) = &prim.primitive else { continue };
            let tex = self.textures.get(&mesh.texture_id);
            let clip = prim.clip_rect.intersect(area);
            if !clip.is_positive() {
                continue;
            }
            let to_px = |p: egui::Pos2| ((p.x - area.left()) * scale, (p.y - area.top()) * scale);
            let (cx0, cy0) = to_px(clip.min);
            let (cx1, cy1) = to_px(clip.max);
            for tri in mesh.indices.chunks_exact(3) {
                let v = [
                    mesh.vertices[tri[0] as usize],
                    mesh.vertices[tri[1] as usize],
                    mesh.vertices[tri[2] as usize],
                ];
                let p = [to_px(v[0].pos), to_px(v[1].pos), to_px(v[2].pos)];
                let area2 = (p[1].0 - p[0].0) * (p[2].1 - p[0].1) - (p[2].0 - p[0].0) * (p[1].1 - p[0].1);
                if area2.abs() < 1.0e-6 {
                    continue;
                }
                let min_x = p.iter().map(|q| q.0).fold(f32::INFINITY, f32::min).max(cx0).max(0.0);
                let max_x = p.iter().map(|q| q.0).fold(f32::NEG_INFINITY, f32::max).min(cx1).min(w as f32);
                let min_y = p.iter().map(|q| q.1).fold(f32::INFINITY, f32::min).max(cy0).max(0.0);
                let max_y = p.iter().map(|q| q.1).fold(f32::NEG_INFINITY, f32::max).min(cy1).min(h as f32);
                if min_x >= max_x || min_y >= max_y {
                    continue;
                }
                let col = |c: Color32| [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, c.a() as f32 / 255.0];
                let c = [col(v[0].color), col(v[1].color), col(v[2].color)];
                for y in (min_y.floor() as usize)..(max_y.ceil() as usize).min(h) {
                    let sy = y as f32 + 0.5;
                    if sy < cy0 || sy >= cy1 {
                        continue;
                    }
                    for x in (min_x.floor() as usize)..(max_x.ceil() as usize).min(w) {
                        let sx = x as f32 + 0.5;
                        if sx < cx0 || sx >= cx1 {
                            continue;
                        }
                        let w0 = ((p[1].0 - sx) * (p[2].1 - sy) - (p[2].0 - sx) * (p[1].1 - sy)) / area2;
                        let w1 = ((p[2].0 - sx) * (p[0].1 - sy) - (p[0].0 - sx) * (p[2].1 - sy)) / area2;
                        let w2 = 1.0 - w0 - w1;
                        if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                            continue;
                        }
                        let mut src = [0.0f32; 4];
                        for k in 0..4 {
                            src[k] = w0 * c[0][k] + w1 * c[1][k] + w2 * c[2][k];
                        }
                        if let Some(t) = tex {
                            let u = w0 * v[0].uv.x + w1 * v[1].uv.x + w2 * v[2].uv.x;
                            let vv = w0 * v[0].uv.y + w1 * v[1].uv.y + w2 * v[2].uv.y;
                            let texel = t.sample(u, vv);
                            for k in 0..4 {
                                src[k] *= texel[k];
                            }
                        }
                        let dst = &mut buf[y * w + x];
                        let keep = 1.0 - src[3];
                        for k in 0..4 {
                            dst[k] = src[k] + dst[k] * keep;
                        }
                    }
                }
            }
        }
        let mut pixmap = tiny_skia::Pixmap::new(w as u32, h as u32).ok_or("an empty picture")?;
        for (out, px) in pixmap.data_mut().chunks_exact_mut(4).zip(&buf) {
            let a = px[3].clamp(0.0, 1.0);
            for k in 0..3 {
                out[k] = (px[k].clamp(0.0, a) * 255.0).round() as u8;
            }
            out[3] = (a * 255.0).round() as u8;
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        pixmap.save_png(path).map_err(|e| e.to_string())
    }
}

/// Pack pictures onto contact sheets, for looking over many modules at once: left to right
/// in rows, each drawn at `scale`, a new sheet when one is full. Writes `sheet_<n>.png` next
/// to the pictures and returns the sheets' paths.
pub(crate) fn sheets(pictures: &[std::path::PathBuf], scale: f32, side: u32) -> Vec<std::path::PathBuf> {
    const GAP: f32 = 6.0;
    let mut out = Vec::new();
    let Some(dir) = pictures.first().and_then(|p| p.parent()).map(|d| d.to_path_buf()) else { return out };
    let mut sheet = tiny_skia::Pixmap::new(side, side).expect("a sheet");
    let (mut x, mut y, mut row_h, mut used) = (GAP, GAP, 0.0f32, false);
    let flush = |sheet: &mut tiny_skia::Pixmap, out: &mut Vec<std::path::PathBuf>| {
        let path = dir.join(format!("sheet_{}.png", out.len()));
        if sheet.save_png(&path).is_ok() {
            out.push(path);
        }
        sheet.fill(tiny_skia::Color::from_rgba8(0, 0, 0, 255));
    };
    sheet.fill(tiny_skia::Color::from_rgba8(0, 0, 0, 255));
    for p in pictures {
        let Ok(pic) = tiny_skia::Pixmap::load_png(p) else { continue };
        let (w, h) = (pic.width() as f32 * scale, pic.height() as f32 * scale);
        if x + w + GAP > side as f32 && x > GAP {
            x = GAP;
            y += row_h + GAP;
            row_h = 0.0;
        }
        if y + h + GAP > side as f32 && used {
            flush(&mut sheet, &mut out);
            x = GAP;
            y = GAP;
            row_h = 0.0;
        }
        sheet.draw_pixmap(
            0,
            0,
            pic.as_ref(),
            &tiny_skia::PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..Default::default() },
            tiny_skia::Transform::from_scale(scale, scale).post_translate(x, y),
            None,
        );
        used = true;
        x += w + GAP;
        row_h = row_h.max(h);
    }
    if used {
        flush(&mut sheet, &mut out);
    }
    out
}

impl Tex {
    /// Bilinear, clamped at the edges.
    fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        let x = (u * self.w as f32 - 0.5).clamp(0.0, self.w as f32 - 1.0);
        let y = (v * self.h as f32 - 0.5).clamp(0.0, self.h as f32 - 1.0);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let at = |x: usize, y: usize| {
            let c = self.px[y * self.w + x];
            [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, c.a() as f32 / 255.0]
        };
        let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
        let mut out = [0.0f32; 4];
        for k in 0..4 {
            out[k] = (a[k] * (1.0 - fx) + b[k] * fx) * (1.0 - fy) + (c[k] * (1.0 - fx) + d[k] * fx) * fy;
        }
        out
    }
}
