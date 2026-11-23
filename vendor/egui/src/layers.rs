//! Handles paint layers, i.e. how things
//! are sometimes painted behind or in front of other things.

use crate::{Id, IdMap, Rect, ahash, epaint};
use epaint::{ClippedShape, Shape, emath::TSTransform};

/// Different layer categories
#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Ord, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub enum Order {
    /// Painted behind all floating windows
    Background,

    /// Normal moveable windows that you reorder by click
    Middle,

    /// Popups, menus etc that should always be painted on top of windows
    /// Foreground objects can also have tooltips
    Foreground,

    /// Things floating on top of everything else, like tooltips.
    /// You cannot interact with these.
    Tooltip,

    /// Debug layer, always painted last / on top
    Debug,
}

impl Order {
    const COUNT: usize = 5;
    const ALL: [Self; Self::COUNT] = [
        Self::Background,
        Self::Middle,
        Self::Foreground,
        Self::Tooltip,
        Self::Debug,
    ];
    pub const TOP: Self = Self::Debug;

    #[inline(always)]
    pub fn allow_interaction(&self) -> bool {
        match self {
            Self::Background | Self::Middle | Self::Foreground | Self::Tooltip | Self::Debug => {
                true
            }
        }
    }

    /// Short and readable summary
    pub fn short_debug_format(&self) -> &'static str {
        match self {
            Self::Background => "backg",
            Self::Middle => "middl",
            Self::Foreground => "foreg",
            Self::Tooltip => "toolt",
            Self::Debug => "debug",
        }
    }
}

/// An identifier for a paint layer.
/// Also acts as an identifier for [`crate::Area`]:s.
#[derive(Clone, Copy, Hash, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct LayerId {
    pub order: Order,
    pub id: Id,
}

impl LayerId {
    pub fn new(order: Order, id: Id) -> Self {
        Self { order, id }
    }

    pub fn debug() -> Self {
        Self {
            order: Order::Debug,
            id: Id::new("debug"),
        }
    }

    pub fn background() -> Self {
        Self {
            order: Order::Background,
            id: Id::new("background"),
        }
    }

    #[inline(always)]
    #[deprecated = "Use `Memory::allows_interaction` instead"]
    pub fn allow_interaction(&self) -> bool {
        self.order.allow_interaction()
    }

    /// Short and readable summary
    pub fn short_debug_format(&self) -> String {
        format!(
            "{} {}",
            self.order.short_debug_format(),
            self.id.short_debug_format()
        )
    }
}

impl std::fmt::Debug for LayerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { order, id } = self;
        write!(f, "LayerId {{ {order:?} {id:?} }}")
    }
}

/// A unique identifier of a specific [`Shape`] in a [`PaintList`].

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShapeIdx(pub usize);

/// A list of [`Shape`]s paired with a clip rectangle.
#[derive(Clone, Default)]
pub struct PaintList(Vec<ClippedShape>);

impl PaintList {
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn next_idx(&self) -> ShapeIdx {
        ShapeIdx(self.0.len())
    }

    /// Returns the index of the new [`Shape`] that can be used with `PaintList::set`.
    #[inline(always)]
    pub fn add(&mut self, clip_rect: Rect, shape: Shape) -> ShapeIdx {
        let idx = self.next_idx();
        self.0.push(ClippedShape { clip_rect, shape });
        idx
    }

    pub fn extend<I: IntoIterator<Item = Shape>>(&mut self, clip_rect: Rect, shapes: I) {
        self.0.extend(
            shapes
                .into_iter()
                .map(|shape| ClippedShape { clip_rect, shape }),
        );
    }

    /// Modify an existing [`Shape`].
    ///
    /// Sometimes you want to paint a frame behind some contents, but don't know how large the frame needs to be
    /// until the contents have been added, and therefor also painted to the [`PaintList`].
    ///
    /// The solution is to allocate a [`Shape`] using `let idx = paint_list.add(cr, Shape::Noop);`
    /// and then later setting it using `paint_list.set(idx, cr, frame);`.
    #[inline(always)]
    pub fn set(&mut self, idx: ShapeIdx, clip_rect: Rect, shape: Shape) {
        if self.0.len() <= idx.0 {
            log::warn!("Index {} is out of bounds for PaintList", idx.0);
            return;
        }

        self.0[idx.0] = ClippedShape { clip_rect, shape };
    }

    /// Set the given shape to be empty (a `Shape::Noop`).
    #[inline(always)]
    pub fn reset_shape(&mut self, idx: ShapeIdx) {
        self.0[idx.0].shape = Shape::Noop;
    }

    /// Mutate the shape at the given index, if any.
    pub fn mutate_shape(&mut self, idx: ShapeIdx, f: impl FnOnce(&mut ClippedShape)) {
        self.0.get_mut(idx.0).map(f);
    }

    /// Transform each [`Shape`] and clip rectangle by this much, in-place
    pub fn transform(&mut self, transform: TSTransform) {
        for ClippedShape { clip_rect, shape } in &mut self.0 {
            *clip_rect = transform.mul_rect(*clip_rect);
            shape.transform(transform);
        }
    }

    /// Transform each [`Shape`] and clip rectangle in range by this much, in-place
    pub fn transform_range(&mut self, start: ShapeIdx, end: ShapeIdx, transform: TSTransform) {
        for ClippedShape { clip_rect, shape } in &mut self.0[start.0..end.0] {
            *clip_rect = transform.mul_rect(*clip_rect);
            shape.transform(transform);
        }
    }

    /// Read-only access to all held shapes.
    pub fn all_entries(&self) -> impl ExactSizeIterator<Item = &ClippedShape> {
        self.0.iter()
    }
}

/// This is where painted [`Shape`]s end up during a frame.
#[derive(Clone, Default)]
pub struct GraphicLayers([IdMap<PaintList>; Order::COUNT]);

impl GraphicLayers {
    /// Get or insert the [`PaintList`] for the given [`LayerId`].
    pub fn entry(&mut self, layer_id: LayerId) -> &mut PaintList {
        self.0[layer_id.order as usize]
            .entry(layer_id.id)
            .or_default()
    }

    /// Get the [`PaintList`] for the given [`LayerId`].
    pub fn get(&self, layer_id: LayerId) -> Option<&PaintList> {
        self.0[layer_id.order as usize].get(&layer_id.id)
    }

    /// Get the [`PaintList`] for the given [`LayerId`].
    pub fn get_mut(&mut self, layer_id: LayerId) -> Option<&mut PaintList> {
        self.0[layer_id.order as usize].get_mut(&layer_id.id)
    }

    /// FLEXINPUT PATCH: text in a layer that's scaled up - a zoomed-in canvas, a layout drawn
    /// bigger to fill its panel - laid out again at the resolution it ends up on screen.
    ///
    /// Text is laid out for the screen's pixels per point, its glyphs rasterized to match, and a
    /// layer's transform only stretched the finished glyphs: two times zoomed was two times
    /// blurry, worst on a screen at 100% (a high-DPI one rasterizes finer to begin with). The
    /// same text laid out at pixels per point times the zoom keeps its size and place in points
    /// and gets glyphs rasterized at the size they're drawn. The zoom is rounded up to an
    /// eighth of an octave, so a zoom in progress rasterizes a new set of glyphs a step at a
    /// time rather than every frame, and goes no further than four times.
    ///
    /// Call before the font atlas's delta is taken, so the glyphs this rasterizes go up with it
    /// the same frame, and before [`Self::drain`] applies the transforms.
    pub fn relayout_scaled_text(
        &mut self,
        to_global: &ahash::HashMap<LayerId, TSTransform>,
        fonts: &mut epaint::text::Fonts,
        pixels_per_point: f32,
    ) {
        #[expect(clippy::iter_over_hash_type)]
        for (layer_id, transform) in to_global {
            let Some(raster) = text_raster_scale(transform.scaling) else {
                continue;
            };
            let Some(list) = self.get_mut(*layer_id) else {
                continue;
            };
            let mut view = fonts.with_pixels_per_point(pixels_per_point * raster);
            for clipped in &mut list.0 {
                relayout_text_in(&mut clipped.shape, &mut view, pixels_per_point);
            }
        }
    }

    pub fn drain(
        &mut self,
        area_order: &[LayerId],
        to_global: &ahash::HashMap<LayerId, TSTransform>,
    ) -> Vec<ClippedShape> {
        profiling::function_scope!();

        let mut all_shapes: Vec<_> = Default::default();

        for &order in &Order::ALL {
            let order_map = &mut self.0[order as usize];

            // If a layer is empty at the start of the frame
            // then nobody has added to it, and it is old and defunct.
            // Free it to save memory:
            order_map.retain(|_, list| !list.is_empty());

            // First do the layers part of area_order:
            for layer_id in area_order {
                if layer_id.order == order
                    && let Some(list) = order_map.get_mut(&layer_id.id)
                {
                    if let Some(to_global) = to_global.get(layer_id) {
                        for clipped_shape in &mut list.0 {
                            clipped_shape.transform(*to_global);
                        }
                    }
                    all_shapes.append(&mut list.0);
                }
            }

            // Also draw areas that are missing in `area_order`:
            // NOTE: We don't think we end up here in normal situations.
            // This is just a safety net in case we have some bug somewhere.
            #[expect(clippy::iter_over_hash_type)]
            for (id, list) in order_map {
                let layer_id = LayerId::new(order, *id);

                if let Some(to_global) = to_global.get(&layer_id) {
                    for clipped_shape in &mut list.0 {
                        clipped_shape.transform(*to_global);
                    }
                }

                all_shapes.append(&mut list.0);
            }
        }

        all_shapes
    }
}


/// FLEXINPUT PATCH: how much finer than the screen to rasterize the text of a layer scaled by
/// `scaling`: rounded up to an eighth of an octave, at most four times; `None` where the layer
/// isn't scaled up (stretching isn't the problem there).
pub fn text_raster_scale(scaling: f32) -> Option<f32> {
    if !scaling.is_finite() || scaling <= 1.01 {
        return None;
    }
    let steps = (scaling.log2() * 8.0 - 1e-4).ceil();
    Some(2f32.powf(steps / 8.0).min(4.0))
}

/// FLEXINPUT PATCH: the text in `shape` laid out again with `fonts` (see
/// [`GraphicLayers::relayout_scaled_text`]).
fn relayout_text_in(shape: &mut Shape, fonts: &mut epaint::text::FontsView<'_>, pixels_per_point: f32) {
    match shape {
        Shape::Text(text) => relayout_text(text, fonts, pixels_per_point),
        Shape::Vec(shapes) => {
            for shape in shapes {
                relayout_text_in(shape, fonts, pixels_per_point);
            }
        }
        _ => {}
    }
}

/// FLEXINPUT PATCH: one text shape laid out again from its own job. Only where it comes out
/// the same - the same rows, the same number of vertices in each - and with the colours the
/// old one was painted in, which a selection's highlight or a tint may have changed since it
/// was laid out; otherwise it stays as it was. Marked as laid out for the screen's pixels per
/// point (`pixels_per_point`), which the tessellator checks, since its points are those.
fn relayout_text(text: &mut epaint::TextShape, fonts: &mut epaint::text::FontsView<'_>, pixels_per_point: f32) {
    let old = &text.galley;
    if old.rows.is_empty() || old.pixels_per_point != pixels_per_point {
        return;
    }
    let fresh = fonts.layout_job((*old.job).clone());
    let same_shape = fresh.rows.len() == old.rows.len()
        && fresh.rows.iter().zip(&old.rows).all(|(a, b)| {
            a.row.visuals.mesh.vertices.len() == b.row.visuals.mesh.vertices.len()
        });
    if !same_shape {
        return;
    }
    let mut galley = (*fresh).clone();
    for (new, was) in galley.rows.iter_mut().zip(&old.rows) {
        let recolour = new
            .row
            .visuals
            .mesh
            .vertices
            .iter()
            .zip(&was.row.visuals.mesh.vertices)
            .any(|(a, b)| a.color != b.color);
        if recolour {
            let row = std::sync::Arc::make_mut(&mut new.row);
            for (a, b) in row.visuals.mesh.vertices.iter_mut().zip(&was.row.visuals.mesh.vertices) {
                a.color = b.color;
            }
        }
    }
    galley.pixels_per_point = pixels_per_point;
    // Text already scaled on its way here - drawn under `Ui::with_visual_transform`, as a
    // layout being edited draws its pinned widgets - keeps that scale: the fresh layout is
    // the job's own size, and replacing the old with it drew the text unscaled, too big for
    // a widget pinned smaller than its design (its words ran out of their buttons).
    // Only a scale well past what rounding the rows to a finer pixel grid makes of the same
    // text (a percent or two): taken for a scale, that moved text that wasn't scaled at all.
    let scale = if galley.rect.height() > 1e-3 { old.rect.height() / galley.rect.height() } else { 1.0 };
    let mut galley = if scale.is_finite() && scale > 0.0 && (scale - 1.0).abs() > 0.04 {
        scale_galley(galley, scale)
    } else {
        galley
    };
    // Only the glyphs are new: every row stays where the old layout had it, which is where a
    // text field puts its caret and its selection. Rows rounded to the finer pixel grid come
    // out a fraction of a point shorter or taller, and taken as they were, each line was drawn
    // that much off the last - by a whole line in a long code editor (JSM Config's), its caret
    // under the text it was in.
    for (new, was) in galley.rows.iter_mut().zip(&old.rows) {
        new.pos = was.pos;
    }
    galley.mesh_bounds = galley.mesh_bounds.union(old.mesh_bounds);
    galley.rect = old.rect;
    text.galley = std::sync::Arc::new(galley);
}

/// FLEXINPUT PATCH: a galley scaled about its origin by `s`, by epaint's own
/// `TextShape::transform` (its rects, its rows and their glyphs' vertices), for
/// [`relayout_text`] to carry a scale the text already had.
fn scale_galley(galley: epaint::Galley, s: f32) -> epaint::Galley {
    let mut shape = epaint::TextShape::new(emath::Pos2::ZERO, std::sync::Arc::new(galley), epaint::Color32::PLACEHOLDER);
    shape.transform(TSTransform::from_scaling(s));
    std::sync::Arc::unwrap_or_clone(shape.galley)
}
