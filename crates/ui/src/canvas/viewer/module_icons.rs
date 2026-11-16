//! The module glyphs: one mark per small operation, painted into the room a module
//! has left under its pins.
//!
//! The logic gates' glyphs are the drawings in `app/assets/modules/`. The arithmetic
//! and comparison glyphs were cut to the same idiom, so a patch of gates and
//! operators reads as one family: a 59x59 box, a 4-point stroke with round caps and
//! round joins, no fill, and the mark itself filling the band the gate bodies fill
//! (x 16.5..41.5, y 18.5..40.5). A gate glyph carries lead stubs, because that is what
//! makes a gate a gate; an operator glyph doesn't, because a stub running into the bar
//! of a plus is just a mess of lines.
//!
//! A glyph is painted, never allocated: it cannot grow a module or take a row from the
//! module's own pins, and it makes itself smaller when the pins need the room.

use super::*;

const ADD: &str = include_str!("../../../../../app/assets/modules/ADD.svg");
const SUBTRACT: &str = include_str!("../../../../../app/assets/modules/SUBTRACT.svg");
const MULTIPLY: &str = include_str!("../../../../../app/assets/modules/MULTIPLY.svg");
const DIVIDE: &str = include_str!("../../../../../app/assets/modules/DIVIDE.svg");
const MIN_MAX: &str = include_str!("../../../../../app/assets/modules/MIN_MAX.svg");
const ABS: &str = include_str!("../../../../../app/assets/modules/ABS.svg");
const CLAMP: &str = include_str!("../../../../../app/assets/modules/CLAMP.svg");
const MAP_RANGE: &str = include_str!("../../../../../app/assets/modules/MAP_RANGE.svg");
const AXIS_TO_VEC: &str = include_str!("../../../../../app/assets/modules/AXIS_TO_VEC.svg");
const VEC_TO_AXIS: &str = include_str!("../../../../../app/assets/modules/VEC_TO_AXIS.svg");
/// Vec to Deflection: the vector's own path bent away from straight ahead, with the arc
/// that says by how much. Same lead stub as its Converters siblings.
const VEC_TO_DEFLECTION: &str = include_str!("../../../../../app/assets/modules/VEC_TO_DEFLECTION.svg");
/// The Quantize mark: a stepped curve. It was first drawn for Sample and Hold, which now
/// shows its two live readings instead, and came here - where it says what the module does.
const QUANTIZE_STEPS: &str = include_str!("../../../../../app/assets/modules/QUANTIZE.svg");
const AND: &str = include_str!("../../../../../app/assets/modules/AND.svg");
const OR: &str = include_str!("../../../../../app/assets/modules/OR.svg");
const NOT: &str = include_str!("../../../../../app/assets/modules/NOT.svg");
const XOR: &str = include_str!("../../../../../app/assets/modules/XOR.svg");
const NAND: &str = include_str!("../../../../../app/assets/modules/NAND.svg");
const NOR: &str = include_str!("../../../../../app/assets/modules/NOR.svg");
const XNOR: &str = include_str!("../../../../../app/assets/modules/XNOR.svg");
const EQUAL: &str = include_str!("../../../../../app/assets/modules/EQUAL.svg");
const NOT_EQUAL: &str = include_str!("../../../../../app/assets/modules/NOT_EQUAL.svg");
const GREATER_THAN: &str = include_str!("../../../../../app/assets/modules/GREATER_THAN.svg");
const GREATER_OR_EQUAL: &str = include_str!("../../../../../app/assets/modules/GREATER_OR_EQUAL.svg");
const LESS_THAN: &str = include_str!("../../../../../app/assets/modules/LESS_THAN.svg");
const LESS_OR_EQUAL: &str = include_str!("../../../../../app/assets/modules/LESS_OR_EQUAL.svg");

/// Every module drawn with a glyph, as (module id, the asset).
pub(crate) const ICONS: &[(&str, &str)] = &[
    ("math.add", ADD),
    ("math.subtract", SUBTRACT),
    ("math.multiply", MULTIPLY),
    ("math.divide", DIVIDE),
    ("math.min_max", MIN_MAX),
    ("math.clamp", CLAMP),
    ("math.quantize", QUANTIZE_STEPS),
    ("math.map_range", MAP_RANGE),
    ("module.axis_to_vec", AXIS_TO_VEC),
    ("module.vec_to_axis", VEC_TO_AXIS),
    ("module.vec_to_deflection", VEC_TO_DEFLECTION),
    ("logic.and", AND),
    ("logic.or", OR),
    ("logic.not", NOT),
    ("logic.xor", XOR),
    ("logic.nand", NAND),
    ("logic.nor", NOR),
    ("logic.xnor", XNOR),
    ("math.abs", ABS),
    ("logic.equal", EQUAL),
    ("logic.not_equal", NOT_EQUAL),
    ("logic.greater_than", GREATER_THAN),
    ("logic.less_than", LESS_THAN),
];

/// The glyph for a module, or none. A comparison takes the or-equal drawing while its
/// `or_equal` setting is on, the same way its header mark does.
pub(crate) fn icon_svg(module_id: &str, or_equal: bool) -> Option<&'static str> {
    let svg = ICONS.iter().find(|(id, _)| *id == module_id).map(|&(_, s)| s)?;
    if !or_equal {
        return Some(svg);
    }
    Some(match module_id {
        "logic.greater_than" => GREATER_OR_EQUAL,
        "logic.less_than" => LESS_OR_EQUAL,
        _ => svg,
    })
}

/// Whether this module is drawn with a glyph at all: what decides both that it gets a
/// body to paint one into and that its body is the glyph and nothing else.
pub(crate) fn has_icon(module_id: &str) -> bool {
    ICONS.iter().any(|(id, _)| *id == module_id)
}

/// The modules whose shipped pins fill only one row — Abs and NOT — so a mark of their
/// own would have nowhere to sit and they come out a block shorter than their siblings.
/// They reserve a band in the body instead, which is what makes them square.
///
/// Keyed on the module, never on its measured height: a reservation that read the node's
/// current size back in would fight it from frame to frame.
pub(crate) fn square_body(module_id: &str) -> bool {
    matches!(module_id, "math.abs" | "logic.not")
}

/// The band those modules reserve: enough for a full-block mark inside the frame, and
/// still short enough that the module stays two blocks tall.
pub(crate) const SQUARE_BAND: f32 = 68.0;

/// The width every body drawn from a mark reserves in the flow. The mark itself is painted
/// and asks for nothing, so without this the module's width is its pin labels' business
/// alone - and these modules were reaching two blocks only because the word in their header
/// was wide. A name too long for the frame is left out rather than cropped, so the body has
/// to ask for the size on its own. Constant, so adding inputs costs it nothing.
pub(crate) const MIN_BODY_W: f32 = 40.0;

/// The largest a glyph is drawn — one grid block — and the smallest worth drawing:
/// below the second the strokes stop reading as a shape. Between those the band the
/// module leaves decides, so a mark fills the room it has without ever crowding the
/// pin rows above it.
const MAX_SIDE: f32 = 56.0;
const MIN_SIDE: f32 = 16.0;
/// Air between the glyph and the module's frame, on top of the frame's own inner
/// margin, which [`module_inner`] has already taken off.
const PAD: f32 = 6.0;

/// The margin a node frame keeps between its border and its content.
const NODE_FRAME_MARGIN: f32 = 6.0;

/// The module's frame as the canvas last drew it, shrunk to what's inside the frame.
///
/// The glyph is placed against this rather than against the body's own area: the body
/// is only as wide as its content happens to be, and its sides are fenced in by pin
/// labels of different widths — `a` and `b` on one side, `out` on the other — so a mark
/// centred in the body sits visibly off the module's centre. One frame of lag on a
/// module drawn for the first time, which is a frame where nothing is drawn anyway.
pub(crate) fn module_inner(ui: &egui::Ui, view_salt: u64, node: NodeId) -> Option<egui::Rect> {
    let map = ui.ctx().data(|d| d.get_temp::<NodeRectMap>(node_rects_id(view_salt)))?;
    let [x, y, w, h] = *map.get(&node.0)?;
    let frame = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h));
    Some(frame.shrink(NODE_FRAME_MARGIN))
}

/// The square a glyph is drawn in: centred on the module (`inner`), in the band that
/// starts under whatever its body has already drawn (`band_top`) and ends at the frame,
/// as large as that band and the module's width allow up to one block — or nothing at
/// all once the band is too small to read.
pub(crate) fn glyph_rect(inner: egui::Rect, band_top: f32) -> Option<egui::Rect> {
    let band = egui::Rect::from_min_max(egui::pos2(inner.left(), band_top), inner.max);
    let side = (inner.width() - 2.0 * PAD)
        .min(band.height() - 2.0 * PAD)
        .min(MAX_SIDE);
    if side < MIN_SIDE {
        return None;
    }
    Some(egui::Rect::from_center_size(
        egui::pos2(inner.center().x, band.center().y),
        egui::vec2(side, side),
    ))
}

/// Paint the module's glyph, centred on the module, into the band its pin rows and
/// controls leave over. Nothing is allocated: the module's size comes from its header
/// and its pins, and the glyph fits itself into the gap left over — which is why adding
/// inputs makes the glyph smaller rather than pushing the module out to another block.
///
/// `inner` is the module's frame area from [`module_inner`]. Without it, the body's own
/// area is the best there is, and the mark lands a little to one side for a frame.
///
/// `band_top` is where the mark may start. The caller says, because only the caller
/// knows whether what it reserved is space *for* the mark (Abs, NOT: reserved so a
/// one-row module has a band at all) or space taken away from it (the comparisons'
/// toggle). Deriving it from the cursor gets that wrong for the first case: the
/// reservation the mark exists for is then the very thing that leaves no room for it.
pub(crate) fn paint_glyph(
    ui: &mut egui::Ui,
    node: NodeId,
    module_id: &str,
    or_equal: bool,
    inner: Option<egui::Rect>,
    band_top: f32,
) {
    let Some(svg) = icon_svg(module_id, or_equal) else { return };
    // The x comes from the module, not from `available_rect_before_wrap`: a control laid
    // out across the row leaves the cursor at the row's right edge, and the width there
    // reads as zero.
    let max = ui.max_rect();
    let Some(rect) = glyph_rect(inner.unwrap_or(max), band_top) else { return };
    // Leave a trace of what was drawn. Nothing reads it at runtime; without it, a mark
    // that silently stopped being painted — a band that shrank under the minimum size —
    // would be invisible to the tests, and that is exactly the failure that bit Abs and
    // NOT once already.
    ui.ctx().data_mut(|d| {
        d.insert_temp(egui::Id::new(("module_glyph_side", node.0)), rect.width() as i32)
    });
    // Identity rather than content: the glyph is the weakest thing in the module, so
    // the header and the pin labels stay the parts you read.
    let tint = ui.visuals().weak_text_color();
    // Rasterized at twice the size it's drawn at, so zooming in to the canvas's 2x
    // limit still lands on pixels rather than an upscaled blur. Cached per size step.
    let px = ((rect.width() * 2.0 * ui.ctx().pixels_per_point()) as u32).clamp(32, 512);
    let key = egui::Id::new(("module_glyph", svg.as_ptr() as usize, px, tint.to_array()));
    let tex = crate::widgets::tex_cache::get(ui.ctx(), key).or_else(|| {
        let img = rasterize_svg_recolored(svg, px, px, "override", tint)?;
        let handle = ui.ctx().load_texture(
            format!("module_glyph_{:p}_{px}", svg.as_ptr()),
            img,
            egui::TextureOptions::LINEAR,
        );
        crate::widgets::tex_cache::put(ui.ctx(), key, handle.clone());
        Some(handle)
    });
    if let Some(tex) = tex {
        ui.painter().image(
            tex.id(),
            rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }
}

#[cfg(test)]
mod glyph_tests {
    use super::*;

    // Every asset is read in at build time, so what is worth checking here is that
    // each one is drawn the way the set is drawn: stroked rather than filled, round
    // tips, a 3-point stroke, and the 59x59 box the rest of the set shares. (A drawing
    // exported under a stretch carries the stroke that comes out at 3 - DIVIDE's is 2.83 -
    // so the weight is held to a hair either side rather than to the digit.)
    #[test]
    fn every_glyph_is_stroked_in_the_same_box() {
        for (id, svg) in ICONS {
            assert!(svg.contains("viewBox=\"0 0 59 59\""), "{id}: not the shared 59x59 box");
            assert!(svg.contains("stroke-linecap:round"), "{id}: the tips are not round");
            let weights: Vec<f32> = svg
                .split("stroke-width:")
                .skip(1)
                .filter_map(|rest| rest.split("px").next().and_then(|n| n.trim().parse().ok()))
                .collect();
            assert!(
                !weights.is_empty() && weights.iter().all(|w| (2.8..=3.2).contains(w)),
                "{id}: the stroke is not 3 points: {weights:?}"
            );
            assert!(svg.contains("fill:none"), "{id}: filled rather than stroked");
            assert!(usvg::Tree::from_str(svg, &usvg::Options::default()).is_ok(), "{id}: does not parse");
        }
    }

    // The rasterizer the canvas draws through: the asset has strokes in it, and the
    // tint reaches them.
    #[test]
    fn glyphs_rasterize_and_take_a_tint() {
        let tint = egui::Color32::from_rgb(200, 60, 60);
        let img = rasterize_svg_recolored(AND, 64, 64, "override", tint).expect("AND rasterizes");
        let painted = img.pixels.iter().filter(|p| p.a() > 0).count();
        assert!(painted > 200, "only {painted} pixels drawn: the glyph is missing");
        let wrong = img.pixels.iter()
            .filter(|p| p.a() > 250)
            .filter(|p| {
                (p.r() as i32 - 200).abs() > 8
                    || (p.g() as i32 - 60).abs() > 8
                    || (p.b() as i32 - 60).abs() > 8
            })
            .count();
        assert_eq!(wrong, 0, "the tint did not reach the stroke");
    }

    #[test]
    fn a_glyph_is_centred_on_the_module_not_on_the_body() {
        // A module two blocks wide, its body area fenced in by pin labels: `a`/`b` on
        // the left, the wider `out` on the right, so the body is off to one side. The
        // glyph ignores that and centres on the module.
        let module = egui::Rect::from_min_max(egui::pos2(112.0, 294.0), egui::pos2(224.0, 406.0));
        let inner = module.shrink(NODE_FRAME_MARGIN);
        let band_top = 343.0; // under the pin rows and the toggle
        let r = glyph_rect(inner, band_top).expect("there is room to read");
        assert_eq!(r.center().x, inner.center().x, "{r:?} is not on the module's centre");
        // The body's own idea of the middle is a good deal to the left: that's the bug.
        let body = egui::Rect::from_min_max(egui::pos2(113.2, 325.0), egui::pos2(176.5, 398.0));
        assert!(body.center().x + 10.0 < r.center().x, "{body:?} vs {r:?}: no offset to correct");
        // Big as the band allows, capped at one block, and squared.
        let tall = glyph_rect(inner, inner.top()).expect("a band the height of the module");
        assert_eq!(tall.width(), MAX_SIDE, "{tall:?} grew past a block");
        assert_eq!(tall.width(), tall.height(), "{tall:?} is not square");
        // And once the pins have eaten into the band, nothing is drawn at all: a glyph
        // too small to read is noise.
        assert!(glyph_rect(inner, inner.bottom() - 20.0).is_none());
    }

    // The sets the glyph is keyed by have to stay in step with the modules that ask
    // for one, and the comparisons need both of their forms.
    #[test]
    fn the_table_covers_the_modules_and_both_comparison_forms() {
        for (id, _) in ICONS {
            assert!(has_icon(id), "{id} is not known by its own table");
        }
        assert!(has_icon("math.abs"), "Abs has no glyph");
        for id in ["logic.and", "logic.nand", "logic.or", "logic.nor", "logic.xor", "logic.xnor", "logic.not"] {
            assert!(has_icon(id), "{id} has no glyph");
        }
        assert!(!has_icon("logic.counter"), "Counter is not a mark, it has controls");
        for id in ["logic.greater_than", "logic.less_than"] {
            let strict = icon_svg(id, false);
            assert!(strict.is_some(), "{id}: no strict glyph");
            assert_ne!(icon_svg(id, true), strict, "{id}: or-equal shows the strict glyph");
            assert!(icon_svg(id, true).is_some(), "{id}: no or-equal glyph");
        }
        assert_eq!(icon_svg("math.add", true), icon_svg("math.add", false), "or_equal means nothing to Add");
        assert_eq!(icon_svg("module.delay", false), None, "Delay has no glyph");
    }
}
