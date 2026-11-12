//! The module body kit: the one layout module bodies are built from, so every
//! module reads the same way and sizes by the canvas grid's blocks.
//!
//! The rules it implements are in `docs/MODULE_UI_GUIDELINES.md`. In short:
//!
//! - A body is a column of rows, each one label cell wide on the left, then
//!   its control, ending on the body's shared right edge.
//! - A row has at most one flexible part (a slider rail, a choice strip) that
//!   takes the width left over; everything else is a fixed cell, so the cells
//!   of every row line up.
//! - Labels are small and weak; values sit in fixed-width boxes at the right
//!   edge; sections are captioned with a hairline, not boxed.
//! - The body's width is handed to it ([`body_width`]): it fills the node,
//!   whose width is the narrowest the rows allow rounded up to whole blocks,
//!   or the width the user dragged it to. The body reports exactly that width
//!   whatever its rows draw ([`body`]), so nothing feeds back.

use std::ops::RangeInclusive;

use super::*;

/// The label cell every row starts with.
pub(crate) const LABEL_W: f32 = 64.0;
/// The editable value box at a row's right edge.
pub(crate) const VALUE_W: f32 = 50.0;
/// Space between the cells of a row.
pub(crate) const GAP: f32 = 4.0;
/// Space between rows.
/// Space above a section caption.
#[allow(dead_code)] // As `Kit::section`.
pub(crate) const SECTION_GAP: f32 = 6.0;
/// The narrowest a row's flexible part gets.
pub(crate) const MIN_FLEX: f32 = 52.0;
/// The narrowest body: a label, the narrowest rail, and a value box.
pub(crate) const MIN_W: f32 = LABEL_W + GAP + MIN_FLEX + GAP + VALUE_W;

/// The params key holding the frame width the user dragged a kit node to.
pub(crate) const WIDTH_PARAM: &str = "ui_w";
/// The params key holding the frame height the user dragged a stretchy node to.
pub(crate) const HEIGHT_PARAM: &str = "ui_h";

/// Modules that take the corner grip but aren't built from kit rows: their body is one
/// surface that fills whatever frame it's given — a Text module's field — rather than a
/// stack of rows wanting a wider label column. Same grip, same block snapping, and the
/// height is theirs to use as well as the width.
pub(crate) fn stretchy(module_id: &str) -> bool {
    matches!(module_id, "module.label" | "display.readout" | "module.knob")
}

/// How tall a stretchy body is this frame. The mirror of [`body_width`] with one
/// difference that matters: the requested height is a **floor**, never a bound. A body
/// that runs out of room grows itself another block — a Text field with more lines than
/// fit — and the grip has to let that stand rather than pull the frame back to the
/// number last dragged.
pub(crate) fn body_height(
    node: &NodeData,
    avail: f32,
    frame_h: Option<f32>,
    min_h: f32,
    block: Option<f32>,
    default: Option<f32>,
) -> f32 {
    let whole = |w: f32| block.map_or(w, |b| ((w - 0.5) / b).ceil().max(1.0) * b);
    let wanted = node
        .params
        .get(HEIGHT_PARAM)
        .and_then(|v| v.as_f64())
        .map(|h| whole(h as f32))
        .or_else(|| default.map(whole));
    let Some(frame_h) = frame_h else { return wanted.unwrap_or(avail).max(min_h).floor() };
    let around = frame_h - avail;
    let frame = wanted.unwrap_or(frame_h).max(min_h);
    (frame - around).max(min_h).floor()
}

/// Bodies that are a mark and a couple of pills, painted across the frame and reserving
/// nothing of their own: the small operations and gates (which `module_icons::has_icon`
/// covers), plus the Switch and Sample and Hold, which draw their own two-state mark rather
/// than a glyph. They take their height from the grid rather than from their content, which
/// is why [`super::FlexViewer::show_body`] has to say that height out loud - and why nothing
/// else belongs here: a body that lays its own rows out reacts to the room it's given, and a
/// reservation it reacts to can feed its own width back into the frame.
pub(crate) fn mark_body(module_id: &str) -> bool {
    super::module_icons::has_icon(module_id)
        || matches!(
            module_id,
            "module.switch" | "module.sample_hold" | "math.negate" | "logic.has_changed"
        )
}

/// Modules whose bodies are built from the kit: they get a width grip, and
/// their body width comes from [`body_width`]. Only modules with something in
/// them that can take a wider frame belong here — a grip that grows a frame
/// nothing stretches into is worse than no grip.
/// Which modules get a width from the kit (`body_width`) and the corner grip that sets it.
/// A module whose body allocates nothing — Quantize, Inverse, Logic Delay, Has Changed,
/// Sample and Hold, Vec to Deflection: a mark and pills painted across the frame — isn't in
/// it: there is no row layout to give room, so a grip would drag a handle that does nothing.
/// The modules whose body is a stack of labelled kit rows, and so needs a width the rows
/// can lay out in: they take the corner grip and a floor the rows work out. The per-channel
/// filters used to be here; their rows were two sliders and a channel count, and the labels
/// on them were the whole reason a Delay was 252 points wide for one number. They are pills
/// now, sized by a constant box like the other small bodies, so they size themselves.
pub(crate) fn uses_kit(module_id: &str) -> bool {
    matches!(module_id, "logic.counter")
}

/// The modules whose channels are their content: a matched In/Out pin pair each, added and
/// dropped by the `− +` at the right of their header (see `show_header`) rather than a row
/// across the body. The range is what `channels_row` allowed when the stepper lived in the
/// body; the names it gives the pins are unchanged, since a patch stores them.
pub(crate) fn channel_range(module_id: &str) -> Option<std::ops::RangeInclusive<usize>> {
    match module_id {
        "module.delay" | "module.average" | "module.dc_filter" => Some(1..=64),
        _ => None,
    }
}

/// The modules small enough that a block of width matters. Their pins are drawn in short
/// form on the canvas — the name the patch stores is never touched — with the full name on
/// hover. A bigger module with room to spare keeps its labels spelled out.
pub(crate) fn pin_abbreviates(module_id: &str) -> bool {
    matches!(
        module_id,
        "math.add"
            | "math.subtract"
            | "math.multiply"
            | "math.divide"
            | "math.min_max"
            | "math.abs"
            | "math.negate"
            | "math.clamp"
            | "math.map_range"
            | "math.quantize"
            | "logic.and"
            | "logic.or"
            | "logic.not"
            | "logic.xor"
            | "logic.nand"
            | "logic.nor"
            | "logic.xnor"
            | "logic.equal"
            | "logic.not_equal"
            | "logic.greater_than"
            | "logic.less_than"
            | "logic.delay"
            | "logic.has_changed"
            // Six functional pin words, fixed by the module and the same on every canvas:
            // spelled out, "increase" and "decrease" are what hold this module wide.
            | "logic.counter"
            | "module.switch"
            | "module.split"
            | "module.selector"
            | "module.knob"
            | "module.sample_hold"
            | "module.vec_to_axis"
            | "module.axis_to_vec"
            | "module.vec_to_deflection"
            | "module.delay"
            | "module.average"
            | "module.dc_filter"
    )
}

/// The short form a pin's name is drawn as, and whether it was shortened at all. A
/// numbered generic pin comes down to its number - the column it sits in already says
/// in or out, so the number is everything the label adds. The kit's common words have
/// fixed short forms; anything else longer than four letters loses its tail.
pub(crate) fn pin_short(name: &str) -> (String, bool) {
    // A numbered generic pin, in either of the two ways the app writes it: `in_2` from the
    // steppers, `In 2` from the per-channel filters that add a matched pair. The column it
    // sits in already says in or out, so the number is everything the label adds.
    if let Some((base, index)) = name.split_once(['_', ' ']) {
        let digits = !index.is_empty() && index.chars().all(|c| c.is_ascii_digit());
        if digits && matches!(base, "in" | "out") {
            return (index.to_string(), true);
        }
    }
    const SHORT: &[(&str, &str)] = &[
        ("direct", "dir"),
        ("latch", "lat"),
        ("select", "sel"),
        ("changed", "chg"),
        ("increased", "inc"),
        ("decreased", "dec"),
        // The Counter's inputs are verbs, and they are the widest thing in it: at 252 points
        // the module is mostly the words "increase" and "decrease" standing beside six pins.
        ("increase", "inc"),
        ("decrease", "dec"),
        ("reset", "rst"),
        ("deflection", "def"),
        ("angle", "ang"),
        ("factor", "fac"),
        ("trigger", "trig"),
        ("hysteresis", "hys"),
        ("orientation", "ori"),
        ("position", "pos"),
        ("rotation", "rot"),
        ("velocity", "vel"),
        ("strength", "str"),
        ("duration", "dur"),
    ];
    if let Some((_, short)) = SHORT.iter().find(|(word, _)| word.eq_ignore_ascii_case(name)) {
        return (short.to_string(), true);
    }
    if name.chars().count() > 4 {
        let keep: String = name.chars().take(4).collect();
        return (keep, true);
    }
    (name.to_string(), false)
}

/// Modules whose output labels are not drawn at all: the body says what comes out.
pub(crate) fn pin_hides_outputs(module_id: &str) -> bool {
    matches!(module_id, "module.knob")
}

/// How wide a kit body is this frame. `avail` is the width the node gives the
/// body this frame (its size less the pin columns), `frame_w` the node's frame
/// width this frame, `min_w` the narrowest the body's rows allow, `block` the
/// grid's block when nodes snap to it.
///
/// The body always fills its node. The node is as wide as the user dragged it
/// (`ui_w`), or as it already is, but never narrower than the narrowest frame
/// the body fits in, rounded up to whole blocks: asking for less gets that
/// frame, filled, rather than a body squeezed inside a frame that can't shrink.
/// The body reports exactly this width whatever its rows draw (see [`body`]),
/// so the node settles on it.
pub(crate) fn body_width(
    node: &NodeData,
    avail: f32,
    frame_w: Option<f32>,
    min_w: f32,
    block: Option<f32>,
    default: Option<f32>,
) -> (f32, f32) {
    let Some(frame_w) = frame_w else { return (avail.max(min_w).floor(), min_w) };
    let whole = |w: f32| block.map_or(w, |b| ((w - 0.5) / b).ceil().max(1.0) * b);
    // Everything around the body: the pin columns, their spacing, the frame.
    let around = frame_w - avail;
    let narrowest = whole(around + min_w);
    // A body with something to say about its width before anyone drags it (`default`)
    // starts there rather than at its own floor, so a note opens as a note and not as a
    // column of one-word lines.
    let wanted = node
        .params
        .get(WIDTH_PARAM)
        .and_then(|v| v.as_f64())
        .map(|w| whole(w as f32))
        .or_else(|| default.map(whole));
    // A kit module nobody has dragged starts at its own floor, not at the frame size it was
    // handed. The body reports back the width it's given, so a body born into a default frame
    // keeps that frame for ever: the Counter came out 252 points wide for a strip of words and
    // three numbers because that is what an untouched node starts with, not because anything
    // in it asked. What the cells need is the answer (`narrowest`), and the grip above still
    // overrules it.
    let frame = wanted.unwrap_or(narrowest).max(narrowest);
    // The second value is the floor the *grip* is held to. It's worked out from what the
    // body needs, never from the frame as it stands — a floor that follows the current
    // size is a ratchet: the node grows and can never come back.
    ((frame - around).max(min_w).floor(), narrowest)
}

/// Where a group of controls `h` tall starts, so it reads as centred.
///
/// Horizontally the centre is the module's (`module_icons::module_inner`): the body
/// column is fenced in by pin columns of different widths, so its middle is not the
/// module's. Vertically it is the body column - what's under the header. Centring a
/// stack on the module instead parks it high, with all the empty space below the
/// controls, which reads as uncentred even though the arithmetic is symmetric. The two
/// differ because the module's width is measured against its pin labels and its height
/// against its header; see `docs/MODULE_UI_GUIDELINES.md`.
pub(crate) fn group_top(column: egui::Rect, h: f32) -> f32 {
    column.center().y - h * 0.5
}

fn row_h(ui: &egui::Ui) -> f32 {
    ui.spacing().interact_size.y
}

/// The options as a strip of equal cells `w` wide, for a row's flexible part.
pub(crate) fn strip<T: PartialEq + Clone>(
    ui: &mut egui::Ui,
    w: f32,
    value: &mut T,
    options: &[(T, &str, &str)],
) -> bool {
    let n = options.len().max(1) as f32;
    let cell = ((w - GAP * (n - 1.0)) / n).max(8.0);
    let mut changed = false;
    for (i, (option, text, hover)) in options.iter().enumerate() {
        if i > 0 {
            ui.add_space(GAP);
        }
        let selected = *value == *option;
        let mut resp = ui.add_sized(
            [cell, row_h(ui)],
            egui::Button::selectable(selected, egui::RichText::new(*text).small()),
        );
        if !hover.is_empty() {
            resp = resp.on_hover_text(*hover);
        }
        if resp.clicked() && !selected {
            *value = option.clone();
            changed = true;
        }
    }
    changed
}

// ── The header's stepper ──────────────────────────────────────────────────────

/// The modules whose pins the user counts out, and how many they may have: their
/// `− +` sits in the node's header ([`header_stepper`], drawn by
/// `FlexViewer::show_header`) and they draw no body at all.
///
/// A module whose content is a row of controls wants a width to lay them out in. A
/// module whose content *is* its pins wants no body at all: the buttons move into the
/// header it already has, and its height is then exactly its header plus its pin rows,
/// which the stepper adds and takes away a row at a time. What the header pays for
/// them is width — the row keeps a box for the buttons beside the name — and that is
/// the part of the trade worth knowing before adding a module to this list.
///
/// The logic gates are deliberately not here: the engine reads two inputs from them
/// (`crates/engine/src/eval/compute.rs`), so there is no count for a stepper to set.
///
/// The most inputs is where `pin_letter` runs out of single letters: past `z` a new
/// pin would be named `in_26`, and a module that tall has outgrown the canvas anyway.
pub(crate) fn variadic_inputs(module_id: &str) -> Option<RangeInclusive<usize>> {
    match module_id {
        "math.add" | "math.subtract" | "math.multiply" | "math.divide" | "math.min_max" => {
            Some(2..=26)
        }
        _ => None,
    }
}

/// How tall [`header_stepper`]'s buttons are, and how wide the pair is: two squares
/// with [`GAP`] between them. The header reserves this much beside its title, so the
/// two can't overlap; the stepper draws against the header's own right edge, so it
/// reads as right-aligned whenever the node is wider than that.
pub(crate) fn stepper_h(ui: &egui::Ui) -> f32 {
    row_h(ui)
}

pub(crate) fn stepper_w(ui: &egui::Ui) -> f32 {
    2.0 * stepper_h(ui) + GAP
}

/// The header's `− +`: [`Kit::stepper`]'s two buttons without the count between
/// them, because on these modules the pins *are* the count.
///
/// `row` is the box the header reserved for it and `right` the header's right edge.
/// It draws in a child of `ui` instead of in the header's own row so that placing it
/// at the right edge can't make the header measure that wide: the child takes the
/// clicks, the header still reports only what its contents need, and a node is never
/// locked at a width it once happened to have.
///
/// Inside that box the two squares are put at rects worked out here, not laid out in
/// a row: a row's own item spacing and fit rules decide where the second widget
/// lands, and that is not the measurement the box was reserved with — on the canvas
/// it landed the `+` over the node's frame border. Returns the step the user asked
/// for.
pub(crate) fn header_stepper(
    ui: &mut egui::Ui,
    count: usize,
    range: RangeInclusive<usize>,
    hover: (&str, &str),
    row: egui::Rect,
    right: f32,
) -> Option<isize> {
    let h = stepper_h(ui);
    let w = stepper_w(ui);
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(egui::Rect::from_min_max(egui::pos2(right - w, row.top()), egui::pos2(right, row.bottom())))
            .id_salt("pin_stepper"),
    );
    let (plus, minus) = (
        egui::Rect::from_min_max(egui::pos2(right - h, row.top()), egui::pos2(right, row.bottom())),
        egui::Rect::from_min_max(egui::pos2(right - w, row.top()), egui::pos2(right - w + h, row.bottom())),
    );
    let mut step = None;
    let r = child.add_enabled_ui(count > *range.start(), |ui| {
        ui.put(minus, egui::Button::new("−").min_size(egui::vec2(h, h)))
    }).inner;
    if r.on_hover_text(hover.0).clicked() {
        step = Some(-1);
    }
    let r = child.add_enabled_ui(count < *range.end(), |ui| {
        ui.put(plus, egui::Button::new("+").min_size(egui::vec2(h, h)))
    }).inner;
    if r.on_hover_text(hover.1).clicked() {
        step = Some(1);
    }
    step
}

/// The vertical padding of a Combiner port row's drop frame. The row's height is its
/// widgets plus twice this, on both sides of the arithmetic: the body sets its widgets to
/// that height and the pin row is allocated the same, which is the only way they stay level
/// - the pins are laid out before the body is drawn, so neither can measure the other.
pub(crate) const COMBINER_PAD_V: f32 = 3.0;

/// The modules whose ports are a column of strips in the body, and the height of a strip -
/// which is also the height a pin row is drawn at, so each socket sits level with the strip
/// that names it. Their numbered pins drop the label beside the socket: the strip says the
/// same word, and a word twice over is what made these modules wide for nothing.
/// The smallest pitch a strip - and the pin row beside it - is drawn at.
pub(crate) fn strip_min_pitch(module_id: &str, ui: &egui::Ui) -> Option<f32> {
    match module_id {
        "module.selector" | "module.split" => Some(super::simple_bodies::STRIP_H),
        "module.automap_combiner" => Some(ui.spacing().interact_size.y + 2.0 * COMBINER_PAD_V),
        _ => None,
    }
}

/// How many bands a module's port list occupies: the strips, plus any band of its own a pin
/// or setting takes (a Selector's `select` row; a Split's interpolate toggle shares the first
/// strip band, so it costs no band). Both sides need this number to agree on the pitch.
pub(crate) fn strip_bands(module_id: &str, inputs: usize, outputs: usize) -> usize {
    match module_id {
        "module.selector" | "module.automap_combiner" => inputs,
        "module.split" => outputs.max(inputs),
        _ => 0,
    }
}

/// The height a pin row in a strip list is **allocated**. In a `Coil` layout the column adds
/// its own row spacing on top of what each row asks for - and it decides the row heights from
/// the *other* side's rows, which is why the strip bodies can't just measure their own column
/// - so the strips are painted at `strip_pitch + spacing`: the row allocation plus what the
/// column puts between rows. Get this wrong on either side and the sockets drift off the
/// strips that name them by the spacing per row, and the list stops reaching the bottom of the
/// body it was given: the pins' column, being taller than the strips' reservation, sets the
/// module's height and the leftover lands under the last strip, widening with every port.
///
/// The pitch is deliberately a constant rather than derived from the frame the module happens
/// to have. Filling the frame needs two numbers the layout doesn't hand over honestly: what
/// the pin column will actually cost (its rows come from the other side's *stored* heights, a
/// frame behind, so a new pitch is applied to the frame after the one it was chosen for), and
/// the node's own overhead - the inner height minus the pin height measured from the stored
/// state swung between 45 and 93 points across frames of one module, because the body can be
/// taller than the pins beside it. A three-point error there tips the snapped height a whole
/// block, which is worse than the constant margin this buys back. A constant half-block pitch
/// can't make the leftover tight at both parities of port count; what it does guarantee is
/// that the leftover is the same size every time.
pub(crate) fn strip_pitch(module_id: &str, _bands: usize, ui: &egui::Ui) -> Option<f32> {
    strip_min_pitch(module_id, ui)
}

/// The pitch the strips are painted at: a row and the spacing the pin column adds between
/// rows. See [`strip_pitch`].
pub(crate) fn strip_advance(module_id: &str, ui: &egui::Ui) -> Option<f32> {
    Some(strip_min_pitch(module_id, ui)? + ui.style().spacing.item_spacing.y)
}

/// Whether this pin is one the module draws as a strip in its body, and therefore draws
/// no label beside its socket. **Positional, not by name**: a strip's name field renames
/// the pin, and the port it names is still the same port - a rule keyed to `in_0` would
/// leave a port called `kick` saying its name twice.
pub(crate) fn strip_pin(module_id: &str, input: bool, index: usize) -> bool {
    match (module_id, input) {
        // A Selector's first input is its `select` pin, which has a row of its own.
        ("module.selector", true) => index >= 1,
        ("module.split", false) => true,
        ("module.automap_combiner", true) => true,
        _ => false,
    }
}

/// The modules whose ports the user adds from the header - a single `+` at its right edge,
/// because dropping a port is the `×` on its strip, which knows which one it belongs to -
/// and the noun for what `+` adds.
pub(crate) fn header_plus(module_id: &str) -> Option<&'static str> {
    match module_id {
        "module.selector" => Some("input"),
        "module.split" => Some("output"),
        "module.automap_combiner" => Some("input"),
        _ => None,
    }
}

/// One square at the header's right edge, in a child for the reason [`header_stepper`]
/// gives: the clicks are taken in the child, the header still reports only what its
/// contents need. Returns whether it was clicked.
pub(crate) fn header_plus_button(ui: &mut egui::Ui, hover: &str, row: egui::Rect, right: f32) -> bool {
    let h = stepper_h(ui);
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(egui::Rect::from_min_max(egui::pos2(right - h, row.top()), egui::pos2(right, row.bottom())))
            .id_salt("header_plus"),
    );
    let plus = egui::Rect::from_min_max(egui::pos2(right - h, row.top()), egui::pos2(right, row.bottom()));
    let resp = child.put(plus, egui::Button::new("+").min_size(egui::vec2(h, h)));
    resp.on_hover_text(hover).clicked()
}

// ── The mark a module is known by ─────────────────────────────────────────────

/// Whether a header's title fits `room` points, once `extra` of them are held back for
/// what sits at the header's right edge (`uni`/`bi` on the Knob, the stepper on the
/// variadic operations).
///
/// `room` is the node's frame width less its margins - measured from the node's layout
/// state, because the header's own UI is laid out in a box the size of its contents, so
/// its `available_width` says nothing about the frame the title has to live in.
///
/// A title that doesn't fit is left out rather than cropped in half: the header then
/// reports only what it actually holds, so the module's narrowest frame comes from its
/// pins and its controls instead of from a word that was once drawn there. Widen the
/// module and the name comes back. The alternative - letting the name set the floor - is
/// what makes a small module wider than anything in it needs.
pub(crate) fn title_fits(ui: &egui::Ui, title: &str, extra: f32, room: f32) -> bool {
    if title.is_empty() {
        return true;
    }
    let font = egui::TextStyle::Body.resolve(ui.style());
    let w = ui
        .painter()
        .layout_no_wrap(title.to_string(), font, egui::Color32::PLACEHOLDER)
        .size()
        .x;
    w + extra <= room
}

/// Modules that read better as a symbol than as a word: (module id, the name it
/// ships with, the mark). A header shows the mark where that name would be, and the
/// name in the hover text. It does two things at once: the row shortens enough that
/// a small module stays inside two blocks even when the same row carries a `− +`,
/// and a patch is still readable zoomed out past the point where words are.
///
/// AND, OR, NOT and XOR are deliberately absent: they're already shorter than the
/// symbols they'd be drawn as, and `∧ ∨ ¬ ⊕` ask more of the reader than they give
/// back. So are Equal and Not Equal: their mark would be one glyph wide, which is
/// under the smallest footprint a module keeps on the grid, and they'd end up
/// narrower than the gates they sit beside. So is Inverse: `±` reads as arithmetic
/// rather than as "negate", and its name fits the two blocks its pins need anyway. A
/// module the user renamed keeps the name they gave it.
pub(crate) const MARKS: &[(&str, &str, &str)] = &[
    ("math.add", "Add", "+"),
    ("math.subtract", "Subtract", "−"),
    ("math.multiply", "Multiply", "×"),
    ("math.divide", "Divide", "÷"),
    ("math.min_max", "Min/Max", "↕"),
    ("logic.greater_than", "Greater Than", ">"),
    ("logic.less_than", "Less Than", "<"),
    // Two letters rather than a name: this module's header also carries its mode button,
    // and "Sample and Hold" plus a button is wider than the two blocks its pins need.
    ("module.sample_hold", "Sample and Hold", "S/H"),
    // A vector bent into an angle, and a pulse when something moved: both names cost a
    // block their pins don't need.
    ("module.vec_to_deflection", "Vec to Deflection", "∠"),
    ("logic.has_changed", "Has Changed", "Δ"),
];

/// The mark a node's header shows, or none: a module known by a symbol, still under
/// the name it shipped with. Comparisons fold `or_equal` into the mark, so the
/// header says `≥` when that's what the module means.
pub(crate) fn node_mark(node: &NodeData) -> Option<&'static str> {
    let (shipped_name, base) = MARKS
        .iter()
        .find(|(id, _, _)| *id == node.module_id)
        .map(|&(_, name, m)| (name, m))?;
    if node.display_name != shipped_name {
        return None;
    }
    let or_equal = node.params.get("or_equal").and_then(|v| v.as_bool()).unwrap_or(false);
    match (base, or_equal) {
        (">", true) => Some("≥"),
        ("<", true) => Some("≤"),
        _ => Some(base),
    }
}

#[cfg(test)]
mod mark_tests {
    use super::*;

    fn registered(id: &str) -> ModuleDescriptor {
        flexinput_modules::all_modules()
            .into_iter()
            .map(|r| r.descriptor)
            .find(|d| d.id == id)
            .unwrap_or_else(|| panic!("{id} is not registered"))
    }

    // A mark only shows while the node still carries the name it shipped with, so a
    // name changed in the registry would silently drop the mark. This keeps the two
    // from drifting apart.
    #[test]
    fn marks_match_the_registered_names() {
        for (id, shipped, mark) in MARKS {
            assert_eq!(registered(id).display_name, *shipped, "{id}: the name the mark expects");
            assert!(!mark.is_empty(), "{id}: no mark to draw");
        }
    }

    #[test]
    fn a_mark_covers_its_module_and_its_or_equal() {
        let mut node = NodeData::from(&registered("logic.greater_than"));
        assert_eq!(node_mark(&node), Some(">"), "the mark it ships under");
        node.params.insert("or_equal".into(), Value::Bool(true));
        assert_eq!(node_mark(&node), Some("≥"), "or_equal reads as ≥");
        node.display_name = "Loud enough?".into();
        assert_eq!(node_mark(&node), None, "a renamed module keeps the name it was given");
        assert_eq!(node_mark(&NodeData::from(&registered("math.add"))), Some("+"));
        assert_eq!(node_mark(&NodeData::from(&registered("logic.and"))), None, "the gates keep their words");
    }
}
