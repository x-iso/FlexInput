//! The Area Mapper body's settings in the shape the other modules' bodies have: value pills,
//! joined segments and lit word switches laid out in bands (`curve_rows::bands`) under
//! folding headings (`curve_rows::section`). They were rows of checkboxes, bare drag fields
//! and combo boxes at whatever width each came to.
//!
//! The pinned rows keep the plain rows in the parent module (`show_area_*_row`): a pinned
//! frame scales those as a whole. Both publish their gamepad field rects in the one order
//! the nav's field lists for the `options`, `layers`, `border` and `press` elements mirror.

use super::*;
use crate::canvas::viewer::curve_rows as cr;
use crate::canvas::viewer::kit::GAP;

/// `v` at the rows' zoom: 1 in the body, the frame's scale in a Style 2 pin. The group widths
/// handed to `pill_bands` are at 1×; a size cut out of a laid-out cell is zoomed.
fn z(v: f32) -> f32 {
    cr::zoomed(v)
}

/// The narrowest the rows read at: the widest of their groups.
pub(super) const ROWS_MIN_W: f32 = 300.0;

use cr::{pill_bands, word_w};

/// A value pill (`curve_rows::real_pill`) whose shown text `show` makes from the value.
#[allow(clippy::too_many_arguments)]
fn pill(
    ui: &mut egui::Ui,
    node: NodeId,
    salt: &str,
    rect: egui::Rect,
    label: &str,
    value: &mut f32,
    show: impl Fn(f32) -> String,
    range: std::ops::RangeInclusive<f32>,
    per_px: f32,
    hover: &str,
) -> bool {
    let shown = show(*value);
    cr::real_pill(ui, node, salt, rect, label, value, &shown, range, per_px, hover)
}

/// A plain button filling `rect`, its text small.
fn button_at(ui: &mut egui::Ui, rect: egui::Rect, text: &str, selected: bool) -> egui::Response {
    ui.put(rect, egui::Button::selectable(selected, crate::canvas::viewer::simple_bodies::control_text(text)).frame_when_inactive(true))
}

/// A child ui exactly over `rect`, its widgets as tall as it is.
fn cell_ui(ui: &mut egui::Ui, rect: egui::Rect, salt: impl std::hash::Hash) -> egui::Ui {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight))
            .id_salt(salt),
    );
    child.spacing_mut().interact_size.y = rect.height();
    child
}

// ── Input ─────────────────────────────────────────────────────────────────────

/// Input, shape, symmetric editing, pass-through, the selected cell's touch gate (a stick)
/// and its rounding and pressure (a rectangle); then the presets and the screen overlay.
/// Field rects publish in `show_area_options_row`'s order.
pub(super) fn options(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    live_signals: &LiveSignals,
    automap_parent: Option<&AutomapGlowParent<'_>>,
    w: f32,
) -> bool {
    let devs = remapper_upstream_device_ids(snarl, node_id, 0, automap_parent);
    let carried = |pin: &str| devs.iter().any(|d| live_signals.contains_key(&(d.clone(), pin.to_string())));
    let mut inputs: Vec<&str> = AREA_INPUTS.iter().copied().filter(|p| input_carried(p, carried)).collect();
    let Some(n) = snarl.get_node(node_id) else { return false };
    let current = area_input(n);
    if inputs.is_empty() {
        inputs = AREA_INPUTS.to_vec();
    }
    if !inputs.contains(&current.as_str()) {
        if let Some(p) = AREA_INPUTS.iter().find(|p| **p == current) {
            inputs.push(p);
        }
    }
    let shape = area_layout(n).shape;
    let mut round = n.params.get(AREA_CELL_ROUND).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32 * 100.0;
    let mut press = n.params.get(AREA_CELL_PRESSURE).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32 * 100.0;
    let mut sym = area_symmetric(n);
    let mut pass = n.params.get(AREA_PASS_SOURCE_PARAM).and_then(|v| v.as_bool()).unwrap_or(false);
    let mut touched = n.params.get(AREA_CELL_TOUCH).and_then(|v| v.as_bool()).unwrap_or(false);
    let overlay = n.params.get(AREA_OVERLAY_PARAM).and_then(|v| v.as_str()).unwrap_or("off").to_string();
    let cell = selected_cell(n, &area_layout(n));
    let stick = !current.starts_with("touch");
    let touch_pin = area_touch_pin(&current);
    let touch_known = devs.iter().any(|d| live_signals.contains_key(&(d.clone(), touch_pin.clone())));
    let two_pads = devs.iter().any(|d| crate::model::is_steam_controller(d));
    let label_of = |p: &str| match p {
        "left_stick" => "left stick",
        "right_stick" => "right stick",
        "touch1" if two_pads => "touchpad L",
        "touch2" if two_pads => "touchpad R",
        "touch1" => "touch 1",
        "touch2" => "touch 2",
        _ => "?",
    };

    let mut set: Vec<(&str, Value)> = Vec::new();
    let mut rects: Vec<egui::Rect> = Vec::new();
    let touched_word = format!("cell {cell} touched");
    let switches = if stick {
        vec![word_w(ui, "sym"), word_w(ui, "pass"), word_w(ui, &touched_word)]
    } else {
        vec![word_w(ui, "sym"), word_w(ui, "pass")]
    };
    let mut mins = vec![150.0, 120.0, switches.iter().sum::<f32>() + GAP * (switches.len() - 1) as f32];
    if shape == Shape::Rect {
        mins.extend([140.0, 120.0]);
    }
    let cells = pill_bands(ui, w, &mins);

    let opts: Vec<(String, &str)> = inputs.iter().map(|p| (p.to_string(), label_of(p))).collect();
    if let Some(p) = cr::pill_combo(ui, cells[0], "input", &current, &opts,
        "The XY pair laid onto the area. It is consumed: the bus carries it on at rest unless Pass is on.",
        ("area_input_pill", node_id.0))
    {
        set.push((AREA_INPUT_PARAM, json!(p)));
    }
    rects.push(cells[0]);
    const SHAPE_TIP: &str = "Circular: rings cut into sectors. Rectangular: rows cut into columns. Switching starts the layout over; cards on the eight directions and the centre keep their cells.";
    if let Some(s) = cr::segmented(ui, cells[1], &shape,
        &[(Shape::Circle, "◯ circle", SHAPE_TIP), (Shape::Rect, "▢ rect", SHAPE_TIP)], ("area_shape_seg", node_id.0))
    {
        set.push((AREA_SHAPE_PARAM, json!(s.as_str())));
    }
    rects.push(cells[1]);
    let sw = cr::split(cells[2], &switches);
    if cr::word_toggle(ui, sw[0], &mut sym, "sym|symmetric",
        "Symmetric editing: a border placed in one quarter appears mirrored in all four, and they move together.",
        ("area_sym", node_id.0))
    {
        set.push((AREA_SYM_PARAM, json!(sym)));
    }
    rects.push(sw[0]);
    if cr::word_toggle(ui, sw[1], &mut pass, "pass|pass through",
        "Pass the input through as well as mapping it. Off: the bus carries it on at rest.", ("area_pass", node_id.0))
    {
        set.push((AREA_PASS_SOURCE_PARAM, json!(pass)));
    }
    rects.push(sw[1]);
    // A touchpad point only exists while touched, so the gate is for sticks.
    if stick {
        let tip = if touch_known {
            format!("Cell {cell} only counts while the stick is touched (capacitive stick).")
        } else {
            format!("Cell {cell} only counts while the stick is touched. Needs a pad that reports stick touch — none connected does.")
        };
        let flipped = ui
            .add_enabled_ui(touch_known || touched, |ui| cr::word_toggle(ui, sw[2], &mut touched, &touched_word, &tip, ("area_touched", node_id.0)))
            .inner;
        if flipped {
            set.push((AREA_CELL_TOUCH, json!(touched)));
        }
        rects.push(sw[2]);
    }
    // A rectangle cell rounds its inner corners, up to a circle.
    if shape == Shape::Rect {
        let label = format!("round|cell {cell} round");
        if pill(ui, node_id, "area_round", cells[3], &label, &mut round, |v| format!("{v:.0}%"), 0.0..=100.0, 0.5,
            "Round this cell like a bubble: same area, up to a circle (an ellipse for an oblong cell). Past its sides it shares the overlap with its neighbours, or pushes into them with more pressure; it gives up its corners. Equal bubbles meet in flat seams.")
        {
            set.push((AREA_CELL_ROUND, json!(round / 100.0)));
        }
        rects.push(cells[3]);
        if pill(ui, node_id, "area_press", cells[4], "press|pressure", &mut press, |v| format!("{v:.0}%"), 0.0..=100.0, 0.5,
            "Where this cell's shape overlaps another's, the higher-pressure one pushes into the lower; at equal pressure they share the overlap. A round cell at full pressure among unpressurised ones takes its whole circle.")
        {
            set.push((AREA_CELL_PRESSURE, json!(press / 100.0)));
        }
        rects.push(cells[4]);
    }
    publish_nav_field_rects(ui, node_id, &rects);

    // The presets, and when the area shows over the game and where.
    let mut mins = vec![96.0, 180.0];
    if overlay != "off" {
        mins.push(word_w(ui, "🖵 position…").max(96.0));
    }
    let cells = pill_bands(ui, w, &mins);
    let mut changed = {
        let mut c = cell_ui(ui, cells[0], ("area_presets_cell", node_id.0));
        presets_menu(node_id, &mut c, snarl)
    };
    let modes: Vec<(String, &str)> = AREA_OVERLAY_MODES.iter().map(|(v, name, _)| (v.to_string(), *name)).collect();
    if let Some(v) = cr::pill_combo(ui, cells[1], "overlay", &overlay, &modes,
        "Show the area on the screen, over the game, while it is in use.", ("area_overlay_pill", node_id.0))
    {
        set.push((AREA_OVERLAY_PARAM, json!(v)));
    }
    if overlay != "off" {
        let outer_uid = crate::canvas::viewer::root_subpatch_id(automap_parent).map(|n| n.0);
        let editing = crate::menu_overlay::menu_edit_target(ui.ctx()) == Some((outer_uid, node_id.0));
        if button_at(ui, cells[2], "🖵 position…", editing)
            .on_hover_text("Place and size the area on the screen: it appears as an editable overlay widget — drag to move, corner to resize, Esc / Done saves.")
            .clicked()
        {
            crate::menu_overlay::request_menu_edit(ui.ctx(), outer_uid, node_id.0);
        }
    }

    let Some(n) = snarl.get_node_mut(node_id) else { return changed };
    changed |= !set.is_empty();
    for (k, v) in set {
        n.params.insert(k.to_string(), v);
    }
    if changed {
        area_sync(n);
    }
    changed
}

// ── Layers ────────────────────────────────────────────────────────────────────

/// The layer tabs as one joined pill, each segment its layer's number in its colour.
fn layer_tabs(ui: &mut egui::Ui, rect: egui::Rect, colors: &[[u8; 4]], active: usize, salt: u64) -> Option<usize> {
    let n = colors.len().max(1);
    let w = rect.width() / n as f32;
    let v = ui.visuals().clone();
    let font = crate::canvas::viewer::simple_bodies::control_font();
    let mut picked = None;
    for (k, c) in colors.iter().enumerate() {
        let r = egui::Rect::from_min_size(egui::pos2(rect.left() + k as f32 * w, rect.top()), egui::vec2(w, rect.height()));
        let resp = ui.interact(r, egui::Id::new(("area_layer_tab", salt, k)), egui::Sense::click()).on_hover_text(format!("Edit layer {}", k + 1));
        let on = k == active;
        let fill = if on {
            v.selection.bg_fill
        } else if resp.hovered() {
            v.widgets.hovered.weak_bg_fill
        } else {
            v.widgets.inactive.weak_bg_fill
        };
        let rad = 3u8;
        let corners = egui::CornerRadius {
            nw: if k == 0 { rad } else { 0 },
            sw: if k == 0 { rad } else { 0 },
            ne: if k + 1 == n { rad } else { 0 },
            se: if k + 1 == n { rad } else { 0 },
        };
        ui.painter().rect_filled(r, corners, fill);
        let col = egui::Color32::from_rgb(c[0], c[1], c[2]);
        let text_col = if on { v.strong_text_color() } else { v.text_color() };
        let g = ui.painter().layout_no_wrap(format!("{}", k + 1), font.clone(), text_col);
        let dot_r = 3.5;
        let total = dot_r * 2.0 + 4.0 + g.size().x;
        let x0 = r.center().x - total * 0.5;
        ui.painter().circle_filled(egui::pos2(x0 + dot_r, r.center().y), dot_r, if on { col } else { col.gamma_multiply(0.7) });
        ui.painter().galley(egui::pos2(x0 + dot_r * 2.0 + 4.0, r.center().y - g.size().y * 0.5), g, text_col);
        if k > 0 {
            ui.painter().line_segment(
                [egui::pos2(r.left(), r.top() + 3.0), egui::pos2(r.left(), r.bottom() - 3.0)],
                egui::Stroke::new(1.0_f32, v.extreme_bg_color),
            );
        }
        if resp.clicked() && !on {
            picked = Some(k);
        }
    }
    picked
}

/// The layer tabs (click to edit one; + adds, ✕ removes the edited one) and its colour. Its
/// analog mode is a widget of its own ([`analog`]): on the same row, turning it on grew the
/// row by its settings, and a pin of the row took a new shape every time. Field rects
/// publish in the nav's order for `layers`.
pub(super) fn layers(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, w: f32) -> bool {
    let Some(n) = snarl.get_node(node_id) else { return false };
    let count = layer_count(n);
    let active = loaded_layer(n);
    let colors: Vec<[u8; 4]> = (0..count)
        .map(|k| {
            if k == active {
                layer_color(|key| n.params.get(key).cloned(), k)
            } else {
                let l = n.params.get(AREA_LAYERS_PARAM).and_then(|v| v.as_array()).and_then(|a| a.get(k)).cloned().unwrap_or(json!({}));
                layer_color(|key| l.get(key).cloned(), k)
            }
        })
        .collect();
    let mut set: Vec<(&str, Value)> = Vec::new();
    let (mut add, mut remove) = (false, false);
    let mut rects: Vec<egui::Rect> = Vec::new();
    const SQ: f32 = 24.0;
    let tabs_w = count as f32 * 34.0 + (SQ + GAP) * if count > 1 { 2.0 } else { 1.0 };
    let cells = pill_bands(ui, w, &[tabs_w, 40.0]);
    let pick = {
        let r = cells[0];
        let buttons = if count > 1 { 2.0 } else { 1.0 };
        let tabs = egui::Rect::from_min_max(r.min, egui::pos2(r.right() - z(SQ + GAP) * buttons, r.bottom()));
        let picked = layer_tabs(ui, tabs, &colors, active, node_id.0 as u64);
        rects.push(tabs);
        let plus = egui::Rect::from_min_size(egui::pos2(tabs.right() + z(GAP), r.top()), egui::vec2(z(SQ), r.height()));
        if button_at(ui, plus, "+", false)
            .on_hover_text("Add a layer: it acts on the same input alongside the others (e.g. an outer ring holding Shift whichever way you push).")
            .clicked()
        {
            add = true;
        }
        if count > 1 {
            let x = egui::Rect::from_min_size(egui::pos2(plus.right() + z(GAP), r.top()), egui::vec2(z(SQ), r.height()));
            if button_at(ui, x, "✕", false).on_hover_text("Remove this layer and its mappings.").clicked() {
                remove = true;
            }
        }
        picked
    };
    {
        let swatch = cells[1];
        let c = colors[active];
        let mut c32 = egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
        let mut child = cell_ui(ui, swatch, ("area_layer_colour", node_id.0));
        child.spacing_mut().interact_size = swatch.size();
        if egui::widgets::color_picker::color_edit_button_srgba(&mut child, &mut c32, egui::widgets::color_picker::Alpha::OnlyBlend)
            .on_hover_text("This layer's colour.")
            .changed()
        {
            set.push((AREA_LAYER_COLOR_PARAM, json!(c32.to_srgba_unmultiplied())));
        }
    }
    publish_nav_field_rects(ui, node_id, &rects);
    apply_layer_edits(snarl, node_id, pick, add, remove, active, set)
}

/// What [`layers`] and [`analog`] decided, written to the node: a layer picked, added or
/// removed, its settings set. Returns whether the patch changed (picking a layer to edit
/// isn't a change).
fn apply_layer_edits(
    snarl: &mut Snarl<NodeData>,
    node_id: NodeId,
    pick: Option<usize>,
    add: bool,
    remove: bool,
    active: usize,
    set: Vec<(&str, Value)>,
) -> bool {
    let Some(n) = snarl.get_node_mut(node_id) else { return false };
    if let Some(k) = pick {
        n.params.insert(AREA_LAYER_PARAM.into(), json!(k));
        area_sync(n);
    }
    let changed = !set.is_empty() || add || remove;
    if add {
        add_layer(n);
    }
    if remove {
        remove_layer(n, active);
    }
    for (k, v) in set {
        n.params.insert(k.to_string(), v);
    }
    if changed {
        area_sync(n);
    }
    changed
}

/// The layer's analog mode, a widget of its own. Its settings always take their room - two
/// bands, every cell of them, the ramp's too under fixed PWM (greyed) - so turning analog
/// on or off, or switching how it pulses, moves nothing: off, one outlined "enable analog"
/// button covers the room the settings take when it is on. Field rects publish in the
/// nav's order for `analog`.
pub(super) fn analog(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, w: f32) -> bool {
    let Some(n) = snarl.get_node(node_id) else { return false };
    let mut analog = n.params.get(AREA_LAYER_ANALOG_PARAM).and_then(|v| v.as_bool()).unwrap_or(false);
    let mut ms = n.params.get(AREA_LAYER_MS_PARAM).and_then(|v| v.as_f64()).unwrap_or(AREA_ANALOG_MS_DEFAULT as f64) as f32;
    let mut hold = n.params.get(AREA_LAYER_HOLD_PARAM).and_then(|v| v.as_f64()).unwrap_or(AREA_ANALOG_HOLD_DEFAULT as f64) as f32;
    let by_direction = n.params.get(AREA_LAYER_MIX_PARAM).and_then(|v| v.as_str()) != Some("keys");
    let smooth = n.params.get(AREA_LAYER_PULSE_PARAM).and_then(|v| v.as_str()) != Some("pwm");
    let mut ramp = n.params.get(AREA_LAYER_RAMP_PARAM).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    let mut mid = n.params.get(AREA_LAYER_MID_PARAM).and_then(|v| v.as_f64()).unwrap_or(0.5) as f32 * 100.0;
    let active = loaded_layer(n);
    let mut set: Vec<(&str, Value)> = Vec::new();
    let mut rects: Vec<egui::Rect> = Vec::new();
    const ANALOG_TIP: &str = "Drive this layer's keys from the stick's deflection instead of its borders: past the centre (the deadzone) the keys pulse so the game moves the way and as far as you push. Borders and gradients don't apply. Best on a 4-way circle with one key per direction.";

    // The two bands, laid out the same whichever way it is.
    let top = pill_bands(ui, w, &[word_w(ui, "analog"), 150.0, 150.0]);
    let bottom = pill_bands(ui, w, &[110.0, 110.0, 120.0]);
    if !analog {
        let all = top.iter().chain(bottom.iter()).fold(egui::Rect::NOTHING, |a, r| a.union(*r));
        let resp = ui
            .interact(all, egui::Id::new(("area_enable_analog", node_id.0)), egui::Sense::click())
            .on_hover_text(ANALOG_TIP);
        let v = ui.visuals();
        // Outlined in the weak text colour: the widgets' own stroke is all but clear here.
        let stroke = if resp.hovered() { v.text_color() } else { v.weak_text_color() };
        ui.painter().rect_stroke(all, 4.0, egui::Stroke::new(1.0_f32, stroke), egui::StrokeKind::Inside);
        ui.painter().text(all.center(), egui::Align2::CENTER_CENTER, "enable analog",
            crate::canvas::viewer::simple_bodies::control_font(), if resp.hovered() { v.strong_text_color() } else { v.text_color() });
        if resp.clicked() {
            set.push((AREA_LAYER_ANALOG_PARAM, json!(true)));
        }
        rects.push(all);
        publish_nav_field_rects(ui, node_id, &rects);
        return apply_layer_edits(snarl, node_id, None, false, false, active, set);
    }
    if cr::word_toggle(ui, top[0], &mut analog, "analog", ANALOG_TIP, ("area_analog", node_id.0)) {
        set.push((AREA_LAYER_ANALOG_PARAM, json!(analog)));
    }
    rects.push(top[0]);
    {
        let cells = [top[1], top[2], bottom[0], bottom[1], bottom[2]];
        const MIX_TIP: &str = "Direction: steer between a key and the diagonal (W ↔ W+D): a full push always walks, only its direction pulses — for games that walk every way at the same speed, most of them. Per key: each key carries its own axis (W at the push up, D at the push right) — for games whose diagonal is faster than straight.";
        if let Some(d) = cr::segmented(ui, cells[0], &by_direction, &[(true, "direction", MIX_TIP), (false, "per key", MIX_TIP)], ("area_mix_seg", node_id.0)) {
            set.push((AREA_LAYER_MIX_PARAM, json!(if d { "direction" } else { "keys" })));
        }
        rects.push(cells[0]);
        const PULSE_TIP: &str = "Smooth: every press and gap at least the minimum, as often as the push allows; a change of push shows at once, and a light push gives short taps far apart instead of presses too short for the game to see. Fixed PWM: one press per fixed period, its length the push — short pushes can make presses shorter than a frame, which games miss.";
        if let Some(s) = cr::segmented(ui, cells[1], &smooth, &[(true, "smooth", PULSE_TIP), (false, "fixed PWM", PULSE_TIP)], ("area_pulse_seg", node_id.0)) {
            set.push((AREA_LAYER_PULSE_PARAM, json!(if s { "smooth" } else { "pwm" })));
        }
        rects.push(cells[1]);
        if smooth {
            if pill(ui, node_id, "area_min", cells[2], "min|shortest press", &mut hold, |v| format!("{v:.0} ms"), 1.0..=500.0, 1.0,
                "The shortest press or gap. Make it at least two of the game's frames (≈ 34 ms at 60 fps, 14 ms at 144) so it reads every one; longer if the game ignores short taps.")
            {
                set.push((AREA_LAYER_HOLD_PARAM, json!(hold)));
            }
        } else if pill(ui, node_id, "area_pwm", cells[2], "PWM|PWM period", &mut ms, |v| format!("{v:.0} ms"), 1.0..=1000.0, 1.0,
            "The pulse period. Games read keys once a frame, so keep it a few frames long (≈ 50–100 ms at 60 fps): shorter pulses get sampled at random and feel jittery.")
        {
            set.push((AREA_LAYER_MS_PARAM, json!(ms)));
        }
        rects.push(cells[2]);
        // The ramp keeps its room under fixed PWM, greyed: it times smooth pulses only.
        let ramp_changed = ui
            .add_enabled_ui(smooth, |ui| {
                pill(ui, node_id, "area_ramp", cells[3], "ramp|ramp time", &mut ramp, |v| format!("{v:.0} ms"), 0.0..=1000.0, 2.0,
                    "How long the game takes to get from standing to full speed (and back). Set it to match and the presses are timed against the game's own acceleration, holding its speed at your push instead of stopping and starting. Raise it until the movement stops stuttering; if the speed then wanders or lags, it's too high. 0 = the game moves at full speed at once.")
            })
            .inner;
        if ramp_changed {
            set.push((AREA_LAYER_RAMP_PARAM, json!(ramp)));
        }
        if smooth {
            rects.push(cells[3]);
        }
        let k = 4;
        if pill(ui, node_id, "area_mid", cells[k], "half|half at", &mut mid, |v| format!("{v:.0}%"), 5.0..=95.0, 0.5,
            "How far you push for the keys to hold half the time. Below 50% light pushes hold longer (faster, fewer gaps at low deflection); above, finer control near the centre.")
        {
            set.push((AREA_LAYER_MID_PARAM, json!(mid / 100.0)));
        }
        rects.push(cells[k]);
    }
    publish_nav_field_rects(ui, node_id, &rects);
    apply_layer_edits(snarl, node_id, None, false, false, active, set)
}

// ── Border ────────────────────────────────────────────────────────────────────

/// The selected border's title, for its heading: `None` when no border is selected.
pub(super) fn border_title(snarl: &Snarl<NodeData>, node_id: NodeId) -> Option<String> {
    let node = snarl.get_node(node_id)?;
    let layout = area_layout(node);
    let r = selected_border(node, &layout)?;
    let mirrored = if area_symmetric(node) { layout.mirror_set(r).len() } else { 1 };
    let mut title = border_noun(layout.shape, r).to_string();
    if mirrored > 1 {
        title.push_str(&format!(" ×{mirrored}"));
    }
    Some(title)
}

/// The selected border's settings under its heading: hard or gradient, a ring's squareness,
/// the gradient's width and curve (with the curve editor when `with_curve` - the body; a pin
/// keeps to the rows), and how keys follow it. Field rects publish in `border_settings`'
/// order.
pub(super) fn border(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, colors: ZoneColors, w: f32, with_curve: bool) -> bool {
    let Some(node) = snarl.get_node(node_id) else { return false };
    let mut layout = area_layout(node);
    let sym = area_symmetric(node);
    let Some(r) = selected_border(node, &layout) else {
        ui.label(egui::RichText::new("No border selected — click one in the field.").small().weak());
        return false;
    };
    let Some(b) = layout.border(r).cloned() else { return false };
    let mut changed = false;
    let mut close = false;
    let mut grad = b.gradient.clone();
    let mut rects: Vec<egui::Rect> = Vec::new();
    let ring = matches!((layout.shape, r), (Shape::Circle, BorderRef::Edge(_)));

    // Gradient, and the ring's squareness.
    let mut mins = vec![word_w(ui, "gradient") + GAP + 24.0];
    if ring {
        mins.extend([120.0, word_w(ui, "round corners")]);
    }
    let cells = pill_bands(ui, w, &mins);
    {
        let c = cells[0];
        let tog = egui::Rect::from_min_max(c.min, egui::pos2(c.right() - z(24.0 + GAP), c.bottom()));
        let mut on = grad.is_some();
        if cr::word_toggle(ui, tog, &mut on, "gradient", "Crossfade the cells on either side of this border instead of switching at it.", ("area_grad_on", node_id.0)) {
            grad = on.then(|| Gradient::with_width(default_gradient_width(&layout, r)));
            changed = true;
        }
        rects.push(tog);
        let x = egui::Rect::from_min_max(egui::pos2(c.right() - z(24.0), c.top()), c.max);
        if button_at(ui, x, "✕", false).on_hover_text("Close").clicked() {
            close = true;
        }
    }
    let mut new_square: Option<f32> = None;
    let mut new_corners: Option<bool> = None;
    if ring {
        let mut pct = b.square * 100.0;
        if pill(ui, node_id, "area_square", cells[1], "square", &mut pct, |v| format!("{v:.0}%"), 0.0..=100.0, 0.5,
            "Take this ring from a circle (0%) to a square (100%) — e.g. a square deadzone. It stops short where it would touch the next ring.")
        {
            new_square = Some(pct / 100.0);
        }
        rects.push(cells[1]);
        let mut corners = b.corners;
        if cr::word_toggle(ui, cells[2], &mut corners, "round corners",
            "On: a square with its corners rounded off — the corner radius shrinks from the whole circle (0%) to nothing (100%). Off: the circle and the square blended.",
            ("area_corners", node_id.0))
        {
            new_corners = Some(corners);
        }
        rects.push(cells[2]);
    }
    if let Some(g) = grad.as_mut() {
        // Width, in the border's own units: degrees for a sector border, else a share of the
        // area's half-width; and the curve's preset.
        let cells = pill_bands(ui, w, &[120.0, 160.0]);
        let deg = border_width_in_degrees(layout.shape, r);
        let (scale, suffix, max) = if deg { (360.0, "°", 120.0) } else { (100.0, "%", 100.0) };
        let mut shown = g.width * scale;
        if pill(ui, node_id, "area_grad_w", cells[0], "width", &mut shown, |v| format!("{v:.0}{suffix}"), 1.0..=max, 0.5,
            "How wide the crossfade is, centred on the border.")
        {
            g.width = shown / scale;
            changed = true;
        }
        rects.push(cells[0]);
        let preset = curve_preset_of(&g.curve).to_string();
        let presets: Vec<(String, &str)> = CURVE_PRESETS.iter().filter(|n| **n != "custom").map(|n| (n.to_string(), curve_preset_label(n))).collect();
        let mut presets = presets;
        if preset == "custom" {
            presets.push(("custom".to_string(), curve_preset_label("custom")));
        }
        if let Some(name) = cr::pill_combo(ui, cells[1], "curve", &preset, &presets,
            "How the far cell's share grows across the band (the near cell gets the rest).", ("area_grad_curve_pill", node_id.0))
        {
            g.curve = curve_preset_points(&name).unwrap_or_default();
            changed = true;
        }
        rects.push(cells[1]);
        let mut pts = if g.curve.is_empty() { identity_curve() } else { g.curve.clone() };
        if with_curve && mapping_curve_editor(
            ui, egui::Id::new((node_id, "area_grad_curve")), &mut pts, None, None,
            colors.accent, &ui.visuals().clone(), None, None, false, None,
        ) {
            g.curve = if curve_preset_of(&pts) == "linear" { Vec::new() } else { pts };
            changed = true;
        }
        // How a key in a fading cell is driven.
        let pwm = g.digital == DigitalMode::Pwm;
        let mut mins = vec![150.0, 120.0];
        if pwm {
            mins.push(170.0);
        }
        let cells = pill_bands(ui, w, &mins);
        let keys: [(DigitalMode, &str); 3] = [(DigitalMode::Pwm, "PWM"), (DigitalMode::Taps, "tap train"), (DigitalMode::Threshold, "threshold")];
        if let Some(m) = cr::pill_combo(ui, cells[0], "keys", &g.digital, &keys,
            "How a key or button in a fading cell is driven. PWM: pulses with duty = the cell's share. Tap train: taps faster as the share grows. Threshold: held once the share reaches a level. Analog outputs always take the share itself.",
            ("area_grad_keys_pill", node_id.0))
        {
            g.digital = m;
            changed = true;
        }
        rects.push(cells[0]);
        if g.digital == DigitalMode::Threshold {
            let mut t = g.threshold;
            if pill(ui, node_id, "area_grad_thr", cells[1], "level", &mut t, |v| format!("{v:.2}"), 0.0..=1.0, 0.005, "The share at which the key holds.") {
                g.threshold = t;
                changed = true;
            }
        } else {
            let mut ms = g.period_ms;
            let tip = if pwm { "PWM period." } else { "Slowest tap period (at the smallest share)." };
            if pill(ui, node_id, "area_grad_ms", cells[1], "period", &mut ms, |v| format!("{v:.0} ms"), 1.0..=1000.0, 1.0, tip) {
                g.period_ms = ms;
                changed = true;
            }
        }
        rects.push(cells[1]);
        if pwm {
            const PHASE_TIP: &str = "Alternate: the cells take turns within one period, each for its share — never both at once, so two keys emulate one analog direction. Independent: each cell pulses on its own clock; pulses may overlap.";
            if let Some(ph) = cr::segmented(ui, cells[2], &g.phase,
                &[(Phase::Alternate, "alternate", PHASE_TIP), (Phase::Independent, "independent", PHASE_TIP)], ("area_grad_phase_seg", node_id.0))
            {
                g.phase = ph;
                changed = true;
            }
            rects.push(cells[2]);
        }
    }
    publish_nav_field_rects(ui, node_id, &rects);

    let Some(node) = snarl.get_node_mut(node_id) else { return false };
    if close {
        node.params.remove(SEL_BORDER);
    }
    if changed {
        layout.set_gradient(r, grad, sym);
    }
    if let (Some(rc), BorderRef::Edge(e)) = (new_corners, r) {
        layout.set_edge_corners(e, rc);
        changed = true;
    }
    if let (Some(sq), BorderRef::Edge(e)) = (new_square, r) {
        layout.set_edge_square(e, sq);
        changed = true;
    }
    if changed {
        set_layout(node, &layout);
    }
    changed
}

// ── Cell ──────────────────────────────────────────────────────────────────────

/// The selected cell: its icon and name, and the haptic it plays as the point crosses its
/// border.
pub(super) fn cell(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, w: f32) -> bool {
    let Some(node) = snarl.get_node(node_id) else { return false };
    let layout = area_layout(node);
    let cell = selected_cell(node, &layout);
    let meta = menu_zone_meta(node).get(&cell).cloned().unwrap_or_default();
    let current = cell_haptic(node, cell);
    let mut on = current.is_some();
    let mut pct = current.map_or(1.0, |h| h.strength) * 100.0;
    let tone = current.and_then(|h| h.tone);
    let mut tone_on = tone.is_some();
    let (default_hz, default_ms) = if area_input(node).starts_with("touch") {
        (flexinput_core::automap::PAD_CLICK_HZ, flexinput_core::automap::PAD_CLICK_MS)
    } else {
        (flexinput_core::automap::TRIGGER_CLICK_HZ, flexinput_core::automap::TRIGGER_CLICK_MS)
    };
    let mut t = tone.unwrap_or(CellTone { hz: default_hz, ms: default_ms, soft: 0.0 });

    // Its icon and name.
    let mut new_meta: Option<ZoneMeta> = None;
    let cells = pill_bands(ui, w, &[40.0 + GAP + 150.0]);
    {
        let r = cells[0];
        let icon = egui::Rect::from_min_size(r.min, egui::vec2(z(40.0), r.height()));
        let mut c = cell_ui(ui, icon, ("area_cell_icon_cell", node_id.0, cell));
        if let Some((k, s)) = icon_picker_button(&mut c, egui::Id::new((node_id, "area_cell_icon", cell)), &meta.icon, &meta.svg) {
            new_meta = Some(ZoneMeta { icon: k, svg: s, label: meta.label.clone() });
        }
        let name = egui::Rect::from_min_max(egui::pos2(icon.right() + z(GAP), r.top()), r.max);
        let mut label = meta.label.clone();
        if ui
            .put(name, egui::TextEdit::singleline(&mut label).hint_text("name").id_salt((node_id, "area_cell_name", cell)))
            .on_hover_text("This cell's name, shown on it. The icon beside it shows one picked icon on the cell instead of its mapping icons.")
            .changed()
        {
            new_meta = Some(ZoneMeta { icon: meta.icon.clone(), svg: meta.svg.clone(), label });
        }
    }

    // Its haptic: whether, how strong, for every cell of the layer; and its own tone.
    let mut changed = false;
    let all;
    let cells = pill_bands(ui, w, &[word_w(ui, "haptic") + GAP + 110.0, word_w(ui, "all cells in layer")]);
    {
        let r = cells[0];
        let hw = z(word_w(ui, "haptic"));
        let tog = egui::Rect::from_min_size(r.min, egui::vec2(hw, r.height()));
        changed |= cr::word_toggle(ui, tog, &mut on, "haptic",
            "Play a haptic as the point crosses this cell's border, on the way in or out: on the trackpad for a touchpad input, on that side's grip motor for a stick. A pad without those gets a short rumble. Landing or lifting a finger doesn't count.",
            ("area_haptic", node_id.0));
        let pill = egui::Rect::from_min_max(egui::pos2(tog.right() + z(GAP), r.top()), r.max);
        let shown = format!("{pct:.0}%");
        changed |= ui
            .add_enabled_ui(on, |ui| cr::real_pill(ui, node_id, "area_haptic_pct", pill, "strength", &mut pct, &shown, 5.0..=100.0, 0.5, "How strong the haptic is."))
            .inner;
        all = button_at(ui, cells[1], "all cells in layer", false)
            .on_hover_text("Give every cell in this layer this cell's haptic setting.")
            .clicked();
    }
    // The tone band always takes its room, greyed while the cell has no haptic: turning
    // the haptic on grew the widget by a band, and a pin of it took a new shape.
    {
        let cells = pill_bands(ui, w, &[word_w(ui, "tone"), 110.0, 100.0, 100.0]);
        changed |= ui
            .add_enabled_ui(on, |ui| {
                cr::word_toggle(ui, cells[0], &mut tone_on, "tone",
                    "Give this cell's click its own pitch, length and softness. Off, it plays the pad's plain click. A pad without these actuators gets its short rumble either way.",
                    ("area_tone", node_id.0))
            })
            .inner;
        ui.add_enabled_ui(on && tone_on, |ui| {
            let shown = format!("{:.0} Hz", t.hz);
            changed |= cr::real_pill(ui, node_id, "area_tone_hz", cells[1], "pitch", &mut t.hz, &shown, 60.0..=600.0, 1.0,
                "The click's pitch. Lower feels like a thud, higher like a tick.");
            let shown = format!("{:.0} ms", t.ms);
            changed |= cr::real_pill(ui, node_id, "area_tone_ms", cells[2], "length", &mut t.ms, &shown, 4.0..=80.0, 0.2,
                "The click's length: short feels crisp, long feels more like a pulse.");
            let mut soft = t.soft * 100.0;
            let shown = format!("{soft:.0}%");
            if cr::real_pill(ui, node_id, "area_tone_soft", cells[3], "soft|softness", &mut soft, &shown, 0.0..=100.0, 0.5,
                "Sharp to soft. At 0 the click starts and stops dead; higher, more of it fades in and out, until at 100% it swells and dies away.")
            {
                t.soft = soft / 100.0;
                changed = true;
            }
        });
    }
    let haptic = on.then_some(CellHaptic { strength: pct / 100.0, tone: tone_on.then_some(t) });

    let Some(node) = snarl.get_node_mut(node_id) else { return false };
    if let Some(m) = &new_meta {
        write_zone_meta_entry(node, cell, m);
    }
    if changed {
        set_cell_haptics(node, &[cell], haptic);
    }
    if all {
        set_cell_haptics(node, &layout.cell_ids(), haptic);
    }
    new_meta.is_some() || changed || all
}

// ── Press ─────────────────────────────────────────────────────────────────────

/// One press mode for every mapping: when on, its mode, time, Hold and Turbo rule every card
/// of every layer. Field rects publish in `show_area_press_row`'s order.
pub(super) fn press(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, w: f32) -> bool {
    let Some(n) = snarl.get_node(node_id) else { return false };
    let mut lock = n.params.get(AREA_PRESS_LOCK_PARAM).and_then(|v| v.as_bool()).unwrap_or(false);
    let mut mode = n.params.get(AREA_PRESS_MODE_PARAM).and_then(|v| v.as_str()).unwrap_or("down").to_string();
    let mut ms = n.params.get(AREA_PRESS_MS_PARAM).and_then(|v| v.as_f64()).unwrap_or(200.0) as f32;
    let mut hold = n.params.get(AREA_PRESS_HOLD_PARAM).and_then(|v| v.as_bool()).unwrap_or(false);
    let mut turbo = n.params.get(AREA_PRESS_TURBO_PARAM).and_then(|v| v.as_bool()).unwrap_or(false);
    let mut changed = false;
    let mut rects: Vec<egui::Rect> = Vec::new();
    let mut mins = vec![word_w(ui, "one press mode for all")];
    if lock {
        mins.extend([150.0, 100.0, word_w(ui, "hold") + GAP + word_w(ui, "turbo")]);
    }
    let cells = pill_bands(ui, w, &mins);
    changed |= cr::word_toggle(ui, cells[0], &mut lock, "one press mode for all",
        "Rule every mapping's press mode from here, instead of card by card.", ("area_press_lock", node_id.0));
    rects.push(cells[0]);
    if lock {
        let modes: Vec<(String, &str)> = AREA_PRESS_MODES.iter().map(|m| (m.to_string(), press_mode_label(m))).collect();
        if let Some(m) = cr::pill_combo(ui, cells[1], "mode", &mode, &modes, "The press mode every mapping takes.", ("area_press_mode_pill", node_id.0)) {
            mode = m;
            changed = true;
        }
        rects.push(cells[1]);
        changed |= pill(ui, node_id, "area_press_ms", cells[2], "time", &mut ms, |v| format!("{v:.0} ms"), 1.0..=5000.0, 2.0,
            "The press mode's time window (pulse length for On press / On release, Enter / Leave).");
        rects.push(cells[2]);
        let s = cr::split(cells[3], &[word_w(ui, "hold"), word_w(ui, "turbo")]);
        changed |= cr::word_toggle(ui, s[0], &mut hold, "hold", "Hold the output for as long as the press lasts.", ("area_press_hold", node_id.0));
        rects.push(s[0]);
        changed |= cr::word_toggle(ui, s[1], &mut turbo, "turbo", "Repeat the output while held.", ("area_press_turbo", node_id.0));
        rects.push(s[1]);
    }
    publish_nav_field_rects(ui, node_id, &rects);
    if changed {
        if let Some(n) = snarl.get_node_mut(node_id) {
            n.params.insert(AREA_PRESS_LOCK_PARAM.into(), json!(lock));
            n.params.insert(AREA_PRESS_MODE_PARAM.into(), json!(mode));
            n.params.insert(AREA_PRESS_MS_PARAM.into(), json!(ms));
            n.params.insert(AREA_PRESS_HOLD_PARAM.into(), json!(hold));
            n.params.insert(AREA_PRESS_TURBO_PARAM.into(), json!(turbo));
            area_sync(n);
        }
    }
    changed
}
