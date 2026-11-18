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
/// surface that fills whatever frame it's given — a Text module's field, an Envelope's
/// graph — rather than a stack of rows wanting a wider label column. Same grip, same block
/// snapping, and the height is theirs to use as well as the width.
/// Modules whose body is a fixed grid of cells: the corner grip is taken away, because
/// stretching them adds nothing the cells needed - a Counter at 336 points is the same six
/// cells with more air between them - and a module that can be any size is a module that
/// shows up at four sizes in one patch. What they get is the width their content asks for.
pub(crate) fn fixed_size(module_id: &str) -> bool {
    matches!(module_id, "logic.counter")
}

pub(crate) fn stretchy(module_id: &str) -> bool {
    matches!(
        module_id,
        "module.label" | "display.readout" | "module.knob" | "generator.envelope"
    ) || surface_spec(module_id).is_some()
}

/// A module whose body is one big surface - a graph, a scope, a picture - with rows of
/// settings around it. The surface is sized by the module's corner grip like everything
/// else on the canvas: these each used to carry a grip of their own in the surface's corner,
/// so a module had two sizes, one of them not on the grid and not in the patch.
#[derive(Clone, Copy)]
pub(crate) struct SurfaceSpec {
    /// The smallest the surface is drawn, whatever the grip says.
    pub min: egui::Vec2,
    /// The frame a new module opens at, in blocks.
    pub default_blocks: (f32, f32),
}

pub(crate) fn surface_spec(module_id: &str) -> Option<SurfaceSpec> {
    let spec = |min: (f32, f32), default_blocks: (f32, f32)| {
        Some(SurfaceSpec { min: egui::vec2(min.0, min.1), default_blocks })
    };
    match module_id {
        "display.oscilloscope" | "display.trigscope" => spec((160.0, 80.0), (15.0, 11.0)),
        "display.vectorscope" => spec((56.0, 56.0), (7.0, 7.0)),
        "module.response_curve"
        | "module.vec_response_curve"
        | "module.twoway_response_curve"
        | "module.automap_response_curve"
        | "module.automap_twoway_response_curve" => spec((120.0, 100.0), (12.0, 13.0)),
        "module.vec_reshape" => spec((140.0, 160.0), (11.0, 15.0)),
        "display.controller3d" => spec((80.0, 60.0), (11.0, 14.0)),
        // The 3DOF module's viewport: the gimbal and the output plane. Its rows of settings
        // and its two Lean card lists are what the surface shares the frame with.
        "processing.gyro_3dof" => spec((112.0, 84.0), (18.0, 26.0)),
        "module.audio_stream_haptics" => spec((180.0, 90.0), (12.0, 17.0)),
        "module.jsm" => spec((200.0, 120.0), (16.0, 12.0)),
        "module.touch_zones" => spec((200.0, 120.0), (17.0, 12.0)),
        // The calibration view: a ruler, a room, or both.
        "processing.rws" => spec((236.0, 80.0), (14.0, 4.0)),
        "module.menu" => spec((160.0, 100.0), (17.0, 11.0)),
        // A picture: as big as the grip makes it, kept to its own shape inside.
        "module.svg" => spec((32.0, 32.0), (6.0, 7.0)),
        "module.area_mapper" => spec((140.0, 140.0), (18.0, 27.0)),
        // Gyro to Stick Rotation's live circle.
        "module.stick_rotation" => spec((208.0, 96.0), (9.0, 14.0)),
        // The card lists: the width is the scale of the cards (380 points of body at 1:1),
        // the height the window onto the list.
        "module.remapper" | "module.map_action" => spec((150.0, 56.0), (16.0, 9.0)),
        _ => None,
    }
}

/// Modules whose surface has one shape whatever the frame: their width is their size.
pub(crate) fn keeps_aspect(module_id: &str) -> bool {
    matches!(module_id, "display.vectorscope")
}

/// The surface of a [`surface_spec`] module, this frame: as wide as the body it was handed
/// (never narrower than the rows beside it), and as tall as what the frame leaves once those
/// rows have had their height. The rows are flow widgets, so their size is only known once
/// they are laid out: it is measured each frame and used the next, and since no row's size
/// depends on the surface's it settles in one.
///
/// [`Surface::begin`] before anything is laid out, [`Surface::need`] with the rows' width,
/// [`Surface::end`] after the last row, with the same `ui`.
pub(crate) struct Surface {
    key: egui::Id,
    pub size: egui::Vec2,
    top: f32,
    rows_w: f32,
}

impl Surface {
    pub(crate) fn begin(ui: &egui::Ui, node: NodeId, body_w: f32, body_h: f32, min: egui::Vec2) -> Self {
        // Scoped to where it is drawn: node ids repeat between tabs and sub-patch editors.
        let key = egui::Id::new(("kit_surface", ui.ctx().viewport_id(), ui.layer_id(), node.0));
        let rows: egui::Vec2 = ui.ctx().data(|d| d.get_temp(key)).unwrap_or_default();
        Self {
            key,
            size: egui::vec2(body_w.max(rows.x).max(min.x), (body_h - rows.y).max(min.y)),
            top: ui.cursor().top(),
            rows_w: 0.0,
        }
    }

    /// Rows of settings laid out beside the surface, which fill whatever width they are
    /// given: the width they need is the narrowest the surface is drawn, since
    /// what they drew is the surface's own width and would hold it there.
    pub(crate) fn need(&mut self, width: f32) {
        self.rows_w = self.rows_w.max(width);
    }

    /// End a frame in which the surface was folded away and not drawn: what the body drew
    /// is all rows, so the surface gets the rest of the frame when it is unfolded again.
    pub(crate) fn end_folded(self, ui: &egui::Ui) {
        let bottom = ui.cursor().top().max(ui.min_rect().bottom());
        let rows_h = (bottom - self.top).max(0.0);
        ui.ctx().data_mut(|d| d.insert_temp(self.key, egui::vec2(self.rows_w, rows_h)));
    }

    pub(crate) fn end(self, ui: &egui::Ui) {
        // How far down the body got: the cursor in a column, the lowest thing drawn in a row
        // (a node's body is a row, and some bodies put a column of their own in it).
        let bottom = ui.cursor().top().max(ui.min_rect().bottom());
        let rows_h = (bottom - self.top - self.size.y).max(0.0);
        ui.ctx().data_mut(|d| d.insert_temp(self.key, egui::vec2(self.rows_w, rows_h)));
    }
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
        // The curves and the scopes: their "Ch + -" was a row at the bottom of the body.
        "module.response_curve" | "module.vec_response_curve" | "module.twoway_response_curve" => Some(1..=16),
        "display.oscilloscope" | "display.vectorscope" => Some(1..=16),
        // A Trigger Scope's first input is its trigger, which is not a channel.
        "display.trigscope" => Some(2..=17),
        _ => None,
    }
}

/// The pins one more channel adds to a [`channel_range`] module that has `n_inputs` inputs
/// already: an input, and for a module that passes its channels through, the output beside
/// it. The names and types are the ones each module's own `+` gave them, since a patch
/// stores them.
pub(crate) fn channel_pins(node: &NodeData) -> (PinDescriptor, Option<PinDescriptor>) {
    let next = node.inputs.len() + 1;
    let pair = |ty: SignalType| {
        (
            PinDescriptor::new(format!("In {next}"), ty),
            Some(PinDescriptor::new(format!("Out {next}"), ty)),
        )
    };
    match node.module_id.as_str() {
        "display.oscilloscope" => (PinDescriptor::new(format!("ch{next}"), SignalType::Float), None),
        // Counted without the trigger.
        "display.trigscope" => (PinDescriptor::new(format!("ch{}", next - 1), SignalType::Float), None),
        "display.vectorscope" => (PinDescriptor::new(format!("ch{next}"), SignalType::Vec2), None),
        "module.vec_response_curve" => pair(SignalType::Vec2),
        "module.twoway_response_curve"
            if node.params.get("vec_mode").and_then(|v| v.as_bool()).unwrap_or(false) =>
        {
            pair(SignalType::Vec2)
        }
        _ => pair(SignalType::Float),
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
            // Trigger / Time / Out: at three blocks wide the full words are what push the
            // pin columns apart, and the shorter form is the one used everywhere else.
            | "generator.envelope"
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
        // Map Range's four bounds: cut to four letters they were `in_m`, `in_m`, `out_`,
        // `out_` - two pairs of twins. An arrow says which end of which range.
        ("in_min", "in↓"),
        ("in_max", "in↑"),
        ("out_min", "out↓"),
        ("out_max", "out↑"),
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

/// What a pin's label says on the canvas, and whether that is short for its name. The
/// Response Curve family's channels are `i1 o1 i2 o2`: `In 1` and `Out 1` are different
/// widths, so the two columns either side of the graph never matched, and a curve's pins
/// need no more than which channel they are. The small modules take [`pin_short`]; the
/// rest keep their names.
pub(crate) fn pin_label(module_id: &str, name: &str) -> (String, bool) {
    let curve = matches!(
        module_id,
        "module.response_curve" | "module.vec_response_curve" | "module.twoway_response_curve"
    );
    if curve {
        let (word, n) = name.split_once(' ').unwrap_or((name, "1"));
        let digits = !n.is_empty() && n.chars().all(|c| c.is_ascii_digit());
        match (word, digits) {
            ("In", true) => return (format!("i{n}"), true),
            ("Out", true) => return (format!("o{n}"), true),
            _ => {}
        }
    }
    if pin_abbreviates(module_id) {
        pin_short(name)
    } else {
        (name.to_string(), false)
    }
}

/// An Auto-Map pin with no word beside it. The bus's socket is its own shape and colour, and
/// the words on it - `Device`, `AutoMap`, `in`, `out` - are different lengths on the two
/// sides of a module, which is all they added. The device nodes keep theirs (the word is
/// the device's side of the bus), and so does a sub-patch, whose ports the user names.
pub(crate) fn unlabelled_bus_pin(module_id: &str, ty: SignalType) -> bool {
    ty == SignalType::AutoMap && !module_id.starts_with("device.") && !module_id.starts_with("subpatch")
}

/// A bar across `rect` that resizes what is next to it: drag it up or down (or left and
/// right, with `vertical_drag` off), double-click to put it back. Returns the drag this
/// frame along its axis, and whether it was double-clicked.
pub(crate) fn resize_bar(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    id: impl std::hash::Hash,
    vertical_drag: bool,
    hover: &str,
) -> (f32, bool) {
    let resp = ui
        .interact(rect, egui::Id::new(("kit_resize_bar", id, ui.layer_id())), egui::Sense::click_and_drag())
        .on_hover_cursor(if vertical_drag { egui::CursorIcon::ResizeVertical } else { egui::CursorIcon::ResizeHorizontal })
        .on_hover_text(hover);
    let col = if resp.hovered() || resp.dragged() {
        ui.visuals().widgets.hovered.fg_stroke.color
    } else {
        ui.visuals().widgets.inactive.bg_fill
    };
    let c = rect.center();
    let half = if vertical_drag { egui::vec2(16.0, 0.0) } else { egui::vec2(0.0, 16.0) };
    ui.painter().line_segment([c - half, c + half], egui::Stroke::new(2.0_f32, col));
    let d = resp.drag_delta();
    (if resp.dragged() { if vertical_drag { d.y } else { d.x } } else { 0.0 }, resp.double_clicked())
}

/// Grow (or, negative, shrink) a surface module's frame by `dy` points, as its corner grip
/// would: what one part of the body is given then comes out of the frame, not out of the
/// surface. A frame never dragged starts from the module's default size in `block`s.
pub(crate) fn grow_frame(node: &mut NodeData, block: f32, dy: f32) {
    let current = node
        .params
        .get(HEIGHT_PARAM)
        .and_then(|v| v.as_f64())
        .map(|h| h as f32)
        .or_else(|| surface_spec(&node.module_id).map(|s| s.default_blocks.1 * block))
        .unwrap_or(0.0);
    if let Some(n) = serde_json::Number::from_f64(f64::from((current + dy).max(block))) {
        node.params.insert(HEIGHT_PARAM.into(), serde_json::Value::Number(n));
    }
}

/// Whether a bus socket is drawn on the header row: an unlabelled one ([`unlabelled_bus_pin`])
/// that is the only bus pin on its side, and not one of a strip module's numbered ports -
/// those stay level with the strip that names them. With no word beside it, a socket in the
/// pin column was a dot halfway down one side; on the header it is where it is when the
/// module is folded, and the input and output line up with each other. `side` is the
/// signal types of every pin on that side.
pub(crate) fn bus_pin_on_header(
    module_id: &str,
    strip_pin: bool,
    ty: SignalType,
    side: impl Iterator<Item = SignalType>,
) -> bool {
    unlabelled_bus_pin(module_id, ty) && !strip_pin && side.filter(|t| *t == SignalType::AutoMap).count() == 1
}

/// Two switches that belong together, as the two halves of one pill at `rect`: the left
/// half `left_w` wide, a hairline between them, each lit while its switch is on. Returns
/// whether each was clicked.
pub(crate) fn header_toggle_pair(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    left_w: f32,
    a: (&str, bool, &str),
    b: (&str, bool, &str),
    id_salt: impl std::hash::Hash + Copy,
) -> (bool, bool) {
    let split = rect.left() + left_w;
    let halves = [
        (egui::Rect::from_min_max(rect.min, egui::pos2(split, rect.bottom())), a, true),
        (egui::Rect::from_min_max(egui::pos2(split, rect.top()), rect.max), b, false),
    ];
    let font = super::simple_bodies::control_font();
    let mut clicked = [false, false];
    for (i, (half, (label, on, hover), left)) in halves.into_iter().enumerate() {
        let resp = ui
            .interact(half, egui::Id::new(("header_toggle_pair", id_salt, i)), egui::Sense::click())
            .on_hover_text(hover);
        let v = ui.visuals();
        let (fill, text) = if on {
            (v.selection.bg_fill, v.strong_text_color())
        } else if resp.hovered() {
            (v.widgets.hovered.weak_bg_fill, v.text_color())
        } else {
            (v.widgets.inactive.weak_bg_fill, v.text_color())
        };
        let r = 3u8;
        let corners = if left {
            egui::CornerRadius { nw: r, sw: r, ne: 0, se: 0 }
        } else {
            egui::CornerRadius { nw: 0, sw: 0, ne: r, se: r }
        };
        ui.painter().rect_filled(half, corners, fill);
        let fitted = super::simple_bodies::fit_font(ui, label, font.clone(), half.width() - 4.0);
        ui.painter().text(half.center(), egui::Align2::CENTER_CENTER, label, fitted, text);
        clicked[i] = resp.clicked();
    }
    ui.painter().line_segment(
        [egui::pos2(split, rect.top() + 2.0), egui::pos2(split, rect.bottom() - 2.0)],
        egui::Stroke::new(1.0_f32, ui.visuals().extreme_bg_color),
    );
    (clicked[0], clicked[1])
}

/// What hovering a pin's label says: the name it is short for, and the module at the other
/// end of each wire on it (`from` / `to`) with the pin there. Nothing when the label is the
/// whole name and nothing is wired.
pub(crate) fn pin_hover<'a>(
    name: &str,
    shortened: bool,
    way: &str,
    ends: impl Iterator<Item = (&'a str, &'a str)>,
) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    for (module, pin) in ends {
        lines.push(if pin.is_empty() { format!("{way} {module}") } else { format!("{way} {module} · {pin}") });
    }
    if !shortened && lines.is_empty() {
        return None;
    }
    lines.insert(0, name.to_string());
    Some(lines.join("\n"))
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

/// The modules whose ports are a column of strips in the body, and the height of a strip -
/// which is also the height a pin row is drawn at, so each socket sits level with the strip
/// that names it. Their numbered pins drop the label beside the socket: the strip says the
/// same word, and a word twice over is what made these modules wide for nothing.
/// The smallest pitch a strip - and the pin row beside it - is drawn at.
/// The name the port-strip rules know a node by: its module, or - for a MIDI port's device
/// node, a `device.source` / `device.sink` like any pad's, told apart by its device -
/// `device.midi_in` / `device.midi_out`, whose messages are strips.
pub(crate) fn strip_key(node: &crate::canvas::node::NodeData) -> &str {
    let dev = node.params.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
    match node.module_id.as_str() {
        "device.source" if dev.starts_with("midi_in:") => "device.midi_in",
        "device.sink" if dev.starts_with("midi_out:") => "device.midi_out",
        m => m,
    }
}

pub(crate) fn strip_min_pitch(module_id: &str, _ui: &egui::Ui) -> Option<f32> {
    match module_id {
        "device.midi_in" | "device.midi_out" | "module.selector"
        | "module.split"
        | "module.macro"
        | "module.automap_combiner"
        | "module.automap_fork"
        | "module.automap_selector"
        | "module.automap_split"
        | "module.automap_collect" => Some(super::simple_bodies::STRIP_H),
        _ => None,
    }
}

/// How many bands a module's port list occupies: the strips, plus any band of its own a pin
/// or setting takes (a Selector's `select` row; a Split's interpolate toggle shares the first
/// strip band, so it costs no band). Both sides need this number to agree on the pitch.
pub(crate) fn strip_bands(module_id: &str, inputs: usize, outputs: usize) -> usize {
    match module_id {
        "module.selector" | "module.automap_combiner" | "module.automap_selector" => inputs,
        "module.split" | "module.automap_fork" => outputs.max(inputs),
        // The bus pin's row (`add pin`) and one strip per port it took off the bus.
        "module.automap_split" => outputs,
        "module.automap_collect" => inputs,
        // A MIDI port's messages, its Auto-Map port on the header.
        "device.midi_in" => outputs,
        "device.midi_out" => inputs,
        // A Macro's ports are its outputs, one strip each; the body adds no band of its own,
        // the `+` being in the header.
        "module.macro" => outputs,
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

/// Whether a module's bus pin, drawn on its header, gives up its row in the pin column: the
/// AutoMap Splitter and Collector list the ports they take off the bus from the top of the
/// body, with `add pin` after them, so the bus has no row of its own to hold.
pub(crate) fn bus_row_collapsed(module_id: &str) -> bool {
    matches!(module_id, "module.automap_split" | "module.automap_collect")
}

/// Where a port list's rows are, for the body that draws them: the one place that knows how
/// the pin column lays its sockets out, so a list can't drift off the sockets it names.
///
/// The pin column starts at the body's top, gives each socket a row [`strip_pitch`] tall and
/// puts the column's spacing *after* it. So row `i` is the band from `top + i * advance`,
/// `row_h` tall, and the socket is at that band's centre - not at the centre of the whole
/// advance, and not wherever a list centred in the body would put it.
#[derive(Clone, Copy)]
pub(crate) struct StripRows {
    top: f32,
    /// From one row's top to the next: a row and the spacing under it.
    pub advance: f32,
    /// The height of a row itself - what its socket is centred in.
    pub row_h: f32,
}

impl StripRows {
    /// The rows starting at the body's cursor. Take this **before** the body allocates its
    /// list, while the cursor is still at the body's top.
    pub(crate) fn at_cursor(module_id: &str, ui: &egui::Ui) -> Self {
        let row_h = strip_min_pitch(module_id, ui).unwrap_or(super::simple_bodies::STRIP_H);
        Self {
            top: ui.cursor().top(),
            advance: row_h + ui.style().spacing.item_spacing.y,
            row_h,
        }
    }

    /// The rows after a pin whose row was collapsed to nothing ([`bus_row_collapsed`]): the
    /// column still puts its spacing after it.
    pub(crate) fn past_empty_row(mut self) -> Self {
        self.top += self.advance - self.row_h;
        self
    }

    /// The height `rows` of them reserve in the body.
    pub(crate) fn height(&self, rows: usize) -> f32 {
        rows.max(1) as f32 * self.advance
    }

    /// The y of row `i`'s socket.
    pub(crate) fn center_y(&self, row: usize) -> f32 {
        self.top + row as f32 * self.advance + self.row_h * 0.5
    }

    /// Row `i`'s band, between `left` and `left + width`.
    pub(crate) fn band(&self, row: usize, left: f32, width: f32) -> egui::Rect {
        egui::Rect::from_min_size(
            egui::pos2(left, self.top + row as f32 * self.advance),
            egui::vec2(width, self.row_h),
        )
    }
}

/// Whether this pin is one the module draws as a strip in its body, and therefore draws
/// no label beside its socket. **Positional, not by name**: a strip's name field renames
/// the pin, and the port it names is still the same port - a rule keyed to `in_0` would
/// leave a port called `kick` saying its name twice.
pub(crate) fn strip_pin(module_id: &str, input: bool, index: usize) -> bool {
    match (module_id, input) {
        // A Selector's first input is its `select` pin, which has a row of its own.
        ("module.selector" | "module.automap_selector", true) => index >= 1,
        ("module.split" | "module.automap_fork", false) => true,
        ("module.automap_combiner", true) => true,
        // The row already carries the port's name in a field that renames it.
        ("module.macro", false) => true,
        // The bus pin first, then the ports taken off it, each named on its strip.
        ("module.automap_split", false) | ("module.automap_collect", true) => index >= 1,
        // Every message of a MIDI port: its Auto-Map port is on the header, wherever it is
        // in the list (first on a node made since, last on one migrated), and the caller
        // leaves it out.
        ("device.midi_in", false) | ("device.midi_out", true) => true,
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
        "module.automap_combiner" | "module.automap_selector" => Some("input"),
        "module.automap_fork" => Some("output"),
        _ => None,
    }
}

/// One square at the header's right edge, in a child for the reason [`header_stepper`]
/// gives: the clicks are taken in the child, the header still reports only what its
/// contents need. Returns whether it was clicked.
/// A small word button against a header's right edge, in the same squares as the stepper's
/// and the `+`'s: filled while the switch it stands for is on. Returns whether it was
/// clicked. Options that belong to a module's one big control - an Envelope graph's grid, its
/// labels - go here rather than in a row of the body, where two full-size buttons cost a
/// quarter of the module's height.
pub(crate) fn header_toggle(ui: &mut egui::Ui, label: &str, on: bool, rect: egui::Rect, id_salt: impl std::hash::Hash) -> bool {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .id_salt(("header_toggle", id_salt))
            .layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight))
    );
    // In the control font, drawn smaller if a large one doesn't fit the switch.
    let font = super::simple_bodies::fit_font(ui, label, super::simple_bodies::control_font(), rect.width() - 8.0);
    let btn = egui::Button::selectable(on, egui::RichText::new(label).font(font))
        .min_size(egui::vec2(rect.width(), rect.height()));
    child.put(rect, btn).clicked()
}

/// One of the Envelope's header switches, and what both of them plus the gap between them
/// take off the header's room for a name.
pub(crate) const HEADER_TOGGLE_W: f32 = 54.0;

pub(crate) fn header_toggles_w(ui: &egui::Ui) -> f32 {
    2.0 * HEADER_TOGGLE_W + 6.0 + ui.spacing().item_spacing.x
}

/// A square button with a drawing on it, at a place in a header: the stepper's square, the
/// drawing tinted like a button's text. Returns whether it was clicked.
pub(crate) fn header_icon_button(
    ui: &mut egui::Ui,
    glyph: super::glyphs::Glyph,
    hover: &str,
    rect: egui::Rect,
    id_salt: impl std::hash::Hash,
) -> bool {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .id_salt(("header_icon", id_salt))
            .layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight)),
    );
    let resp = child.put(rect, egui::Button::new("").min_size(rect.size())).on_hover_text(hover);
    let colour = child.style().interact(&resp).text_color();
    super::glyphs::paint(child.painter(), rect.expand(1.0), glyph, colour);
    resp.clicked()
}

/// A Response Curve's header controls, right to left: Reset, Load, Save as drawings, then
/// the `labels` and `grid` switches - narrower than the Envelope's pair, because this header
/// has three buttons to fit beside them.
pub(crate) const CURVE_TOGGLE_W: [f32; 3] = [34.0, 42.0, 44.0];
pub(crate) const CURVE_HEADER_GAP: f32 = 3.0;

/// The SVG module's tint swatch: the header buttons' height, a little wider.
pub(crate) fn svg_swatch_w(ui: &egui::Ui) -> f32 {
    (stepper_h(ui) * 1.4).round()
}

/// The SVG module's header controls, right to left: the tint swatch, Load, Clear - the
/// room comes off the name, which is painted in what is left (cut with dots), never laid
/// out beside them, so the module can be as narrow as its controls.
pub(crate) fn svg_header_w(ui: &egui::Ui) -> f32 {
    svg_swatch_w(ui) + 2.0 * stepper_h(ui) + 3.0 * CURVE_HEADER_GAP + 4.0
}

pub(crate) fn curve_header_w(ui: &egui::Ui) -> f32 {
    CURVE_TOGGLE_W.iter().sum::<f32>() + 3.0 * stepper_h(ui) + 5.0 * CURVE_HEADER_GAP + 4.0
}

/// A button in a module's body, in the house look: an action row's button
/// (`remapper_bodies::action_button` - the controls' font a point up, as tall as a
/// Remapper's Learn), as wide as its word. `selected` fills it, for one that stays on (a
/// Learn listening). egui's own small button was a thin strip in the small font, grey on
/// grey, beside pills and strips a size up.
pub(crate) fn body_button(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let font = super::simple_bodies::control_font_at(super::simple_bodies::control_font_size() + 1.0);
    let text_w = ui.painter().layout_no_wrap(label.to_string(), font, egui::Color32::PLACEHOLDER).size().x;
    let w = (text_w + 2.0 * ui.spacing().button_padding.x + 12.0).max(44.0);
    ui.add(super::remapper_bodies::action_button(label, w).selected(selected))
}

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

/// What a module does, in one line: the second line of a header's tooltip, so a module on
/// the canvas can be asked what it is for without opening the docs or selecting it.
/// Generated from the Purpose lines of docs/MODULES_REFERENCE.md, plus the handful the
/// reference doesn't spell out yet. A module missing here just gets its name and no line.
const HELP: &[(&str, &str)] = &[
    ("module.constant", "Outputs a fixed float value"),
    ("module.switch", "Toggles between on/off states, can be triggered by inputs or UI"),
    ("module.knob", "User-adjustable float parameter (persistent across patches)"),
    ("module.selector", "Selects one of N inputs based on a selector value (0.0 to 1.0)"),
    ("module.dropdown", "Provides discrete selection from a list of options"),
    ("module.label", "Displays or edits a text label"),
    ("module.svg", "Renders SVG graphics in node body"),
    ("module.split", "Splits a signal into multiple outputs based on selector"),
    ("subpatch", "Contains an inline sub-graph with declared I/O pins"),
    ("math.add", "Sums multiple signals (Float or Vec2)"),
    ("math.subtract", "Subtracts subsequent inputs from the first"),
    ("math.multiply", "Multiplies signals together"),
    ("math.divide", "Divides first signal by subsequent signals"),
    ("math.clamp", "Constrains signal to a range"),
    ("math.abs", "Returns absolute value"),
    ("math.negate", "Inverts a signal — sign flip, or a unipolar mirror inside `0..max`"),
    ("math.min_max", "Reports the largest and smallest of all its inputs"),
    ("math.quantize", "Snaps a signal to a grid"),
    ("math.map_range", "Remaps value from one range to another"),
    ("logic.and", "Logical AND of two boolean inputs"),
    ("logic.or", "Logical OR of two boolean inputs"),
    ("logic.not", "Logical negation"),
    ("logic.xor", "Exclusive OR of two boolean inputs"),
    ("logic.equal", "Compares two signals for equality"),
    ("logic.not_equal", "Compares two signals for inequality"),
    ("logic.greater_than", "Tests if first signal is greater than second"),
    ("logic.less_than", "Tests if first signal is less than second"),
    ("logic.has_changed", "Detects when input signal changes value"),
    ("logic.delay", "Delays boolean signal by specified time"),
    ("logic.counter", "Counts rising edges on increment input"),
    ("module.delay", "Delays signal by specified time with per-channel buffers"),
    ("module.average", "Averages signal over a sliding window with optional spike rejection"),
    ("module.dc_filter", "Removes DC offset from signal using adaptive estimation"),
    ("module.sample_hold", "Holds a signal of any type, on a trigger or for as long as a gate is high"),
    ("module.response_curve", "Applies a user-defined curve to signal with bias control"),
    ("module.vec_response_curve", "Applies response curve to vector magnitude, preserving direction"),
    ("module.vec_reshape", "Directionally reshapes 2D vectors with boundary and gain curves"),
    ("module.twoway_response_curve", "Applies different curves for rising vs falling input (hysteresis)"),
    ("processing.gyro_3dof", "Processes gyroscope data for 3-degree-of-freedom mapping"),
    ("processing.rws", "Real-World-Sensitivity camera aiming. Scales a rotation-rate Vec2"),
    ("module.vec_to_deflection", "Cartesian → polar: how far a vector is pushed, and which way"),
    ("display.readout", "Displays current signal value numerically"),
    ("display.oscilloscope", "Real-time waveform visualization"),
    ("display.trigscope", "Oscilloscope with trigger input for stable display of repetitive signals"),
    ("display.vectorscope", "2D vector visualization (sticks, touchpad)"),
    ("display.controller3d", "3D model visualization with orientation tracking"),
    ("generator.oscillator", "Generates periodic waveforms"),
    ("generator.envelope", "Generates amplitude envelopes with configurable shape"),
    ("module.automap_split", "Extracts individual pins from an AutoMap bus wire"),
    ("module.automap_collect", "Injects signals into an AutoMap bus for downstream routing"),
    ("module.automap_fork", "Duplicates an AutoMap bus to multiple outputs"),
    ("module.automap_selector", "Selects one of N AutoMap buses based on selector input"),
    ("module.automap_combiner", "Merges multiple AutoMap buses using configurable policy per pin"),
    ("module.automap_response_curve", "Response Curve on one signal of an AutoMap bus. The header's **Signal**"),
    ("module.automap_twoway_response_curve", "Two-way Response Curve on one signal of an AutoMap bus — same pick and"),
    ("module.stick_rotation", "Turning the pad turns one stick of an AutoMap bus that is already"),
    ("module.area_mapper", "Lays one XY pair of an AutoMap bus (the header's **Input**: left stick,"),
    ("module.touch_zones", "Divides touchpad into configurable zones with typed outputs"),
    ("module.menu", "A summoned on-screen radial/grid menu whose zones are pointed at with an"),
    ("module.remapper", "Maps AutoMap signals to other AutoMap signals with per-mapping overrides"),
    ("module.map_action", "Evaluates mapping cards and outputs gate/analog signals"),
    ("module.feedback_control", "Drives the upstream physical pad's feedback (rumble, light bar,"),
    ("module.jsm", "Applies a JoyShockMapper config text to the bus with JSM's own"),
    ("module.audio_stream_haptics", "Routes audio loopback to haptic feedback without HIDMaestro driver"),
    ("subpatch.inlet", "Bridge from outer graph to inner sub-patch"),
    ("subpatch.outlet", "Bridge from inner sub-patch to outer graph"),
    ("module.network_send", "Transmits AutoMap bus to remote instance over network"),
    ("module.network_recv", "Receives AutoMap bus from remote instance and injects into graph"),
    ("module.macro", "Reads from macro namespace published by mapping evaluators"),
    ("logic.nand", "False only when both inputs are true"),
    ("logic.nor", "True only when neither input is true"),
    ("logic.xnor", "True when the two inputs agree"),
    ("module.axis_to_vec", "Turns two axes into a vector"),
    ("module.input_viewer", "Shows what an input is sending, as the patch sees it"),
    ("module.vec_to_axis", "Splits a vector into two axes"),
];

/// The name a module ships under, by its id: what a header's tooltip puts after the name
/// the user gave a module, so a node called "Aim curve" still says it is a Response Curve.
pub(crate) fn shipped_name(module_id: &str) -> Option<&'static str> {
    static NAMES: std::sync::OnceLock<std::collections::HashMap<&'static str, &'static str>> =
        std::sync::OnceLock::new();
    NAMES
        .get_or_init(|| {
            flexinput_modules::all_modules()
                .into_iter()
                .map(|r| (r.descriptor.id, r.descriptor.display_name))
                .collect()
        })
        .get(module_id)
        .copied()
}

pub(crate) fn module_help(module_id: &str) -> Option<&'static str> {
    HELP.iter()
        .find(|(id, _)| *id == module_id)
        .map(|(_, help)| *help)
}

/// The name as it should be painted in a header with `room` points left for it: whole when
/// it fits, cut with a dot-dot-dot when it doesn't, and nothing at all when the room can't
/// hold a word worth reading. The same rule on every module - painting as much of the name
/// as happens to fit leaves half a letter under whatever sits at the right edge.
pub(crate) fn title_galley(
    ui: &egui::Ui,
    title: &str,
    room: f32,
) -> Option<std::sync::Arc<egui::Galley>> {
    const MIN_TITLE_W: f32 = 40.0;
    let font = egui::TextStyle::Body.resolve(ui.style());
    let colour = ui.visuals().text_color();
    let whole = ui.painter().layout_no_wrap(title.to_owned(), font.clone(), colour);
    if whole.size().x <= room {
        return Some(whole);
    }
    if room < MIN_TITLE_W {
        return None;
    }
    let mut chars: Vec<char> = title.chars().collect();
    loop {
        if chars.is_empty() {
            return None;
        }
        chars.pop();
        let cut: String = chars.iter().collect();
        let galley = ui
            .painter()
            .layout_no_wrap(format!("{cut}\u{2026}"), font.clone(), colour);
        if galley.size().x <= room {
            return Some(galley);
        }
    }
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

/// The drawing a node's header shows where its name would be, or none. For a module whose
/// name doesn't fit beside its header control and has no symbol a font can be trusted to
/// carry: Delay is two blocks wide with a `- n +` in its header, which leaves the word no
/// room, and a clock is not in every font. The name goes over the setting in the body and
/// into the hover text; a module the user renamed keeps its name, cut to fit as any other.
pub(crate) fn node_glyph(node: &NodeData) -> Option<super::glyphs::Glyph> {
    match (node.module_id.as_str(), node.display_name.as_str()) {
        ("module.delay", "Delay") => Some(super::glyphs::Glyph::Stopwatch),
        _ => None,
    }
}

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
