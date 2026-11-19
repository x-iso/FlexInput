//! Module bodies built from the body kit (`kit.rs`): the first modules moved
//! onto the shared layout. Each keeps the pinnable element ids (and rects) it
//! had, so pins made before keep working.

use super::kit;
use super::*;

fn param_f32(snarl: &Snarl<NodeData>, node_id: NodeId, key: &str, default: f32) -> f32 {
    snarl
        .get_node(node_id)
        .and_then(|n| n.params.get(key).and_then(|v| v.as_f64()))
        .map_or(default, |v| v as f32)
}

fn param_str(snarl: &Snarl<NodeData>, node_id: NodeId, key: &str, default: &str) -> String {
    snarl
        .get_node(node_id)
        .and_then(|n| n.params.get(key).and_then(|v| v.as_str()))
        .unwrap_or(default)
        .to_string()
}

fn param_bool(snarl: &Snarl<NodeData>, node_id: NodeId, key: &str, default: bool) -> bool {
    snarl
        .get_node(node_id)
        .and_then(|n| n.params.get(key).and_then(|v| v.as_bool()))
        .unwrap_or(default)
}

fn set_f32(snarl: &mut Snarl<NodeData>, node_id: NodeId, key: &str, v: f32) {
    if let (Some(node), Some(n)) = (snarl.get_node_mut(node_id), Number::from_f64(v as f64)) {
        node.params.insert(key.into(), Value::Number(n));
    }
}

fn set_value(snarl: &mut Snarl<NodeData>, node_id: NodeId, key: &str, v: Value) {
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.params.insert(key.into(), v);
    }
}

fn wired(inputs: &[InPin], idx: usize) -> bool {
    inputs.get(idx).is_some_and(|p| !p.remotes.is_empty())
}

/// The params key a kit node's width grip writes (for tests outside the viewer).
#[cfg(test)]
pub(crate) fn kit_width_param() -> &'static str {
    kit::WIDTH_PARAM
}

/// The narrowest each kit module's rows allow. Quantize isn't in it any more: its body
/// allocates nothing at all, so its width is its pins' business like the other marks.
pub(crate) fn kit_min_width(module_id: &str) -> f32 {
    match module_id {
        // The cells two across: two 56-point cells and the gap between. 56 is where the
        // longest mode word ("Unlimited") still reads whole, so it is what the module settles
        // at rather than a room it was handed. The old floor was 240 because a row was a label
        // cell, a control and a value box, and the labels ("Output", "Step", "Range") were most
        // of it.
        "logic.counter" => 116.0,
        // Its option column at the two blocks it has always had; the grip widens it.
        "module.dropdown" => 120.0,
        _ => kit::MIN_W,
    }
}

/// The line a setting's word sits on above its pill: `Small`, which is what draws it.
const CAP_H: f32 = 14.0;

/// The pill group the small bodies share: a **constant box** reserved in the flow, and the
/// middle of it. Reserving a constant - not what the text happens to measure - is what keeps
/// a body from feeding its own width back into the frame, and it is the only way a module
/// gets to be narrower than a stack of label rows: the labels were the width, not the
/// controls. Horizontally the pills sit on the module's centre line rather than the column's,
/// because the pin columns are different widths (`In` one letter, `Out` three); vertically they
/// sit on the body column's, because the frame's middle includes the header and reads as high.
///
/// `captioned` gives every row a line of its own above the pill — the setting's word, which
/// is what the three-letter cell inside a pill used to stand in for. That read as a puzzle
/// (`win`/`spk`/`dec`) for a module that has the room to say Window, Spikes and Decay.
///
/// Returns nothing on the module's first frame, before the canvas has a frame to centre on;
/// the box is still reserved, so the size is right as soon as the controls are drawn.
fn pill_group(
    ui: &mut egui::Ui,
    node_id: NodeId,
    view_salt: u64,
    pill_w: f32,
    rows: usize,
    captioned: bool,
) -> Option<(f32, f32)> {
    use super::simple_bodies::PILL_H;
    const ROW: f32 = 4.0;
    let row_h = if captioned { CAP_H + PILL_H } else { PILL_H };
    let content_h = rows as f32 * row_h + (rows.saturating_sub(1)) as f32 * ROW;
    // At least two rows of height whatever the module actually has, so one setting doesn't
    // make a module shallower than its neighbours: the body floor that a coarse grid used to
    // supply has to be asked for by name.
    let h = content_h.max(48.0);
    ui.allocate_exact_size(egui::vec2(pill_w, h), egui::Sense::hover());
    let inner = super::module_icons::module_inner(ui, view_salt, node_id)?;
    let column = ui.max_rect();
    // Traced so a test can hold the pills to the module's centre, not just to its size.
    simple_bodies::trace_rect(ui, node_id, "body_column", column);
    // The floor decides the module's *size*; where the controls sit is a separate question, and
    // answering it from the floored box left Delay's one pill against the header with all the
    // room below it. Centre what is actually drawn, in the body column - the space under the
    // header - since the frame's middle is 24 points higher again and reads as high.
    Some((inner.center().x, column.center().y - content_h * 0.5))
}

/// The word above a pill, painted in the row's caption line. Painted rather than allocated,
/// so a longer setting name can't widen the module: the box was reserved at a constant.
fn caption_at(ui: &mut egui::Ui, left: egui::Pos2, w: f32, title: &str) {
    let font = egui::TextStyle::Small.resolve(ui.style());
    let galley = ui
        .painter()
        .layout_no_wrap(title.to_owned(), font, ui.visuals().weak_text_color());
    ui.painter().galley(
        egui::pos2(left.x + (w - galley.size().x) * 0.5, left.y),
        galley,
        ui.visuals().weak_text_color(),
    );
}

/// One value pill at `left`, painted rather than allocated (the rect is given, so nothing it
/// measures folds back into the body's size). Traced under `{salt}_pill` so a test can hold
/// the placement, and returns the response plus a number the user typed into it.
fn pill_at(
    ui: &mut egui::Ui,
    node_id: NodeId,
    salt: &str,
    left: egui::Pos2,
    w: f32,
    label: &str,
    shown: &str,
    range: std::ops::RangeInclusive<f64>,
) -> (egui::Response, Option<f64>) {
    use super::simple_bodies::{value_pill_number, PILL_H};
    let rect = egui::Rect::from_min_size(left, egui::vec2(w, PILL_H));
    // The label cell is the word plus a little air, measured. At a fixed 26 the Counter's
    // "step" ran into its own number while the number's box, taking what was left of a 116-point
    // row, was three times as wide as any number in it.
    let label_w = super::simple_bodies::pill_label_width(ui, label);
    simple_bodies::trace_rect(ui, node_id, &format!("{salt}_pill"), rect);
    value_pill_number(ui, node_id, salt, rect, label, shown, label_w, range)
}

/// Milliseconds written compactly: a delay of a second is `1000 ms`, a window of five
/// milliseconds is `5 ms`, and nothing pretends at precision below that.
fn ms_text(v: f32) -> String {
    if v.abs() >= 10.0 {
        format!("{} ms", v.round() as i64)
    } else {
        format!("{v:.1} ms")
    }
}

/// Delay (`module.delay`): every channel delayed by the same time. It used to be a "Delay"
/// label, a slider and a "Channels" row - 252 points of module for one number - and its
/// channels are added from the header now, like the variadic operations' pins.
pub(crate) fn show_delay_body(
    node_id: NodeId,
    _inputs: &[InPin],
    _outputs: &[OutPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    view_salt: u64,
) {
    use super::simple_bodies::PILL_H;
    let ms = param_f32(snarl, node_id, "delay_ms", 100.0);
    // The header is too narrow for the word beside its `− n +` and wears a stopwatch
    // instead (`kit::node_glyph`), so the word is here, over the number it names - the
    // way Average and DC Filter caption theirs.
    const PILL_W: f32 = 72.0;
    let Some((cx, top)) = pill_group(ui, node_id, view_salt, PILL_W, 1, true) else { return };
    caption_at(ui, egui::pos2(cx - PILL_W * 0.5, top), PILL_W, "Delay");
    let top = top + CAP_H;
    let (resp, typed) = pill_at(
        ui,
        node_id,
        "delay_ms",
        egui::pos2(cx - PILL_W * 0.5, top),
        PILL_W,
        "",
        &ms_text(ms),
        0.0..=60_000.0,
    );
    let resp = resp.on_hover_text(
        "How long every channel is delayed by.\nDrag across the pill to change it, double-click to type.",
    );
    if let Some(v) = typed {
        set_f32(snarl, node_id, "delay_ms", v as f32);
    } else if resp.dragged() {
        set_f32(snarl, node_id, "delay_ms", (ms + resp.drag_delta().x * 25.0).clamp(0.0, 60_000.0));
    }
    register_exposable_element(ui, node_id, "ms", egui::Rect::from_min_size(egui::pos2(cx - PILL_W * 0.5, top), egui::vec2(PILL_W, PILL_H)));
}

/// Average (`module.average`): a moving average over a time window, with outliers dropped
/// past a spike threshold. Two pills, the window and the threshold in median absolute
/// deviations; the channels come from the header.
pub(crate) fn show_average_body(
    node_id: NodeId,
    _inputs: &[InPin],
    _outputs: &[OutPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    view_salt: u64,
) {
    use super::simple_bodies::PILL_H;
    let ms = snarl
        .get_node(node_id)
        .map_or(5.0, |n| flexinput_engine::eval::average_window_ms(&n.params) as f32);
    let mad = param_f32(snarl, node_id, "spike_mad", 0.0);
    const PILL_W: f32 = 72.0;
    let Some((cx, top)) = pill_group(ui, node_id, view_salt, PILL_W, 2, true) else { return };
    let left = egui::pos2(cx - PILL_W * 0.5, top + CAP_H);
    caption_at(ui, egui::pos2(cx - PILL_W * 0.5, top), PILL_W, "Window");
    let (r1, t1) = pill_at(ui, node_id, "samples", left, PILL_W, "", &ms_text(ms), 0.5..=5_000.0);
    let r1 = r1.on_hover_text(
        "How much of the recent signal the average runs over.\nDrag to change it, double-click to type.",
    );
    if let Some(v) = t1 {
        set_f32(snarl, node_id, "window_ms", v as f32);
    } else if r1.dragged() {
        set_f32(snarl, node_id, "window_ms", (ms + r1.drag_delta().x * 5.0).clamp(0.5, 5_000.0));
    }
    let left2 = egui::pos2(left.x, left.y + CAP_H + PILL_H + 4.0);
    caption_at(ui, egui::pos2(left2.x, top + CAP_H + PILL_H + 4.0), PILL_W, "Spikes");
    let shown2 = if mad > 0.0 { format!("{mad:.1}") } else { "off".into() };
    let (r2, t2) = pill_at(ui, node_id, "spike_mad", left2, PILL_W, "", &shown2, 0.0..=20.0);
    let r2 = r2.on_hover_text(
        "Outlier threshold in median absolute deviations: a reading further from the middle than this is dropped.\n0 = off. Try 3.0 to start.",
    );
    if let Some(v) = t2 {
        set_f32(snarl, node_id, "spike_mad", v as f32);
    } else if r2.dragged() {
        set_f32(snarl, node_id, "spike_mad", (mad + r2.drag_delta().x * 0.05).clamp(0.0, 20.0));
    }
    register_exposable_element(ui, node_id, "samples", egui::Rect::from_min_size(left, egui::vec2(PILL_W, PILL_H)));
    register_exposable_element(ui, node_id, "spike_mad", egui::Rect::from_min_size(left2, egui::vec2(PILL_W, PILL_H)));
}

/// DC Filter (`module.dc_filter`): removes a slowly moving offset. The window is how much
/// offset it takes out, the decay how quickly it lets a real change through; two pills,
/// channels from the header.
pub(crate) fn show_dc_filter_body(
    node_id: NodeId,
    _inputs: &[InPin],
    _outputs: &[OutPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    view_salt: u64,
) {
    use super::simple_bodies::PILL_H;
    let window = param_f32(snarl, node_id, "window_ms", 500.0);
    let decay = param_f32(snarl, node_id, "decay_ms", 200.0);
    const PILL_W: f32 = 72.0;
    let Some((cx, top)) = pill_group(ui, node_id, view_salt, PILL_W, 2, true) else { return };
    let left = egui::pos2(cx - PILL_W * 0.5, top + CAP_H);
    caption_at(ui, egui::pos2(cx - PILL_W * 0.5, top), PILL_W, "Window");
    let (r1, t1) = pill_at(ui, node_id, "window_ms", left, PILL_W, "", &ms_text(window), 10.0..=60_000.0);
    let r1 = r1.on_hover_text(
        "How slowly a change has to move to count as offset rather than signal.\nDrag to change it, double-click to type.",
    );
    if let Some(v) = t1 {
        set_f32(snarl, node_id, "window_ms", v as f32);
    } else if r1.dragged() {
        set_f32(snarl, node_id, "window_ms", (window + r1.drag_delta().x * 25.0).clamp(10.0, 60_000.0));
    }
    let left2 = egui::pos2(left.x, left.y + CAP_H + PILL_H + 4.0);
    caption_at(ui, egui::pos2(left2.x, top + CAP_H + PILL_H + 4.0), PILL_W, "Decay");
    let (r2, t2) = pill_at(ui, node_id, "decay_ms", left2, PILL_W, "", &ms_text(decay), 10.0..=60_000.0);
    let r2 = r2.on_hover_text(
        "How quickly the filter settles after a real change goes through it.",
    );
    if let Some(v) = t2 {
        set_f32(snarl, node_id, "decay_ms", v as f32);
    } else if r2.dragged() {
        set_f32(snarl, node_id, "decay_ms", (decay + r2.drag_delta().x * 25.0).clamp(10.0, 60_000.0));
    }
    register_exposable_element(ui, node_id, "window_ms", egui::Rect::from_min_size(left, egui::vec2(PILL_W, PILL_H)));
    register_exposable_element(ui, node_id, "decay_ms", egui::Rect::from_min_size(left2, egui::vec2(PILL_W, PILL_H)));
}

/// Inverse (`math.negate`): flips the sign, or mirrors inside `0..max`. Its two settings
/// are a mode button and, in range mode, the max - centred as a pair in the body column, on
/// the module's own centre line. It used to be five blocks: a "Flip" choice row spelling out
/// "Sign" and "In 0..max", and a "Max" row.
pub(crate) fn show_inverse_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, _w: f32, view_salt: u64) {
    use super::simple_bodies::{value_pill_number, PILL_H};
    let mut unipolar = param_bool(snarl, node_id, "unipolar", false);
    let mut max = param_f32(snarl, node_id, "unipolar_max", 1.0);
    // What a body may reserve here is what is left of two blocks once the pins are
    // counted: 14 of margin, about 40 of pins and their labels, 8 of spacing — past ~48 it
    // tips into a third block. The box is reserved in the flow at a constant size and the
    // controls are painted into it: a body that allocates nothing came out one block wide
    // with 82-point pills spilling out of it, and a box sized from what the text happens to
    // measure is a ratchet. The height is constant whatever the mode too, so switching
    // modes never resizes the module.
    const BOX_W: f32 = 48.0;
    const BOX_H: f32 = 42.0;
    const ROW: f32 = 4.0;
    let (band, _) = ui.allocate_exact_size(egui::vec2(BOX_W, BOX_H), egui::Sense::hover());
    // Centred on the module, not on the column between the pin labels: `in` and `out` are
    // different widths, so the column's middle isn't the module's. Vertically it is the
    // column's middle the pair sits on, not the module's - the header is above both.
    let inner = module_icons::module_inner(ui, view_salt, node_id);
    let centre_x = inner.map(|r| r.center().x).unwrap_or_else(|| band.center().x);
    let column = ui.max_rect();
    let rows = if unipolar { 2 } else { 1 };
    let group_h = TOGGLE_SMALL.y + (rows - 1) as f32 * (PILL_H + ROW);
    let top = kit::group_top(column, group_h);
    // Traced so a test can hold the placement to the rule, not just the module's size.
    simple_bodies::trace_rect(ui, node_id, "body_column", column);
    let group = egui::Rect::from_min_size(egui::pos2(centre_x - BOX_W * 0.5, top), egui::vec2(BOX_W, group_h));
    simple_bodies::trace_rect(ui, node_id, "inverse_group", group);
    let row1 = egui::Rect::from_min_size(
        egui::pos2(centre_x - TOGGLE_SMALL.x * 0.5, top),
        TOGGLE_SMALL,
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(row1)
            .layout(egui::Layout::centered_and_justified(egui::Direction::TopDown))
            .id_salt("inverse_flip"),
    );
    let resp = toggle_button(&mut child, unipolar, if unipolar { "range" } else { "sign" }, TOGGLE_SMALL);
    drop(child);
    let resp = resp.on_hover_text(
        "Sign: out = -in.\nRange: mirror the value inside 0..max instead of flipping its sign.",
    );
    if resp.clicked() {
        unipolar = !unipolar;
        set_value(snarl, node_id, "unipolar", Value::Bool(unipolar));
    }
    // The max, under the mode, only when the mode asks for one. Dragged across the pill or
    // typed into it: a drag alone can't land on an exact number.
    if unipolar {
        let r_max = egui::Rect::from_min_size(
            egui::pos2(centre_x - BOX_W * 0.5, top + TOGGLE_SMALL.y + ROW),
            egui::vec2(BOX_W, PILL_H),
        );
        let shown = format!("{max:.2}");
        simple_bodies::trace_rect(ui, node_id, "inverse_max_pill", r_max);
        let (resp, typed) = value_pill_number(ui, node_id, "inverse_max", r_max, "", &shown, 0.0, 0.0..=1000.0);
        let resp = resp.on_hover_text(
            "The top of the range the value is mirrored inside. Drag to change, double-click to type.",
        );
        if let Some(v) = typed {
            max = v as f32;
            set_f32(snarl, node_id, "unipolar_max", max);
        } else if resp.dragged() {
            max = (max + resp.drag_delta().x * 0.05).clamp(0.0, 1000.0);
            set_f32(snarl, node_id, "unipolar_max", max);
        }
    }
    // `unipolar` is the id the pinned field and the nav know this setting by; the group
    // covers the mode button and the max cell, as the old two-row registration did.
    register_exposable_element(ui, node_id, "unipolar", group);
}

/// The steps a dragged Quantize factor snaps through: doubling, which is what a step
/// count is usually wanted to be. A factor set elsewhere (the pinned field, or a wire)
/// still shows as itself; the drag just lands on one of these.
const QUANTIZE_STEPS: [f32; 11] = [1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0, 256.0, 512.0, 1024.0];

/// The same list, for the test that checks a drag lands on one of them.
#[cfg(test)]
pub(crate) fn quantize_step_values() -> &'static [f32] {
    &QUANTIZE_STEPS
}

fn quantize_step_label(v: f32) -> String {
    if v >= 1000.0 {
        format!("{}k", (v / 1000.0).round() as i32)
    } else {
        format!("{}", v.round() as i32)
    }
}

/// Quantize (`math.quantize`): the staircase says what it does, and its two settings are
/// a row of pills under the pin rows. It used to be four blocks wide — a factor drag box
/// and a rounding choice spelling out four words, each on its own row — which was more
/// module than two settings and three pins ask for.
pub(crate) fn show_quantize_body(
    node_id: NodeId,
    inputs: &[InPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    _w: f32,
    view_salt: u64,
) {
    use super::simple_bodies::{pill_detent, setting_pill, value_pill_number, PILL_H};
    let mut factor = param_f32(snarl, node_id, "factor", 1.0);
    let mode = param_str(snarl, node_id, "mode", "round");
    let factor_wired = wired(inputs, 1);
    // When the factor comes down a wire, the pill shows what the wire says and a drag
    // would be a lie, so it doesn't do anything.
    if factor_wired {
        if let Some(flexinput_core::Signal::Float(v)) = snarl
            .get_node(node_id)
            .and_then(|n| n.extra.last_signals.get(1).copied().flatten())
        {
            factor = v;
        }
    }
    let inner = module_icons::module_inner(ui, view_salt, node_id);
    let column = ui.max_rect();
    // A constant box, reserved whatever the frame turns out to be. The mark and the pills
    // are painted rather than allocated, so without it the module's width is its pin labels'
    // business alone - and the word "Quantize" in the header was what carried it to two
    // blocks. A header that can't fit its title leaves it out, so the body has to ask for
    // the size on its own.
    ui.allocate_exact_size(egui::vec2(40.0, PILL_H + 4.0), egui::Sense::hover());
    let Some(inner) = inner else { return };
    // The pill row along the bottom of the frame and the mark filling everything above it,
    // across the pin rows - the shape the modules whose pins are already wide use. Centring
    // the pills instead leaves the mark a band too short to read, and the mark is what says
    // what the module does.
    let pills_top = inner.bottom() - PILL_H - 4.0;
    module_icons::paint_glyph(
        ui,
        node_id,
        "math.quantize",
        false,
        Some(egui::Rect::from_min_max(inner.min, egui::pos2(inner.right(), pills_top))),
        column.top(),
    );
    let band = egui::Rect::from_min_max(egui::pos2(inner.left(), pills_top), inner.max);
    if band.width() < 90.0 || band.height() < PILL_H {
        return;
    }
    let modes: [(&str, &str, &str); 4] = [
        ("round", "round", "To the nearest step"),
        ("floor", "floor", "Down to the step below"),
        ("ceil", "ceil", "Up to the step above"),
        ("trunc", "trunc", "Toward zero"),
    ];
    // The two pills have to fit inside the frame with air either side, so their widths come
    // from what the two blocks leave over (100 points inside a 112 frame) and not from what
    // their text happens to measure. `÷` gets a 14-point cell — one glyph, and it's the
    // division sign the number is read with, not a word — and the pair ends 6 points short
    // of the frame rather than a point past it, which is what cut the division pill off.
    let step_w = 44.0;
    let mode_w = 40.0;
    let x0 = inner.center().x - (step_w + mode_w + 4.0) * 0.5;
    let r_step = egui::Rect::from_min_size(egui::pos2(x0, pills_top), egui::vec2(step_w, PILL_H));
    let r_mode = egui::Rect::from_min_size(egui::pos2(r_step.right() + 4.0, pills_top), egui::vec2(mode_w, PILL_H));
    simple_bodies::trace_rect(ui, node_id, "body_column", column);
    simple_bodies::trace_rect(ui, node_id, "quantize_factor_pill", r_step);
    simple_bodies::trace_rect(ui, node_id, "quantize_mode_pill", r_mode);
    // The label is a division sign, not the word: the number is subdivisions per unit, and
    // "step" barely fitted beside its value.
    let (resp, typed) = value_pill_number(
        ui,
        node_id,
        "quantize_factor",
        r_step,
        "÷",
        &quantize_step_label(factor),
        14.0,
        1.0..=1_000_000.0,
    );
    let resp = resp.on_hover_text(if factor_wired {
        "Steps per unit, from the wire: 1 = integers, 2 = halves, 4 = quarters.\nUnwire it to set it here."
    } else {
        "Steps per unit: 1 = integers, 2 = halves, 4 = quarters.\nDrag to snap through doublings, double-click to type any number."
    });
    if !factor_wired {
        if let Some(v) = typed {
            factor = v as f32;
            set_f32(snarl, node_id, "factor", factor);
        }
        if let Some(pos) = resp.interact_pointer_pos().filter(|_| resp.dragged() || resp.clicked()) {
            let cell = super::simple_bodies::pill_value_cell(r_step, 14.0);
            let at = pill_detent(cell, pos, QUANTIZE_STEPS.len() as i32 - 1) as usize;
            let v = QUANTIZE_STEPS[at.min(QUANTIZE_STEPS.len() - 1)];
            if v != factor {
                factor = v;
                set_f32(snarl, node_id, "factor", factor);
            }
        }
    }
    let at = modes.iter().position(|(id, _, _)| *id == mode).unwrap_or(0);
    // Rounding is four named choices, so the pill opens a list rather than being dragged
    // through them: a drag across four words lands on one you didn't mean. The list is
    // painted and interacted with by hand, in the node's own space - egui's own popup
    // places itself against the layer the canvas draws into and lands across the canvas,
    // since the nodes are under the pan/zoom transform and it isn't.
    let open_id = egui::Id::new(("quantize_mode_open", node_id.0));
    let mut open = ui.ctx().data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
    let (resp, _) = setting_pill(ui, "quantize_mode", r_mode, "", modes[at].1, 0.0);
    let resp = resp.on_hover_text(
        "Rounding, click to choose:\nround - to the nearest step\nfloor - down to the step below\nceil - up to the step above\ntrunc - toward zero",
    );
    let just_opened = resp.clicked();
    if just_opened {
        open = !open;
    }
    if open {
        let row_h = 16.0;
        let list = egui::Rect::from_min_size(
            egui::pos2(r_mode.left(), r_mode.bottom() + 2.0),
            egui::vec2(mode_w.max(56.0), row_h * modes.len() as f32 + 4.0),
        );
        // A trace of where the list was put, so a test clicks it rather than guessing.
        ui.ctx().data_mut(|d| {
            d.insert_temp(
                egui::Id::new(("quantize_menu_rect", node_id.0)),
                super::simple_bodies::Xysr([list.min.x, list.min.y, list.width(), list.height()]),
            );
        });
        let painter = ui.painter_at(list.expand(6.0));
        let window = ui.visuals().window_fill;
        let hovered = ui.visuals().widgets.hovered.bg_fill;
        let text = ui.visuals().text_color();
        let strong = ui.visuals().strong_text_color();
        let font = crate::settings::ui_font(12.0);
        painter.rect_filled(list, 4.0, window);
        let mut picked: Option<String> = None;
        let mut elsewhere = false;
        for (i, (id, label, _)) in modes.iter().enumerate() {
            let r = egui::Rect::from_min_max(
                egui::pos2(list.left() + 2.0, list.top() + 2.0 + i as f32 * row_h),
                egui::pos2(list.right() - 2.0, list.top() + 2.0 + (i + 1) as f32 * row_h),
            );
            let item = ui.interact(r, egui::Id::new(("quantize_mode_item", node_id.0, i)), egui::Sense::click());
            if item.hovered() {
                painter.rect_filled(r, 2.0, hovered);
            }
            painter.text(
                egui::pos2(r.left() + 4.0, r.center().y),
                egui::Align2::LEFT_CENTER,
                *label,
                font.clone(),
                if i == at { strong } else { text },
            );
            if item.clicked() {
                picked = Some((*id).to_string());
            }
            // Anywhere else - on the module, on the canvas, on another node - closes it.
            if item.clicked_elsewhere() {
                elsewhere = true;
            }
        }
        // Not on the frame that opened it, or the click that opened it would close it.
        if !just_opened && (picked.is_some() || elsewhere) {
            open = false;
        }
        if let Some(next) = picked.filter(|m| *m != mode) {
            set_value(snarl, node_id, "mode", Value::String(next));
        }
    } else {
        ui.ctx().data_mut(|d| d.remove::<super::simple_bodies::Xysr>(egui::Id::new(("quantize_menu_rect", node_id.0))));
    }
    ui.ctx().data_mut(|d| d.insert_temp(open_id, open));
    // Both pills keep the element ids the value box and the choice row had: the pinned
    // field (`pinned.rs`) and the gamepad/touch nav (`nav/fields.rs`) look up this module's
    // settings by those ids, and a patch that pinned one keeps working. Registered last, so
    // in layout/pick mode the element is what answers the click, not the pill behind it.
    register_exposable_element(ui, node_id, "factor", r_step);
    register_exposable_element(ui, node_id, "mode", r_mode);
}

/// Logic Delay (`logic.delay`): which edge waits, how long, and in what unit. Three
/// settings, so three rows of controls in a box as wide as two blocks allow; it used to be
/// five blocks wide because each row spelled its label out ("Delay", "Time", "Unit") and
/// the words beside the values ("On"/"Off", "ms"/"frames").
pub(crate) fn show_logic_delay_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, _w: f32, view_salt: u64) {
    use super::simple_bodies::{value_pill_number, PILL_H};
    let mut mode = param_str(snarl, node_id, "mode", "delay_false");
    let mut time = param_f32(snarl, node_id, "time", 100.0);
    let mut unit = param_str(snarl, node_id, "unit", "ms");
    let ms = unit != "samples";
    // The same arithmetic as Inverse: two blocks leave about 48 points of body once the
    // pins are counted, and the box is constant so the module can't ratchet on its text.
    const BOX_W: f32 = 48.0;
    const BOX_H: f32 = 68.0;
    const ROW: f32 = 4.0;
    let (band, _) = ui.allocate_exact_size(egui::vec2(BOX_W, BOX_H), egui::Sense::hover());
    let inner = module_icons::module_inner(ui, view_salt, node_id);
    let centre_x = inner.map(|r| r.center().x).unwrap_or_else(|| band.center().x);
    let column = ui.max_rect();
    // The three rows as one stack, centred in the column under the header and on the
    // module's centre line.
    let rows_h = 2.0 * TOGGLE_SMALL.y + PILL_H + 2.0 * ROW;
    let top = kit::group_top(column, rows_h);
    simple_bodies::trace_rect(ui, node_id, "body_column", column);
    simple_bodies::trace_rect(
        ui,
        node_id,
        "delay_group",
        egui::Rect::from_min_size(egui::pos2(centre_x - BOX_W * 0.5, top), egui::vec2(BOX_W, rows_h)),
    );
    // Which edge waits: turning on, or turning off.
    let r_mode = egui::Rect::from_min_size(egui::pos2(centre_x - TOGGLE_SMALL.x * 0.5, top), TOGGLE_SMALL);
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(r_mode)
            .layout(egui::Layout::centered_and_justified(egui::Direction::TopDown))
            .id_salt("delay_mode"),
    );
    let resp = toggle_button(&mut child, mode == "delay_true", if mode == "delay_true" { "on" } else { "off" }, TOGGLE_SMALL);
    drop(child);
    let resp = resp.on_hover_text(
        "On: turning on waits the delay, turning off is immediate.\nOff: turning off waits the delay, turning on is immediate.",
    );
    if resp.clicked() {
        mode = if mode == "delay_true" { "delay_false".to_string() } else { "delay_true".to_string() };
        set_value(snarl, node_id, "mode", Value::String(mode.clone()));
    }
    // How long: dragged across the pill, or typed into it.
    let limit: f32 = if ms { 60_000.0 } else { 10_000.0 };
    let r_time = egui::Rect::from_min_size(
        egui::pos2(centre_x - BOX_W * 0.5, top + TOGGLE_SMALL.y + ROW),
        egui::vec2(BOX_W, PILL_H),
    );
    let shown = format!("{}", time.round() as i32);
    simple_bodies::trace_rect(ui, node_id, "delay_time_pill", r_time);
    let (resp, typed) = value_pill_number(ui, node_id, "delay_time", r_time, "", &shown, 0.0, 0.0..=limit as f64);
    let resp = resp.on_hover_text(if ms {
        "How long, in milliseconds. Drag to change, double-click to type, or use the pinned field."
    } else {
        "How long, in engine frames. Drag to change, double-click to type, or use the pinned field."
    });
    if let Some(v) = typed {
        time = v as f32;
        set_f32(snarl, node_id, "time", time);
    } else if resp.dragged() {
        // Half a millisecond per point of drag, a fifth of a frame: fine enough to land on
        // a round number without fighting the pointer.
        time = (time + resp.drag_delta().x * if ms { 0.5 } else { 0.2 }).clamp(0.0, limit);
        set_f32(snarl, node_id, "time", time);
    }
    // And the unit, under it: one click switches ms and frames.
    let r_unit = egui::Rect::from_min_size(
        egui::pos2(centre_x - TOGGLE_SMALL.x * 0.5, top + TOGGLE_SMALL.y + PILL_H + 2.0 * ROW),
        TOGGLE_SMALL,
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(r_unit)
            .layout(egui::Layout::centered_and_justified(egui::Direction::TopDown))
            .id_salt("delay_unit"),
    );
    let resp = toggle_button(&mut child, !ms, if ms { "ms" } else { "frame" }, TOGGLE_SMALL);
    drop(child);
    let resp = resp.on_hover_text("Milliseconds, or engine frames. Click to switch.");
    if resp.clicked() {
        unit = if ms { "samples".to_string() } else { "ms".to_string() };
        set_value(snarl, node_id, "unit", Value::String(unit.clone()));
    }
    // The pinned field and the gamepad/touch nav look these up by id, and a patch that
    // pinned one before this body was rebuilt still expects it: same ids as the old rows,
    // now over the pills. (`time` covers the unit cell too, as it did before.)
    register_exposable_element(ui, node_id, "mode", r_mode);
    register_exposable_element(ui, node_id, "time", r_time.union(r_unit));
}

/// Counter (`logic.counter`): counts steps between limits. Its four settings used to be four
/// labelled rows - a mode strip, an "Output" row, "Step", and a "Range" row with two captioned
/// boxes - 335 points of module for a strip of words and three numbers, most of it the words
/// "Output", "Step" and "Range" beside them. It is a stack of cells now: the mode as two rows of
/// two so the words stay whole, the output pair with the reset at the end of its row, then the
/// range and the step as pills. The ids the pinned fields and the nav look the controls up by
/// are unchanged, and so are the params.
pub(crate) fn show_counter_body(
    node_id: NodeId,
    inputs: &[InPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    w: f32,
    view_salt: u64,
) {
    use super::simple_bodies::PILL_H;
    let mut mode = param_str(snarl, node_id, "mode", "loop");
    let mut normalized = param_bool(snarl, node_id, "normalized", false);
    let mut step = param_f32(snarl, node_id, "step_param", 1.0);
    let mut min = param_f32(snarl, node_id, "min_param", 0.0);
    let mut max = param_f32(snarl, node_id, "max_param", 10.0);
    let (step_wired, min_wired, max_wired) = (wired(inputs, 3), wired(inputs, 4), wired(inputs, 5));
    // Cells two across, sized from the width the body was given rather than from what any of
    // them happens to say: the grip can still take this module wider, and the cells use it.
    // Cells two across, with a floor of 56 - the point where the longest mode word
    // ("Unlimited") still reads whole - which is what the module settles at. It stretches to
    // fill a wider frame the grip has asked for, but the floor is worked out from the cells,
    // never from the frame as it stands (`kit::body_width`), so the module no longer starts at
    // whatever width it happened to be handed: it was 252 points for a strip of words and
    // three numbers.
    const CELL: f32 = 56.0;
    const ROW: f32 = 4.0;
    const ROWS: usize = 5;
    let cell_w = ((w - ROW) * 0.5).max(CELL);
    let col_w = 2.0 * cell_w + ROW;
    let group_h = ROWS as f32 * PILL_H + (ROWS as f32 - 1.0) * ROW;
    ui.allocate_exact_size(egui::vec2(col_w, group_h), egui::Sense::hover());
    let inner = match super::module_icons::module_inner(ui, view_salt, node_id) {
        Some(r) => r,
        None => return,
    };
    let column = ui.max_rect();
    let cx = inner.center().x;
    let top = kit::group_top(column, group_h);
    let left = cx - col_w * 0.5;
    let row_top = |row: usize| top + row as f32 * (PILL_H + ROW);
    let mut changed = false;
    // The mode: four options, two per row. In one strip of four the words need 184 points,
    // which is a block more module than this asks for; split over two rows it costs height the
    // module's six input pins already give it for free.
    const MODES: [(&str, &str, &str); 4] = [
        ("loop", "Loop", "Past the max, start again at the min"),
        ("limit", "Limit", "Stop at the min and max"),
        ("bounce", "Bounce", "Turn around at the min and max"),
        ("unlimited", "Unlimited", "Count on past the max"),
    ];
    let mut mode_area: Option<egui::Rect> = None;
    for row in 0..2 {
        let rect = egui::Rect::from_min_size(egui::pos2(left, row_top(row)), egui::vec2(col_w, PILL_H));
        let opts: Vec<(String, &str, &str)> =
            MODES[row * 2..row * 2 + 2].iter().map(|(v, t, h)| (v.to_string(), *t, *h)).collect();
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center))
                .id_salt(("counter_mode", row)),
        );
        changed |= kit::strip(&mut child, col_w, &mut mode, &opts);
        drop(child);
        simple_bodies::trace_rect(ui, node_id, &format!("counter_mode_{row}"), rect);
        mode_area = Some(mode_area.map_or(rect, |a: egui::Rect| a.union(rect)));
    }
    if let Some(r) = mode_area {
        register_exposable_element(ui, node_id, "mode", r);
    }
    // The output: the count itself, or the count across min..max as 0..1. "Output" was the row's
    // label; the two cells say it, and the row is theirs - the reset button that used to share
    // it crowded the second cell, so it sits at the end of the step row instead.
    let out_y = row_top(2);
    let out_rect = egui::Rect::from_min_size(egui::pos2(left, out_y), egui::vec2(col_w, PILL_H));
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(out_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center))
            .id_salt("counter_out"),
    );
    changed |= kit::strip(&mut child, out_rect.width(), &mut normalized, &[
        (false, "Raw", "The count itself"),
        (true, "0..1", "The count across min..max as 0..1"),
    ]);
    drop(child);
    let reset_rect = egui::Rect::from_min_size(
        egui::pos2(cx + col_w * 0.5 - PILL_H, row_top(4)),
        egui::vec2(PILL_H, PILL_H),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(reset_rect)
            .layout(egui::Layout::centered_and_justified(egui::Direction::TopDown))
            .id_salt("counter_reset"),
    );
    let reset = child.add(egui::Button::new("↺")).on_hover_text("Reset the count").clicked();
    drop(child);
    simple_bodies::trace_rect(ui, node_id, "counter_out", out_rect);
    register_exposable_element(ui, node_id, "range_mode", out_rect);
    simple_bodies::trace_rect(ui, node_id, "counter_reset", reset_rect);
    // A value that arrives down a wire shows what the wire says, and a drag would be a lie, so
    // the pill doesn't take one. `max` is out of use in the Unlimited mode.
    let wired_value = |idx: usize| -> Option<String> {
        snarl
            .get_node(node_id)
            .and_then(|n| n.extra.last_signals.get(idx).copied().flatten())
            .and_then(|s| match s {
                flexinput_core::Signal::Float(v) => Some(trim_num(v)),
                _ => None,
            })
    };
    let max_off = mode == "unlimited";
    let shown_min = if min_wired { wired_value(4).unwrap_or_else(|| trim_num(min)) } else { trim_num(min) };
    let shown_max = if max_wired { wired_value(5).unwrap_or_else(|| trim_num(max)) } else { trim_num(max) };
    let shown_step = if step_wired { wired_value(3).unwrap_or_else(|| trim_num(step)) } else { trim_num(step) };
    let (r_min, t_min) = pill_at(ui, node_id, "min", egui::pos2(left, row_top(3)), cell_w, "min", &shown_min, -1e6..=1e6);
    let (r_max, t_max) = pill_at(ui, node_id, "max", egui::pos2(cx + ROW * 0.5, row_top(3)), cell_w, "max", &shown_max, -1e6..=1e6);
    let (r_step, t_step) = pill_at(
        ui,
        node_id,
        "step",
        egui::pos2(left, row_top(4)),
        col_w - PILL_H - ROW,
        "step",
        &shown_step,
        0.001..=10_000.0,
    );
    let r_min = r_min.on_hover_text("The bottom of the count: it stops, loops or turns here, depending on the mode.");
    let r_max = r_max.on_hover_text(if max_off {
        "The top of the count - not used while the mode is Unlimited."
    } else {
        "The top of the count: it stops, loops or turns here, depending on the mode."
    });
    let r_step = r_step.on_hover_text("How much one step adds to the count.");
    if !min_wired {
        if let Some(v) = t_min {
            min = v as f32;
            changed = true;
        } else if r_min.dragged() {
            min = (min + r_min.drag_delta().x * 0.1).clamp(-1e6, 1e6);
            changed = true;
        }
    }
    if !max_wired && !max_off {
        if let Some(v) = t_max {
            max = v as f32;
            changed = true;
        } else if r_max.dragged() {
            max = (max + r_max.drag_delta().x * 0.1).clamp(-1e6, 1e6);
            changed = true;
        }
    }
    if !step_wired {
        if let Some(v) = t_step {
            step = v as f32;
            changed = true;
        } else if r_step.dragged() {
            step = (step + r_step.drag_delta().x * 0.05).clamp(0.001, 10_000.0);
            changed = true;
        }
    }
    register_exposable_element(
        ui,
        node_id,
        "min_max",
        egui::Rect::from_min_size(egui::pos2(left, row_top(3)), egui::vec2(col_w, PILL_H)),
    );
    register_exposable_element(
        ui,
        node_id,
        "step",
        egui::Rect::from_min_size(egui::pos2(left, row_top(4)), egui::vec2(col_w, PILL_H)),
    );
    if reset {
        if let Some(node) = snarl.get_node_mut(node_id) {
            while node.extra.aux_f32.len() < 2 {
                node.extra.aux_f32.push(0.0);
            }
            node.extra.aux_f32[0] = 0.0;
            node.extra.aux_f32[1] = 1.0;
            node.extra.aux_f32_dirty = true;
        }
    }
    if changed {
        set_value(snarl, node_id, "mode", Value::String(mode.clone()));
        set_value(snarl, node_id, "normalized", Value::Bool(normalized));
        set_f32(snarl, node_id, "step_param", step);
        set_f32(snarl, node_id, "min_param", min);
        set_f32(snarl, node_id, "max_param", max);
    }
}

/// A number without the zeros that don't say anything: a step of one is `1`, not `1.00`.
fn trim_num(v: f32) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}


/// Has Changed (`logic.has_changed`): how far the input has to move to count. The one
/// setting is painted across the module — a pill centred on it, allocating a token box so
/// the module keeps its two blocks — because the module's width is already spoken for by
/// "changed / increased / decreased", and a labelled row on top of that took it to six
/// blocks.
pub(crate) fn show_has_changed_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, _w: f32, view_salt: u64) {
    use super::simple_bodies::{value_pill_number, PILL_H};
    let Some((field, mut shown)) = has_changed_hysteresis(node_id, snarl) else { return };
    const BOX_W: f32 = 40.0;
    /// Wider than the box it reserves: the reading carries a unit (`°/s`, `G`) as well as
    /// its digits, and it's painted across the frame rather than into the column between
    /// the pin labels.
    const PILL_W: f32 = 56.0;
    let (band, _) = ui.allocate_exact_size(egui::vec2(BOX_W, BOX_W * 0.5), egui::Sense::hover());
    let inner = module_icons::module_inner(ui, view_salt, node_id);
    let centre_x = inner.map(|r| r.center().x).unwrap_or_else(|| band.center().x);
    let column = ui.max_rect();
    // Painted across the frame, but not into the pin labels: `chg`/`inc`/`dec` are wide, and
    // a pill spanning the whole frame sat under them. The width is only what's drawn - the
    // module's size stays its pins' and its reserved box's business.
    let pill_w = PILL_W.min(column.width().max(48.0));
    let r = egui::Rect::from_min_size(
        egui::pos2(centre_x - pill_w * 0.5, kit::group_top(column, PILL_H)),
        egui::vec2(pill_w, PILL_H),
    );
    simple_bodies::trace_rect(ui, node_id, "body_column", column);
    simple_bodies::trace_rect(ui, node_id, "hysteresis_pill", r);
    let (resp, typed) = value_pill_number(
        ui,
        node_id,
        "has_changed_hysteresis",
        r,
        "",
        &field.text(shown),
        0.0,
        0.0..=(100.0 * field.scale) as f64,
    );
    let resp = resp.on_hover_text(
        "Ignores movement smaller than this, like noise or a shaky hand: it reacts once the input \
         has moved this far from where it last reacted. 0 = every change counts. Drag to change, \
         double-click to type.",
    );
    if let Some(v) = typed {
        shown = v as f32;
        set_has_changed_hysteresis(node_id, snarl, shown, &field);
    } else if resp.dragged() {
        shown = (shown + resp.drag_delta().x * field.nav_step).clamp(0.0, 100.0 * field.scale);
        set_has_changed_hysteresis(node_id, snarl, shown, &field);
    }
    // The id the pinned field and the nav already use for this setting.
    register_exposable_element(ui, node_id, "hysteresis", r);
}

/// Vec to Deflection (`module.vec_to_deflection`): the mark says what it does, the pill
/// under it picks the Angle output's unit. It used to be six blocks — a labelled "Angle"
/// row with two words on it, on top of "Deflection" and "Angle" as pin labels — and then
/// briefly one block wide, because its body allocated nothing and left the width to the pin
/// labels alone. A mark and a pill painted across a frame that narrow don't get drawn at
/// all, and a module too narrow to draw them in stays too narrow: the shrink never comes
/// back. So it reserves a constant box, which is what asks for its two blocks.
pub(crate) fn show_vec_to_deflection_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, _w: f32, view_salt: u64) {
    use super::simple_bodies::{setting_pill, PILL_H};
    let mut degrees = param_bool(snarl, node_id, "degrees", false);
    const BOX_W: f32 = 40.0;
    const BOX_H: f32 = 40.0;
    /// The pill is painted wider than the box, across the frame rather than in the column
    /// between the pin labels: "0..360°" is the widest thing it has to hold.
    const PILL_W: f32 = 56.0;
    // Reserved first, at a constant size, before the frame is even read: the mark and the
    // pill are painted into whatever the frame turns out to be.
    ui.allocate_exact_size(egui::vec2(BOX_W, BOX_H), egui::Sense::hover());
    let column = ui.max_rect();
    // The frame's inner rect is unknown on the first frame, when the body column is a
    // sliver; in that case the mark and the pill simply aren't drawn this frame.
    let Some(inner) = module_icons::module_inner(ui, view_salt, node_id) else { return };
    let pill_top = inner.bottom() - PILL_H - 6.0;
    // The mark over the pin rows, the pill along the bottom of the frame: same family as
    // Quantize, where centring the control would leave the mark too short a band to read.
    module_icons::paint_glyph(
        ui,
        node_id,
        "module.vec_to_deflection",
        false,
        Some(egui::Rect::from_min_max(inner.min, egui::pos2(inner.right(), pill_top - 2.0))),
        column.top(),
    );
    let pill_w = PILL_W.min(column.width().max(48.0));
    let r = egui::Rect::from_min_size(
        egui::pos2(inner.center().x - pill_w * 0.5, pill_top),
        egui::vec2(pill_w, PILL_H),
    );
    simple_bodies::trace_rect(ui, node_id, "body_column", column);
    simple_bodies::trace_rect(ui, node_id, "unit_pill", r);
    let (resp, _) = setting_pill(ui, "deflection_unit", r, "", if degrees { "0..360°" } else { "0..1" }, 0.0);
    let resp = resp.on_hover_text(
        "The Angle output, as a fraction of a full turn (0..1, wrapping) or in degrees (0..360°, wrapping). Click to switch.",
    );
    if resp.clicked() {
        degrees = !degrees;
        set_value(snarl, node_id, "degrees", Value::Bool(degrees));
    }
    // Same element id as the "Angle" row this pill replaced.
    register_exposable_element(ui, node_id, "angle_unit", r);
}

/// The modules whose whole body is their glyph: Add, Subtract, Multiply, Divide,
/// Min/Max and the gates. Their controls, if they have any, are in the header, so the
/// body exists only to paint the mark centred on the module — see
/// [`module_icons::paint_glyph`], which allocates nothing and so can't grow them.
pub(crate) fn show_glyph_body(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &Snarl<NodeData>,
    view_salt: u64,
) {
    let module_id = snarl.get_node(node_id).map(|n| n.module_id.clone()).unwrap_or_default();
    // Abs and NOT have one pin row between them, so they reserve the band a mark needs
    // and come out square like everything else. The mark starts where that reservation
    // starts, not after it: reserving room for a mark and then painting below the room
    // leaves nothing to paint in, which is how both of them ended up icon-less.
    let band_top = ui.cursor().top();
    if module_icons::square_body(&module_id) {
        ui.allocate_exact_size(
            egui::vec2(0.0, module_icons::SQUARE_BAND),
            egui::Sense::hover(),
        );
    }
    // A token width, constant whatever the pins: the mark is painted and asks for nothing,
    // so this is what makes a small operation hold its two blocks out of its own content.
    ui.allocate_exact_size(egui::vec2(module_icons::MIN_BODY_W, 0.0), egui::Sense::hover());
    let inner = module_icons::module_inner(ui, view_salt, node_id);
    module_icons::paint_glyph(ui, node_id, &module_id, false, inner, band_top);
}

/// The size of the Switch module's button, so a boolean setting is the same shape and
/// size wherever it sits — its fill/stroke/label recipe is in
/// [`super::simple_bodies::show_switch_body`].
pub(crate) const TOGGLE: egui::Vec2 = egui::Vec2::new(52.0, 20.0);

/// The Switch-style button itself — the fill/stroke/label recipe every two-state setting
/// wears, in a body or in a header. `setting_button` places it inside a reserved box; a
/// header adds it to its own row.
pub(crate) fn toggle_button(ui: &mut egui::Ui, on: bool, label: &str, size: egui::Vec2) -> egui::Response {
    let (fill, stroke, text) = if on {
        (
            ui.style().visuals.selection.bg_fill,
            ui.style().visuals.selection.stroke.color,
            ui.style().visuals.strong_text_color(),
        )
    } else {
        (
            ui.style().visuals.widgets.inactive.bg_fill,
            ui.style().visuals.widgets.inactive.bg_stroke.color,
            ui.style().visuals.text_color(),
        )
    };
    let label = super::simple_bodies::fitting_word(ui, label, size.x - 2.0 * ui.spacing().button_padding.x);
    let font = super::simple_bodies::fit_font(ui, label, super::simple_bodies::control_font(), size.x - 2.0 * ui.spacing().button_padding.x);
    ui.add(
        egui::Button::new(egui::RichText::new(label).font(font).color(text))
            .fill(fill)
            .stroke(egui::Stroke::new(1.0_f32, stroke))
            .corner_radius(4.0)
            .min_size(size),
    )
}

/// A two-state setting where the house size won't fit: a module whose pins already take
/// most of its two blocks. `TOGGLE`'s 52 points would cost Sample and Hold a third block.
pub(crate) const TOGGLE_SMALL: egui::Vec2 = egui::Vec2::new(40.0, 18.0);
/// The Knob's Uni/Bi switch in the header: its two words and no more. The house 40-point
/// button left the header's right edge over the fold chevron at one block wide, and there
/// is no other use for the room — the header is chevron, name, switch.
pub(crate) const KNOB_TOGGLE: egui::Vec2 = egui::Vec2::new(24.0, 18.0);

/// The two-state setting control at a chosen size, otherwise identical to
/// [`setting_button`] — the same box reserved in the flow, the same drawing.
pub(crate) fn setting_button_size(
    ui: &mut egui::Ui,
    salt: &str,
    centre_x: f32,
    on: bool,
    label: &str,
    size: egui::Vec2,
) -> (egui::Response, f32) {
    let row_top = ui.cursor().top();
    ui.allocate_exact_size(size, egui::Sense::hover());
    let row = egui::Rect::from_min_size(egui::pos2(centre_x - size.x * 0.5, row_top), size);
    let mut child = ui
        .new_child(
            egui::UiBuilder::new()
                .max_rect(row)
                .layout(egui::Layout::centered_and_justified(egui::Direction::TopDown))
                .id_salt(salt),
        );
    let resp = toggle_button(&mut child, on, label, size);
    (resp, row_top + size.y)
}

/// The two-state setting control: a Switch-style button, [`TOGGLE`] on a side, drawn
/// centred on the module inside a box reserved in the body's flow. A setting with two
/// values gets this rather than the setting pill: one click is the whole interaction,
/// and it's the shape the Switch module already uses, so it isn't a new thing to learn.
///
/// The box is what a glyph's band starts under, and it's a constant — sizing it from the
/// label, or placing the allocation by a position read back from the module, walks the
/// node a block wider. Returns the response and the y just under the reserved box.
pub(crate) fn setting_button(
    ui: &mut egui::Ui,
    salt: &str,
    centre_x: f32,
    on: bool,
    label: &str,
) -> (egui::Response, f32) {
    setting_button_size(ui, salt, centre_x, on, label, TOGGLE)
}

/// Greater / Less Than (`logic.greater_than`, `logic.less_than`). The comparison is
/// the mark in the module's header, and `or_equal` turns it into `≥` / `≤`, so the
/// body is the one setting it has: a Switch-style button labelled `or equal`, centred
/// on the module, with the glyph under it. No width grip: nothing here stretches.
pub(crate) fn show_or_equal_body(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    view_salt: u64,
) {
    let module_id = snarl.get_node(node_id).map(|n| n.module_id.clone()).unwrap_or_default();
    let mut or_equal = param_bool(snarl, node_id, "or_equal", false);
    let on = or_equal;
    let inner = module_icons::module_inner(ui, view_salt, node_id);
    let centre_x = inner.map(|r| r.center().x).unwrap_or_else(|| ui.max_rect().center().x);
    let (resp, _) = setting_button(ui, "or_equal_toggle", centre_x, on, "or equal");
    if resp.on_hover_text("Also true when the two are equal").clicked() {
        or_equal = !or_equal;
        set_value(snarl, node_id, "or_equal", Value::Bool(or_equal));
    }
    // The glyph in the band the button leaves under it, skipped when that band is too
    // small to read. Here the reservation is a control, so the mark does start after it.
    let band_top = ui.cursor().top().max(ui.min_rect().bottom());
    module_icons::paint_glyph(ui, node_id, &module_id, or_equal, inner, band_top);
}
