//! Small leaf node bodies: generic pin helpers, math/selector/split,
//! dropdown, sink, constant, switch, label/SVG, counter/logic, knob,
//! delay/average/DC filter.

use super::*;

// ── Generic pin removal helpers ───────────────────────────────────────────────

pub(crate) fn remove_input_pin(node_id: NodeId, rm_idx: usize, inputs: &[InPin], snarl: &mut Snarl<NodeData>) {
    let tail: Vec<Vec<OutPinId>> = inputs[rm_idx..].iter().map(|p| p.remotes.clone()).collect();
    for i in 0..tail.len() {
        snarl.drop_inputs(InPinId { node: node_id, input: rm_idx + i });
    }
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.inputs.remove(rm_idx);
    }
    for (shift, remotes) in tail.into_iter().enumerate().skip(1) {
        let new_in = InPinId { node: node_id, input: rm_idx + shift - 1 };
        for remote in remotes {
            snarl.connect(remote, new_in);
        }
    }
}

pub(crate) fn remove_output_pin(node_id: NodeId, rm_idx: usize, outputs: &[OutPin], snarl: &mut Snarl<NodeData>) {
    let tail: Vec<Vec<egui_snarl::InPinId>> = outputs[rm_idx..].iter().map(|o| o.remotes.clone()).collect();
    for i in 0..tail.len() {
        snarl.drop_outputs(OutPinId { node: node_id, output: rm_idx + i });
    }
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.outputs.remove(rm_idx);
    }
    for (shift, remotes) in tail.into_iter().enumerate().skip(1) {
        let new_out = OutPinId { node: node_id, output: rm_idx + shift - 1 };
        for remote in remotes {
            snarl.connect(new_out, remote);
        }
    }
}

// ── Pin names ─────────────────────────────────────────────────────────────────

pub(crate) fn pin_letter(idx: usize) -> String {
    if idx < 26 { format!("{}", (b'a' + idx as u8) as char) }
    else { format!("in_{}", idx) }
}

// ── Port strips: Selector and Split ───────────────────────────────────────────
//
// Both modules are one setting and a list of ports, so both are drawn the same way: a
// column of strips, one per port, in the body, and `+` in the header. A strip is the
// port's name - which is the pin's own name, so editing the strip edits the pin and the
// name a patch stores - a handle that drags the port up or down the list, and a `×` that
// appears once there are more than two to choose between.
//
// The strips are as tall as the pin rows they belong to, and the pin rows are drawn at
// that height (with the label left to the strip, which says the same word), so a socket
// and the strip naming it are one row. That is what makes the module's two columns and
// its body read as a single list rather than three things that happen to line up.

/// The pitch of a port strip, and of the pin row drawn against it. It is half a grid block
/// (plus the pin column's row spacing), so two rows land on a block exactly and the list
/// never drifts out of step with the blocks the way a 27-point row does.
pub(crate) const STRIP_H: f32 = 25.0;
/// The drag handle, the remove button, and the air inside a strip. The `×` is the same
/// square as the Combiner's `small_button("×")` as near as a 22-point strip allows, so the
/// two kinds of strip read as the same control.
pub(crate) const STRIP_HANDLE_W: f32 = 13.0;
pub(crate) const STRIP_REMOVE_W: f32 = 14.0;
/// The remove button's height, centred on the strip.
pub(crate) const STRIP_REMOVE_H: f32 = STRIP_H - 4.0;
pub(crate) const STRIP_GAP: f32 = 4.0;
pub(crate) const STRIP_PAD: f32 = 5.0;
/// A name cell never shorter than this (a port with no name is still worth clicking) and
/// never wider than this: past it the module is wider than any patch wants.
const STRIP_NAME_MIN: f32 = 28.0;
/// Ten blocks of name: past that a name is a sentence, and is cut with an ellipsis.
const STRIP_NAME_MAX: f32 = 280.0;

/// Select everything in a text field that has just been opened on existing text, so typing
/// replaces it and a click or an arrow key edits it. egui has no select-all-on-focus; this
/// writes the selection into the field's own state, which it reads on its next frame.
pub(crate) fn select_all(ctx: &egui::Context, field: egui::Id, text: &str) {
    let mut state = egui::TextEdit::load_state(ctx, field).unwrap_or_default();
    state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
        egui::text::CCursor::new(0),
        egui::text::CCursor::new(text.chars().count()),
    )));
    state.store(ctx, field);
}

/// Which side of the node the strips list.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum StripSide {
    Inputs,
    Outputs,
}

/// The state of one strip's name field: which port is being renamed, and what's typed so
/// far. Per node, in egui's temp data, like the Dropdown's option rename.
#[derive(Clone, Default)]
struct StripEdit {
    port: Option<usize>,
    buf: String,
    focus: bool,
}

fn strip_edit_key(node: NodeId) -> egui::Id {
    egui::Id::new(("fxi_strip_edit", node.0))
}

/// The port the header's `+` adds. Named as the list already names its members, so the
/// new strip reads like its neighbours; the engine reads these by position, so the name is
/// for the reader and the user can change it on the strip.
pub(crate) fn add_port(node_id: NodeId, module_id: &str, snarl: &mut Snarl<NodeData>) {
    let Some(node) = snarl.get_node_mut(node_id) else { return };
    match module_id {
        // A Selector's first input is its `select` pin, which is not a port.
        "module.selector" => node.inputs.push(PinDescriptor::new(
            format!("in_{}", node.inputs.len().saturating_sub(1)),
            SignalType::Any,
        )),
        "module.automap_combiner" => node.inputs.push(PinDescriptor::new(
            format!("in_{}", node.inputs.len()),
            SignalType::AutoMap,
        )),
        "module.split" => node.outputs.push(PinDescriptor::new(
            format!("out_{}", node.outputs.len()),
            SignalType::Any,
        )),
        "module.automap_fork" => node.outputs.push(PinDescriptor::new(
            format!("out_{}", node.outputs.len()),
            SignalType::AutoMap,
        )),
        // Like the Selector: the first input is `select`.
        "module.automap_selector" => node.inputs.push(PinDescriptor::new(
            format!("in_{}", node.inputs.len().saturating_sub(1)),
            SignalType::AutoMap,
        )),
        _ => {}
    }
}

/// Which band the interpolate toggle sits on: for both modules the `select` pin's band,
/// which is the first strip band for a Split and the one above the strips for a Selector.
#[derive(Clone, Copy)]
pub(crate) struct InterpRow(usize);

impl InterpRow {
    /// A list with no interpolate toggle: the AutoMap Fork and Selector pass one whole bus or
    /// another, and there is nothing between two buses to blend.
    pub(crate) const NONE: InterpRow = InterpRow(usize::MAX);

    fn band(self) -> Option<usize> {
        (self.0 != usize::MAX).then_some(self.0)
    }
}

/// Whether a node is unfolded. A folded node's body is laid out and not drawn - that is
/// `show_body`'s doing, for every module - so a body only asks when it wants to skip work.
pub(crate) fn node_open(snarl: &Snarl<NodeData>, node: NodeId) -> bool {
    snarl.get_node_info(node).map(|i| i.open).unwrap_or(true)
}

/// What a folded module still says under its header: the number it is set to or is showing.
/// A module with nothing worth reading off it folded says nothing.
fn folded_reading(node: &NodeData) -> Option<String> {
    match node.module_id.as_str() {
        "module.knob" => {
            let bipolar = node.params.get("bipolar").and_then(|v| v.as_bool()).unwrap_or(false);
            let v = node.params.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            Some(format!("{:.2}", v.clamp(if bipolar { -1.0 } else { 0.0 }, 1.0)))
        }
        "module.constant" => {
            let stored = node.params.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let res = node
                .params
                .get("res")
                .and_then(|v| v.as_f64())
                .map(|r| r as i32)
                .unwrap_or(CONSTANT_RES_DEFAULT)
                .clamp(0, CONSTANT_RES_MAX);
            Some(constant_text(constant_snap(stored, res), res))
        }
        "display.readout" => Some(signal_text(node.extra.last_signals.first().copied().flatten().as_ref())),
        _ => None,
    }
}

/// Paint a folded module's reading in the band its frame keeps under the header. On the grid
/// a folded frame is two blocks and the header one, so the band is there and empty. Painted
/// before the body's painter is switched off, and clipped to the frame rather than to the
/// body's column, which is nothing while the node is folded.
pub(crate) fn paint_folded_reading(ui: &egui::Ui, snarl: &Snarl<NodeData>, node_id: NodeId, view_salt: u64) {
    let Some(text) = snarl.get_node(node_id).and_then(folded_reading) else { return };
    let Some(inner) = module_icons::module_inner(ui, view_salt, node_id) else { return };
    let band = egui::Rect::from_min_max(egui::pos2(inner.left(), inner.top() + 30.0), inner.max);
    if band.height() <= 12.0 {
        return;
    }
    let col = ui.visuals().strong_text_color();
    let p = ui.painter().with_clip_rect(inner);
    // The size it fits at: a long number gives up size before it gives up digits.
    let mut font = egui::TextStyle::Body.resolve(ui.style());
    let mut g = p.layout_no_wrap(text.clone(), font.clone(), col);
    if g.size().x > band.width() {
        font.size = (font.size * band.width() / g.size().x).max(8.0);
        g = p.layout_no_wrap(text, font, col);
    }
    let at = egui::pos2(band.center().x - g.size().x * 0.5, band.center().y - g.size().y * 0.5);
    let rect = egui::Rect::from_min_size(at, g.size());
    trace_rect(ui, node_id, "folded_reading", rect);
    if snarl.get_node(node_id).map_or(false, |n| n.module_id == "module.knob") {
        trace_rect(ui, node_id, "knob_number", rect);
    }
    p.galley(at, g, col);
}

/// The body of a Selector (its value inputs) or a Split (its outputs): `pin_from` is the
/// index in that side where the listed ports start - a Selector's `select` pin is not a
/// strip - and `interp_row` is the band the interpolate toggle sits on.
pub(crate) fn show_port_strips(
    node_id: NodeId,
    inputs: &[InPin],
    outputs: &[OutPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    module_id: &str,
    side: StripSide,
    pin_from: usize,
    first_row: usize,
    interp_row: InterpRow,
) {
    // The port names come from the node, up front: the loop can't hold a borrow of snarl
    // while it mutates pins.
    let names: Vec<String> = snarl.get_node(node_id).map_or_else(Vec::new, |n| {
        let pins: &[PinDescriptor] = match side {
            StripSide::Inputs => &n.inputs,
            StripSide::Outputs => &n.outputs,
        };
        pins[pin_from.min(pins.len())..].iter().map(|p| p.name.clone()).collect()
    });
    let n = names.len();
    if n == 0 {
        return;
    }
    let bands = (first_row + n).max(interp_row.band().map_or(0, |b| b + 1));
    // The strips and their sockets are one grid, and `StripRows` is what knows where its rows
    // are: taken here, before anything is allocated, while the cursor is at the body's top.
    let rows_at = super::kit::StripRows::at_cursor(module_id, ui);
    let pitch = rows_at.advance;
    let mut edit = ui.ctx().data(|d| d.get_temp::<StripEdit>(strip_edit_key(node_id))).unwrap_or_default();
    let font = egui::TextStyle::Small.resolve(ui.style());
    // The text being typed into a name is one of the names: the strip widens as it is typed.
    let name_w = names
        .iter()
        .map(String::as_str)
        .chain(edit.port.is_some().then_some(edit.buf.as_str()))
        .map(|s| ui.painter().layout_no_wrap(s.to_string(), font.clone(), egui::Color32::PLACEHOLDER).size().x + 6.0)
        .fold(0.0f32, f32::max)
        .clamp(STRIP_NAME_MIN, STRIP_NAME_MAX);
    // The row's parts: handle, optional `×`, and the name - the floor a strip needs to hold
    // the longest name in the list.
    let need_w = 2.0 * STRIP_PAD
        + STRIP_HANDLE_W
        + if n > 2 { STRIP_REMOVE_W + STRIP_GAP } else { 0.0 }
        + STRIP_GAP
        + name_w;
    let toggle_w = TOGGLE_SMALL.x;
    let left = ui.cursor().left();
    // The strips hug the side their sockets are on: a Selector's ports are inputs, on the
    // left, so its strips start at the left; a Split's are outputs, on the right, so its
    // strips end at the right. Either way they are all the same width from the same edge.
    let right_aligned = side == StripSide::Outputs;
    // The interpolate toggle sits on the `select` pin's row - the first strip band for a
    // Split, the band above the strips for a Selector (whose first input is that pin). When
    // it shares a band the list pays for it: the toggle has its room at the left and every
    // strip keeps the same width from the same edge, so the setting can make a Split wider
    // than a Selector but never ragged or overlapping.
    let interp_band = interp_row.band();
    let shares_first = interp_band == Some(first_row);
    let share_room = if shares_first { toggle_w + STRIP_GAP } else { 0.0 };
    // What the body **asks for** is content: the longest name plus the parts around it, and
    // the toggle's room when it shares a band. That is the floor the grip can be dragged
    // down to, and it's why a short-named list fits three blocks.
    // What the strips are **painted at** is the column the frame leaves them, so the list
    // reaches the edge the sockets are on instead of stopping where the label happens to end
    // - and when a name outgrows the column, the ask steps the module a block wider.
    let alloc_w = need_w + share_room;
    let col_w = ui.max_rect().width().max(0.0);
    let strip_w = (col_w - share_room).max(need_w);
    // The body reserves the whole list, so the module's size is its ports' business: more
    // of them, or a longer name, and the module takes the room; either goes back when it
    // stops being needed.
    let rows = bands;
    let list_w = alloc_w;
    let list_h = rows_at.height(rows);
    ui.allocate_exact_size(egui::vec2(list_w, list_h), egui::Sense::hover());
    // Folded, the list keeps the frame's blocks and draws none of itself. Snarl runs a collapsed
    // node's body and shifts it up under the header's clip: the strips were painted above the
    // frame, out of sight, and their handles and name fields still answered the pointer there.
    // The sockets stay on the header row; their names can't come back beside them, because the
    // pin column of a collapsed node is laid out above the header and clipped there.
    if !node_open(snarl, node_id) {
        return;
    }

    let mut to_remove: Option<usize> = None;
    let mut to_move: Option<(usize, usize)> = None;
    let mut renamed: Option<(usize, String)> = None;
    let mut interp = snarl
        .get_node(node_id)
        .and_then(|nd| nd.params.get("interpolate").and_then(|v| v.as_bool()))
        .unwrap_or(false);
    let mut interp_changed = false;

    // The interpolate toggle, on the band its own pin sits on.
    if let Some(row) = interp_band {
        let r = egui::Rect::from_min_size(
            egui::pos2(left, rows_at.center_y(row) - TOGGLE_SMALL.y * 0.5),
            TOGGLE_SMALL,
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(r)
                .layout(egui::Layout::centered_and_justified(egui::Direction::TopDown))
                .id_salt(("strip_interp", node_id.0)),
        );
        let resp = toggle_button(&mut child, interp, "interp", TOGGLE_SMALL);
        drop(child);
        let resp = resp.on_hover_text(
            "Blend between the ports the selection is passing: a value in the middle of \
             two gives a mix of the two rather than one or the other.",
        );
        if resp.clicked() {
            interp = !interp;
            interp_changed = true;
        }
    }

    let strip_font = font.clone();
    // Drag preview. Which port is being pulled and how far, plus the offset each strip is
    // painted at this frame: those ease toward their target, so a reorder reads as the
    // strips sliding past each other rather than a jump, and the one being dragged rides
    // under the finger. The pins themselves don't move until the drag ends, and when they
    // do the offsets are re-keyed to the new order - the strips carry on from exactly where
    // they were painted.
    let mut anim = StripAnim::begin(ui.ctx(), strip_anim_key(node_id), n, pitch);

    // Read once, before the loop takes the UI mutably for its widgets.
    let (fill_active, fill_hovered, fill_idle) = {
        let v = ui.visuals();
        (v.widgets.active.bg_fill, v.widgets.hovered.bg_fill, v.widgets.inactive.weak_bg_fill)
    };
    let weak = ui.visuals().weak_text_color();
    let name_color = ui.visuals().text_color();
    let outline = ui.visuals().widgets.hovered.fg_stroke.color;
    let glow_color = ui.visuals().selection.bg_fill;
    // Which port the selection is pointing at right now: the value on the `select` pin,
    // which the engine mirrors into the node for exactly this. With interpolate on, the two
    // ports either side of the value light in proportion, so the glow slides along the list
    // instead of jumping from one to the next.
    let mut glow = vec![0.0f32; n];
    if let Some(v) = snarl
        .get_node(node_id)
        .filter(|_| interp_band.is_some())
        .and_then(|nd| nd.extra.last_signals.first())
        .and_then(|s| s.as_ref())
        .and_then(|sig| match sig {
            Signal::Float(x) => Some(*x),
            Signal::Int(i) => Some(*i as f32),
            Signal::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        })
    {
        let pos = v.clamp(0.0, 1.0) * n.saturating_sub(1) as f32;
        if interp {
            let i0 = pos.floor() as usize;
            let frac = pos - pos.floor();
            if i0 < n {
                glow[i0] = 1.0 - frac;
            }
            if i0 + 1 < n {
                glow[i0 + 1] = frac;
            }
        } else if n > 0 {
            glow[((pos + 0.5) as usize).min(n - 1)] = 1.0;
        }
    }
    for (i, name) in names.iter().enumerate() {
        let row = first_row + i;
        // Every strip the same width, filling the column from the edge its sockets are on:
        // flush right for a Split's outputs, flush left for a Selector's inputs. On the band
        // the interpolate toggle shares, the toggle has its room at the left and the strips
        // begin after it - same width, same edge, nothing overlapping.
        let w = strip_w;
        let x = if right_aligned {
            left + col_w - w
        } else {
            left
        };
        // A point of air above and below, so two strips are two strips; the strip stays
        // centred on its socket.
        let strip = rows_at
            .band(row, x, w)
            .shrink2(egui::vec2(0.0, 1.0))
            .translate(egui::vec2(0.0, anim.off[i]));
        trace_rect(ui, node_id, &format!("strip_{i}"), strip);
        let dragged = anim.drag == Some(i);
        let hovered = n > 2 && ui.rect_contains_pointer(strip);
        let fill = if dragged || edit.port == Some(i) {
            fill_active
        } else if hovered {
            fill_hovered
        } else {
            fill_idle
        };
        ui.painter().rect(strip, 4.0, fill, egui::Stroke::NONE, egui::StrokeKind::Inside);
        // The port the selection is pointing at wears a glowing edge. Interpolate mode gives
        // the two ports either side of the value their share of it, so the light fades along
        // the list rather than jumping from port to port.
        let g = glow[i];
        if g > 0.02 {
            let (gr, gg, gb) = (glow_color.r(), glow_color.g(), glow_color.b());
            ui.painter().rect(
                strip.expand(1.0),
                5.0,
                egui::Color32::from_rgba_unmultiplied(gr, gg, gb, (g * 34.0) as u8),
                egui::Stroke::new(1.5_f32, egui::Color32::from_rgba_unmultiplied(gr, gg, gb, (g * 235.0) as u8)),
                egui::StrokeKind::Outside,
            );
            trace_rect(ui, node_id, &format!("strip_glow_{i}"), strip.expand(1.0));
        }
        // The same reading the Combiner's rows give: the strip the pointer is on picks up an
        // edge, and the one being dragged keeps it while it moves.
        if dragged || hovered {
            ui.painter().rect(
                strip,
                4.0,
                egui::Color32::TRANSPARENT,
                egui::Stroke::new(1.0_f32, outline),
                egui::StrokeKind::Inside,
            );
        }

        let mut cx = strip.left() + STRIP_PAD;
        // The handle: dragged a whole pitch in either direction, the port moves. The
        // delta is accumulated per port so a slow drag still crosses the line instead of
        // stalling below it.
        let handle = egui::Rect::from_min_size(
            egui::pos2(cx, strip.center().y - pitch * 0.5),
            egui::vec2(STRIP_HANDLE_W, pitch),
        );
        let hresp = ui
            .interact(handle, egui::Id::new(("strip_handle", node_id.0, i)), egui::Sense::drag())
            .on_hover_cursor(egui::CursorIcon::Grab);
        ui.painter().text(
            handle.center(),
            egui::Align2::CENTER_CENTER,
            "⠿",
            strip_font.clone(),
            weak,
        );
        if let Some(mv) = anim.handle(&hresp, i) {
            to_move = Some(mv);
        }
        hresp.on_hover_text("Drag to move this port up or down the list");
        cx += STRIP_HANDLE_W + STRIP_GAP * 0.5;

        if n > 2 {
            let xrect = egui::Rect::from_min_size(
                egui::pos2(cx, strip.center().y - STRIP_REMOVE_H * 0.5),
                egui::vec2(STRIP_REMOVE_W, STRIP_REMOVE_H),
            );
            let r = ui.put(
                xrect,
                egui::Button::new(egui::RichText::new("×").size(14.0))
                    .min_size(egui::vec2(STRIP_REMOVE_W, STRIP_REMOVE_H)),
            );
            let r = r.on_hover_text("Remove this port");
            if r.clicked() {
                to_remove = Some(i);
            }
            cx += STRIP_REMOVE_W + STRIP_GAP * 0.5;
        }

        // The name: a click picks nothing (the socket is what carries the signal), so
        // double-click or Ctrl-click edits it where it stands.
        let field = egui::Rect::from_min_max(
            egui::pos2(cx, strip.top() + 2.0),
            egui::pos2(strip.right() - STRIP_PAD, strip.bottom() - 2.0),
        );
        if edit.port == Some(i) {
            let mut buf = edit.buf.clone();
            let resp = ui.put(
                field,
                egui::TextEdit::singleline(&mut buf)
                    .font(strip_font.clone())
                    .hint_text("name")
                    .desired_width(field.width()),
            );
            if edit.focus {
                resp.request_focus();
                select_all(ui.ctx(), resp.id, &buf);
                edit.focus = false;
            }
            let enter = ui.input(|inp| inp.key_pressed(egui::Key::Enter));
            let esc = ui.input(|inp| inp.key_pressed(egui::Key::Escape));
            if esc {
                edit = StripEdit::default();
            } else if enter || resp.lost_focus() {
                let typed = buf.trim().to_string();
                // Typing nothing means "never mind", not "this port has no name".
                if !typed.is_empty() {
                    renamed = Some((i, typed));
                }
                edit = StripEdit::default();
            } else {
                edit.buf = buf;
            }
        } else {
            let label = egui::Rect::from_min_max(
                egui::pos2(cx, strip.center().y - pitch * 0.5),
                egui::pos2(field.right(), strip.center().y + pitch * 0.5),
            );
            let resp = ui.interact(label, egui::Id::new(("strip_name", node_id.0, i)), egui::Sense::click());
            let shown = if name.is_empty() { "\u{2014}".to_string() } else { name.clone() };
            let avail = (label.right() - label.left()).max(8.0);
            let mut galley = ui.painter().layout_no_wrap(shown.clone(), strip_font.clone(), name_color);
            // A name longer than the cell - the cell's own width is clamped, so this is a
            // name the user typed - is cut with an ellipsis rather than painted over the
            // strip's edge. The whole name is in the hover text either way.
            let mut elided = false;
            if galley.size().x > avail {
                let chars: Vec<char> = shown.chars().collect();
                for k in (1..chars.len()).rev() {
                    let trial: String = chars[..k].iter().collect::<String>() + "\u{2026}";
                    let g = ui.painter().layout_no_wrap(trial, strip_font.clone(), name_color);
                    if g.size().x <= avail {
                        galley = g;
                        elided = true;
                        break;
                    }
                }
            }
            ui.painter().galley(
                egui::pos2(label.left(), label.center().y - galley.size().y * 0.5),
                galley,
                egui::Color32::PLACEHOLDER,
            );
            let mods = ui.input(|inp| inp.modifiers);
            if resp.double_clicked() || (mods.command && resp.clicked()) {
                // Open on the name it has, all of it selected: typing replaces it, a click
                // or an arrow key puts the cursor in it.
                edit = StripEdit { port: Some(i), buf: name.clone(), focus: true };
            }
            let hover = if elided {
                format!("{name}\nDouble-click to rename this port. The name is what the patch stores.")
            } else {
                "Double-click to rename this port. The name is what the patch stores.".to_string()
            };
            resp.on_hover_text(hover);
        }
    }

    ui.ctx().data_mut(|d| d.insert_temp(strip_edit_key(node_id), edit));
    anim.end(ui.ctx(), strip_anim_key(node_id));

    // Applied after the loop: these change the pins, and the loop is reading them.
    if let Some(i) = to_remove {
        match side {
            StripSide::Inputs => remove_input_pin(node_id, pin_from + i, inputs, snarl),
            StripSide::Outputs => remove_output_pin(node_id, pin_from + i, outputs, snarl),
        }
    }
    if let Some((from, to)) = to_move {
        match side {
            StripSide::Inputs => move_dynamic_pin::<Inputs>(node_id, pin_from + from, pin_from + to, inputs, snarl),
            StripSide::Outputs => move_dynamic_pin::<Outputs>(node_id, pin_from + from, pin_from + to, outputs, snarl),
        }
    }
    if let Some((i, name)) = renamed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            let idx = pin_from + i;
            let pins: &mut Vec<PinDescriptor> = match side {
                StripSide::Inputs => &mut node.inputs,
                StripSide::Outputs => &mut node.outputs,
            };
            if let Some(pin) = pins.get_mut(idx) {
                pin.name = name;
            }
        }
    }
    if interp_changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("interpolate".to_string(), Value::Bool(interp));
        }
    }
}

fn strip_anim_key(node: NodeId) -> egui::Id {
    egui::Id::new(("fxi_strip_anim", node.0))
}

/// The live state of a list's drag-to-reorder: which row is under the finger, how far it's
/// been pulled, and the offset each row is currently painted at. One of these drives every
/// list of strips on the canvas (Selector, Split, Macro Output, AutoMap Combiner), so a row
/// picked up behaves the same on all of them: it rides under the finger, the rows it passes
/// slide a pitch aside, and on release they carry on from where they were painted.
///
/// A frame of it: [`StripAnim::begin`] before the rows, [`StripAnim::offset`] for where to
/// paint row `i`, [`StripAnim::handle`] with each response that can drag row `i` (it returns
/// the move when the finger lets go), [`StripAnim::end`] after the rows.
#[derive(Clone, Default)]
pub(crate) struct StripAnim {
    drag: Option<usize>,
    acc: f32,
    off: Vec<f32>,
    n: usize,
    pitch: f32,
    settling: bool,
}

impl StripAnim {
    /// Load the list's state and ease every row a step toward where it belongs this frame.
    pub(crate) fn begin(ctx: &egui::Context, key: egui::Id, n: usize, pitch: f32) -> Self {
        let mut anim = ctx.data(|d| d.get_temp::<StripAnim>(key)).unwrap_or_default();
        anim.off.resize(n, 0.0);
        anim.n = n;
        anim.pitch = pitch;
        anim.settling = false;
        let d = anim.drag.filter(|d| *d < n);
        // Where the held row would land if it were let go now - the same rounding the release
        // uses - and so which rows it has passed: those step a pitch into the gap it left.
        // (This counted whole pitches and left out the row at the far end, so a row dragged
        // onto its neighbour sat on top of it until the finger lifted.)
        let land = d.map(|d| {
            let s = ((anim.acc / pitch).round() as isize).max(-(d as isize)).min(n as isize - 1 - d as isize);
            (d as isize + s) as usize
        });
        let acc = anim.acc;
        for (i, off) in anim.off.iter_mut().enumerate() {
            let target = match (d, land) {
                (Some(d), _) if i == d => acc.max(-(i as f32) * pitch).min(((n - 1 - i) as f32) * pitch),
                (Some(d), Some(to)) if i > d && i <= to => -pitch,
                (Some(d), Some(to)) if i < d && i >= to => pitch,
                _ => 0.0,
            };
            *off += (target - *off) * 0.5;
            if (target - *off).abs() > 0.4 {
                anim.settling = true;
            }
        }
        anim
    }

    /// How far from its own row the row `i` is painted this frame.
    pub(crate) fn offset(&self, i: usize) -> f32 {
        self.off.get(i).copied().unwrap_or(0.0)
    }

    /// Whether row `i` is the one under the finger.
    pub(crate) fn dragged(&self, i: usize) -> bool {
        self.drag == Some(i)
    }

    /// Feed a response that drags row `i` - its handle, or anything else on the row that is
    /// a grip. Returns `(from, to)` on the frame the finger lets go.
    pub(crate) fn handle(&mut self, resp: &egui::Response, i: usize) -> Option<(usize, usize)> {
        let (n, pitch) = (self.n, self.pitch);
        if resp.drag_started() {
            self.drag = Some(i);
            self.acc = 0.0;
        }
        if resp.dragged() && self.drag == Some(i) {
            let floor = -(i as f32) * pitch;
            let ceil = (n.saturating_sub(1 + i) as f32) * pitch;
            self.acc = (self.acc + resp.drag_delta().y).max(floor).min(ceil);
        }
        let mut moved = None;
        if resp.drag_stopped() && self.drag == Some(i) {
            let steps = (self.acc / pitch).round() as isize;
            let to = (i as isize + steps).max(0).min(n as isize - 1) as usize;
            if to != i {
                // Carry the painted offsets across the reorder: the rows go on from exactly
                // where the preview left them rather than snapping back.
                self.off = remap_offsets(&self.off, i, to, pitch);
                self.settling = true;
            }
            self.drag = None;
            self.acc = 0.0;
            moved = Some((i, to));
        }
        self.settling = self.settling || self.drag.is_some();
        moved
    }

    /// Keep the state for the next frame, and keep the frames coming while rows are still
    /// easing: the offsets move a step per frame.
    pub(crate) fn end(mut self, ctx: &egui::Context, key: egui::Id) {
        // A finger that left without the handle saying so (the row went away under it).
        if self.drag.is_some() && !ctx.input(|i| i.pointer.any_down()) {
            self.drag = None;
            self.acc = 0.0;
        }
        let settling = self.settling;
        ctx.data_mut(|d| d.insert_temp(key, self));
        if settling {
            ctx.request_repaint();
        }
    }
}

/// Re-key the painted offsets for a list after port `from` was moved to `to`: a strip that
/// ends up one row lower has to lose one row's worth of offset to stay where it is on
/// screen, which is what makes the release seamless instead of a snap.
fn remap_offsets(off: &[f32], from: usize, to: usize, pitch: f32) -> Vec<f32> {
    let n = off.len();
    let mut out = vec![0.0f32; n];
    for p in 0..n {
        let new = if p == from {
            to
        } else if from < to && p > from && p <= to {
            p - 1
        } else if to < from && p >= to && p < from {
            p + 1
        } else {
            p
        };
        if new < n {
            out[new] = off[p] + (p as f32 - new as f32) * pitch;
        }
    }
    out
}

/// Selector (`module.selector`): `select` picks one of the ports below it, so the strips
/// are its value inputs and the toggle sits level with the `select` pin. It used to be a
/// stack of `× / in_0` rows with a `+ input` button and a checkbox under them, which lined
/// up with nothing.
pub(crate) fn show_selector_body(
    node_id: NodeId,
    inputs: &[InPin],
    outputs: &[OutPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
) {
    show_port_strips(node_id, inputs, outputs, ui, snarl, "module.selector", StripSide::Inputs, 1, 1, InterpRow(0));
}

/// Split (`module.split`): `select` routes the one input to one of the ports, so the
/// strips are its outputs - level with the output sockets, hugging the right edge those
/// sockets are on, all of them ending at the same place. Its interpolate toggle sits on the
/// first strip's band, at the left, on the row its own pin is on; that strip is the short
/// one, so the toggle costs the module no width.
pub(crate) fn show_split_body(
    node_id: NodeId,
    inputs: &[InPin],
    outputs: &[OutPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
) {
    show_port_strips(node_id, inputs, outputs, ui, snarl, "module.split", StripSide::Outputs, 0, 0, InterpRow(0));
}

// ── Dropdown ──────────────────────────────────────────────────────────────────
//
// Body layout (top-down):
//   • ComboBox of the current options (pinnable element "selection",
//     resizable width via egui::Resize so a pinned widget honours the
//     user's chosen size in a sub-patch layout).
//   • Vertical list of entries. Each row:
//       [≡ drag handle] [× remove] [label (double-click / Alt-click to edit)]
//   • "+" button below the last row to append a new entry.
//
// Persisted params:
//   options:        Array<String>
//   selected_index: u64
//   box_width:      f64 (optional, persists the user's resize of the ComboBox)
//
// When the currently-selected entry is removed, selection clamps to the new
// last index (matches the "Clamp to last" behaviour discussed in design).
pub(crate) fn dropdown_read_options(node: &NodeData) -> Vec<String> {
    node.params.get("options")
        .and_then(|v| v.as_array())
        .map(|a| a.iter()
            .map(|v| v.as_str().unwrap_or("").to_string())
            .collect())
        .unwrap_or_else(|| vec!["Option 1".to_string(), "Option 2".to_string()])
}

pub(crate) fn dropdown_write_options(node: &mut NodeData, opts: &[String]) {
    let arr: Vec<Value> = opts.iter().map(|s| Value::String(s.clone())).collect();
    node.params.insert("options".to_string(), Value::Array(arr));
}

pub(crate) fn dropdown_read_selected(node: &NodeData, n: usize) -> usize {
    let idx = node.params.get("selected_index")
        .and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    if n == 0 { 0 } else { idx.min(n - 1) }
}

pub(crate) fn dropdown_write_selected(node: &mut NodeData, idx: usize) {
    node.params.insert("selected_index".to_string(), Value::from(idx as u64));
}

/// The option a Dropdown is sending out right now, where `selected` is the one it was set to.
/// The two differ while a wire on the `shift` pin is stepping the selection.
///
/// Read from the module's own `index` output, not worked out again from what is on the pin:
/// the engine mirrors every node's *outputs* to the UI (`last_out`) and only a handful of
/// modules' *inputs* (`last_signals`), and a Dropdown is not one of those - so a body that read
/// the pin always saw nothing on it, and marked the option it was set to whatever the wire was
/// doing. The output is also simply the truth: no second copy of the engine's wrap to keep in
/// step. With nothing wired the option set here is the one that comes out, and saying so
/// directly keeps a click from waiting a tick on the engine to be seen.
pub(crate) fn dropdown_live_index(snarl: &Snarl<NodeData>, node_id: NodeId, n: usize, selected: usize) -> usize {
    if n == 0 {
        return 0;
    }
    let wired = !snarl.in_pin(InPinId { node: node_id, input: 0 }).remotes.is_empty();
    if !wired {
        return selected.min(n - 1);
    }
    match snarl.get_node(node_id).and_then(|nd| nd.extra.last_out.get(1).copied().flatten()) {
        Some(Signal::Int(i)) if i >= 0 => (i as usize).min(n - 1),
        _ => selected.min(n - 1),
    }
}

/// What to store so that option `want` is the one that comes out, while the `shift` pin is
/// holding the list `live - selected` steps away from where it was set: choosing an option
/// under a shift moves the base, it doesn't fight the pin.
pub(crate) fn dropdown_base_for(want: usize, selected: usize, live: usize, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    let m = n as i64;
    let shift = live as i64 - selected as i64;
    (((want as i64 - shift) % m + m) % m) as usize
}

/// Per-node editing state held in egui memory: which row is being renamed and
/// the in-progress text buffer for it. Cleared when editing ends (Enter,
/// Escape, focus loss, or row delete).
/// A reorder drag in progress on the option list: which row's handle the finger is holding,
/// where the option it grabbed stands now, and the leftover pixels of its travel.
#[derive(Clone, Copy, Default)]
struct OptionDrag {
    held: usize,
    current: usize,
    acc: f32,
    /// How far the finger has gone since the row was picked up, unstepped: what the
    /// row under it follows, so the move is visible while it is happening.
    lift: f32,
    active: bool,
}

#[derive(Clone, Default)]
pub(crate) struct DropdownEditState {
    editing: Option<usize>,
    buf: String,
    /// Set when we just entered edit mode so we can focus the TextEdit on the
    /// next frame.
    request_focus: bool,
}

pub(crate) fn dropdown_edit_state(ctx: &egui::Context, node_id: NodeId) -> DropdownEditState {
    ctx.data(|d| d.get_temp::<DropdownEditState>(
        egui::Id::new(("fxi_dropdown_edit", node_id.0))
    )).unwrap_or_default()
}

pub(crate) fn dropdown_set_edit_state(ctx: &egui::Context, node_id: NodeId, st: DropdownEditState) {
    ctx.data_mut(|d| d.insert_temp(
        egui::Id::new(("fxi_dropdown_edit", node_id.0)),
        st,
    ));
}

pub(crate) fn show_dropdown_body(
    node_id: NodeId,
    inputs: &[InPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    view_salt: u64,
) {
    // Snapshot params up-front.
    let mut options = snarl.get_node(node_id).map(dropdown_read_options).unwrap_or_default();
    let selected = snarl.get_node(node_id).map(|n| dropdown_read_selected(n, options.len())).unwrap_or(0);

    let mut new_selected = selected;
    let mut options_changed = false;
    let mut selected_changed = false;

    // A patch saved before this module took a `shift` input has no pin for it: a node's pins are
    // stored with it rather than read back from the registry, so an existing Dropdown would go on
    // without one. The first frame it draws gives it the pin it's missing, the way a fresh Macro
    // Output seeds its first port.
    if snarl.get_node(node_id).is_some_and(|n| n.inputs.is_empty()) {
        if let Some(n) = snarl.get_node_mut(node_id) {
            n.inputs.push(PinDescriptor::new("shift", SignalType::Float).optional());
        }
    }

    // The list column, and the reading above it, at constants - not a `box_width` param the
    // user could dial to 600 and not the frame the module was handed. A row is a handle, the
    // option's text cut at the column's edge with the whole of it on hover, and the `×` at the
    // right where every other list puts its remove button. The `+` that adds an option is in
    // the header, with the other lists' controls, rather than at the end of the list where it
    // read as one more option.
    // 120 rather than the 140 the old `box_width` default gave it: an option's name is cut at
    // the column's edge and reads whole on hover and in the popup, so the column is sized to
    // the module's two blocks, not to the longest name someone might type.
    const LIST_W: f32 = 120.0;
    const ROW: f32 = 4.0;
    const HANDLE_W: f32 = 18.0;
    const BTN_W: f32 = 24.0;
    const READ_W: f32 = LIST_W;
    let name_w = (LIST_W - HANDLE_W - BTN_W - 2.0 * ROW).max(40.0);
    // The reading, and one row per option.
    let rows = options.len() + 1;
    let list_h = rows as f32 * PILL_H + (rows as f32 - 1.0) * ROW;

    // Reserved once, so none of the text in it can reach back into the frame's width.
    ui.allocate_exact_size(egui::vec2(LIST_W, list_h), egui::Sense::hover());
    // Folded, the column is still reserved and nothing in it is drawn: a collapsed node's body
    // runs under the header's clip, and an option row painted itself across the word "Dropdown".
    if !node_open(snarl, node_id) {
        return;
    }
    let inner = match super::module_icons::module_inner(ui, view_salt, node_id) {
        Some(r) => r,
        None => return,
    };
    let cx = inner.center().x;
    let top = kit::group_top(ui.max_rect(), list_h);
    let row_top = |row: usize| top + row as f32 * (PILL_H + ROW);
    let row_rect = |row: usize| {
        egui::Rect::from_min_size(egui::pos2(cx - LIST_W * 0.5, row_top(row)), egui::vec2(LIST_W, PILL_H))
    };

    // The `shift` pin, if anyone wired it: what it says is *added* to the option chosen here
    // rather than replacing it, so the module keeps being the starting point and a dial or a
    // comparator walks the list from there. `effective` is the option actually coming out.
    let n_opts = options.len();
    // Wired or not matters to the marks below: with nothing on the pin, the
    // option this module was set to is the option that comes out, and there is nothing extra
    // to say about it.
    let shift_wired = inputs.first().is_some_and(|p| !p.remotes.is_empty());
    let effective = dropdown_live_index(snarl, node_id, n_opts, selected);

    // ── The reading (pinnable): the option coming out of the module, shift included ──
    let reading = row_rect(0);
    {
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(reading)
                .layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight))
                .id_salt(("dropdown_combo", node_id)),
        );
        let current = options.get(effective).cloned().unwrap_or_default();
        // The whole list when there's room for it: egui caps a menu at `combo_height`, about
        // four rows, so a menu of eight made you scroll in the middle of an empty canvas.
        let want = (n_opts as f32 * (child.spacing().interact_size.y + 4.0) + 12.0)
            .max(80.0)
            .min((ui.ctx().content_rect().bottom() - reading.bottom() - 16.0).max(80.0));
        egui::ComboBox::from_id_salt(("dropdown_combo_sel", node_id.0))
            .selected_text(if current.is_empty() { "—".to_string() } else { current })
            .width(READ_W)
            .height(want)
            // The same ComboBox minimum as above: without a width the button is 150 points or
            // the length of the option's text, whichever is longer, so a long option widened the
            // module instead of being cut at its edge. The popup still sizes itself to its rows.
            .width(READ_W)
            .truncate()
            .show_ui(&mut child, |ui| {
                for (i, opt) in options.iter().enumerate() {
                    let label = if opt.is_empty() { format!("(empty {i})") } else { opt.clone() };
                    if ui.selectable_label(i == effective, label).clicked() {
                        // The base is what this module stores, so choosing an option while a
                        // shift pin is holding it somewhere else means choosing where that
                        // addition should land: the option you clicked is the one that comes
                        // out, and the menu doesn't fight the pin.
                        new_selected = dropdown_base_for(i, selected, effective, n_opts);
                        selected_changed = true;
                    }
                }
            });
        register_exposable_element(ui, node_id, "selection", reading);
        drop(child);
    }

    let mut to_remove: Option<usize> = None;
    let mut to_move: Option<(usize, isize)> = None;
    let mut edit_state = dropdown_edit_state(ui.ctx(), node_id);
    let alt = ui.input(|i| i.modifiers.alt);
    let drag_key = egui::Id::new(("fxi_dropdown_drag", node_id.0));
    let mut drag = ui.ctx().data(|d| d.get_temp::<OptionDrag>(drag_key)).unwrap_or_default();

    // A row that is picked up follows the finger, and the rows the gap has travelled
    // through step a pitch aside: the language the port strips use, so a row you grab
    // behaves like a pin you grab. The lifts read the state as it stood coming into this
    // frame, which is what makes the slide continuous instead of a jump at each crossing.
    let pitch = PILL_H + ROW;
    let lifts: Vec<f32> = if drag.active {
        (0..options.len())
            .map(|i| {
                if i == drag.held {
                    let lo = -(pitch * drag.held as f32);
                    let hi = pitch * (options.len() as f32 - 1.0 - drag.held as f32);
                    drag.lift.clamp(lo, hi)
                } else if drag.held < drag.current && i > drag.held && i <= drag.current {
                    -pitch
                } else if drag.held > drag.current && i < drag.held && i >= drag.current {
                    pitch
                } else {
                    0.0
                }
            })
            .collect()
    } else {
        vec![0.0; options.len()]
    };

    for i in 0..options.len() {
        let mut row_remove = false;
        let mut row_move: isize = 0;
        let row_from = i;
        let mut commit_edit: Option<String> = None;
        let mut cancel_edit = false;
        let row = row_rect(i + 1).translate(egui::vec2(0.0, lifts[i]));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(row)
                .layout(egui::Layout::left_to_right(egui::Align::Center))
                .id_salt(("dropdown_row", node_id, i)),
        );
        child.spacing_mut().item_spacing.x = 0.0;
        // Two marks that mean different things. The option this module was set to wears
        // the fill a chosen strip cell wears. The option actually coming out of the module -
        // with a `shift` pin wired, not necessarily that one - is outlined in green, the colour
        // the pin's label wears; an outline rather than a fill so both can be read at once when
        // the pin is holding the list at its starting point. With no pin wired there is nothing
        // to outline.
        if i == selected {
            let fill = ui.visuals().selection.bg_fill;
            child.painter().rect_filled(row, 4.0, fill);
        }
        if i == effective {
            if shift_wired {
                child.painter().rect_stroke(
                    row,
                    4.0_f32,
                    egui::Stroke::new(1.5_f32, super::pins::CHOICE_GREEN),
                    egui::StrokeKind::Inside,
                );
            }
            trace_rect(ui, node_id, "dropdown_effective", row);
        }
        // The dots say that a row can be picked up. They are a grab in their own right, and
        // so is the option's name beside them - that is where a finger lands, and making it
        // the only place a drag could start is what made a reorder feel like it fought the
        // list's own selection.
        let handle = child.add_sized(
            [HANDLE_W, PILL_H],
            egui::Label::new("").sense(egui::Sense::drag()),
        );
        // The dots are painted rather than being a widget of their own: as a label they picked
        // up the list's selection colours whenever a drag was open, which read as the grip
        // lighting up for no reason. They brighten only for the row being held.
        child.painter().text(
            handle.rect.center(),
            egui::Align2::CENTER_CENTER,
            "⠿",
            egui::FontId::proportional(12.0),
            if drag.active && drag.held == i {
                ui.visuals().strong_text_color()
            } else {
                ui.visuals().weak_text_color()
            },
        );
        handle.clone().on_hover_cursor(egui::CursorIcon::Grab);
        handle.clone().on_hover_text("Drag to reorder");

        let mut name: Option<egui::Response> = None;
        child.add_space(ROW);
        let is_editing = edit_state.editing == Some(i);
        if is_editing {
            let resp = child.add_sized(
                [name_w, PILL_H],
                egui::TextEdit::singleline(&mut edit_state.buf),
            );
            if edit_state.request_focus {
                resp.request_focus();
                edit_state.request_focus = false;
            }
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if esc {
                cancel_edit = true;
            } else if enter || resp.lost_focus() {
                commit_edit = Some(edit_state.buf.clone());
            }
        } else {
            let label_text = if options[i].is_empty() {
                format!("(empty {i})")
            } else {
                options[i].clone()
            };
            let label = child.add_sized(
                [name_w, PILL_H],
                egui::Label::new(egui::RichText::new(label_text).small())
                .truncate()
                .sense(egui::Sense::click_and_drag()),
            );
            name = Some(label.clone());
            label.clone().on_hover_cursor(egui::CursorIcon::Grab);
            label.clone().on_hover_text(format!(
                "{} - click to choose, drag to reorder, double-click to rename",
                options[i],
            ));

            // Double-click OR Alt-click -> enter edit mode.
            if label.double_clicked() || (alt && label.clicked()) {
                edit_state.editing = Some(i);
                edit_state.buf = options[i].clone();
                edit_state.request_focus = true;
            }
            // Single click (no Alt) -> choose this entry, undoing the shift pin's step so the
            // option clicked is the option that comes out.
            if label.clicked() && !alt && !label.double_clicked() {
                new_selected = dropdown_base_for(i, selected, effective, n_opts);
                selected_changed = true;
            }
            // Right-click context menu.
            crate::touch::context_menu(&label, |ui| {
                if ui.button("Rename").clicked() {
                    edit_state.editing = Some(i);
                    edit_state.buf = options[i].clone();
                    edit_state.request_focus = true;
                    ui.close();
                }
                ui.separator();
                if ui.add_enabled(i > 0, egui::Button::new("Move up")).clicked() {
                    row_move = -1;
                    ui.close();
                }
                if ui.add_enabled(i + 1 < options.len(), egui::Button::new("Move down")).clicked() {
                    row_move = 1;
                    ui.close();
                }
                ui.separator();
                if ui.button("Delete").clicked() {
                    row_remove = true;
                    ui.close();
                }
            });
        }
        // The grab: the handle or the option's name, since a finger lands on the word rather
        // than on the column of dots beside it. A click stays a click - egui stops reporting
        // one once the pointer has travelled - which is what stops a reorder from also
        // choosing the option it moved.
        let grabbed = handle.dragged() || name.as_ref().is_some_and(|n| n.dragged());
        if grabbed {
            if !drag.active || drag.held != i {
                drag = OptionDrag { held: i, current: i, acc: 0.0, lift: 0.0, active: true };
            }
            // The distance comes from the pointer rather than from the widget: a frame in
            // which something else claims the pointer would otherwise drop what has been
            // carried so far, and a drag then lands wherever the last live frame left it.
            let raw = ui.input(|i| i.pointer.delta()).y;
            let d = if grabbed {
                name
                    .as_ref()
                    .filter(|n| n.dragged())
                    .map_or_else(|| handle.drag_delta().y, |n| n.drag_delta().y)
            } else {
                raw
            };
            // Accumulate in egui memory so a slow drag still crosses the threshold instead of
            // stalling on per-frame sub-pixel deltas, and keep the unstepped distance so the
            // row follows the finger.
            drag.acc += d;
            drag.lift += d;
            // Round rather than truncate: a move is promised once the drag has crossed half
            // a row, and a truncated quotient only moves after a whole one.
            let steps = (drag.acc / pitch).round() as isize;
            if steps != 0 {
                // Only the row's target moves here. The list itself is left alone until the
                // finger lets go: moving the option and drawing the same move as an offset in
                // the same frame is what made a row flicker back and forth under the finger,
                // and committing a step at a time is what stopped a long drag at its neighbour.
                let target = (drag.current as isize + steps).clamp(0, options.len() as isize - 1) as usize;
                drag.current = target;
                drag.acc -= steps as f32 * pitch;
            }
        }
        child.add_space(ROW);
        if child.small_button("×").clicked() {
            row_remove = true;
        }
        drop(child);
        trace_rect(ui, node_id, &format!("dropdown_row_{i}"), row);

        if let Some(new_text) = commit_edit {
            options[i] = new_text;
            options_changed = true;
            edit_state.editing = None;
            edit_state.buf.clear();
        }
        if cancel_edit {
            edit_state.editing = None;
            edit_state.buf.clear();
        }
        if row_remove { to_remove = Some(i); }
        if row_move != 0 { to_move = Some((row_from, row_move)); }
    }


    // The move lands when the finger leaves, at wherever the held row was being held.
    if drag.active && !ui.input(|i| i.pointer.any_down()) && drag.held != drag.current {
        let (from, to) = (drag.held, drag.current);
        to_move = Some((from, to as isize - from as isize));
    }

    // ── Apply mutations ─────────────────────────────────────────────────
    if let Some((i, dir)) = to_move {
        let target = (i as isize + dir).clamp(0, options.len() as isize - 1) as usize;
        if target != i {
            // A dragged option travels: everything it passed steps up one, rather than the
            // two ends trading places, which is what a drag looks like while it is happening.
            let moved_option = options.remove(i);
            options.insert(target, moved_option);
            options_changed = true;
            // Keep selection pointing at the same logical entry.
            if new_selected == i {
                new_selected = target;
                selected_changed = true;
            } else if i < target && new_selected > i && new_selected <= target {
                new_selected -= 1;
                selected_changed = true;
            } else if target < i && new_selected >= target && new_selected < i {
                new_selected += 1;
                selected_changed = true;
            }
        }
    }

    if let Some(rm) = to_remove {
        if rm < options.len() {
            options.remove(rm);
            options_changed = true;
            // Stop editing if the deleted row was the active editor.
            if edit_state.editing == Some(rm) {
                edit_state.editing = None;
                edit_state.buf.clear();
            } else if let Some(e) = edit_state.editing {
                if e > rm { edit_state.editing = Some(e - 1); }
            }
            // Selection clamp: shift down if a row before the selection
            // was deleted, otherwise clamp to the new last index.
            if options.is_empty() {
                new_selected = 0;
            } else if rm < new_selected {
                new_selected = new_selected.saturating_sub(1);
                selected_changed = true;
            } else if new_selected >= options.len() {
                new_selected = options.len() - 1;
                selected_changed = true;
            }
        }
    }

    // A drag is over when the finger leaves, not when a frame happens to report the row as
    // no longer dragged: egui hands the pointer to whatever is under it, and dropping the
    // state then loses the distance already carried, which is what made a reorder feel like
    // it had a mind of its own.
    // fresh one at whatever row it lands on.
    if drag.active && ui.input(|i| i.pointer.any_down()) {
        ui.ctx().data_mut(|d| d.insert_temp(drag_key, drag));
    } else {
        ui.ctx().data_mut(|d| d.remove_temp::<OptionDrag>(drag_key));
    }

    dropdown_set_edit_state(ui.ctx(), node_id, edit_state);

    if options_changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            dropdown_write_options(node, &options);
        }
    }
    if selected_changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            dropdown_write_selected(node, new_selected);
        }
    }
}


/// Renderer used when the dropdown's ComboBox is pinned into a sub-patch
/// layout. Fills the full container (both axes) so the user can resize the
/// layout slot freely in either direction; the selected-option text scales
/// with the container's height. Click opens a popup of options.
pub(crate) fn render_dropdown_selection(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let options = inner_snarl.get_node(inner_id).map(dropdown_read_options).unwrap_or_default();
    let selected = inner_snarl.get_node(inner_id)
        .map(|n| dropdown_read_selected(n, options.len()))
        .unwrap_or(0);
    // The pinned box says what the module's own reading says: the option coming out, which a
    // wire on `shift` may have stepped away from the one stored.
    let live = dropdown_live_index(inner_snarl, inner_id, options.len(), selected);
    let current = options.get(live).cloned().unwrap_or_default();

    let avail = egui::vec2(container.x.max(40.0), container.y.max(16.0));
    let (rect, resp) = ui.allocate_exact_size(avail, egui::Sense::click());

    // Visual: a button-like frame matching the active widget visuals so it
    // sits naturally next to other pinned widgets in the layout.
    let visuals = ui.style().interact(&resp);
    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        visuals.corner_radius,
        visuals.bg_fill,
        visuals.bg_stroke,
        egui::StrokeKind::Inside,
    );

    // Reserve a slice on the right for the ▼ caret so the label never overlaps it.
    let caret_w = (rect.height() * 0.7).clamp(10.0, 24.0);
    let text_rect = egui::Rect::from_min_max(
        rect.min + egui::vec2(6.0, 0.0),
        egui::pos2(rect.max.x - caret_w, rect.max.y),
    );

    // Text size scales with available height. Leave ~25% vertical padding so
    // the glyphs don't kiss the frame; clamp to a sensible range.
    let mut font_size = (rect.height() * 0.7).clamp(8.0, 64.0);
    let label = if current.is_empty() { "—".to_string() } else { current };
    // If the laid-out text is wider than the text slot at the height-derived
    // size, shrink the font to fit so long entries stay readable.
    let max_w = text_rect.width().max(1.0);
    let measure = |size: f32| -> f32 {
        let galley = painter.layout_no_wrap(
            label.clone(),
            egui::FontId::proportional(size),
            visuals.text_color(),
        );
        galley.size().x
    };
    let w = measure(font_size);
    if w > max_w {
        font_size = (font_size * (max_w / w)).max(8.0);
    }

    painter.text(
        egui::pos2(text_rect.min.x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        &label,
        egui::FontId::proportional(font_size),
        visuals.text_color(),
    );

    // ▼ caret, centred in the reserved slot, scaled to height.
    let caret_size = (rect.height() * 0.45).clamp(8.0, 22.0);
    painter.text(
        egui::pos2(rect.max.x - caret_w * 0.5, rect.center().y),
        egui::Align2::CENTER_CENTER,
        "▼",
        egui::FontId::proportional(caret_size),
        visuals.text_color(),
    );

    // Popup menu: open on click, close on selection or outside-click.
    let popup_id = egui::Id::new(("dropdown_pinned_popup", inner_id.0));
    if resp.clicked() {
        egui::Popup::toggle_id(ui.ctx(), popup_id);
    }
    let mut chosen: Option<usize> = None;
    popup_below_widget(
        &resp, popup_id,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            ui.set_min_width(rect.width().max(80.0));
            for (i, opt) in options.iter().enumerate() {
                let label = if opt.is_empty() { format!("(empty {i})") } else { opt.clone() };
                if ui.selectable_label(i == live, label).clicked() {
                    chosen = Some(dropdown_base_for(i, selected, live, options.len()));
                }
            }
        },
    );
    if chosen.is_some() {
        egui::Popup::close_id(ui.ctx(), popup_id);
    }
    if let Some(i) = chosen {
        if let Some(node) = inner_snarl.get_node_mut(inner_id) {
            dropdown_write_selected(node, i);
        }
    }
}

// Per-device rumble-feedback shaping controls on a virtual gamepad's sink node.
// Floor/Max/Curve shape ONLY the game/app rumble forwarded back to a physical
// pad via AutoMap (see `shape_hd_feedback`); direct rumble wiring is untouched.
// Defaults (floor 0.35, max 1.0, exp 0.6) match the original tuned boost.
pub(crate) fn show_sink_body(node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let device_id = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("device_id").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();

    if device_id != "virtual.keymouse" {
        // Virtual gamepad sinks have no body controls — the rumble-feedback
        // shaping (floor/max/curve) lives in the node header alongside the
        // keymouse Mouse × slider (see show_header).
        return;
    }

    let fixed_count = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("fixed_input_count").and_then(|v| v.as_u64()))
        .unwrap_or(0) as usize;

    let is_learning = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("learning").and_then(|v| v.as_bool()))
        .unwrap_or(false);

    if is_learning {
        ui.label(egui::RichText::new("Press a key… (Esc cancels)").italics().small());

        let key_pressed = ui.input(|i| {
            i.events.iter().find_map(|e| {
                if let egui::Event::Key { key, pressed: true, .. } = e {
                    Some(*key)
                } else {
                    None
                }
            })
        });

        if let Some(key) = key_pressed {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("learning".to_string(), Value::Bool(false));
            }

            if key != egui::Key::Escape {
                let pin_name = format!("{key:?}");
                let already_has = snarl
                    .get_node(node_id)
                    .map(|n| n.inputs.iter().any(|p| p.name == pin_name))
                    .unwrap_or(false);

                if !already_has {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.inputs.push(PinDescriptor::new(&pin_name, SignalType::Bool));
                        // Keep input_pin_ids in sync for routing.
                        if let Some(Value::Array(ids)) = node.params.get_mut("input_pin_ids") {
                            ids.push(Value::String(pin_name));
                        }
                    }
                }
            }
        }
    } else {
        ui.horizontal(|ui| {
            if ui.small_button("+ Learn key").clicked() {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert("learning".to_string(), Value::Bool(true));
                }
            }
            let has_unused_learned = inputs.iter().skip(fixed_count).any(|p| p.remotes.is_empty());
            if has_unused_learned && ui.small_button("Clear unused").clicked() {
                clear_unused_inputs(node_id, inputs, fixed_count, snarl);
            }
        });
    }
}

// ── The setting pill ──────────────────────────────────────────────────────────
//
// The house way to show a setting with a short list of values: one filled shape, a
// label cell on the left in the selection colour and the current value on the right.
// No outline anywhere — an outlined thing inside a module reads as a button waiting
// to be pressed, which is the opposite of a setting that answers a drag — and the two
// fills meeting is the only divider. The value cell takes the mouse across its whole
// width: where the pointer is picks the setting, so the list is scrubbed rather than
// clicked through one item at a time.
//
// Like the glyph, it reserves a constant box in the body's flow and draws in a child,
// because absolute placement or a scope here folds the drawn rect back into the
// module's measured width and walks it out a block.

/// The pill's height, shared so a setting row lines up wherever it sits.
pub(crate) const PILL_H: f32 = 20.0;
/// The corner radius, and the font, so the label and value of two different pills
/// sitting one under the other read as the same kind of thing.
const PILL_RADIUS: f32 = 4.0;
pub(crate) const PILL_FONT: f32 = 12.0;

/// The pill font at the rows' current zoom (`curve_rows::zoom`: 1 in a body).
pub(crate) fn pill_font() -> egui::FontId {
    egui::FontId::monospace(super::scale::quantize_font_size((PILL_FONT * super::curve_rows::zoom()).max(4.0)))
}

/// The value cell of a pill drawn across `rect`: everything right of the `label_w`-wide
/// label cell. That's the half that answers the pointer, and what a caller maps a pointer
/// position onto - so it comes from the same arithmetic the pill is painted with.
/// How wide a pill's label cell has to be to sit behind `label`: measured in the monospace the
/// pill is painted with, not in the proportional text style. Measuring with the style's small
/// font made the blue backdrop narrower than the word it stands behind - "min" and "step" both
/// ran out of their own colour block.
pub(crate) fn pill_label_width(ui: &egui::Ui, label: &str) -> f32 {
    let label = pill_label_parts(label).0;
    if label.is_empty() {
        0.0
    } else {
        ui.painter()
            .layout_no_wrap(
                label.to_owned(),
                pill_font(),
                ui.visuals().text_color(),
            )
            .size()
            .x
            + 8.0 * super::curve_rows::zoom()
    }
}

/// A pill's label as `(short, long)`. A label can be written `short|long` - `stab|stabilise`
/// - and the pill shows the long one whenever it and the value both fit the pill's width
/// ([`pill_label_for`]); everything that sizes a pill measures the short one, so the long
/// name only ever fills room that is there. A label with no `|` is both.
pub(crate) fn pill_label_parts(label: &str) -> (&str, &str) {
    label.split_once('|').unwrap_or((label, label))
}

/// Which of a `short|long` label a pill `width` wide shows beside `value`, and how wide its
/// label cell is. `short_w` is the short label's cell, as the caller measured it.
pub(crate) fn pill_label_for<'a>(ui: &egui::Ui, label: &'a str, short_w: f32, value: &str, width: f32) -> (&'a str, f32) {
    let (short, long) = pill_label_parts(label);
    if long == short || short_w <= 0.0 {
        return (short, short_w);
    }
    let z = super::curve_rows::zoom();
    let measure = |t: &str| ui.painter().layout_no_wrap(t.to_owned(), pill_font(), egui::Color32::PLACEHOLDER).size().x;
    let long_w = measure(long) + 8.0 * z;
    // The value keeps air either side of it, as it has in a pill sized for the short name.
    if long_w + measure(value) + 12.0 * z <= width {
        (long, long_w)
    } else {
        (short, short_w)
    }
}
pub(crate) fn pill_value_cell(rect: egui::Rect, label_w: f32) -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(rect.left() + label_w, rect.top()), rect.max)
}

/// Draw a pill into `rect` and return the value cell's response plus the cell itself,
/// which is what the caller maps a pointer position onto.
pub(crate) fn setting_pill(
    ui: &mut egui::Ui,
    salt: &str,
    rect: egui::Rect,
    label: &str,
    value: &str,
    label_w: f32,
) -> (egui::Response, egui::Rect) {
    let (label, label_w) = pill_label_for(ui, label, label_w, value, rect.width());
    let label_cell =
        egui::Rect::from_min_max(rect.min, egui::pos2(rect.left() + label_w, rect.bottom()));
    let value_cell = pill_value_cell(rect, label_w);
    let visuals = ui.visuals();
    let label_fill = visuals.selection.bg_fill;
    let label_text = visuals.strong_text_color();
    let inactive = visuals.widgets.inactive.bg_fill;
    let hovered_fill = visuals.widgets.active.bg_fill;
    let value_text = visuals.text_color();
    let resp = ui
        .new_child(
            egui::UiBuilder::new()
                .max_rect(value_cell)
                .layout(egui::Layout::right_to_left(egui::Align::Center))
                .id_salt(("setting_pill", salt)),
        )
        .allocate_rect(value_cell, egui::Sense::click_and_drag());
    // Feedback from the fill, not from an edge.
    let fill = if resp.dragged() {
        label_fill
    } else if resp.hovered() {
        hovered_fill
    } else {
        inactive
    };
    let painter = ui.painter_at(rect);
    // A pill with no label cell is one rounded shape. Splitting the corners on the label
    // cell's width instead gave it square left corners, which read as the pill being cut
    // off by the frame when the label was "" (Inverse, Logic Delay, Has Changed, Vec to
    // Deflection all paint their pills label-less now).
    let has_label = label_cell.width() > 0.5;
    if has_label {
        painter.rect(
            label_cell,
            egui::CornerRadius { nw: PILL_RADIUS as u8, sw: PILL_RADIUS as u8, ne: 0, se: 0 },
            label_fill,
            egui::Stroke::NONE,
            egui::StrokeKind::Inside,
        );
    }
    painter.rect(
        value_cell,
        if has_label {
            egui::CornerRadius { nw: 0, sw: 0, ne: PILL_RADIUS as u8, se: PILL_RADIUS as u8 }
        } else {
            egui::CornerRadius::same(PILL_RADIUS as u8)
        },
        fill,
        egui::Stroke::NONE,
        egui::StrokeKind::Inside,
    );
    let font = pill_font();
    painter.text(label_cell.center(), egui::Align2::CENTER_CENTER, label, font.clone(), label_text);
    painter.text(value_cell.center(), egui::Align2::CENTER_CENTER, value, font, value_text);
    (resp, value_cell)
}

/// Which of `steps + 1` settings the pointer sits over within the value cell.
pub(crate) fn pill_detent(value_cell: egui::Rect, pos: egui::Pos2, steps: i32) -> i32 {
    let t = (pos.x - value_cell.left()) / value_cell.width();
    (t * steps as f32).round().clamp(0.0, steps as f32) as i32
}

/// The state of a pill's inline edit: a value that can be dragged can't be *landed on*
/// exactly, so double-click or Ctrl-click opens a text field over the value cell and the
/// pill becomes both controls. Held per node and per pill, in egui's temp data, the way
/// the Dropdown's row rename is.
#[derive(Clone, Default)]
struct PillEdit {
    editing: bool,
    buf: String,
    /// The frame the edit opened needs to hand the field focus on the frame after, since
    /// the field doesn't exist yet when the click that opened it lands.
    focus: bool,
    /// And one frame after that, the contents are selected. egui gives a field that has
    /// just taken focus a caret at the end, and a field you can only type in front of -
    /// or behind - is a reset button rather than an editor.
    select: bool,
}

fn pill_edit_key(node: NodeId, salt: &str) -> egui::Id {
    egui::Id::new(("fxi_pill_edit", node.0, salt))
}

/// The house value pill: [`setting_pill`]'s painted shape, dragged across its value cell
/// or typed into it. Returns the response - the caller maps a drag onto its own units -
/// and the text the user committed, which the caller parses, clamps and stores.
///
/// While the field is open the response can't drag: the caller's scrub and the field would
/// both answer the same pointer, and typing a digit would nudge the value.
pub(crate) fn value_pill(
    ui: &mut egui::Ui,
    node: NodeId,
    salt: &str,
    rect: egui::Rect,
    label: &str,
    shown: &str,
    label_w: f32,
) -> (egui::Response, Option<String>) {
    let key = pill_edit_key(node, salt);
    let mut edit = ui
        .ctx()
        .data(|d| d.get_temp::<PillEdit>(key))
        .unwrap_or_default();
    let (resp, cell) = setting_pill(ui, salt, rect, label, shown, label_w);
    if edit.editing {
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(cell)
                .layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight))
                .id_salt(("pill_edit", salt)),
        );
        let mut buf = edit.buf.clone();
        let field = child.add(
            egui::TextEdit::singleline(&mut buf)
                .font(pill_font())
                .horizontal_align(egui::Align::Center)
                .margin(egui::Vec2::new(2.0, 1.0))
                .hint_text(shown)
                .desired_width((cell.width() - 6.0).max(12.0)),
        );
        if edit.focus {
            field.request_focus();
            edit.focus = false;
            edit.select = true;
        } else if edit.select {
            // Focus has landed by now, so the selection survives this frame's write-back.
            edit.select = false;
            let id = field.id;
            let end = buf.chars().count();
            ui.ctx().data_mut(|d| {
                if let Some(mut st) = d.get_temp::<egui::widgets::text_edit::TextEditState>(id) {
                    st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(end),
                    )));
                    d.insert_temp(id, st);
                }
            });
        }
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let mut commit = None;
        if esc {
            edit = PillEdit::default();
        } else if enter || field.lost_focus() {
            commit = Some(buf.clone());
            edit = PillEdit::default();
        } else {
            edit.buf = buf;
        }
        ui.ctx().data_mut(|d| d.insert_temp(key, edit));
        // A response that can't be dragged, so the caller's scrub stays asleep while the
        // field has the pointer. Taken from the child: allocating it in the body would
        // fold the painted rect into the body's measured size and grow the module.
        return (child.allocate_rect(cell, egui::Sense::hover()), commit);
    }
    let typing = ui.input(|i| i.modifiers.command);
    if resp.double_clicked() || (typing && resp.clicked()) {
        // The field opens with the value in it, selected on the frame after focus lands, so
        // you can edit it in place or type over all of it. Opening it empty with the old
        // value painted behind as a hint meant a pill could only ever be reset, not edited.
        edit = PillEdit {
            editing: true,
            buf: shown.to_owned(),
            focus: true,
            select: false,
        };
        ui.ctx().data_mut(|d| d.insert_temp(key, edit));
    }
    (resp, None)
}

/// The same pill for a number, with the two halves of the work in one place: the text
/// committed is read back as a number - a unit suffix rides along in the field, so anything
/// that isn't part of a number is dropped - and clamped to `range`. Returns the response and
/// the new value, if the user typed one.
pub(crate) fn value_pill_number(
    ui: &mut egui::Ui,
    node: NodeId,
    salt: &str,
    rect: egui::Rect,
    label: &str,
    shown: &str,
    label_w: f32,
    range: std::ops::RangeInclusive<f64>,
) -> (egui::Response, Option<f64>) {
    let (resp, typed) = value_pill(ui, node, salt, rect, label, shown, label_w);
    let typed = typed
        .map(|t| {
            t.chars()
                .filter(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
                .collect::<String>()
        })
        .and_then(|t| t.parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .map(|v| v.clamp(*range.start(), *range.end()));
    (resp, typed)
}

/// A trace of where a body put a control, and of the column it centred it in. Nothing
/// reads it at runtime: the rig does, because a module can measure two blocks square and
/// still have its controls off to one side, and a size test alone won't notice. Same idea
/// as `module_icons::paint_glyph` leaving the side of the mark it drew.
pub(crate) fn trace_rect(ui: &egui::Ui, node: NodeId, what: &str, rect: egui::Rect) {
    ui.ctx().data_mut(|d| {
        d.insert_temp(
            egui::Id::new(("module_ctrl_rect", node.0, what)),
            Xysr([rect.min.x, rect.min.y, rect.width(), rect.height()]),
        )
    });
}

/// The trace key [`trace_rect`] writes to, for a test that wants to read a placement back.
#[cfg(test)]
pub(crate) fn traced_rect(ctx: &egui::Context, node: NodeId, what: &str) -> Option<egui::Rect> {
    let Xysr([x, y, w, h]) = ctx.data(|d| d.get_temp::<Xysr>(egui::Id::new(("module_ctrl_rect", node.0, what))))?;
    Some(egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h)))
}

/// How a wire's value is written where a body shows it: four decimals until they stop
/// meaning anything, then scientific notation, so a reading is roughly the width the box
/// around it was sized for. Shared by the Readout and by Sample and Hold's live values,
/// which are the same number seen twice.
pub(crate) fn signal_text(sig: Option<&flexinput_core::Signal>) -> String {
    use flexinput_core::Signal;
    match sig {
        Some(Signal::Float(f)) if f.abs() < 9999.5 => format!("{f:.4}"),
        Some(Signal::Float(f)) => format!("{f:.4e}"),
        Some(Signal::Bool(b)) => if *b { "true".into() } else { "false".into() },
        Some(Signal::Vec2(v)) => format!("({:.2}, {:.2})", v.x, v.y),
        Some(Signal::Vec4(v)) => format!("({:.2}, {:.2}, {:.2}, {:.2})", v.x, v.y, v.z, v.w),
        Some(Signal::Int(i)) => format!("{i}"),
        None => "\u{2014}".into(),
    }
}

/// The box a Constant's number sits in. Its digits are a step larger than a row's value
/// cell, sized so `9999.99` still fits inside a module two blocks wide; a longer number
/// takes the module a block wider, and handing the value back hands the block back.
const CONSTANT_BOX_H: f32 = 30.0;
const CONSTANT_FONT: f32 = 12.0;
/// Air each side of the digits inside the box. Sized so `9999.99` sits inside a module
/// two blocks wide with room to spare, which is the limit the box is built around.
const CONSTANT_PAD: f32 = 8.0;
/// The number the box is sized to hold at rest.
const CONSTANT_FITS: &str = "9999.99";
/// The range a Constant accepts. The wire carries a 32-bit float, so past roughly 3.4e38
/// a value stops being a number downstream at all; the limit here is set well inside
/// that, and said out loud in the box's hover text rather than left to be discovered by
/// watching the digits stop answering the mouse.
const CONSTANT_MAX: f64 = 1.0e12;

/// How many digits a Constant keeps after the dot. `0` means whole numbers; the wire
/// carries a 32-bit float, so past four there is nothing left to keep.
pub(crate) const CONSTANT_RES_MAX: i32 = 4;
/// What a Constant that never set one keeps. Four digits is what a 32-bit float on the
/// wire resolves to anyway, so the default is "as precise as the wire".
pub(crate) const CONSTANT_RES_DEFAULT: i32 = 4;
/// Snap a value to a resolution: `1` keeps tenths, `0` whole numbers. The engine applies
/// the same rule, so what the box shows is what downstream gets.
pub(crate) fn constant_snap(v: f64, res: i32) -> f64 {
    let scale = 10f64.powi(res.clamp(0, CONSTANT_RES_MAX));
    (v * scale).round() / scale
}
/// The pill's label for a resolution: `int` for whole numbers, otherwise the count of
/// digits kept. Writing the step itself (`0.0001`) would grow with every step; the count
/// doesn't, which is what keeps the pill the same width.
fn constant_res_label(res: i32) -> String {
    if res == 0 { "int".to_string() } else { res.to_string() }
}
/// The resolution pill under the box, and the gap above it. Its height is the shared
/// [`PILL_H`], so a setting row reads the same wherever it sits.
const CONSTANT_PILL_LABEL_W: f32 = 34.0;
const CONSTANT_ROW_GAP: f32 = 4.0;

/// How a Constant's number is written in the box: up to `res` digits after the dot, with
/// the trailing zeros trimmed. The box is measured against this same text, so the width
/// follows the digits that are actually shown - a whole number doesn't pay for a dot and
/// two zeros it never sees.
fn constant_text(v: f64, res: i32) -> String {
    let s = format!("{:.*}", res.max(0) as usize, v);
    match s.split_once('.') {
        Some((head, tail)) => {
            let tail = tail.trim_end_matches('0');
            if tail.is_empty() { head.to_string() } else { format!("{head}.{tail}") }
        }
        None => s,
    }
}

pub(crate) fn show_constant_body(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    view_salt: u64,
) {
    // Kept as a 64-bit number, which is what the param stores. Casting to f32 here - as
    // this used to do - is what made an eight-digit value round to something else and
    // then refuse to answer the mouse: at that size the gap between neighbouring f32s is
    // wider than a drag step, so every drag computed a change and stored nothing.
    let stored = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("value").and_then(|v| v.as_f64()))
        .unwrap_or(0.0);
    let res = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("res").and_then(|v| v.as_f64()))
        .map(|r| r as i32)
        .unwrap_or(CONSTANT_RES_DEFAULT)
        .clamp(0, CONSTANT_RES_MAX);
    // The box shows the number the wire will carry, not the one that was typed: a `3.7`
    // read at whole numbers shows `4`, so the setting is never a surprise downstream.
    let value = constant_snap(stored, res);
    let mut v = value;
    let font = egui::FontId::monospace(CONSTANT_FONT);
    let width_of = |text: &str| {
        ui.painter()
            .layout_no_wrap(text.to_string(), font.clone(), egui::Color32::PLACEHOLDER)
            .size()
            .x
            + 2.0 * CONSTANT_PAD
    };
    // The box is as wide as the digits it holds, never as wide as the module happens to
    // be: that's what lets a long value extend the frame and a short one shrink it again,
    // in both directions, without the width ever sticking. The clamp bounds how long
    // those digits can get, so the frame has a ceiling without needing one of its own.
    let shown = constant_text(value, res);
    let box_w = width_of(&shown).max(width_of(CONSTANT_FITS));
    // Box and pill together get reserved in the body's own flow, so the module measures
    // the pair. They are then drawn in children: horizontally centred on the module,
    // because the body column's left edge is fenced in by the pin labels; vertically
    // centred in that column, because the column is what's under the header - centring on
    // the module instead would sit the pair high, with the empty space all below it.
    let group_h = CONSTANT_BOX_H + CONSTANT_ROW_GAP + PILL_H;
    ui.allocate_exact_size(egui::vec2(box_w, group_h), egui::Sense::hover());
    let inner = module_icons::module_inner(ui, view_salt, node_id);
    let column = ui.max_rect();
    let centre_x = inner.map(|r| r.center().x).unwrap_or_else(|| column.center().x);
    let left = centre_x - box_w * 0.5;
    let top = column.center().y - group_h * 0.5;
    let box_rect = egui::Rect::from_min_size(
        egui::pos2(left, top),
        egui::vec2(box_w, CONSTANT_BOX_H),
    );
    let pill_rect = egui::Rect::from_min_size(
        egui::pos2(left, top + CONSTANT_BOX_H + CONSTANT_ROW_GAP),
        egui::vec2(box_w, PILL_H),
    );
    // The digits' size is set on this body's own UI rather than through a scope: a scope
    // would fold the centred child's rect back into the body's measured width, and the
    // module would read its own previous width as content and walk wider.
    ui.style_mut()
        .text_styles
        .insert(egui::TextStyle::Monospace, font);
    // The resolution is the drag's granularity: a box reading tenths moves in tenths.
    // Past four digits the step also grows by powers of ten, or a drag on a big number
    // would take a thousand pixels to move the ones place - but it stays a whole number
    // of resolution steps, so the box never moves in units it can't show. The number
    // stays 64-bit in the box, so a drag answers at any size: it was the f32 read that
    // made a large value look frozen, not the step.
    let coarse = (value.abs().log10() - 4.0).max(0.0).floor() as i32;
    let step = 10f64.powi(-res + coarse);
    let resp = ui
        .new_child(
            egui::UiBuilder::new()
                .max_rect(box_rect)
                .layout(egui::Layout::centered_and_justified(egui::Direction::TopDown))
                .id_salt("constant_box"),
        )
        .add_sized(
            [box_w, CONSTANT_BOX_H],
            egui::DragValue::new(&mut v)
                .speed(step)
                .range(-CONSTANT_MAX..=CONSTANT_MAX)
                .max_decimals(res as usize),
        );
    let resp = resp.on_hover_text(format!(
        "The wire carries a 32-bit float: about 7 digits of it are exact. \
         Values are kept as entered, within \u{00b1}{CONSTANT_MAX:.0}."
    ));
    if resp.changed() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if let Some(n) = Number::from_f64(constant_snap(v, res)) {
                node.params.insert("value".to_string(), Value::Number(n));
            }
        }
    }
    register_exposable_element(ui, node_id, "value", resp.rect);

    // The resolution pill, as wide as the number box above it: `res` and the digits it
    // keeps. Dragging across the value cell scrubs whole numbers -> 1 -> 2 -> 3 -> 4.
    let (pill, value_cell) = setting_pill(
        ui,
        "constant_res",
        pill_rect,
        "res",
        &constant_res_label(res),
        CONSTANT_PILL_LABEL_W,
    );
    let pill = pill.on_hover_text(format!(
        "Digits kept after the dot. Drag across to set it: whole numbers, then 1 to {}.\n\
         The value snaps to this, and the drag step is this size too.",
        CONSTANT_RES_MAX
    ));
    if let Some(pos) = pill.interact_pointer_pos() {
        if pill.dragged() || pill.clicked() {
            let next = pill_detent(value_cell, pos, CONSTANT_RES_MAX);
            if next != res {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert("res".to_string(), Value::Number(Number::from(i64::from(next))));
                }
            }
        }
    }
    register_exposable_element(ui, node_id, "res", pill_rect);
}

// ── Switch ────────────────────────────────────────────────────────────────────
//
// Persisted params (all optional, defaults applied at read time):
//   active:                   bool   — current state. UI clicks and the engine
//                                      both write here; engine reconciles with
//                                      the direct/latch inputs and writes back.
//   caption_on / caption_off: String — text shown for each state.
//   svg_on   / svg_off:       String — SVG source for the icon (empty = none).
//   svg_on_rev / svg_off_rev: u64    — bump on edit to invalidate texture cache.
//   caption_pos_on / caption_pos_off: "top" | "bottom" | "left" | "right"
//                                    — placement of caption relative to icon.

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptionPos { Top, Bottom, Left, Right }

impl CaptionPos {
    fn from_str(s: &str) -> Self {
        match s { "top" => Self::Top, "bottom" => Self::Bottom, "left" => Self::Left, _ => Self::Right }
    }
    fn as_str(self) -> &'static str {
        match self { Self::Top => "top", Self::Bottom => "bottom", Self::Left => "left", Self::Right => "right" }
    }
}

pub(crate) struct SwitchState {
    pub(crate) caption: String,
    pub(crate) svg_data: String,
    pub(crate) svg_rev: u64,
    pub(crate) pos: CaptionPos,
}

pub(crate) fn read_switch_state(node: &NodeData, on: bool) -> SwitchState {
    let (cap_key, svg_key, rev_key, pos_key, default_cap, default_pos) = if on {
        ("caption_on", "svg_on", "svg_on_rev", "caption_pos_on", "ON", "right")
    } else {
        ("caption_off", "svg_off", "svg_off_rev", "caption_pos_off", "OFF", "right")
    };
    SwitchState {
        caption: node.params.get(cap_key).and_then(|v| v.as_str())
            .unwrap_or(default_cap).to_string(),
        svg_data: node.params.get(svg_key).and_then(|v| v.as_str())
            .unwrap_or("").to_string(),
        svg_rev: node.params.get(rev_key).and_then(|v| v.as_u64()).unwrap_or(0),
        pos: CaptionPos::from_str(
            node.params.get(pos_key).and_then(|v| v.as_str()).unwrap_or(default_pos)
        ),
    }
}

/// Right-click submenu under "ON state…" / "OFF state…". Lets the user rename
/// the caption, load/clear an SVG icon, and choose where the caption sits
/// relative to the icon. All actions write directly into `node.params`.
pub(crate) fn switch_state_submenu(
    ui: &mut egui::Ui,
    node_id: NodeId,
    snarl: &mut Snarl<NodeData>,
    on: bool,
) {
    let st = snarl.get_node(node_id).map(|n| read_switch_state(n, on)).unwrap_or(SwitchState {
        caption: (if on { "ON" } else { "OFF" }).to_string(),
        svg_data: String::new(), svg_rev: 0, pos: CaptionPos::Right,
    });
    let (cap_key, svg_key, rev_key, pos_key) = if on {
        ("caption_on", "svg_on", "svg_on_rev", "caption_pos_on")
    } else {
        ("caption_off", "svg_off", "svg_off_rev", "caption_pos_off")
    };

    // Caption text-edit.
    let mut caption = st.caption.clone();
    ui.horizontal(|ui| {
        ui.label("Caption:");
        let resp = ui.add(egui::TextEdit::singleline(&mut caption).desired_width(120.0));
        if resp.changed() {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert(cap_key.to_string(), Value::String(caption.clone()));
            }
        }
    });

    ui.separator();

    // SVG icon: Load… / Clear.
    let has_svg = !st.svg_data.is_empty();
    ui.horizontal(|ui| {
        if ui.button("Load SVG…").clicked() {
            if let Some(path) = crate::overlay::with_overlay_not_topmost(|| {
                rfd::FileDialog::new().add_filter("SVG", &["svg"]).pick_file()
            }) {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert(svg_key.to_string(), Value::String(text));
                        node.params.insert(rev_key.to_string(), Value::from(st.svg_rev + 1));
                    }
                }
            }
            ui.close();
        }
        if has_svg && ui.button("Clear icon").clicked() {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.remove(svg_key);
                node.params.insert(rev_key.to_string(), Value::from(st.svg_rev + 1));
            }
            ui.close();
        }
    });

    ui.separator();

    // Caption position relative to icon. Only meaningful when an icon is set,
    // but we still expose the choice so the user can pre-configure.
    ui.label("Caption position:");
    let mut chosen = st.pos;
    for (label, value) in [
        ("Above", CaptionPos::Top),
        ("Below", CaptionPos::Bottom),
        ("Left",  CaptionPos::Left),
        ("Right", CaptionPos::Right),
    ] {
        if ui.radio(chosen == value, label).clicked() {
            chosen = value;
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert(pos_key.to_string(), Value::String(value.as_str().to_string()));
            }
        }
    }
}

/// Rasterise an SVG to a square texture, cached per (node, side, rev, size).
/// Returns None on parse failure or empty SVG.
pub(crate) fn switch_icon_texture(
    ui: &egui::Ui,
    node_uid: usize,
    side: &'static str,  // "on" or "off"
    svg: &str,
    rev: u64,
    px: u32,
) -> Option<egui::TextureHandle> {
    if svg.is_empty() || px == 0 { return None; }
    let key = egui::Id::new(("switch_icon_tex", node_uid, side, rev, px));
    if let Some(h) = crate::widgets::tex_cache::get(ui.ctx(), key) {
        return Some(h);
    }
    let img = rasterize_svg_recolored(svg, px, px, "override", egui::Color32::TRANSPARENT)?;
    let handle = ui.ctx().load_texture(
        format!("switch-{}-{}-{}", node_uid, side, rev),
        img,
        egui::TextureOptions::LINEAR,
    );
    crate::widgets::tex_cache::put(ui.ctx(), key, handle.clone());
    Some(handle)
}

/// Lay out caption + icon inside `rect` according to `pos`. Returns (caption_rect, icon_rect).
/// Either rect may be empty (caption empty → no caption_rect; no icon → no icon_rect).
pub(crate) fn switch_layout_icon_caption(
    rect: egui::Rect,
    has_caption: bool,
    has_icon: bool,
    pos: CaptionPos,
) -> (Option<egui::Rect>, Option<egui::Rect>) {
    if !has_caption && !has_icon { return (None, None); }
    if !has_caption { return (None, Some(rect)); }
    if !has_icon    { return (Some(rect), None); }

    // Both present: caption sits on the chosen side. Icon claims the
    // remaining square (or the larger axis if the rect isn't square).
    let pad = 2.0_f32;
    match pos {
        CaptionPos::Left => {
            // The icon gets a square the button's height; the caption gets the rest, which
            // is what the button was sized around (see `show_switch_body`). A percentage of
            // the width made the caption slot shrink and grow with the frame instead.
            let icon_side = rect.height().min(rect.width() * 0.6);
            let cap_w = (rect.width() - icon_side - pad).max(20.0);
            let cap = egui::Rect::from_min_size(rect.min, egui::vec2(cap_w, rect.height()));
            let icon = egui::Rect::from_min_max(
                egui::pos2(rect.min.x + cap_w + pad, rect.min.y),
                rect.max,
            );
            (Some(cap), Some(icon))
        }
        CaptionPos::Right => {
            let icon_side = rect.height().min(rect.width() * 0.6);
            let cap_w = (rect.width() - icon_side - pad).max(20.0);
            let icon = egui::Rect::from_min_max(
                rect.min,
                egui::pos2(rect.max.x - cap_w - pad, rect.max.y),
            );
            let cap = egui::Rect::from_min_max(
                egui::pos2(rect.max.x - cap_w, rect.min.y),
                rect.max,
            );
            (Some(cap), Some(icon))
        }
        CaptionPos::Top => {
            let cap_h = (rect.height() * 0.35).clamp(12.0, rect.height() - 16.0);
            let cap = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), cap_h));
            let icon = egui::Rect::from_min_max(
                egui::pos2(rect.min.x, rect.min.y + cap_h + pad),
                rect.max,
            );
            (Some(cap), Some(icon))
        }
        CaptionPos::Bottom => {
            let cap_h = (rect.height() * 0.35).clamp(12.0, rect.height() - 16.0);
            let icon = egui::Rect::from_min_max(
                rect.min,
                egui::pos2(rect.max.x, rect.max.y - cap_h - pad),
            );
            let cap = egui::Rect::from_min_size(
                egui::pos2(rect.min.x, rect.max.y - cap_h),
                egui::vec2(rect.width(), cap_h),
            );
            (Some(cap), Some(icon))
        }
    }
}

/// Paint icon + caption into the button rect for the current state.
/// `fill_text` is the caption color; `icon_tint` is the SVG tint
/// (Color32::WHITE for no tint, fully transparent for tint disabled).
pub(crate) fn paint_switch_content(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    node_uid: usize,
    state: &SwitchState,
    on: bool,
    text_col: egui::Color32,
) {
    let has_caption = !state.caption.is_empty();
    let has_icon = !state.svg_data.is_empty();
    let (cap_rect, icon_rect) = switch_layout_icon_caption(rect, has_caption, has_icon, state.pos);

    if let Some(ir) = icon_rect {
        // Square the icon within its slot.
        let side_px = ir.width().min(ir.height()).round().max(1.0);
        let icon_box = egui::Rect::from_center_size(ir.center(), egui::vec2(side_px, side_px));
        let tex_px = (side_px as u32).max(8);
        let side_id = if on { "on" } else { "off" };
        if let Some(tex) = switch_icon_texture(ui, node_uid, side_id, &state.svg_data, state.svg_rev, tex_px) {
            ui.painter().image(
                tex.id(),
                icon_box,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
    }
    if let Some(cr) = cap_rect {
        // Scale the caption to fill its slot. ~75% of slot height,
        // shrink-to-fit horizontally so long captions stay readable.
        let mut size = (cr.height() * 0.7).clamp(8.0, 64.0);
        let painter = ui.painter();
        let measure = |s: f32| -> f32 {
            painter.layout_no_wrap(
                state.caption.clone(),
                egui::FontId::proportional(s),
                text_col,
            ).size().x
        };
        let max_w = cr.width().max(1.0);
        let w = measure(size);
        if w > max_w {
            size = (size * (max_w / w)).max(8.0);
        }
        painter.text(
            cr.center(),
            egui::Align2::CENTER_CENTER,
            &state.caption,
            egui::FontId::proportional(size),
            text_col,
        );
    }
}

pub(crate) fn read_switch_active(node: &NodeData) -> bool {
    // Prefer the engine's last emitted value — it reconciles UI clicks with
    // direct/latch inputs and is the authoritative current state. Fall back
    // to the persisted `active` when no eval result is available yet (patch
    // just opened, paused engine, etc.).
    if let Some(Some(Signal::Bool(b))) = node.extra.last_out.first() {
        return *b;
    }
    node.params.get("active").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Click handler shared by canvas body + pinned renderer. Bumps a monotonic
/// `ui_toggle_seq` counter (engine toggles once per increment) and writes an
/// optimistic flipped `active` so the button visually updates the same frame.
pub(crate) fn switch_handle_click(node: &mut NodeData, current_active: bool) {
    let seq = node.params.get("ui_toggle_seq").and_then(|v| v.as_u64()).unwrap_or(0);
    node.params.insert("ui_toggle_seq".to_string(), Value::from(seq.wrapping_add(1)));
    node.params.insert("active".to_string(), Value::Bool(!current_active));
}

pub(crate) fn show_switch_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, view_salt: u64) {
    let active = snarl.get_node(node_id).map(read_switch_active).unwrap_or(false);
    let state = snarl.get_node(node_id).map(|n| read_switch_state(n, active)).unwrap_or(SwitchState {
        caption: (if active { "ON" } else { "OFF" }).to_string(),
        svg_data: String::new(), svg_rev: 0, pos: CaptionPos::Right,
    });

    // A button size that comfortably fits the chosen caption and icon. The caption is
    // measured at the size it's painted at, so a label set from the context menu - or an
    // icon with the caption beside it rather than under - takes the module another block,
    // and hands it back when it goes. The measurement is of the stored text, never of the
    // frame the module happens to have, so it can't ratchet; past the cap the caption
    // shrinks to fit instead of the module growing without end.
    let has_icon = !state.svg_data.is_empty();
    let has_caption = !state.caption.is_empty();
    const AIR: f32 = 8.0;
    const CAPTION_MAX_W: f32 = 160.0;
    let base_h: f32 = if has_icon { 32.0 } else { 22.0 };
    let text_size = (base_h * 0.7).clamp(8.0, 64.0);
    let text_w = if has_caption {
        ui.painter()
            .layout_no_wrap(
                state.caption.clone(),
                egui::FontId::proportional(text_size),
                egui::Color32::PLACEHOLDER,
            )
            .size()
            .x
            .min(CAPTION_MAX_W)
    } else {
        0.0
    };
    let stacked = matches!(state.pos, CaptionPos::Top | CaptionPos::Bottom);
    let (min_w, h) = match (has_icon, has_caption) {
        (false, false) => (48.0, base_h),
        (true, false) => (base_h + 2.0 * AIR, base_h),
        (false, true) => ((text_w + 2.0 * AIR).max(48.0), base_h),
        (true, true) if stacked => ((base_h + 22.0).max(text_w + 2.0 * AIR), base_h + 22.0),
        (true, true) => (base_h + 4.0 + text_w + 2.0 * AIR, base_h),
    };
    // The button is reserved in the flow - that's what makes the module grow for it - and
    // painted centred on the module rather than on the column between the pin labels.
    let (flow, _) = ui.allocate_exact_size(egui::vec2(min_w, h), egui::Sense::hover());
    let column = ui.max_rect();
    let inner = module_icons::module_inner(ui, view_salt, node_id).unwrap_or(flow);
    // Until the frame has caught up with the reservation - the first frame after a caption
    // is set, when the column is still a sliver - the button is drawn into the allocation
    // rather than handed a negative width.
    let rect = if inner.width() >= min_w + 12.0
        && inner.height() >= h + 24.0
        && inner.bottom() - h - 6.0 > column.top()
    {
        egui::Rect::from_min_size(
            egui::pos2(inner.center().x - min_w * 0.5, super::kit::group_top(column, h)),
            egui::vec2(min_w, h),
        )
    } else {
        flow
    };
    let resp = ui.interact(rect, ui.id().with("switch_toggle"), egui::Sense::click());
    trace_rect(ui, node_id, "body_column", column);
    trace_rect(ui, node_id, "switch_button", rect);

    let visuals = if active {
        ui.style().visuals.selection.bg_fill
    } else {
        ui.style().visuals.widgets.inactive.bg_fill
    };
    let stroke_col = if active {
        ui.style().visuals.selection.stroke.color
    } else {
        ui.style().visuals.widgets.inactive.bg_stroke.color
    };
    let text_col = if active {
        ui.style().visuals.strong_text_color()
    } else {
        ui.style().visuals.text_color()
    };

    let painter = ui.painter_at(rect);
    painter.rect(rect, 4.0, visuals,
        egui::Stroke::new(1.0_f32, stroke_col), egui::StrokeKind::Inside);
    paint_switch_content(ui, rect, node_id.0, &state, active, text_col);

    if resp.clicked() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            switch_handle_click(node, active);
        }
    }
    register_exposable_element(ui, node_id, "toggle", rect);
}

/// The Text module's body is its text field and nothing else. The frame's own corner
/// grip sets the size, in blocks, through the grid's `ui_w` / `ui_h` params, and the
/// field fills what it's given: width is what the text wraps to, height is a floor that
/// the text overruns by growing the module another block. Font size and colour are in
/// the header, so nothing here competes with the writing.
pub(crate) fn show_label_body(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    width: f32,
    height: f32,
    view_salt: u64,
) {
    // The width and height are reserved in the body's flow rather than read from what the
    // text happens to need: the body reports exactly this size whatever the text draws, so
    // the node settles on the size the grip asked for instead of walking out from its own
    // last width.
    // How tall the writing actually is at this width: the field's floor is what the grip
    // asked for, and the text outranks it. egui's multiline edit fills a floor and then
    // clips, so the overrun is measured here and reserved in the flow - the grid rounds
    // the module up to whole blocks from it.
    let (text_now, font_now) = snarl.get_node(node_id).map_or_else(
        || ("Label".to_string(), 14.0_f32),
        |n| {
            (
                n.params.get("text").and_then(|v| v.as_str()).unwrap_or("Label").to_string(),
                n.params.get("font_size").and_then(|v| v.as_f64()).unwrap_or(14.0) as f32,
            )
        },
    );
    let wrapped = egui::WidgetText::from(egui::RichText::new(text_now.as_str()).size(font_now))
        .into_galley(ui, Some(egui::TextWrapMode::Wrap), (width - 4.0).max(40.0), egui::FontId::proportional(font_now));
    let text_h = wrapped.size().y + 6.0;
    let reserved = egui::vec2(width, height.max(text_h));
    let (_, _) = ui.allocate_exact_size(reserved, egui::Sense::hover());
    // The field is drawn across what's inside the frame, not across the body column: that
    // column is padded by the pin columns' spacing, which on a module with no pins reads
    // as a margin that comes and goes with the text. Its bottom is generous rather than
    // exact, so writing more than the floor fits takes the module another block instead
    // of being clipped at it - and stays finite, which egui's layout maths needs.
    let column = ui.max_rect();
    let inner = module_icons::module_inner(ui, view_salt, node_id).unwrap_or(column);
    let rect = egui::Rect::from_min_max(
        egui::pos2(inner.left().min(column.left()), column.top()),
        egui::pos2(inner.right().max(column.right()), column.top() + 8_000.0),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min))
            .id_salt("label_field"),
    );
    show_label_body_sized(node_id, &mut child, snarl, rect.width(), height);
    // A trace of the box the field was drawn into, so a test can check it covers the body
    // rather than sitting inside it at a distance that depends on how much text happens
    // to be there. Nothing at runtime reads it.
    let drawn = child.min_rect();
    let [x, y, w, h] = [drawn.min.x, drawn.min.y, drawn.width(), drawn.height()];
    ui.ctx()
        .data_mut(|d| d.insert_temp(egui::Id::new(("label_field_rect", node_id.0)), xysr(x, y, w, h)));
    register_exposable_element(ui, node_id, "text", drawn);
}

/// A rect in a form egui's temp storage will take.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Xysr(pub [f32; 4]);
fn xysr(x: f32, y: f32, w: f32, h: f32) -> Xysr {
    Xysr([x, y, w, h])
}

/// Same as `show_label_body` but with explicit width / height for use when the
/// label is pinned to a sub-patch body and the user has resized its container.
/// Pass `height = 0.0` to let the text edit auto-size vertically.
pub(crate) fn show_label_body_sized(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    width: f32,
    height: f32,
) {
    let (mut text, font_size, col) = snarl.get_node(node_id).map(|n| {
        let t = n.params.get("text").and_then(|v| v.as_str()).unwrap_or("Label").to_string();
        let f = n.params.get("font_size").and_then(|v| v.as_f64()).unwrap_or(14.0) as f32;
        let c = read_label_color(n);
        (t, f, c)
    }).unwrap_or_else(|| ("Label".to_string(), 14.0, egui::Color32::from_rgb(220, 220, 220)));

    let prev_text = text.clone();

    // No frame of its own: the module's frame is the boundary, and a second border a few
    // points inside it reads as a box that shrank or grew depending on how full it is.
    let mut edit = egui::TextEdit::multiline(&mut text)
        .font(egui::FontId::proportional(font_size))
        .text_color(col)
        .frame(false)
        .margin(egui::Margin::same(2))
        .desired_width((width - 4.0).max(40.0));
    if height > 8.0 {
        // Filled to the height the grip asked for, in points rather than in guessed rows:
        // a row count worked out from an assumed line height leaves a strip of empty
        // body under the text, which is the padding that looked random. `min_size` is a
        // floor — writing past it still takes more room, and the module with it.
        edit = edit.min_size(egui::vec2((width - 4.0).max(40.0), height - 4.0));
    }
    ui.add(edit);

    if text != prev_text {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("text".to_string(), Value::String(text));
        }
    }
}

/// Read the text color stored on a label node. Defaults to a soft gray when
/// the param is missing or malformed (older patches saved before color support).
pub(crate) fn read_label_color(n: &NodeData) -> egui::Color32 {
    match n.params.get("color").and_then(|v| v.as_array()) {
        Some(arr) if arr.len() == 4 => egui::Color32::from_rgba_unmultiplied(
            arr[0].as_u64().unwrap_or(220) as u8,
            arr[1].as_u64().unwrap_or(220) as u8,
            arr[2].as_u64().unwrap_or(220) as u8,
            arr[3].as_u64().unwrap_or(255) as u8,
        ),
        _ => egui::Color32::from_rgb(220, 220, 220),
    }
}

/// Read the SVG tint color. Default is fully transparent (alpha = 0 = no tint).
pub(crate) fn read_svg_tint(n: &NodeData) -> egui::Color32 {
    match n.params.get("tint").and_then(|v| v.as_array()) {
        Some(arr) if arr.len() == 4 => egui::Color32::from_rgba_unmultiplied(
            arr[0].as_u64().unwrap_or(255) as u8,
            arr[1].as_u64().unwrap_or(255) as u8,
            arr[2].as_u64().unwrap_or(255) as u8,
            arr[3].as_u64().unwrap_or(0) as u8,
        ),
        _ => egui::Color32::TRANSPARENT,
    }
}

pub(crate) fn show_svg_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let outer = ui.allocate_ui(egui::vec2(160.0, 0.0), |ui| {
        show_svg_body_sized(node_id, ui, snarl, egui::vec2(160.0, 160.0));
    });
    register_exposable_element(ui, node_id, "image", outer.response.rect);
}

/// Renders the loaded SVG into a resizable area. The SVG is rasterized via
/// usvg/resvg to an RGBA pixmap whose pixels we then recolor according to the
/// chosen mode (Override = blend toward chosen color; Additive = add chosen
/// color on top). The recolored pixmap is uploaded as an egui texture and
/// painted into the rect. The texture is cached in egui memory keyed by
/// (node, rev, target_size, mode, color) so resizing/recoloring force a new
/// rasterization but unchanged frames don't.
pub(crate) fn show_svg_body_sized(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (data, rev, tint, mode) = snarl.get_node(node_id).map(|n| {
        let d = n.params.get("svg_data").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let r = n.params.get("svg_rev").and_then(|v| v.as_u64()).unwrap_or(0);
        let t = read_svg_tint(n);
        let m = n.params.get("color_mode").and_then(|v| v.as_str()).unwrap_or("override").to_string();
        (d, r, t, m)
    }).unwrap_or_default();

    let target = egui::vec2(container.x.max(16.0), container.y.max(16.0));
    let (rect, _) = ui.allocate_exact_size(target, egui::Sense::hover());

    if data.is_empty() {
        let painter = ui.painter_at(rect);
        painter.rect_stroke(rect, 4.0,
            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(70)),
            egui::StrokeKind::Inside);
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No SVG loaded\nclick Load… in header",
            egui::FontId::proportional(11.0),
            egui::Color32::from_gray(150),
        );
        return;
    }

    let pw = (rect.width().round() as u32).max(1);
    let ph = (rect.height().round() as u32).max(1);
    let cache_key = egui::Id::new((
        "svg_tex_cache", node_id.0, rev, pw, ph, mode.as_str(),
        tint.r(), tint.g(), tint.b(), tint.a(),
    ));
    let cached = crate::widgets::tex_cache::get(ui.ctx(), cache_key);
    let tex = match cached {
        Some(t) => t,
        None => {
            match rasterize_svg_recolored(&data, pw, ph, &mode, tint) {
                Some(image) => {
                    let handle = ui.ctx().load_texture(
                        format!("svg-{}-{}", node_id.0, rev),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    crate::widgets::tex_cache::put(ui.ctx(), cache_key, handle.clone());
                    handle
                }
                None => {
                    ui.painter_at(rect).text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "SVG parse failed",
                        egui::FontId::proportional(11.0),
                        egui::Color32::from_rgb(220, 80, 80),
                    );
                    return;
                }
            }
        }
    };

    ui.painter().image(
        tex.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}

// ── Macro Output body ─────────────────────────────────────────────────────────

/// A name too long for its cell, cut with an ellipsis instead of painted over whatever
/// sits beside it. The whole word stays in the hover text, so cutting costs nothing the
/// user has to have on the canvas. Laid out in the font and colour it will be painted in,
/// because the cut has to land where the glyphs do.
pub(crate) fn elide_to_width(ui: &egui::Ui, text: &str, font: egui::FontId, colour: egui::Color32, avail: f32) -> String {
    let painter = ui.painter();
    if painter.layout_no_wrap(text.to_string(), font.clone(), colour).size().x <= avail {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    for k in (1..chars.len()).rev() {
        let trial: String = chars[..k].iter().collect::<String>();
        let g = painter.layout_no_wrap(format!("{trial}\u{2026}"), font.clone(), colour);
        if g.size().x <= avail {
            return g.text().to_string();
        }
    }
    "\u{2026}".to_string()
}

/// Which Macro Output row's name is being edited, and what has been typed so far. Kept
/// between frames like the row's drag: the field has to survive the frame that opened it.
#[derive(Default, Clone)]
struct MacroEdit {
    editing: Option<String>,
    buf: String,
    focus: bool,
}

/// Body for the Macro Output node: the list of user-defined ports, edited
/// inline — icon (menu of the embedded macro icon set), name (painted, and edited in
/// place where it stands), type (Bool /
/// Float / Vec2 / Any), remove. Every change rewrites the `macro_ports`
/// param, the node's dynamic output pins, and `output_pin_ids` together so
/// the three can never drift. Removal drops the port's wires and shifts the
/// ones above down a slot (same surgery as the AutoMap Splitter).
pub(crate) fn show_macro_body(node_id: NodeId, outputs: &[OutPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, view_salt: u64) {
    use flexinput_core::macros as mac;

    let Some(node) = snarl.get_node(node_id) else { return };
    let mut ports = mac::ports_from_params(&node.params);
    let mut changed = false;

    // First open of a fresh node: seed one port so the node has a pin and the
    // picker immediately shows something assignable.
    if ports.is_empty() && node.params.get(mac::MACRO_PORTS_PARAM).is_none() {
        ports.push(mac::MacroPortDef {
            id: mac::new_port_id(),
            name: "Macro 1".to_string(),
            icon: String::new(),
            icon_svg: String::new(),
            signal_type: SignalType::Bool,
        });
        changed = true;
    }

    let mut remove_idx: Option<usize> = None;
    let mut moved: Option<(usize, isize)> = None;

    // The rows are a list like every other list on the canvas: a handle to move the port, the
    // icon, the name, the signal type, and the `×` at the right edge. The column's *ask* comes
    // from its content - the module was 308 points wide once because every row measured itself
    // out as an icon button, a 96-point field, a 58-point dropdown and a button labelled
    // "+ Add output" - but the name's share of it is measured rather than fixed: the widest name
    // in the list, clamped to the 28..140 a port strip clamps to. A Macro's rows *are* its pins,
    // so past that clamp the module pays a block of width rather than the name being cut: reading
    // which port carries what is the only reason the module is on the canvas.
    const ROW: f32 = 4.0;
    const HANDLE_W: f32 = 18.0;
    const ICON_W: f32 = 24.0;
    const TYPE_W: f32 = 46.0;
    const RM_W: f32 = 22.0;
    const NAME_MIN: f32 = 28.0;
    const NAME_MAX: f32 = STRIP_NAME_MAX;
    // What the strip keeps clear between its right edge and the `×`.
    const EDGE: f32 = 2.0;
    // Which port's name is being edited - by its id, so a row that moved under a drag keeps
    // the field it had open. Loaded before the column is measured: the text being typed is
    // one of the names the column has to hold, so the module widens as it is typed rather
    // than when Enter is pressed.
    let edit_key = egui::Id::new(("fxi_macro_edit", node_id.0));
    let mut edit = ui.ctx().data(|d| d.get_temp::<MacroEdit>(edit_key)).unwrap_or_default();
    let name_font = ui
        .style()
        .text_styles
        .get(&egui::TextStyle::Small)
        .cloned()
        .unwrap_or_else(|| egui::FontId::proportional(12.0));
    let name_colour = ui.visuals().text_color();
    let ask_name_w = ports
        .iter()
        .map(|p| if p.name.is_empty() { "—" } else { p.name.as_str() })
        .chain(edit.editing.is_some().then_some(edit.buf.as_str()))
        .map(|n| ui.painter().layout_no_wrap(n.to_string(), name_font.clone(), name_colour).size().x + 6.0)
        .fold(NAME_MIN, |widest, w| widest.max(w))
        .clamp(NAME_MIN, NAME_MAX);
    let list_w_ask = HANDLE_W + ICON_W + ask_name_w + TYPE_W + RM_W + 4.0 * ROW + EDGE;

    // The rows are the rows of the output sockets, from the body's top down - the same
    // `StripRows` a Split's strips stand on, so the two can't disagree about where a socket
    // is. (They did: these rows were laid at the height a pin row is allocated, without the
    // spacing the pin column puts under each one, and the list was centred in the body.)
    let rows_at = kit::StripRows::at_cursor("module.macro", ui);
    let (row_h, pitch) = (rows_at.row_h, rows_at.advance);
    let list_h = rows_at.height(ports.len());
    ui.allocate_exact_size(egui::vec2(list_w_ask, list_h), egui::Sense::hover());
    // Folded, the column is still asked for - the frame keeps its blocks - but no row is drawn:
    // snarl runs a collapsed node's body and shifts it under the header's clip, which is how a
    // folded Macro painted its first port - icon, name, type, `×` - straight across the header's
    // own name. The sockets stay; their names don't come back beside them, because a collapsed
    // node's pin column is laid out above the header and clipped there.
    if !node_open(snarl, node_id) {
        return;
    }
    let inner = match super::module_icons::module_inner(ui, view_salt, node_id) {
        Some(r) => r,
        None => return,
    };
    // The column runs the width the body has, from its left edge to the body's right edge: the
    // strip is what names the port, so the sockets hang off the end of the row it sits level
    // with. Centring it on the frame instead - as a module with pins on both sides is centred -
    // left a strip of dead margin on the left, where this module has no input pins at all.
    let column = ui.max_rect();
    trace_rect(ui, node_id, "body_column", column);
    let list_w = column.width().max(list_w_ask);
    let list_left = column.left().max(inner.left());
    let name_w = (list_w - HANDLE_W - ICON_W - TYPE_W - RM_W - 4.0 * ROW - EDGE).max(NAME_MIN);
    let row_rect = |i: usize| rows_at.band(i, list_left, list_w);
    let (fill_idle, fill_held) = {
        let v = ui.visuals();
        (v.widgets.inactive.weak_bg_fill, v.widgets.active.bg_fill)
    };
    // A port picked up follows the finger and the ports it passes slide a row aside - the
    // same `StripAnim` the Split and Selector strips move by, so a port you grab behaves
    // like a pin you grab.
    let drag_key = egui::Id::new(("fxi_macro_drag", node_id.0));
    let mut anim = StripAnim::begin(ui.ctx(), drag_key, ports.len(), pitch);
    for (i, port) in ports.iter_mut().enumerate() {
        let row = row_rect(i).translate(egui::vec2(0.0, anim.offset(i)));
        let held = anim.dragged(i);
        // The strip a Split's port stands on: the row reads as one thing with its socket at
        // the end of it, and the buttons on it don't each need a plate of their own.
        ui.painter().rect(
            row.shrink2(egui::vec2(0.0, 1.0)),
            4.0,
            if held { fill_held } else { fill_idle },
            egui::Stroke::NONE,
            egui::StrokeKind::Inside,
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(row)
                .layout(egui::Layout::left_to_right(egui::Align::Center))
                .id_salt(("macro_row", node_id, i)),
        );
        child.spacing_mut().item_spacing.x = 0.0;
        // The handle: dragging a port past half a row moves it, counted from where the port
        // stands now rather than the row the finger grabbed, so a two-row drag doesn't bounce
        // it back. The pins and their wires move with it further down.
        let handle = child.add_sized(
            [HANDLE_W, row_h],
            egui::Label::new("").sense(egui::Sense::drag()),
        );
        // Painted dots, for the reason the Dropdown rows now have them: a label grip took on
        // the list's colours mid-drag. Only the held row's grip brightens.
        child.painter().text(
            handle.rect.center(),
            egui::Align2::CENTER_CENTER,
            "⠿",
            egui::FontId::proportional(12.0),
            if held {
                ui.visuals().strong_text_color()
            } else {
                ui.visuals().weak_text_color()
            },
        );
        handle.clone().on_hover_cursor(egui::CursorIcon::Grab).on_hover_text("Drag to reorder");
        child.add_space(ROW);
        // Icon button + picker (shared with the Virtual Menu module): the
        // custom-SVG loader, category dropdown, name search, and the
        // embedded-set grid all live in `icon_picker_button`.
        if let Some((k, svg)) = crate::canvas::menu_body::icon_picker_button(
            &mut child,
            egui::Id::new((node_id, "macro_icon_grid", i)),
            &port.icon,
            &port.icon_svg,
        ) {
            port.icon = k;
            port.icon_svg = svg;
            changed = true;
        }
        child.add_space(ROW);
        // The name's cell is placed rather than flowed into. A row laid out left-to-right with
        // `Align::Center` centres whatever it is handed across the box it asked for, so a short
        // port name started a dozen points right of a long one and the column of names read as
        // a scatter. Painted, cut with an ellipsis, whole name in the hover text - the same
        // bargain a port strip's name makes.
        //
        // It is not a field that is open every frame either: an open `TextEdit` lays its text
        // out at whatever width the text wants, so a port named "Right Trigger, half pressed"
        // ran across the type dropdown, the `×` and the module's own edge - the cell's clamp
        // bounded what the row asked for and never what was drawn inside it. A click opens the
        // field in the cell.
        let name_left = child.cursor().min.x;
        let name_rect = egui::Rect::from_min_size(
            egui::pos2(name_left, row.center().y - row_h * 0.5),
            egui::vec2(name_w, row_h),
        );
        let editing_here = edit.editing.as_deref() == Some(port.id.as_str());
        let name_resp = if editing_here {
            let mut buf = edit.buf.clone();
            let resp = ui.put(
                name_rect,
                egui::TextEdit::singleline(&mut buf)
                    .font(name_font.clone())
                    .hint_text("name")
                    .desired_width((name_rect.width() - 8.0).max(8.0)),
            );
            if edit.focus {
                resp.request_focus();
                select_all(ui.ctx(), resp.id, &buf);
                edit.focus = false;
            }
            let enter = ui.input(|inp| inp.key_pressed(egui::Key::Enter));
            let esc = ui.input(|inp| inp.key_pressed(egui::Key::Escape));
            if esc {
                edit.editing = None;
                edit.buf.clear();
            } else if enter || resp.lost_focus() {
                let typed = buf.trim().to_string();
                // Typing nothing means "never mind", not "this port has no name".
                if !typed.is_empty() && typed != port.name {
                    port.name = typed;
                    changed = true;
                }
                edit.editing = None;
                edit.buf.clear();
            } else {
                edit.buf = buf;
            }
            resp
        } else {
            let empty = port.name.is_empty();
            let shown = if empty {
                "—".to_string()
            } else {
                elide_to_width(
                    &child,
                    &port.name,
                    name_font.clone(),
                    ui.visuals().text_color(),
                    name_rect.width() - 4.0,
                )
            };
            let resp = ui.interact(
                name_rect,
                egui::Id::new(("macro_name", node_id.0, i)),
                egui::Sense::click_and_drag(),
            );
            ui.painter().text(
                egui::pos2(name_rect.left() + 2.0, name_rect.center().y),
                egui::Align2::LEFT_CENTER,
                shown,
                name_font.clone(),
                if empty {
                    ui.visuals().weak_text_color()
                } else {
                    ui.visuals().text_color()
                },
            );
            resp.clone()
                .on_hover_cursor(egui::CursorIcon::Text)
                .on_hover_text(format!("{} - click to rename, drag to reorder", port.name));
            if resp.clicked() {
                edit.editing = Some(port.id.clone());
                // Open on the name it has, all of it selected: typing replaces it, and a
                // click or an arrow key puts the cursor in it to change a letter.
                edit.buf = port.name.clone();
                edit.focus = true;
            }
            resp
        };
        trace_rect(ui, node_id, &format!("macro_name_{i}"), name_resp.rect);
        // The grab is the handle *or* the word - a finger lands on the name, not on the column
        // of dots beside it - and egui stops reporting a click once the pointer has travelled,
        // which is what keeps a reorder from also opening the field it dragged. Nothing in
        // the pin list moves until the finger lets go; the pins and their wires move with the
        // port further down.
        if let Some((from, to)) = anim.handle(&handle, i) {
            moved = Some((from, to as isize - from as isize));
        }
        if !editing_here {
            if let Some((from, to)) = anim.handle(&name_resp, i) {
                moved = Some((from, to as isize - from as isize));
            }
        }
        // The type dropdown and the `×` are pinned to the row's right edge rather than trailing
        // the name. In a flow each one lands wherever the previous widget's own width left the
        // cursor, so the column of `×`s was a column only by accident - and it stopped some
        // fifteen points short of the edge its sockets hang off.
        let btn_h = (row_h - 5.0).max(PILL_H);
        let rm_rect = egui::Rect::from_min_size(
            egui::pos2(row.right() - EDGE - RM_W, row.center().y - btn_h * 0.5),
            egui::vec2(RM_W, btn_h),
        );
        let ty_rect = egui::Rect::from_min_size(
            egui::pos2(rm_rect.left() - ROW - TYPE_W, row.center().y - btn_h * 0.5),
            egui::vec2(TYPE_W, btn_h),
        );
        // The signal type, spelled out. An abbreviation there saves 24 points by making you
        // open a menu to read what the row could already have said, on the one module whose
        // ports are its pins - so the module pays a block of width instead.
        //
        // `.width` because a ComboBox takes `Spacing::combo_width` - 150 points - as its minimum
        // otherwise, whatever the one letter in it is: the button reached past the row and under
        // the output labels. The popup is not held to that width; its entries size it.
        {
            let mut tc = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(ty_rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center))
                    .id_salt(("macro_ty_cell", node_id, i)),
            );
            let ty_resp = egui::ComboBox::from_id_salt((node_id, "macro_ty", i))
                .selected_text(egui::RichText::new(port.signal_type.display_name()).small())
                .width(TYPE_W)
                .show_ui(&mut tc, |ui| {
                    for ty in [SignalType::Bool, SignalType::Float, SignalType::Vec2, SignalType::Any] {
                        if ui.selectable_label(port.signal_type == ty, ty.display_name()).clicked()
                            && port.signal_type != ty
                        {
                            port.signal_type = ty;
                            changed = true;
                        }
                    }
                });
            trace_rect(ui, node_id, &format!("macro_type_{i}"), ty_resp.response.rect);
        }
        let rm = child.put(
            rm_rect,
            egui::Button::new(egui::RichText::new("×").size(14.0))
                .min_size(egui::vec2(RM_W, btn_h)),
        )
        .on_hover_text("Remove port (drops its wires)");
        trace_rect(ui, node_id, &format!("macro_remove_{i}"), rm.rect);
        if rm.clicked() {
            remove_idx = Some(i);
        }
        drop(child);
        trace_rect(ui, node_id, &format!("macro_row_{i}"), row);
        // The same trace name a Split or Selector gives its port rows: a Macro row is one of
        // these, sitting on its socket's row and naming the port by itself.
        trace_rect(ui, node_id, &format!("strip_{i}"), row);
    }

    anim.end(ui.ctx(), drag_key);
    // An open name field has to survive the frame that opened it and every frame the typing
    // arrives in; the body is rebuilt from the params on each of them.
    if edit.editing.is_some() {
        ui.ctx().data_mut(|d| d.insert_temp(edit_key, edit));
    } else {
        ui.ctx().data_mut(|d| d.remove_temp::<MacroEdit>(edit_key));
    }

    if let Some((from, dir)) = moved {
        let to = (from as isize + dir).clamp(0, ports.len() as isize - 1) as usize;
        if to != from {
            // The order of these rows is the order of the node's output pins, and the engine
            // reads them by position: the port moves, its wires go with it, and the `ports`
            // list below is permuted the same way so the pins, the ids and the wires stay in
            // step.
            super::pins::move_dynamic_pin::<super::pins::Outputs>(node_id, from, to, outputs, snarl);
            let port = ports.remove(from);
            ports.insert(to, port);
            changed = true;
        }
    }

    if let Some(rm) = remove_idx {
        // Drop the removed port's wires and reconnect the ones above one slot
        // down, so wiring follows its port.
        let tail: Vec<Vec<egui_snarl::InPinId>> = outputs
            .iter()
            .skip(rm)
            .map(|o| o.remotes.clone())
            .collect();
        for i in 0..tail.len() {
            snarl.drop_outputs(OutPinId { node: node_id, output: rm + i });
        }
        for (shift, remotes) in tail.into_iter().enumerate().skip(1) {
            let new_out = OutPinId { node: node_id, output: rm + shift - 1 };
            for remote in remotes {
                snarl.connect(new_out, remote);
            }
        }
        ports.remove(rm);
        changed = true;
    }

    if changed {
        write_macro_ports(snarl, node_id, &ports);
    }
}


/// Write a Macro Output's port list back: the `macro_ports` param, the `output_pin_ids` the
/// layout keeps beside the pins, and the pins themselves, from one list so the three can't
/// drift. Callers change `ports` and call this - the body, and the header's `+`.
pub(crate) fn write_macro_ports(
    snarl: &mut Snarl<NodeData>,
    node_id: NodeId,
    ports: &[flexinput_core::macros::MacroPortDef],
) {
    use flexinput_core::macros as mac;
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.params.insert(mac::MACRO_PORTS_PARAM.to_string(), mac::ports_to_value(ports));
        node.params.insert(
            "output_pin_ids".to_string(),
            Value::Array(ports.iter().map(|p| Value::String(mac::macro_pin_id(&p.id))).collect()),
        );
        node.outputs = ports
            .iter()
            .map(|p| PinDescriptor::new(p.name.clone(), p.signal_type))
            .collect();
    }
}

/// The Macro Output's `+`, from the header: one more port, named for how many there are.
pub(crate) fn add_macro_port(snarl: &mut Snarl<NodeData>, node_id: NodeId) {
    use flexinput_core::macros as mac;
    let mut ports = snarl
        .get_node(node_id)
        .map(|n| mac::ports_from_params(&n.params))
        .unwrap_or_default();
    ports.push(mac::MacroPortDef {
        id: mac::new_port_id(),
        name: format!("Macro {}", ports.len() + 1),
        icon: String::new(),
        icon_svg: String::new(),
        signal_type: SignalType::Bool,
    });
    write_macro_ports(snarl, node_id, &ports);
}


/// Rasterize an SVG to a rect of `(w, h)` and recolor the resulting pixmap
/// according to `mode`:
///   - "override": each pixel's RGB is lerped toward `tint.rgb` by `tint.a/255`,
///     preserving the SVG's own per-pixel alpha (so silhouette is kept).
///   - "additive": `tint.rgb * (tint.a / 255)` is added to each pixel's RGB
///     (clamped), again preserving the SVG's alpha.
/// Returns None if the SVG can't be parsed.
pub(crate) fn rasterize_svg_recolored(
    svg_text: &str,
    w: u32,
    h: u32,
    mode: &str,
    tint: egui::Color32,
) -> Option<egui::ColorImage> {
    let opt = usvg::Options::default();
    let tree = usvg::Tree::from_str(svg_text, &opt).ok()?;

    let svg_size = tree.size();
    let sx = w as f32 / svg_size.width().max(1.0);
    let sy = h as f32 / svg_size.height().max(1.0);
    let scale = sx.min(sy);
    let render_w = (svg_size.width() * scale).round().max(1.0) as u32;
    let render_h = (svg_size.height() * scale).round().max(1.0) as u32;

    let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
    // Center the rendered SVG in the target rect (preserves aspect ratio).
    let off_x = ((w as f32 - render_w as f32) * 0.5).max(0.0);
    let off_y = ((h as f32 - render_h as f32) * 0.5).max(0.0);
    let transform = tiny_skia::Transform::from_translate(off_x, off_y)
        .post_scale(1.0, 1.0)
        .pre_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // Recolor in place. tiny_skia stores premultiplied RGBA; we work in
    // straight-alpha space then re-premultiply on write.
    let blend = (tint.a() as f32 / 255.0).clamp(0.0, 1.0);
    let tr = tint.r() as f32;
    let tg = tint.g() as f32;
    let tb = tint.b() as f32;
    let pixels = pixmap.pixels_mut();
    if blend > 0.0 {
        match mode {
            "additive" => {
                for px in pixels.iter_mut() {
                    let a = px.alpha() as f32;
                    if a == 0.0 { continue; }
                    // Un-premultiply.
                    let r = (px.red()   as f32) / a * 255.0;
                    let g = (px.green() as f32) / a * 255.0;
                    let b = (px.blue()  as f32) / a * 255.0;
                    let r2 = (r + tr * blend).min(255.0);
                    let g2 = (g + tg * blend).min(255.0);
                    let b2 = (b + tb * blend).min(255.0);
                    // Re-premultiply.
                    let af = a / 255.0;
                    *px = tiny_skia::PremultipliedColorU8::from_rgba(
                        (r2 * af).round() as u8,
                        (g2 * af).round() as u8,
                        (b2 * af).round() as u8,
                        a as u8,
                    ).unwrap_or(*px);
                }
            }
            _ => {
                // Override: lerp original RGB toward tint RGB by `blend`.
                for px in pixels.iter_mut() {
                    let a = px.alpha() as f32;
                    if a == 0.0 { continue; }
                    let r = (px.red()   as f32) / a * 255.0;
                    let g = (px.green() as f32) / a * 255.0;
                    let b = (px.blue()  as f32) / a * 255.0;
                    let r2 = r + (tr - r) * blend;
                    let g2 = g + (tg - g) * blend;
                    let b2 = b + (tb - b) * blend;
                    let af = a / 255.0;
                    *px = tiny_skia::PremultipliedColorU8::from_rgba(
                        (r2 * af).round() as u8,
                        (g2 * af).round() as u8,
                        (b2 * af).round() as u8,
                        a as u8,
                    ).unwrap_or(*px);
                }
            }
        }
    }

    // Convert tiny-skia premultiplied RGBA → egui::ColorImage (also premultiplied).
    let raw = pixmap.data();
    let pixels: Vec<egui::Color32> = raw.chunks_exact(4)
        .map(|c| egui::Color32::from_rgba_premultiplied(c[0], c[1], c[2], c[3]))
        .collect();
    Some(egui::ColorImage {
        size: [w as usize, h as usize],
        pixels,
        source_size: egui::Vec2::new(w as f32, h as f32),
    })
}

/// A glyph as [`rasterize_svg_recolored`] draws it untinted, with `label` cut out
/// of its middle: a hole the background shows through, as on the Steam
/// Controller's own L4 / R5 glyphs. The glyph stays whole if no font loads.
pub(crate) fn rasterize_svg_cutout(svg_text: &str, w: u32, h: u32, label: &str) -> Option<egui::ColorImage> {
    let mut img = rasterize_svg_recolored(svg_text, w, h, "override", egui::Color32::TRANSPARENT)?;
    let Some((fonts, family)) = cutout_font() else { return Some(img) };
    // Bigger the fewer letters, so a side's L or R reads at a chip's size.
    let size = h as f32 * (0.95 / label.chars().count().max(1) as f32).min(0.46);
    let baseline = h as f32 * 0.5 + size * 0.36;
    let escaped: String = label.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    let text = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}"><text x="{x}" y="{baseline}" text-anchor="middle" font-family="{family}" font-weight="700" font-size="{size}" fill="white">{escaped}</text></svg>"#,
        x = w as f32 * 0.5,
    );
    let mut opt = usvg::Options::default();
    opt.fontdb = std::sync::Arc::clone(fonts);
    let Ok(tree) = usvg::Tree::from_str(&text, &opt) else { return Some(img) };
    let Some(mut mask) = tiny_skia::Pixmap::new(w, h) else { return Some(img) };
    resvg::render(&tree, tiny_skia::Transform::identity(), &mut mask.as_mut());
    for (px, m) in img.pixels.iter_mut().zip(mask.pixels()) {
        if m.alpha() > 0 {
            *px = px.gamma_multiply(1.0 - m.alpha() as f32 / 255.0);
        }
    }
    Some(img)
}

/// The font a cut-out label is set in: a bold system face where one is found,
/// else egui's own, and its family name. Loaded once.
fn cutout_font() -> Option<&'static (std::sync::Arc<usvg::fontdb::Database>, String)> {
    static FONT: std::sync::OnceLock<Option<(std::sync::Arc<usvg::fontdb::Database>, String)>> =
        std::sync::OnceLock::new();
    FONT.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        let windows = std::env::var("WINDIR").map(|d| std::path::PathBuf::from(d).join("Fonts"));
        let mut bold: Vec<std::path::PathBuf> = Vec::new();
        if let Ok(dir) = &windows {
            bold.extend(["segoeuib.ttf", "arialbd.ttf"].map(|f| dir.join(f)));
        }
        bold.extend([
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
            "/usr/share/fonts/noto/NotoSans-Bold.ttf",
            "/usr/share/fonts/google-noto/NotoSans-Bold.ttf",
        ].map(std::path::PathBuf::from));
        if !bold.iter().any(|p| db.load_font_file(p).is_ok()) {
            let own = egui::FontDefinitions::default().font_data.get("Ubuntu-Light").map(|d| d.font.to_vec())?;
            db.load_font_data(own);
        }
        let family = db.faces().next()?.families.first()?.0.clone();
        Some((std::sync::Arc::new(db), family))
    }).as_ref()
}

/// A pin glyph's texture, `label` cut out of it when it has one, at `size_px`.
/// Cached by glyph, label and size.
pub(crate) fn glyph_texture(
    ctx: &egui::Context,
    bytes: &'static [u8],
    label: Option<&'static str>,
    size_px: u32,
) -> Option<egui::TextureHandle> {
    let cache_key = egui::Id::new(("remapper_icon", bytes.as_ptr() as usize, label, size_px));
    if let Some(tex) = crate::widgets::tex_cache::get(ctx, cache_key) {
        return Some(tex);
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let img = match label {
        Some(l) => rasterize_svg_cutout(text, size_px, size_px, l)?,
        None => rasterize_svg_recolored(text, size_px, size_px, "override", egui::Color32::TRANSPARENT)?,
    };
    let handle = ctx.load_texture(
        format!("remapper_icon_{:p}_{}", bytes.as_ptr(), label.unwrap_or("")),
        img,
        egui::TextureOptions::LINEAR,
    );
    crate::widgets::tex_cache::put(ctx, cache_key, handle.clone());
    Some(handle)
}



/// How Has Changed's hysteresis reads on its field. It is stored as a percentage
/// of the bus (`hysteresis_pct`, as a Two-way Curve's is), and shown in °/s or G
/// when the input is a motion sensor, whose noise floor is a tiny fraction of its
/// range. Rewiring changes how the value reads, never what it does.
pub(crate) struct HysteresisField {
    /// Shown value per stored percent.
    pub(crate) scale: f32,
    pub(crate) suffix: &'static str,
    speed: f64,
    decimals: usize,
    /// Gamepad step, in shown units.
    pub(crate) nav_step: f32,
}

impl HysteresisField {
    pub(crate) fn of(node: &NodeData) -> Self {
        let (speed, decimals, nav_step) = match node.extra.motion_unit {
            Some(MotionUnit::Gyro) => (0.1, 1, 1.0),
            Some(MotionUnit::Accel) => (0.001, 3, 0.01),
            None => (0.01, 2, 0.1),
        };
        let (scale, suffix) = match node.extra.motion_unit {
            Some(unit) => (unit.full_scale() / 100.0, unit.suffix()),
            None => (1.0, "%"),
        };
        Self { scale, suffix, speed, decimals, nav_step }
    }

    pub(crate) fn drag<'a>(&self, shown: &'a mut f32) -> egui::DragValue<'a> {
        egui::DragValue::new(shown)
            .speed(self.speed)
            .range(0.0..=100.0 * self.scale)
            .max_decimals(self.decimals)
            .suffix(self.suffix)
    }

    /// The shown value as the pill writes it: enough digits for the unit it's in, with the
    /// trailing zeros of a whole number dropped.
    pub(crate) fn text(&self, shown: f32) -> String {
        let s = format!("{:.*}", self.decimals, shown);
        format!("{}{}", s.trim_end_matches('0').trim_end_matches('.'), self.suffix)
    }
}

pub(crate) fn has_changed_hysteresis(node_id: NodeId, snarl: &Snarl<NodeData>) -> Option<(HysteresisField, f32)> {
    let node = snarl.get_node(node_id)?;
    let field = HysteresisField::of(node);
    let pct = node.params.get("hysteresis_pct").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    let shown = pct * field.scale;
    Some((field, shown))
}

pub(crate) fn set_has_changed_hysteresis(node_id: NodeId, snarl: &mut Snarl<NodeData>, shown: f32, field: &HysteresisField) {
    if let (Some(node), Some(n)) = (snarl.get_node_mut(node_id), Number::from_f64((shown / field.scale) as f64)) {
        node.params.insert("hysteresis_pct".into(), Value::Number(n));
    }
}


/// Has Changed's hysteresis row as a pinned widget: the value box takes the
/// frame's spare width.
pub(crate) fn render_has_changed_hysteresis(inner_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, container: egui::Vec2) {
    let Some((field, mut shown)) = has_changed_hysteresis(inner_id, snarl) else { return };
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(140.0, 22.0));
    let changed = ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Hysteresis").weak());
        let w = pin_flex_width(ui, container, 64.0);
        let h = ui.spacing().interact_size.y;
        let r = ui.add_sized([w, h], field.drag(&mut shown));
        publish_nav_field_rects(ui, inner_id, &[r.rect]);
        r.changed()
    }).inner;
    if changed {
        set_has_changed_hysteresis(inner_id, snarl, shown, &field);
        mark_overlay_param_write(ui.ctx());
    }
}

/// What a Sample and Hold's second inlet does in the node's mode. Derived when
/// drawn because the body, a pinned widget and the gamepad editor all write the mode.
/// The second pin's name. "Trig" rather than "Trigger" because the label column is what
/// decides whether the module fits two blocks wide, and the body has values to show now.
pub(crate) fn sample_hold_gate_label(node: &NodeData) -> &'static str {
    if node.params.get("mode").and_then(|v| v.as_str()) == Some("hold") { "Gate" } else { "Trig" }
}

/// The Sample / Hold switch, shared by the node body and its pinned widget.
/// Returns the row's rect and whether the mode changed.
pub(crate) fn sample_hold_mode_row(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, small: bool) -> (egui::Rect, bool) {
    let mut mode = snarl.get_node(node_id)
        .and_then(|n| n.params.get("mode").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| "sample".to_string());
    let mut changed = false;
    let label = |s: &str| {
        let t = egui::RichText::new(s);
        if small { t.small() } else { t }
    };
    let r = ui.horizontal(|ui| {
        changed |= ui.selectable_value(&mut mode, "sample".into(), label("Sample"))
            .on_hover_text("Each 0→1 on Trigger samples In, and Out holds it until the next one")
            .changed();
        changed |= ui.selectable_value(&mut mode, "hold".into(), label("Hold"))
            .on_hover_text("Out follows In, and freezes for as long as Gate is 1")
            .changed();
    });
    if changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("mode".into(), Value::String(mode));
        }
    }
    (r.response.rect, changed)
}

/// Sample and Hold: the body is a mode button and the two numbers that matter - what the
/// wire is carrying in, and what the module is handing out. The mark that used to sit here
/// was a stepped curve, but that drawing says "quantize" more clearly than it says "sample
/// and hold", so it's parked for that module; two live readings say more here, and they
/// show whether holding is happening.
pub(crate) fn show_sample_hold_body(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    view_salt: u64,
) {
    let holding = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("mode").and_then(|v| v.as_str()))
        == Some("hold");
    let inner = module_icons::module_inner(ui, view_salt, node_id);
    let column = ui.max_rect();
    let centre_x = inner.map(|r| r.center().x).unwrap_or_else(|| column.center().x);
    // The mode is a body control at the small size. This module's pins - two in, one out -
    // take 47 of the 98 points a two-block frame has inside its margins, and the house
    // 52-point toggle would tip the whole thing into a third block.
    let (resp, band_top) = super::kit_bodies::setting_button_size(
        ui,
        "sample_hold_mode",
        centre_x,
        holding,
        if holding { "hold" } else { "samp" },
        super::kit_bodies::TOGGLE_SMALL,
    );
    let resp = resp.on_hover_text(
        "Sample: each 0→1 on Trig takes In into Out and holds it until the next one.\n\
         Hold: Out follows In, and freezes for as long as Gate is 1.\n\
         Click to switch - the second pin is named for the mode.",
    );
    if resp.clicked() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert(
                "mode".to_string(),
                Value::String(if holding { "sample" } else { "hold" }.to_string()),
            );
        }
    }
    register_exposable_element(ui, node_id, "mode", resp.rect);

    // The two readings are painted rather than allocated, and they go in the space under
    // the body column: the column is only as tall as the pin rows, and a two-block module
    // has room below them, which is where an empty Sample and Hold used to leave a gap.
    use flexinput_core::Signal;
    let (input, output, engaged) = snarl.get_node(node_id).map_or((None, None, false), |n| {
        let held = |s: Option<&Signal>| match s {
            Some(Signal::Bool(b)) => *b,
            Some(Signal::Float(f)) => *f != 0.0,
            Some(Signal::Int(i)) => *i != 0,
            _ => false,
        };
        (
            n.extra.last_signals.first().and_then(|s| s.as_ref()),
            n.extra.last_out.first().and_then(|s| s.as_ref()),
            held(n.extra.last_signals.get(1).and_then(|s| s.as_ref())),
        )
    });
    let in_text = signal_text(input);
    let out_text = signal_text(output);
    let band = egui::Rect::from_min_max(
        egui::pos2(inner.map_or(column.left(), |r| r.left()), band_top),
        inner.map_or(column.max, |r| r.max),
    );
    // The two lines go at the bottom of the frame, which is the part of a two-block
    // Sample and Hold nothing else uses: the pin labels own the rows above, and the mode
    // button sits between them. Anchoring to the bottom rather than dividing the leftover
    // keeps them clear of the labels whatever the pin count does to the column's height.
    let out_y = band.bottom() - 8.0;
    let in_y = out_y - 15.0;
    if band.height() >= 34.0 {
        // Amber while the trigger or gate is doing something - the moment of the edge in
        // Sample mode, the whole frozen stretch in Hold - and dim otherwise, so "holding"
        // and "following" are different at a glance. The input keeps the header's strong
        // colour, so the live value and the held value read as the two different things
        // they are.
        let dim = ui.visuals().weak_text_color();
        let live = ui.visuals().strong_text_color();
        let active = egui::Color32::from_rgb(230, 170, 60);
        // Fit the pair to the space rather than the other way round: a monospace digit is
        // about 0.62 of its size wide, so this is the largest size the longer reading
        // still fits at, capped at what a body row's text gets.
        let widest = in_text.chars().count().max(out_text.chars().count()) as f32;
        let size = (13.0_f32).min((band.width() - 8.0) / (widest * 0.62).max(1.0)).clamp(8.0, 13.0);
        let font = egui::FontId::monospace(size);
        // A trace of where the pair landed, so a test can prove they are inside the frame
        // and clear of the pin rows rather than eyeballing a screenshot. Nothing reads it.
        ui.ctx().data_mut(|d| {
            d.insert_temp(egui::Id::new(("sample_hold_lines", node_id.0)), xysr(in_y, out_y, size, 0.0));
        });
        let painter = ui.painter_at(band);
        painter.text(
            egui::pos2(centre_x, in_y),
            egui::Align2::CENTER_CENTER,
            &in_text,
            font.clone(),
            live,
        );
        painter.text(
            egui::pos2(centre_x, out_y),
            egui::Align2::CENTER_CENTER,
            &out_text,
            font,
            if engaged { active } else { dim },
        );
    }
}





/// What shape the knob's face takes at the size it's been given.
#[derive(Clone, Copy, PartialEq)]
enum Shape {
    HFader,
    VFader,
    Dial,
}

/// Knob (`module.knob`): the dial, filling whatever frame the corner grip gives it. The
/// face takes the body's box rather than a constant side, so dragging the module wider
/// makes a horizontal fader out of it and taller a vertical one - which is why the module
/// is stretchy like a note or a Readout. A fader is thin by design, so it can be squeezed
/// down to a block that way; a dial can't go below a size that's worth dragging. Its
/// reading is painted beside the control when there's room and in the hover text always.
/// Its output label isn't drawn at all, and the Uni/Bi switch lives in the header.
pub(crate) fn show_knob_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, view_salt: u64, body_w: f32, body_h: f32) {
    let (value, bipolar, name) = snarl.get_node(node_id).map(|n| {
        let v = n.params.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let b = n.params.get("bipolar").and_then(|v| v.as_bool()).unwrap_or(false);
        (v, b, n.display_name.clone())
    }).unwrap_or((0.0, false, "Knob".to_string()));

    let (lo, hi) = if bipolar { (-1.0f32, 1.0f32) } else { (0.0f32, 1.0f32) };
    let mut v = value.clamp(lo, hi);
    let mut value_changed = false;

    // A collapsed knob shows nothing here. Snarl still runs the body when a node is
    // collapsed and shifts it up under the header's clip, which is what made a collapsed
    // 2×2 knob paint a slice of the dial's rim across the header row. The reading it should
    // show instead is drawn in the header (`viewer::show_header`), and not being able to
    // grab the face is the point of collapsing one: it stops a knob being nudged while
    // you're moving things about it.
    let open = snarl.get_node_info(node_id).map(|n| n.open).unwrap_or(true);
    let (flow, _) = ui.allocate_exact_size(egui::vec2(body_w, body_h), egui::Sense::hover());
    if !open {
        // Nothing of the face is drawn. The number a folded knob shows under its header is
        // `paint_folded_reading`'s, for every module that has one.
        trace_rect(ui, node_id, "knob_face", egui::Rect::NOTHING);
        return;
    }
    let column = ui.max_rect();
    let inner = module_icons::module_inner(ui, view_salt, node_id).unwrap_or(flow);
    /// Air between the face and the frame, on top of the frame's own margin, and the
    /// smallest face still worth dragging. A fader needs much less across its thin way:
    /// a track and a thumb, which is what lets a knob be squeezed to one block that way.
    const PAD: f32 = 6.0;
    const MIN_FACE: f32 = 24.0;
    const MIN_TRACK: f32 = 12.0;
    // The shape is decided from the column, before the face is clamped to its minimums:
    // twice as wide as it is tall is a horizontal fader, half as wide a vertical one,
    // anything between a dial.
    let col_aspect = column.width() / column.height().max(1.0);
    let shape = if col_aspect >= 2.0 {
        Shape::HFader
    } else if col_aspect <= 0.5 {
        Shape::VFader
    } else {
        Shape::Dial
    };
    let font = egui::TextStyle::Small.resolve(ui.style());
    let text_col = ui.visuals().strong_text_color();
    let galley = ui.painter().layout_no_wrap(format!("{:.2}", v), font.clone(), text_col);
    let text_h = galley.size().y;
    // The number goes under the control for a **fader** with a line to spare - a tall body has
    // empty space below the track and the track should have the width. A dial's face is round,
    // so when the column is taller than it is wide the spare height is height the face cannot
    // use at all: the reading goes under that face too, and a knob 56 wide and three blocks
    // tall says what it's set to. Where the face already fills the column both ways - the two-
    // block minimum - a line for the text would shrink the thing you drag, so the number stays
    // in the middle if it fits there, and isn't drawn at all when it doesn't.
    let band = text_h + 8.0;
    let face_in = |a: egui::Rect| -> egui::Vec2 {
        let (p_x, p_y, min_side) = match shape {
            Shape::Dial => (PAD, PAD, MIN_FACE),
            _ => (PAD * 0.5, PAD * 0.5, MIN_TRACK),
        };
        let mut f = egui::vec2(
            (a.width() - 2.0 * p_x).max(min_side.min(a.width())),
            (a.height() - 2.0 * p_y).max(min_side.min(a.height())),
        );
        if shape == Shape::Dial {
            let side = f.x.min(f.y);
            f = egui::vec2(side, side);
        }
        f
    };
    let number_under = if shape == Shape::Dial {
        let full = face_in(column);
        let with_text = face_in(egui::Rect::from_min_max(column.min, egui::pos2(column.right(), column.bottom() - band)));
        // Giving up the line costs nothing when the face is limited by the width.
        with_text.x >= full.x - 0.5 && column.height() >= band + MIN_FACE
    } else {
        column.height() >= 58.0 + text_h
    };
    let text_band = if number_under { band } else { 0.0 };
    let area = egui::Rect::from_min_max(column.min, egui::pos2(column.right(), column.bottom() - text_band));
    let (pad_x, pad_y, min_side) = match shape {
        Shape::Dial => (PAD, PAD, MIN_FACE),
        _ => (PAD * 0.5, PAD * 0.5, MIN_TRACK),
    };
    let mut face = egui::vec2(
        (area.width() - 2.0 * pad_x).max(min_side.min(area.width())),
        (area.height() - 2.0 * pad_y).max(min_side.min(area.height())),
    );
    if shape == Shape::Dial {
        // A dial is round: the same side both ways, whichever the column gives less of.
        let side = face.x.min(face.y);
        face = egui::vec2(side, side);
    }

    let rect = egui::Rect::from_center_size(egui::pos2(inner.center().x, area.center().y), face);
    let resp = ui.interact(rect, ui.id().with(("knob_face", node_id)), egui::Sense::click_and_drag());
    trace_rect(ui, node_id, "body_column", column);
    trace_rect(ui, node_id, "knob_face", rect);
    let horizontal = shape == Shape::HFader;

    if resp.double_clicked() {
        v = 0.0f32.clamp(lo, hi);
        value_changed = true;
    } else if resp.dragged() {
        let delta = resp.drag_delta();
        let range = hi - lo;
        let norm_delta = if horizontal {
            delta.x / rect.width()
        } else {
            -delta.y / rect.height()
        };
        v = (v + norm_delta * range).clamp(lo, hi);
        value_changed = true;
    }
    if resp.hovered() {
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll != 0.0 {
            v = (v + scroll * 0.005 * (hi - lo)).clamp(lo, hi);
            value_changed = true;
        }
    }

    let t = (v - lo) / (hi - lo);
    let active = resp.hovered() || resp.dragged();
    let painter = ui.painter_at(rect);
    // A painter made for the face clips to the face - the number written under a track falls
    // outside it and was clipped away, so the reading simply vanished at those sizes. The
    // text gets a painter clipped to the column instead.
    let text_painter = ui.painter_at(column);
    match shape {
        Shape::HFader if !number_under => {
            // A strip a block tall has nothing under it, so the number goes at the end of
            // the track and the track is shortened for it. The track gives up as much as it
            // can while still being something to aim at - a knob 84 wide (a step the finer
            // grid makes reachable) has 27 points of track left after its reading, and a
            // track that thin is still draggable; what it must not do is drop the reading.
            let track_w = rect.width() - galley.size().x - 4.0;
            // A strip one block tall puts the number at the end of its track, but only once the
            // frame is four blocks across: at three the track left over is so short that the
            // reading and the thing you drag crowd each other out.
            let block = crate::canvas::canvas_grid(ui.ctx()).map_or(28.0, |g| g.block);
            // The frame's width, not the body column's: the column is what's left after the
            // pin's label, and it's the module the user counts blocks on. 14 points is the
            // frame's own margin, which `inner` has already given back.
            let roomy_enough = inner.width() + 14.0 >= 4.0 * block;
            if track_w >= 24.0 && roomy_enough {
                let track = egui::Rect::from_min_max(rect.min, egui::pos2(rect.left() + track_w, rect.bottom()));
                draw_knob_h_fader(&painter, track, t, bipolar, active);
                let at = egui::pos2(rect.right() - galley.size().x, rect.center().y - galley.size().y * 0.5);
                trace_rect(ui, node_id, "knob_number", egui::Rect::from_min_size(at, galley.size()));
                text_painter.galley(at, galley.clone(), text_col);
            } else {
                draw_knob_h_fader(&painter, rect, t, bipolar, active);
            }
        }
        Shape::HFader => draw_knob_h_fader(&painter, rect, t, bipolar, active),
        Shape::VFader => draw_knob_v_fader(&painter, rect, t, bipolar, active),
        Shape::Dial => {
            draw_knob_rotary(&painter, rect, t, bipolar, active);
            // A dial keeps its reading in its middle - and its indicator stops short of the
            // middle, so the two don't cross.
            // Only when the number is not going under the face - a dial with a line to spare
            // says its value once, below. Otherwise it has to sit inside the dial without
            // reaching the indicator that stops short of the middle, and the smallest face            // has room for neither: there the face and where it points are the value.
            // The number spans most of a small dial rather than not appearing at all: a
            // three-block knob has a 33-point face and a 27-point reading, and the value being
            // legible matters more here than the margin around the arc. Below that - the two-
            // block minimum - it genuinely doesn't fit, and the face's pointer is the value.
            if !number_under && galley.size().x <= rect.width() * 0.85 {
                let at = egui::pos2(rect.center().x - galley.size().x * 0.5, rect.center().y - galley.size().y * 0.5);
                trace_rect(ui, node_id, "knob_number", egui::Rect::from_min_size(at, galley.size()));
                text_painter.galley(at, galley.clone(), text_col);
            }
        }
    }
    if number_under {
        let at = egui::pos2(inner.center().x - galley.size().x * 0.5, column.bottom() - text_band + 4.0);
        trace_rect(
            ui,
            node_id,
            "knob_number",
            egui::Rect::from_min_size(at, galley.size()),
        );
        text_painter.galley(at, galley, text_col);
    }

    // The reading, and the name the header had to give up when the frame got too narrow
    // for both it and the Uni/Bi switch.
    resp.on_hover_text(format!("{name}\u{a0}\u{a0}{:.3}", v));

    register_exposable_element(ui, node_id, "value", rect);
    if value_changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if let Some(n) = Number::from_f64(v as f64) {
                node.params.insert("value".to_string(), Value::Number(n));
            }
        }
    }
}

// Knob angle mapping: 135° screen angle (lower-left / ~7 o'clock) sweeping
// 270° clockwise to ~45° (lower-right / ~5 o'clock). t=0 → min, t=1 → max.
pub(crate) fn knob_angle_rad(t: f32) -> f32 {
    (135.0_f32 + t * 270.0_f32).to_radians()
}

/// Colours a fader or knob is painted in. `None` on a field keeps the default,
/// so a caller that doesn't care passes `KnobPaint::default()` and gets exactly
/// what the Knob module has always drawn.
#[derive(Clone, Copy, Default)]
pub(crate) struct KnobPaint {
    /// The unfilled part of the track.
    pub track: Option<Color32>,
    /// The filled part, and the rotary's value arc.
    pub accent: Option<Color32>,
    /// The handle / indicator.
    pub handle: Option<Color32>,
}

impl KnobPaint {
    fn accent(&self, bipolar: bool) -> Color32 {
        self.accent.unwrap_or(if bipolar {
            Color32::from_rgb(100, 150, 255)
        } else {
            Color32::from_rgb(80, 200, 120)
        })
    }
    fn handle(&self, active: bool) -> Color32 {
        match self.handle {
            // Still brighten on hover, so a recoloured handle keeps saying
            // whether it has the pointer.
            Some(c) if active => c,
            Some(c) => c.gamma_multiply(0.8),
            None if active => Color32::WHITE,
            None => Color32::from_gray(200),
        }
    }
}

pub(crate) fn draw_knob_rotary(painter: &egui::Painter, rect: egui::Rect, t: f32, bipolar: bool, active: bool) {
    draw_knob_rotary_styled(painter, rect, t, bipolar, active, KnobPaint::default());
}

pub(crate) fn draw_knob_rotary_styled(
    painter: &egui::Painter,
    rect: egui::Rect,
    t: f32,
    bipolar: bool,
    active: bool,
    paint: KnobPaint,
) {
    let center = rect.center();
    let radius = (rect.width().min(rect.height()) * 0.5 - 4.0).max(10.0);
    let track_r = radius * 0.72;
    let n = 64usize;

    painter.circle_filled(center, radius, Color32::from_gray(30));
    painter.circle_stroke(center, radius, egui::Stroke::new(1.0_f32, Color32::from_gray(55)));

    let arc_pts = |t0: f32, t1: f32| -> Vec<egui::Pos2> {
        (0..=n).map(|i| {
            let ti = t0 + (i as f32 / n as f32) * (t1 - t0);
            let a = knob_angle_rad(ti);
            egui::pos2(center.x + track_r * a.cos(), center.y + track_r * a.sin())
        }).collect()
    };

    // Background track
    painter.add(egui::Shape::line(
        arc_pts(0.0, 1.0),
        egui::Stroke::new(3.0_f32, paint.track.unwrap_or(Color32::from_gray(50))),
    ));

    // Value arc
    let accent = paint.accent(bipolar);
    let (a0, a1) = if bipolar { if t >= 0.5 { (0.5, t) } else { (t, 0.5) } } else { (0.0, t) };
    if (a1 - a0).abs() > 0.001 {
        painter.add(egui::Shape::line(arc_pts(a0, a1), egui::Stroke::new(3.0_f32, accent)));
    }

    // Indicator. It starts well out from the middle rather than at the hub: the reading is
    // written in the middle of a dial, and a pointer running across it makes both unreadable.
    let va = knob_angle_rad(t);
    painter.line_segment(
        [
            egui::pos2(center.x + radius * 0.42 * va.cos(), center.y + radius * 0.42 * va.sin()),
            egui::pos2(center.x + radius * 0.78 * va.cos(), center.y + radius * 0.78 * va.sin()),
        ],
        egui::Stroke::new(2.0_f32, paint.handle(active)),
    );
    painter.circle_filled(center, 2.0, Color32::from_gray(85));
}

pub(crate) fn draw_knob_h_fader(painter: &egui::Painter, rect: egui::Rect, t: f32, bipolar: bool, active: bool) {
    draw_knob_h_fader_styled(painter, rect, t, bipolar, active, KnobPaint::default());
}

pub(crate) fn draw_knob_h_fader_styled(
    painter: &egui::Painter,
    rect: egui::Rect,
    t: f32,
    bipolar: bool,
    active: bool,
    paint: KnobPaint,
) {
    let margin = 8.0f32;
    let track_rect = egui::Rect::from_center_size(
        rect.center(),
        egui::vec2(rect.width() - 2.0 * margin, 6.0),
    );
    painter.rect_filled(track_rect, 3.0, paint.track.unwrap_or(Color32::from_gray(40)));

    let handle_x = track_rect.left() + t * track_rect.width();
    let accent = paint.accent(bipolar);

    let fill = if bipolar {
        let cx = track_rect.center().x;
        if handle_x >= cx {
            egui::Rect::from_min_max(egui::pos2(cx, track_rect.top()), egui::pos2(handle_x, track_rect.bottom()))
        } else {
            egui::Rect::from_min_max(egui::pos2(handle_x, track_rect.top()), egui::pos2(cx, track_rect.bottom()))
        }
    } else {
        egui::Rect::from_min_max(track_rect.min, egui::pos2(handle_x, track_rect.bottom()))
    };
    painter.rect_filled(fill, 0.0, accent);

    if bipolar {
        let cx = track_rect.center().x;
        painter.line_segment(
            [egui::pos2(cx, track_rect.top() - 2.0), egui::pos2(cx, track_rect.bottom() + 2.0)],
            egui::Stroke::new(1.0_f32, Color32::from_gray(110)),
        );
    }

    let r = if active { 7.0 } else { 5.0 };
    painter.circle_filled(egui::pos2(handle_x, rect.center().y), r, paint.handle(active));
}

pub(crate) fn draw_knob_v_fader(painter: &egui::Painter, rect: egui::Rect, t: f32, bipolar: bool, active: bool) {
    draw_knob_v_fader_styled(painter, rect, t, bipolar, active, KnobPaint::default());
}

pub(crate) fn draw_knob_v_fader_styled(
    painter: &egui::Painter,
    rect: egui::Rect,
    t: f32,
    bipolar: bool,
    active: bool,
    paint: KnobPaint,
) {
    let margin = 8.0f32;
    let track_rect = egui::Rect::from_center_size(
        rect.center(),
        egui::vec2(6.0, rect.height() - 2.0 * margin),
    );
    painter.rect_filled(track_rect, 3.0, paint.track.unwrap_or(Color32::from_gray(40)));

    // t=1 → top of track, t=0 → bottom
    let handle_y = track_rect.bottom() - t * track_rect.height();
    let accent = paint.accent(bipolar);

    let fill = if bipolar {
        let cy = track_rect.center().y;
        if handle_y <= cy {
            egui::Rect::from_min_max(egui::pos2(track_rect.left(), handle_y), egui::pos2(track_rect.right(), cy))
        } else {
            egui::Rect::from_min_max(egui::pos2(track_rect.left(), cy), egui::pos2(track_rect.right(), handle_y))
        }
    } else {
        egui::Rect::from_min_max(egui::pos2(track_rect.left(), handle_y), track_rect.max)
    };
    painter.rect_filled(fill, 0.0, accent);

    if bipolar {
        let cy = track_rect.center().y;
        painter.line_segment(
            [egui::pos2(track_rect.left() - 2.0, cy), egui::pos2(track_rect.right() + 2.0, cy)],
            egui::Stroke::new(1.0_f32, Color32::from_gray(110)),
        );
    }

    let r = if active { 7.0 } else { 5.0 };
    painter.circle_filled(egui::pos2(rect.center().x, handle_y), r, paint.handle(active));
}

// ── clear_unused helper ───────────────────────────────────────────────────────

pub(crate) fn clear_unused_inputs(
    node_id: NodeId,
    inputs: &[InPin],
    fixed_count: usize,
    snarl: &mut Snarl<NodeData>,
) {
    let connected_removable: Vec<(usize, Vec<OutPinId>)> = inputs
        .iter()
        .skip(fixed_count)
        .filter(|p| !p.remotes.is_empty())
        .map(|p| (p.id.input, p.remotes.clone()))
        .collect();

    for pin in inputs.iter().skip(fixed_count) {
        snarl.drop_inputs(InPinId { node: node_id, input: pin.id.input });
    }

    if let Some(node) = snarl.get_node_mut(node_id) {
        let kept_pins: Vec<_> = connected_removable
            .iter()
            .map(|(idx, _)| node.inputs[*idx].clone())
            .collect();
        let kept_ids: Vec<_> = if let Some(Value::Array(ids)) = node.params.get("input_pin_ids") {
            connected_removable.iter()
                .map(|(idx, _)| ids.get(*idx).cloned().unwrap_or(Value::String(String::new())))
                .collect()
        } else {
            vec![]
        };

        node.inputs.truncate(fixed_count);
        node.inputs.extend(kept_pins);

        if let Some(Value::Array(ids)) = node.params.get_mut("input_pin_ids") {
            ids.truncate(fixed_count);
            ids.extend(kept_ids);
        }
    } else {
        return;
    }

    for (new_idx, (_, remotes)) in connected_removable.iter().enumerate() {
        let new_pin = InPinId { node: node_id, input: fixed_count + new_idx };
        for &remote in remotes {
            snarl.connect(remote, new_pin);
        }
    }
}

// ── Processing module body renderers ──────────────────────────────────────────



