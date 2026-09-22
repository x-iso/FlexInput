//! JSM Config node body: the config tabs, the text editor, and what the parser
//! made of every line.
//!
//! The text is the only source of truth (`docs/JSM_MODULE_PLAN.md`), so the body
//! is mostly an editor: a tab per config file, Load / Save per tab, and the
//! parser's verdict shown two ways — each line tinted in place, and the lines
//! worth explaining listed underneath with the reason.

use super::*;

/// One config file held by the node.
#[derive(Clone, Default)]
pub(crate) struct JsmTab {
    pub name: String,
    pub text: String,
}

const TAB_NAME_W: f32 = 90.0;
/// Default editor size in a node body — wide enough to read a config in, and
/// dragged from there by the grip in its bottom-right corner.
const EDITOR_W: f32 = 380.0;
const EDITOR_H: f32 = 220.0;
const MIN_W: f32 = 200.0;
const MIN_H: f32 = 80.0;
/// Beside the editor, the narrowest a tuning strip can be and still show a
/// setting's name, its value, and a fader worth dragging.
const MIN_STRIP_W: f32 = 150.0;
/// ...and the narrowest the editor can be and still be one.
const MIN_EDITOR_W: f32 = 190.0;
/// Room the summary row needs under a pinned editor.
const SUMMARY_H: f32 = 18.0;

/// Colour for a line's status: errors read, not-yet-live amber, ignored faint.
fn status_color(ui: &egui::Ui, status: &flexinput_engine::eval::JsmLineStatus) -> Option<Color32> {
    use flexinput_engine::eval::JsmLineStatus as S;
    match status {
        S::Error(_) => Some(Color32::from_rgb(226, 104, 92)),
        S::Pending(_) => Some(Color32::from_rgb(214, 168, 74)),
        S::Ignored(_) => Some(ui.visuals().weak_text_color()),
        S::Ok | S::Blank => None,
    }
}

pub(crate) fn show_jsm_body(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    live: &std::collections::HashMap<(String, String), Signal>,
    parent: Option<&AutomapGlowParent<'_>>,
) {
    jsm_body(node_id, ui, snarl, None, live, parent, None);
}

/// Where the tuning strip sits relative to the editor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Side {
    Bottom,
    Top,
    Left,
    Right,
}

impl Side {
    fn from_param(s: &str) -> Self {
        match s {
            "top" => Side::Top,
            "left" => Side::Left,
            "right" => Side::Right,
            _ => Side::Bottom,
        }
    }
    fn as_param(self) -> &'static str {
        match self {
            Side::Top => "top",
            Side::Left => "left",
            Side::Right => "right",
            Side::Bottom => "bottom",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Side::Top => "Above",
            Side::Left => "Left",
            Side::Right => "Right",
            Side::Bottom => "Below",
        }
    }
}

/// The exponent the curve's speed axis is drawn with. A view setting, so it
/// lives on the node and never in the config text — it changes nothing the
/// engine runs.
fn curve_warp(snarl: &Snarl<NodeData>, node_id: NodeId) -> f32 {
    snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("jsm_curve_warp"))
        .and_then(|v| v.as_f64())
        .map(|f| f as f32)
        .unwrap_or(1.0)
}

fn set_curve_warp(snarl: &mut Snarl<NodeData>, node_id: NodeId, warp: f32) {
    if let Some(n) = snarl.get_node_mut(node_id) {
        n.params.insert("jsm_curve_warp".into(), Value::from(warp as f64));
    }
}

fn knob_side(snarl: &Snarl<NodeData>, node_id: NodeId) -> Side {
    Side::from_param(
        snarl
            .get_node(node_id)
            .and_then(|n| n.params.get("jsm_knob_side"))
            .and_then(|v| v.as_str())
            .unwrap_or("bottom"),
    )
}

/// The physical device feeding this node, found by walking the AutoMap wire.
///
/// **Not** from the `_automap_device_id` param: the graph builder injects that
/// into a *clone* of the node's params on its way to the engine and never writes
/// it back to the canvas, so in the UI it is always absent. Reading it here is
/// what left the curve with no live marker — and, quietly, the editor with no
/// "this pad hasn't got that button" notes either, since both asked the same
/// question the same wrong way.
fn upstream_device(
    snarl: &Snarl<NodeData>,
    node_id: NodeId,
    parent: Option<&AutomapGlowParent<'_>>,
) -> Option<String> {
    let idx = snarl
        .get_node(node_id)?
        .inputs
        .iter()
        .position(|p| p.signal_type == SignalType::AutoMap)?;
    let pin = snarl.in_pin(InPinId { node: node_id, input: idx });
    let src = *pin.remotes.first()?;
    crate::app::find_automap_device_id_for_viewer(snarl, src, parent)
}

/// The pins the pad feeding this node actually reports, so the editor can say
/// which buttons a config asks for that this pad hasn't got. Empty when there is
/// no device yet — nothing is claimed on a guess.
fn pins_this_pad_reports(
    snarl: &Snarl<NodeData>,
    node_id: NodeId,
    live: &std::collections::HashMap<(String, String), Signal>,
    parent: Option<&AutomapGlowParent<'_>>,
) -> std::collections::HashSet<String> {
    let dev = upstream_device(snarl, node_id, parent).unwrap_or_default();
    if dev.is_empty() {
        return Default::default();
    }
    live.keys()
        .filter(|(d, _)| *d == dev)
        .map(|(_, pin)| pin.clone())
        .collect()
}

/// The same body at an explicit size — the config overlay's pinned editor.
pub(crate) fn show_jsm_body_sized(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    size: egui::Vec2,
    live: &std::collections::HashMap<(String, String), Signal>,
    parent: Option<&AutomapGlowParent<'_>>,
    paint: super::jsm_widgets::JsmPaint<'_>,
) {
    jsm_body(node_id, ui, snarl, Some(size), live, parent, paint);
}

/// The sensitivity curve on its own, for the config overlay.
pub(crate) fn show_jsm_curve_sized(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    size: egui::Vec2,
    live: &std::collections::HashMap<(String, String), Signal>,
    parent: Option<&AutomapGlowParent<'_>>,
    paint: super::jsm_widgets::JsmPaint<'_>,
) {
    let tabs = read_tabs(snarl, node_id);
    let active = active_tab(snarl, node_id, tabs.len());
    let text = tabs.get(active).map(|t| t.text.clone()).unwrap_or_default();
    let compiled = flexinput_engine::eval::jsm_compile(&text, &[]);
    // Pinned, this can be a large panel — sample it finely enough that the line
    // reads as a curve rather than as the polygon it is.
    let warp = curve_warp(snarl, node_id);
    let points = flexinput_engine::eval::jsm_sens_curve_warped(&compiled, 160, warp);
    let dev = upstream_device(snarl, node_id, parent).unwrap_or_default();
    // The control only appears where taking a row off the graph still leaves a
    // graph; on a short pin the axis keeps whatever the node was set to.
    let room = size.y >= 90.0;
    let graph_h = (size.y - if room { super::jsm_widgets::WARP_H } else { 0.0 }).max(40.0);
    super::jsm_widgets::curve_graph(
        ui,
        size.x,
        graph_h,
        &points,
        super::jsm_widgets::live_turn_speed(live, &dev),
        paint,
        warp,
    );
    if room {
        if let Some(w) = super::jsm_widgets::warp_slider(ui, size.x, warp, paint) {
            set_curve_warp(snarl, node_id, w);
            mark_overlay_param_write(ui.ctx());
        }
    }
}

/// One setting's slider on its own, for the config overlay. The setting is found by
/// name, so it follows the line if the config is edited around it — and says so
/// plainly if the line is gone rather than silently showing nothing.
pub(crate) fn show_jsm_knob_sized(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    size: egui::Vec2,
    name: &str,
    paint: super::jsm_widgets::JsmPaint<'_>,
) {
    let mut tabs = read_tabs(snarl, node_id);
    let active = active_tab(snarl, node_id, tabs.len());
    let Some(tab) = tabs.get(active) else { return };
    let knobs = flexinput_engine::eval::jsm_knobs(&tab.text);
    let Some(knob) = knobs.iter().find(|k| k.name.eq_ignore_ascii_case(name)) else {
        ui.label(
            egui::RichText::new(format!("`{name}` isn't set in this tab any more"))
                .small()
                .weak(),
        );
        return;
    };
    if let Some(v) = super::jsm_widgets::pinned_fader(ui, size, knob, paint) {
        let text = flexinput_engine::eval::jsm_set_knob(&tab.text, knob.line, v, knob.integral);
        tabs[active].text = text;
        write_tabs(snarl, node_id, &tabs, active);
        // Pinned, this ran in the overlay's viewport: bump the canvas generation so
        // an open sub-patch editor re-reads rather than writing its stale copy back.
        mark_overlay_param_write(ui.ctx());
    }
}

/// The numeric settings the node's active tab sets, in the order they are drawn.
///
/// Gamepad nav walks this list, so it has to be the same list — and in the same
/// order — as the faders the body lays out, or the focus ring would point at one
/// setting while the pad edited another.
pub(crate) fn knobs_of(node: &NodeData) -> Vec<flexinput_engine::eval::JsmKnob> {
    active_text_of(node)
        .map(|(_, text)| flexinput_engine::eval::jsm_knobs(text))
        .unwrap_or_default()
}

/// The active tab's text, for callers outside the body (the config overlay's
/// passthrough resolver needs to compile it to know what a setting affects).
pub(crate) fn active_text(node: &NodeData) -> String {
    active_text_of(node).map(|(_, t)| t.to_string()).unwrap_or_default()
}

/// Replace the active tab's text on a node.
///
/// The same resolution `active_text` uses, so a pad can never read one tab and
/// write another.
pub(crate) fn set_active_text(node: &mut NodeData, text: &str) {
    let Some((active, _)) = active_text_of(node) else { return };
    let Some(arr) = node.params.get("jsm_tabs").and_then(|v| v.as_array()).cloned() else {
        return;
    };
    let mut arr = arr;
    let Some(tab) = arr.get_mut(active) else { return };
    tab["text"] = Value::String(text.to_string());
    node.params.insert("jsm_tabs".into(), Value::Array(arr));
}

/// The index of the node's active tab and its text — the one thing both nav
/// helpers need, so they can't disagree about which tab is being driven.
fn active_text_of(node: &NodeData) -> Option<(usize, &str)> {
    let arr = node.params.get("jsm_tabs")?.as_array()?;
    let active = node
        .params
        .get("jsm_active_tab")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let active = if active >= arr.len() { 0 } else { active };
    let text = arr.get(active)?.get("text")?.as_str()?;
    Some((active, text))
}

/// Nudge one setting's fader from the gamepad, by a fraction of its range.
///
/// Unlike every other pinnable value in the app, a JSM setting does not live in a
/// param — it is a number inside the config text, and the text stays the source
/// of truth. So nav edits it the same way a drag does, through `jsm_set_knob`,
/// and the parser sees it on the next frame like any other edit.
///
/// Returns whether anything moved (a setting already at its end does not).
pub(crate) fn nav_nudge_knob(node: &mut NodeData, name: &str, delta: f32) -> bool {
    // Resolved the same way `knobs_of` does it, so the list nav walks and the
    // line nav rewrites can never be from different tabs.
    let Some((active, text)) = active_text_of(node) else { return false };
    let knobs = flexinput_engine::eval::jsm_knobs(text);
    let Some(k) = knobs.iter().find(|k| k.name.eq_ignore_ascii_case(name)) else {
        return false;
    };
    let mut next = k.at(k.t() + delta);
    // A fine step on a narrow range rounds straight back to where it started —
    // `at` writes at most two decimals so the config stays readable — and the
    // fine modifier then looked broken rather than fine. Force the smallest
    // change the config can actually express.
    if next == k.value && delta != 0.0 {
        let least = if k.integral { 1.0 } else { 0.01 };
        next = (k.value + least * delta.signum()).clamp(k.lo.min(k.value), k.hi.max(k.value));
    }
    if (next - k.value).abs() < f32::EPSILON {
        return false;
    }
    let rewritten = flexinput_engine::eval::jsm_set_knob(text, k.line, next, k.integral);
    let mut arr = match node.params.get("jsm_tabs").and_then(|v| v.as_array()) {
        Some(a) => a.clone(),
        None => return false,
    };
    let Some(tab) = arr.get_mut(active) else { return false };
    tab["text"] = Value::String(rewritten);
    node.params.insert("jsm_tabs".into(), Value::Array(arr));
    true
}

/// Which tab the editor has open, clamped to what exists.
fn active_tab(snarl: &Snarl<NodeData>, node_id: NodeId, count: usize) -> usize {
    let active = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("jsm_active_tab"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    if active >= count { 0 } else { active }
}

/// The editor's size in a node body, from the node's own params. Pinned, the
/// container decides instead.
fn editor_size(snarl: &Snarl<NodeData>, node_id: NodeId) -> egui::Vec2 {
    let p = |k: &str, d: f32| snarl.get_node(node_id)
        .and_then(|n| n.params.get(k))
        .and_then(|v| v.as_f64())
        .map(|f| f as f32)
        .unwrap_or(d);
    // With the strip beside it the body holds two columns, so the floor is what
    // both of them need rather than the editor's own.
    let min_w = if show_knobs(snarl, node_id)
        && matches!(knob_side(snarl, node_id), Side::Left | Side::Right)
    {
        MIN_STRIP_W + MIN_EDITOR_W + 8.0
    } else {
        MIN_W
    };
    egui::vec2(p("jsm_editor_w", EDITOR_W).max(min_w), p("jsm_editor_h", EDITOR_H).max(MIN_H))
}

fn jsm_body(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    pinned: Option<egui::Vec2>,
    live: &std::collections::HashMap<(String, String), Signal>,
    parent: Option<&AutomapGlowParent<'_>>,
    paint: super::jsm_widgets::JsmPaint<'_>,
) {
    // A node body is laid out BETWEEN the pin columns, so its parent is a
    // horizontal layout with no width to inherit: the body picks its own size
    // (stored on the node, dragged by the handle below) and stacks its rows in
    // its own vertical layout. Pinned, the container's size wins.
    let size = pinned.unwrap_or_else(|| editor_size(snarl, node_id));
    let side = knob_side(snarl, node_id);
    let knobs_on = show_knobs(snarl, node_id);
    let resizable = pinned.is_none();

    // Beside the editor the strip is a column of its own, so the two are laid
    // out side by side rather than stacked. Above or below, one vertical column
    // does the whole job and `jsm_rows` places the strip itself.
    let gap = ui.spacing().item_spacing.x;
    // Beside the editor only while there is room for both to work. Below the
    // two minimums the strip would be a column of clipped captions next to a
    // sliver of text, so the layout falls back to stacking rather than showing
    // something unusable — and `editor_size` raises the node's own minimum width
    // to match, so dragging it narrow doesn't silently drop out of this layout.
    let beside = knobs_on
        && matches!(side, Side::Left | Side::Right)
        && size.x >= MIN_STRIP_W + MIN_EDITOR_W + gap;

    // The editor pin's own background and frame. The rect they cover isn't known
    // until the body has been laid out, so two slots are reserved here — under
    // everything the body draws — and filled in once it has.
    let plate = super::jsm_widgets::reserve_backdrop(ui, paint);

    if beside {
        let strip_w = (size.x * 0.42).clamp(MIN_STRIP_W, 320.0).min(size.x - MIN_EDITOR_W - gap);
        let col_w = (size.x - strip_w - gap).max(MIN_EDITOR_W);
        ui.horizontal_top(|ui| {
            if side == Side::Left {
                strip_column(node_id, ui, snarl, strip_w, size.y, live, parent, paint, resizable);
            }
            ui.vertical(|ui| {
                ui.set_max_width(col_w);
                jsm_rows(
                    node_id, ui, snarl, egui::vec2(col_w, size.y), resizable, live, parent,
                    paint, false,
                );
            });
            if side == Side::Right {
                strip_column(node_id, ui, snarl, strip_w, size.y, live, parent, paint, resizable);
            }
        });
        super::jsm_widgets::fill_backdrop(ui, plate, paint);
        return;
    }

    ui.vertical(|ui| {
        ui.set_max_width(size.x);
        jsm_rows(node_id, ui, snarl, size, resizable, live, parent, paint, knobs_on);
    });
    super::jsm_widgets::fill_backdrop(ui, plate, paint);
}

/// The tuning strip as a column of its own, for the beside-the-editor layouts.
#[allow(clippy::too_many_arguments)]
fn strip_column(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    width: f32,
    height: f32,
    live: &std::collections::HashMap<(String, String), Signal>,
    parent: Option<&AutomapGlowParent<'_>>,
    paint: super::jsm_widgets::JsmPaint<'_>,
    resizable: bool,
) {
    let mut tabs = read_tabs(snarl, node_id);
    let active = active_tab(snarl, node_id, tabs.len());
    if tabs.get(active).is_none() {
        return;
    }
    // Only as wide as the column really is. Anything the editor column overran by
    // comes out of this one, and drawing past it would clip the values — the one
    // thing on a fader that must never be trimmed.
    let width = width.min(ui.available_width().max(60.0));
    let mut edited = None;
    let mut warp = None;
    ui.vertical(|ui| {
        ui.set_max_width(width);
        // Beside the editor the strip owns its column's full height, whether the
        // body is a node (which grows) or a pin (which does not) — either way it
        // has a column to fill rather than a leftover to squeeze into.
        let (text, w) = knob_rows(
            node_id, ui, snarl, width, &tabs[active].text, live, parent, paint,
            Some(height),
        );
        edited = text;
        warp = w;
    });
    if let Some(w) = warp {
        set_curve_warp(snarl, node_id, w);
        if !resizable {
            mark_overlay_param_write(ui.ctx());
        }
    }
    if let Some(text) = edited {
        tabs[active].text = text;
        write_tabs(snarl, node_id, &tabs, active);
        if !resizable {
            mark_overlay_param_write(ui.ctx());
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn jsm_rows(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    size: egui::Vec2,
    resizable: bool,
    live: &std::collections::HashMap<(String, String), Signal>,
    parent: Option<&AutomapGlowParent<'_>>,
    paint: super::jsm_widgets::JsmPaint<'_>,
    // False when the strip is drawn in a column of its own beside the editor.
    owns_strip: bool,
) {
    let mut tabs = read_tabs(snarl, node_id);
    let mut active = snarl.get_node(node_id)
        .and_then(|n| n.params.get("jsm_active_tab"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    if active >= tabs.len() { active = 0; }
    let mut changed = false;
    // `knob_rows` only borrows the snarl to read; an axis change is applied once
    // the borrow is done.
    let mut pending_warp: Option<f32> = None;
    let side = knob_side(snarl, node_id);

    // ── the strip, when it goes above the editor ─────────────────────────────
    // Drawn first, so the editor's own height budget (which measures what is
    // already in this column) accounts for it without being told.
    if owns_strip && side == Side::Top {
        let budget = (!resizable).then(|| (size.y * 0.5).max(56.0));
        let (text, warp) = knob_rows(
            node_id, ui, snarl, size.x, &tabs[active].text, live, parent, paint, budget,
        );
        if let Some(text) = text {
            tabs[active].text = text;
            changed = true;
        }
        if let Some(w) = warp {
            pending_warp = Some(w);
        }
    }

    // ── tab bar ──────────────────────────────────────────────────────────────
    ui.horizontal_wrapped(|ui| {
        for i in 0..tabs.len() {
            let selected = i == active;
            if ui.selectable_label(selected, egui::RichText::new(&tabs[i].name).small()).clicked() {
                active = i;
                changed = true;
            }
        }
        if ui.small_button("+").on_hover_text("Add a config tab").clicked() {
            tabs.push(JsmTab { name: unique_name(&tabs, "config"), text: String::new() });
            active = tabs.len() - 1;
            changed = true;
        }
        if tabs.len() > 1
            && ui.small_button("✕").on_hover_text("Remove this tab").clicked()
        {
            tabs.remove(active);
            active = active.min(tabs.len() - 1);
            changed = true;
        }
    });

    // ── name + load / save for the active tab ────────────────────────────────
    // Wrapping, not a single row: these controls together are wider than a narrow
    // editor column, and a row that overflows pushes whatever sits beside it off
    // the body — which is what clipped the values off a right-hand tuning strip.
    ui.horizontal_wrapped(|ui| {
        let mut name = tabs[active].name.clone();
        let resp = ui.add(
            egui::TextEdit::singleline(&mut name)
                .desired_width((size.x * 0.35).clamp(60.0, TAB_NAME_W))
                .font(egui::TextStyle::Small)
                .hint_text("name"),
        );
        if resp.changed() {
            tabs[active].name = name;
            changed = true;
        }
        resp.on_hover_text(
            "A binding that loads a config by name finds the tab with that name.\n\
             Paths and .txt are ignored, so `GyroConfigs/vehicle.txt` is the tab `vehicle`.",
        );

        if ui.small_button("Load").on_hover_text("Read a JSM config file into this tab").clicked() {
            if let Some(path) = crate::overlay::with_overlay_not_topmost(|| {
                rfd::FileDialog::new()
                    .add_filter("JSM config", &["txt"])
                    .pick_file()
            }) {
                match std::fs::read_to_string(&path) {
                    Ok(text) => {
                        // The file name — without folder or extension — names the tab.
                        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                            tabs[active].name = stem.to_string();
                        }
                        tabs[active].text = text;
                        changed = true;
                    }
                    Err(e) => eprintln!("[jsm] load failed: {e}"),
                }
            }
        }
        if ui.small_button("Save").on_hover_text("Write this tab back to a .txt file").clicked() {
            if let Some(path) = crate::overlay::with_overlay_not_topmost(|| {
                rfd::FileDialog::new()
                    .add_filter("JSM config", &["txt"])
                    .set_file_name(format!("{}.txt", tabs[active].name))
                    .save_file()
            }) {
                if let Err(e) = std::fs::write(&path, &tabs[active].text) {
                    eprintln!("[jsm] save failed: {e}");
                }
            }
        }
        // The slider strip is opt-in: a config with a dozen numeric settings would
        // otherwise double the body's height before anyone asked for it.
        let mut knobs_on = show_knobs(snarl, node_id);
        if ui
            .selectable_label(knobs_on, egui::RichText::new("Tune").small())
            .on_hover_text(
                "Show the sensitivity curve, and a slider for every numeric setting this                  config sets. A slider rewrites the number on its own line — the text stays                  the config.",
            )
            .clicked()
        {
            knobs_on = !knobs_on;
            if let Some(n) = snarl.get_node_mut(node_id) {
                n.params.insert("jsm_show_knobs".into(), Value::Bool(knobs_on));
            }
        }
        // Which pane the pad is driving, while it is. Without this the bumpers
        // would switch between two halves that look identical until you press
        // something and the wrong one moves.
        if let Some(pane) = super::scale::jsm_nav_pane(ui, node_id) {
            let accent = crate::widgets::NavHighlightStyle::of(ui.ctx()).accent;
            // While the modifier is held, say what it is waiting for. A held
            // modifier with nothing on screen is one people press twice.
            let held = super::scale::jsm_nav_chord(ui, node_id);
            let text = if held { format!("🎮 {pane} · hold") } else { format!("🎮 {pane}") };
            ui.label(egui::RichText::new(text).small().color(accent))
                .on_hover_text(
                    "LB / RB switch between the config text and the faders.
                     LT / RT change config tab.
                     West lists what can stand where the cursor is; North lists every key,                      mouse button and pad output. South inserts, LT / RT jump by heading.
                     Hold South and tap West to delete what the cursor is on, left / right                      to open an empty slot beside it, or up / down to open a new line.
                     Hold South and push the left stick up or down to walk a number,                      one push at a time.",
                );
        }
        // Where the strip goes. Beside the editor suits a wide node; above or
        // below suits a tall one.
        if knobs_on {
            let mut picked = side;
            egui::ComboBox::from_id_salt(("jsm_knob_side", node_id.0))
                .selected_text(egui::RichText::new(side.label()).small())
                .width(64.0)
                .show_ui(ui, |ui| {
                    for s in [Side::Bottom, Side::Top, Side::Left, Side::Right] {
                        ui.selectable_value(&mut picked, s, egui::RichText::new(s.label()).small());
                    }
                });
            if picked != side {
                if let Some(n) = snarl.get_node_mut(node_id) {
                    n.params.insert("jsm_knob_side".into(), Value::from(picked.as_param()));
                }
            }
        }
    });

    // ── the editor, on its own row ───────────────────────────────────────────
    // Compiled against every tab, so a line that switches layers can be checked:
    // one naming a tab that isn't there is an error saying which tabs there are.
    let tab_list: Vec<(String, String)> =
        tabs.iter().map(|t| (t.name.clone(), t.text.clone())).collect();
    let mut compiled = flexinput_engine::eval::jsm_compile(&tabs[active].text, &tab_list);
    // A button this pad hasn't got is the device's business, not the config's, so
    // it lands as a note on the line rather than changing its status.
    flexinput_engine::eval::jsm_note_missing_inputs(
        &mut compiled,
        &pins_this_pad_reports(snarl, node_id, live, parent),
    );
    let mut text = tabs[active].text.clone();
    let line_h = ui.text_style_height(&egui::TextStyle::Monospace).max(10.0);
    // Whatever height is left under the two rows above, in whole lines.
    let used = ui.min_rect().height();
    // The sliders and the curve are laid out *after* the editor, so the editor has
    // to leave room for them up front or the body simply overflows its own size.
    // The command list is drawn under the editor too, so — pinned — the editor
    // has to give up the room for it or the list opens below the bottom edge and
    // you can't see what you asked for.
    let list_st = super::jsm_widgets::command_list_state(ui.ctx(), node_id);
    let list_open = list_st.open;
    let list_want = if list_open { super::jsm_widgets::LIST_H } else { 0.0 };
    let knobs_on = owns_strip && side == Side::Bottom;
    let strip_want = if knobs_on {
        let n = flexinput_engine::eval::jsm_knobs(&tabs[active].text).len() as f32;
        super::jsm_widgets::graph_height()
            + super::jsm_widgets::WARP_H
            + n * super::jsm_widgets::fader_height(ui)
    } else {
        0.0
    };
    let body_left = (size.y - used - SUMMARY_H - list_want).max(0.0);
    let (rows, strip_h) = split_body(body_left, line_h, strip_want);
    // Pinned, the container's height is all there is — so whatever the editor
    // leaves goes to the tuning strip, and the faders scroll inside it. Without a
    // budget the strip simply ran off the bottom of the widget, which looked like
    // a cap on how many settings a config could have. On the canvas the body grows
    // to fit instead, so there every fader is just drawn.
    let strip_budget = (!resizable && knobs_on).then_some(strip_h);
    let statuses: Vec<_> = compiled.lines.iter().map(|l| l.status.clone()).collect();
    // Where the pad is, if it is driving this editor. A byte range rather than a
    // line, so the highlight covers exactly the token the buttons would act on.
    let nav_cur = super::scale::jsm_nav_cursor(ui, node_id);
    let nav_token: Option<(usize, usize)> = nav_cur
        .and_then(|c| flexinput_engine::eval::jsm_selection(&tabs[active].text, c))
        .map(|(_, s, e)| (s, e));
    // The same place as a CHARACTER index, which is what a galley counts in.
    // It does two jobs the highlight can't. A line with nothing on it still has
    // a cursor, and with no token to tint there was nothing at all on screen to
    // say where the pad was — walking a run of blank lines looked like the pad
    // had stopped responding. And wherever the cursor is, the editor scrolls to
    // keep it in view, which a long config needs whether the line is blank or
    // not.
    let nav_head: Option<usize> = nav_cur.map(|c| {
        let body = &tabs[active].text;
        let at = nav_token
            .map(|(s, _)| s)
            .unwrap_or_else(|| flexinput_engine::eval::jsm_line_span(body, c.line).0);
        body[..at].chars().count()
    });
    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = wrap_width;
        let font = egui::FontId::monospace(12.0);
        let plain = ui.visuals().text_color();
        let accent = crate::widgets::NavHighlightStyle::of(ui.ctx()).accent;
        let raw = buf.as_str();
        // One `append` per run, so the highlighted token can carry a background
        // of its own without disturbing the per-line tinting around it.
        let mut at = 0usize;
        for (i, line) in raw.split_inclusive('\n').enumerate() {
            let color = statuses.get(i).and_then(|s| status_color(ui, s)).unwrap_or(plain);
            let fmt = |bg: Color32| egui::TextFormat {
                font_id: font.clone(),
                color,
                background: bg,
                ..Default::default()
            };
            let end = at + line.len();
            match nav_token {
                // The token is on this line: split it out of the run.
                Some((ts, te)) if ts >= at && te <= end && te > ts => {
                    job.append(&raw[at..ts], 0.0, fmt(Color32::TRANSPARENT));
                    job.append(&raw[ts..te], 0.0, fmt(accent.gamma_multiply(0.45)));
                    job.append(&raw[te..end], 0.0, fmt(Color32::TRANSPARENT));
                }
                _ => job.append(line, 0.0, fmt(Color32::TRANSPARENT)),
            }
            at = end;
        }
        ui.ctx().fonts_mut(|f| f.layout_job(job))
    };
    // A solid scroll bar, snug against the text. egui's default bar floats over
    // the content and fades in as the pointer nears it, which around the editor's
    // edge flickered on and off; a bar that is always there, always the same
    // width, can't change anything by being hovered. The margins go because a
    // bar sitting off on its own reads as a gap rather than as this box's edge.
    let mut bar = egui::style::ScrollStyle::solid();
    bar.bar_inner_margin = 0.0;
    bar.bar_outer_margin = 0.0;
    ui.style_mut().spacing.scroll = bar;
    let bar_w = ui.style().spacing.scroll.allocated_width();
    // A fixed width (never `available_width`, which is unbounded in the node's
    // horizontal parent — the editor grew without limit as you typed) and a
    // scroll area, so a long config scrolls instead of stretching the node.
    // `scrolling_body` is what makes the wheel work at all in here; see
    // `canvas/wheel.rs`.
    // The editor paints its OWN background and frame, on top of anything drawn
    // behind it — so styling the pin means styling the TextEdit, not laying a
    // plate under it. `extreme_bg_color` is what a code editor fills with; the
    // three widget strokes are its border in each state, set together so the
    // frame doesn't change colour just because the pointer crossed it.
    super::jsm_widgets::style_text_editor(ui, paint);
    let wheel_id = egui::Id::new(("jsm_wheel", node_id.0));
    let editor = crate::canvas::wheel::scrolling_body(
        ui,
        wheel_id,
        egui::ScrollArea::vertical()
            .id_salt(("jsm_editor", node_id.0))
            .max_height(rows as f32 * line_h)
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible),
        |ui| {
            let out = egui::TextEdit::multiline(&mut text)
                .code_editor()
                .desired_rows(rows)
                .desired_width(size.x - bar_w)
                .clip_text(true)
                .hint_text("Paste or load a JoyShockMapper config")
                .layouter(&mut layouter)
                .show(ui);
            if let Some(head) = nav_head {
                // A zero-width rect spanning the row the cursor sits on.
                let at = out
                    .galley
                    .pos_from_cursor(egui::text::CCursor::new(head))
                    .translate(out.galley_pos.to_vec2());
                let row = egui::Rect::from_min_size(
                    at.min,
                    egui::vec2(2.0, at.height().max(line_h)),
                );
                if nav_token.is_none() {
                    // Nothing under the cursor to tint, so draw the cursor
                    // itself. Solid rather than blinking: this says where the
                    // pad is, and something that spends half its life invisible
                    // is a poor answer to "where am I".
                    let accent = crate::widgets::NavHighlightStyle::of(ui.ctx()).accent;
                    ui.painter().with_clip_rect(out.text_clip_rect).rect_filled(row, 0.0, accent);
                }
                // Follow the cursor, but only when it MOVES. Scrolling every
                // frame would fight the wheel, and reading a long config with
                // the mouse while a pad rests in it is an ordinary thing to do.
                let key = egui::Id::new(("jsm_nav_head", node_id.0))
                    .with(ui.ctx().viewport_id())
                    .with(ui.layer_id());
                if ui.ctx().data(|d| d.get_temp::<usize>(key)) != Some(head) {
                    ui.ctx().data_mut(|d| d.insert_temp(key, head));
                    ui.scroll_to_rect(row, None);
                }
            }
            out
        },
    );
    // The box you see, bar included — not the text inside it, which is taller
    // than the box whenever the config is longer than the editor.
    let editor_rect = editor.inner_rect.with_max_x(editor.inner_rect.max.x + bar_w);
    let resp = editor.inner.response;
    if resp.changed() {
        tabs[active].text = text;
        changed = true;
    }
    // Right-click the editor for the vocabulary. The list is the only place the
    // module tells you what it knows, so it goes where the typing happens.
    resp.context_menu(|ui| {
        if ui.button("Commands…").clicked() {
            let mut st = super::jsm_widgets::command_list_state(ui.ctx(), node_id);
            st.open = true;
            st.index = 0;
            super::jsm_widgets::set_command_list_state(ui.ctx(), node_id, st);
            ui.close();
        }
    });
    // A binding under test must not type into the editor it is being edited in.
    flexinput_engine::eval::set_jsm_editor_focus(resp.has_focus());
    // The editor is what you pin to the config overlay.
    register_exposable_element(ui, node_id, "editor", editor_rect);

    // ── the command list, when it was asked for ──────────────────────────────
    //
    // What it offers depends on where it will land. A pad has a cursor, so the
    // list offers what can legally stand THERE; a mouse has none, so its pick
    // goes on a line of its own, where anything can.
    let list_kinds: &[flexinput_engine::eval::JsmKind] = if list_st.values {
        // North asked for the keys, wherever the cursor happens to be.
        &[flexinput_engine::eval::JsmKind::Binding]
    } else {
        match nav_cur {
            Some(c) => flexinput_engine::eval::jsm_kinds_at(&tabs[active].text, c),
            None => &[
                flexinput_engine::eval::JsmKind::Setting,
                flexinput_engine::eval::JsmKind::Command,
                flexinput_engine::eval::JsmKind::Trigger,
            ],
        }
    };
    if let Some((name, kind)) = super::jsm_widgets::command_list(
        ui, node_id, size.x, &pins_this_pad_reports(snarl, node_id, live, parent),
        (!resizable).then_some(list_want),
        list_kinds,
    ) {
        match nav_cur {
            // The pad picked it, so it lands under the pad's cursor — and the
            // cursor moves on to whatever the line still needs, which for a
            // setting is the slot its number goes in. The nav driver owns that
            // cursor, so the move goes back to it as a request rather than
            // being written here behind its back.
            Some(c) => {
                let (edited, at) =
                    flexinput_engine::eval::jsm_insert_pick(&tabs[active].text, c, &name, kind);
                tabs[active].text = edited;
                super::scale::publish_jsm_cursor_request(ui.ctx(), node_id, at);
            }
            // Appended on a line of its own. Inserting at the caret would need
            // the TextEdit's cursor, which a `context_menu` click has already
            // taken the focus away from — a new line is predictable, and the
            // editor is right there to move it.
            None => {
                let line = super::jsm_widgets::insertion_for(&name, kind);
                let text = &mut tabs[active].text;
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&line);
                text.push('\n');
            }
        }
        changed = true;
    }

    // ── what the parser made of it ───────────────────────────────────────────
    let (errors, pending, ignored) = compiled.summary();
    if errors + pending + ignored > 0 {
        ui.horizontal_wrapped(|ui| {
            if errors > 0 {
                ui.label(egui::RichText::new(format!("{errors} error{}", plural(errors)))
                    .small().color(Color32::from_rgb(226, 104, 92)));
            }
            if pending > 0 {
                ui.label(egui::RichText::new(format!("{pending} not live yet"))
                    .small().color(Color32::from_rgb(214, 168, 74)));
            }
            if ignored > 0 {
                ui.label(egui::RichText::new(format!("{ignored} ignored here")).small().weak());
            }
        });
        // Pinned, the container owns the height: the counts carry the summary and
        // the node body keeps the line-by-line reasons. They scroll rather than
        // growing the body without limit — a config with many notes would otherwise
        // push the sliders off the bottom.
        if resizable {
            super::jsm_widgets::scrolling_notes(ui, node_id, size.x, size.y * 0.4, |ui| {
                show_line_notes(ui, &compiled);
            });
        }
    }

    // ── a slider per numeric setting, and the curve they shape ───────────────
    if knobs_on {
        let (text, warp) = knob_rows(
            node_id, ui, snarl, size.x, &tabs[active].text, live, parent, paint, strip_budget,
        );
        if let Some(text) = text {
            tabs[active].text = text;
            changed = true;
        }
        if let Some(w) = warp {
            pending_warp = Some(w);
        }
    }

    // ── resize handle (node body only) ───────────────────────────────────────
    if resizable {
        if let Some(new_size) = resize_handle(ui, node_id, size) {
            if let Some(n) = snarl.get_node_mut(node_id) {
                n.params.insert("jsm_editor_w".into(), Value::from(new_size.x as f64));
                n.params.insert("jsm_editor_h".into(), Value::from(new_size.y as f64));
            }
        }
    }

    if let Some(w) = pending_warp {
        set_curve_warp(snarl, node_id, w);
        if !resizable {
            mark_overlay_param_write(ui.ctx());
        }
    }
    if changed {
        write_tabs(snarl, node_id, &tabs, active);
        // Pinned, this ran in the overlay's viewport: tell it to bump the canvas
        // generation so an open sub-patch editor re-reads instead of writing its
        // stale copy back over the edit.
        if !resizable {
            mark_overlay_param_write(ui.ctx());
        }
    }
}

/// Split the body's height between the editor and the tuning strip, as
/// `(editor rows, strip height)`.
///
/// Pinned, the container's height is all there is. The editor gets what the strip
/// doesn't want, never below three lines; everything left over goes to the strip,
/// which scrolls — so a config setting thirty numbers is as reachable as one
/// setting three. The strip keeps a floor of roughly one fader, or a short pin
/// would show a curve with nothing under it to tune.
fn split_body(body_left: f32, line_h: f32, strip_want: f32) -> (usize, f32) {
    const MIN_ROWS: i32 = 3;
    const MIN_STRIP: f32 = 56.0;
    let rows = (((body_left - strip_want) / line_h.max(1.0)).floor() as i32).max(MIN_ROWS) as usize;
    (rows, (body_left - rows as f32 * line_h).max(MIN_STRIP))
}

/// Is the slider strip switched on for this node? Off by default: a config with a
/// dozen numeric settings would otherwise double the body's height before anyone
/// asked for it.
fn show_knobs(snarl: &Snarl<NodeData>, node_id: NodeId) -> bool {
    snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("jsm_show_knobs"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// The curve preview and a slider per numeric setting, under the diagnostics.
///
/// Returns the rewritten config text when a slider moved — the text is the source
/// of truth, so a drag is an edit like any other and the parser sees it at once.
///
/// `budget` is the height the strip has to live within (pinned), or `None` to
/// draw every fader and let the body grow (on the canvas).
#[allow(clippy::too_many_arguments)]
fn knob_rows(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &Snarl<NodeData>,
    width: f32,
    text: &str,
    live: &std::collections::HashMap<(String, String), Signal>,
    parent: Option<&AutomapGlowParent<'_>>,
    paint: super::jsm_widgets::JsmPaint<'_>,
    budget: Option<f32>,
) -> (Option<String>, Option<f32>) {
    // The curve first, and it stays put while the faders scroll: it is the thing
    // you are watching as you drag one.
    let curve_h = match budget {
        // Enough of the budget to read, but never so much that no fader is left
        // visible under it.
        Some(h) => ((h - super::jsm_widgets::WARP_H) * 0.45)
            .clamp(44.0, super::jsm_widgets::graph_height()),
        None => super::jsm_widgets::graph_height(),
    };
    let compiled = flexinput_engine::eval::jsm_compile(text, &[]);
    let warp = curve_warp(snarl, node_id);
    let points = flexinput_engine::eval::jsm_sens_curve_warped(&compiled, 96, warp);
    let dev = upstream_device(snarl, node_id, parent).unwrap_or_default();
    let rect = super::jsm_widgets::curve_graph(
        ui,
        width,
        curve_h,
        &points,
        super::jsm_widgets::live_turn_speed(live, &dev),
        paint,
        warp,
    );
    register_exposable_element(ui, node_id, "curve", rect);
    let new_warp = super::jsm_widgets::warp_slider(ui, width, warp, paint);

    let knobs = flexinput_engine::eval::jsm_knobs(text);
    if knobs.is_empty() {
        ui.label(
            egui::RichText::new("no numeric settings to tune yet — add one (GYRO_SENS = 2) and a slider appears")
                .small()
                .weak(),
        );
        return (None, new_warp);
    }

    // Inside a scroll area the bar takes a lane off the right — without allowing
    // for it the fader ran under the bar and the value printed above its right
    // end was half-hidden behind it.
    let fader_w = match budget {
        Some(_) => (width - ui.style().spacing.scroll.allocated_width()).max(24.0),
        None => width,
    };
    // One rect per fader, in the order nav walks them, so the focused-field ring
    // lands on the setting the pad is actually editing.
    let mut field_rects: Vec<egui::Rect> = Vec::with_capacity(knobs.len());
    // The fader gamepad nav has focused, and the one this strip last scrolled to.
    // Only a CHANGE scrolls: holding focus on a row while the wheel is used would
    // otherwise drag the strip straight back, and the mouse would feel stuck.
    let focus = nav_focus_field(ui, node_id);
    let scrolled_to_id = egui::Id::new(("jsm_nav_scrolled", node_id.0));
    let scrolled_to: Option<usize> = ui.ctx().data(|d| d.get_temp(scrolled_to_id));
    let bring_into_view = focus.filter(|f| Some(*f) != scrolled_to);
    if let Some(f) = bring_into_view {
        ui.ctx().data_mut(|d| d.insert_temp(scrolled_to_id, f));
    }
    let faders = |ui: &mut egui::Ui, field_rects: &mut Vec<egui::Rect>| {
        let mut edited = None;
        for (i, knob) in knobs.iter().enumerate() {
            let start = ui.cursor().min;
            if let Some(v) = super::jsm_widgets::fader(ui, fader_w, knob, paint) {
                edited = Some(flexinput_engine::eval::jsm_set_knob(
                    text,
                    knob.line,
                    v,
                    knob.integral,
                ));
            }
            // Each slider pins on its own, so a tuning session can carry just the
            // two or three that matter into the config overlay. A row scrolled out
            // of sight registers nothing: an overlay pick has to land on what the
            // pointer is actually over.
            let row = egui::Rect::from_min_max(start, ui.cursor().min + egui::vec2(fader_w, 0.0));
            if bring_into_view == Some(i) {
                ui.scroll_to_rect(row, None);
            }
            // Clipped to what is actually on screen, so a row scrolled out of the
            // band publishes an empty rect and the focus ring skips it instead of
            // drawing across the container's edge at a control nobody can see.
            field_rects.push(row.intersect(ui.clip_rect()));
            if ui.clip_rect().intersects(row) {
                register_exposable_element(
                    ui,
                    node_id,
                    &super::jsm_widgets::knob_element_id(&knob.name),
                    row,
                );
            }
        }
        edited
    };

    let edited = match budget {
        Some(h) => super::jsm_widgets::scrolling_faders(
            ui,
            node_id,
            width,
            (h - curve_h - super::jsm_widgets::WARP_H - ui.spacing().item_spacing.y)
                .max(super::jsm_widgets::fader_height(ui)),
            |ui| faders(ui, &mut field_rects),
        ),
        None => faders(ui, &mut field_rects),
    };
    // Keyed by (inner node, current element) — the pinned renderer stamps the
    // element before dispatching, which is the same key the focus ring reads.
    publish_nav_field_rects(ui, node_id, &field_rects);
    (edited, new_warp)
}

/// Bottom-right grip that drags the editor's size, like the 3D viewer's.
/// Returns the new size while it is being dragged.
fn resize_handle(ui: &mut egui::Ui, node_id: NodeId, size: egui::Vec2) -> Option<egui::Vec2> {
    const HS: f32 = 12.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size.x, HS), egui::Sense::hover());
    let grip = egui::Rect::from_min_size(egui::pos2(rect.right() - HS, rect.top()), egui::vec2(HS, HS));
    let resp = ui.interact(grip, ui.id().with(("jsm_resize", node_id.0)), egui::Sense::click_and_drag());
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
    }
    // Three short diagonals, the usual grip.
    let painter = ui.painter();
    let color = if resp.hovered() {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    for i in 0..3 {
        let o = 3.0 * i as f32;
        painter.line_segment(
            [egui::pos2(grip.right() - o - 2.0, grip.bottom() - 2.0),
             egui::pos2(grip.right() - 2.0, grip.bottom() - o - 2.0)],
            egui::Stroke::new(1.0, color),
        );
    }
    resp.dragged().then(|| {
        let d = resp.drag_delta();
        egui::vec2((size.x + d.x).max(MIN_W), (size.y + d.y).max(MIN_H))
    })
}

/// The lines worth explaining, with their reason — a few at a time so the body
/// doesn't grow without limit.
fn show_line_notes(ui: &mut egui::Ui, compiled: &flexinput_engine::eval::JsmConfig) {
    use flexinput_engine::eval::JsmLineStatus as S;
    const MAX: usize = 6;
    let mut shown = 0;
    let mut more = 0;
    for (i, line) in compiled.lines.iter().enumerate() {
        let (color, text) = match &line.status {
            S::Error(e) => (Color32::from_rgb(226, 104, 92), e.clone()),
            S::Pending(p) => (Color32::from_rgb(214, 168, 74), p.to_string()),
            S::Ignored(w) => (ui.visuals().weak_text_color(), w.to_string()),
            S::Ok if !line.notes.is_empty() => (ui.visuals().weak_text_color(), line.notes.join("; ")),
            _ => continue,
        };
        if shown >= MAX {
            more += 1;
            continue;
        }
        shown += 1;
        let notes = line.notes.join("; ");
        let label = format!("line {}: {text}", i + 1);
        let resp = ui.label(egui::RichText::new(label).small().color(color));
        if !notes.is_empty() && !matches!(line.status, S::Ok) {
            resp.on_hover_text(notes);
        }
    }
    if more > 0 {
        ui.label(egui::RichText::new(format!("…and {more} more")).small().weak());
    }
}

fn plural(n: usize) -> &'static str { if n == 1 { "" } else { "s" } }

fn unique_name(tabs: &[JsmTab], base: &str) -> String {
    let mut n = 1;
    loop {
        let name = if n == 1 { base.to_string() } else { format!("{base}{n}") };
        if !tabs.iter().any(|t| t.name == name) { return name; }
        n += 1;
    }
}

/// Read the node's tabs, defaulting to a single empty one.
pub(crate) fn read_tabs(snarl: &Snarl<NodeData>, node_id: NodeId) -> Vec<JsmTab> {
    let tabs: Vec<JsmTab> = snarl.get_node(node_id)
        .and_then(|n| n.params.get("jsm_tabs"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(|t| JsmTab {
            name: t.get("name").and_then(|v| v.as_str()).unwrap_or("config").to_string(),
            text: t.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        }).collect())
        .unwrap_or_default();
    if tabs.is_empty() {
        vec![JsmTab { name: "main".to_string(), text: String::new() }]
    } else {
        tabs
    }
}

fn write_tabs(snarl: &mut Snarl<NodeData>, node_id: NodeId, tabs: &[JsmTab], active: usize) {
    let Some(node) = snarl.get_node_mut(node_id) else { return; };
    let arr: Vec<Value> = tabs.iter()
        .map(|t| serde_json::json!({ "name": t.name, "text": t.text }))
        .collect();
    node.params.insert("jsm_tabs".into(), Value::Array(arr));
    node.params.insert("jsm_active_tab".into(), Value::from(active as u64));
}

#[cfg(test)]
mod tests {
    use super::{split_body, unique_name, JsmTab, Side};

    const LINE: f32 = 14.0;

    // The strip's placement is saved on the node, so it has to survive the trip
    // through a param string — and an unknown one has to mean the default rather
    // than a panic or a blank body.
    // The fine modifier scales the nudge down, and on a narrow range that landed
    // inside the two decimals a config is written with — so the value rounded
    // straight back and fine looked broken. It has to move by the least the
    // config can express instead.
    #[test]
    fn a_fine_nudge_too_small_to_round_still_moves_by_the_least_step() {
        use crate::canvas::node::{NodeData, NodeExtra};
        let node_with = |text: &str| {
            let mut n = NodeData {
                module_id: "module.jsm".into(),
                display_name: String::new(),
                category: String::new(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                params: Default::default(),
                subpatch: None,
                extra: NodeExtra::default(),
            };
            n.params.insert("jsm_tabs".into(), serde_json::json!([{ "name": "m", "text": text }]));
            n
        };
        let text_of = |n: &NodeData| n.params["jsm_tabs"][0]["text"].as_str().unwrap().to_string();

        // 0..1 range: a 0.005 nudge is half of what two decimals can hold.
        let mut node = node_with("DECEL_BRAKE_STRENGTH = 0.5\n");
        assert!(super::nav_nudge_knob(&mut node, "DECEL_BRAKE_STRENGTH", 0.005));
        assert_eq!(text_of(&node), "DECEL_BRAKE_STRENGTH = 0.51\n", "up by the least step");
        assert!(super::nav_nudge_knob(&mut node, "DECEL_BRAKE_STRENGTH", -0.005));
        assert_eq!(text_of(&node), "DECEL_BRAKE_STRENGTH = 0.5\n", "and back down");

        // A whole-numbered setting's least step is 1, not 0.01.
        let mut node = node_with("HOLD_PRESS_TIME = 150\n");
        assert!(super::nav_nudge_knob(&mut node, "HOLD_PRESS_TIME", 0.0001));
        assert_eq!(text_of(&node), "HOLD_PRESS_TIME = 151\n");

        // At the end of the range it genuinely cannot move, and says so — the
        // caller leaves the text (and its undo history) alone.
        let mut node = node_with("DECEL_BRAKE_STRENGTH = 1\n");
        assert!(!super::nav_nudge_knob(&mut node, "DECEL_BRAKE_STRENGTH", 0.005));
        assert_eq!(text_of(&node), "DECEL_BRAKE_STRENGTH = 1\n");
    }

    // Gamepad nav walks `knobs_of` while the body draws `jsm_knobs` of the same
    // tab. If the two ever disagreed — about which tab, or about the order — the
    // focus ring would point at one setting while the pad edited another, which
    // is the kind of bug you don't notice until you've wrecked a config.
    #[test]
    fn the_list_nav_walks_is_the_list_the_body_draws() {
        use crate::canvas::node::{NodeData, NodeExtra};
        let mut node = NodeData {
            module_id: "module.jsm".into(),
            display_name: String::new(),
            category: String::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            params: Default::default(),
            subpatch: None,
            extra: NodeExtra::default(),
        };
        node.params.insert(
            "jsm_tabs".into(),
            serde_json::json!([
                { "name": "a", "text": "GYRO_SENS = 1\n" },
                { "name": "b", "text": "FLICK_TIME = 0.1\nGYRO_SENS = 3\nSTICK_POWER = 2\n" },
            ]),
        );
        node.params.insert("jsm_active_tab".into(), serde_json::json!(1));

        let names: Vec<String> = super::knobs_of(&node).iter().map(|k| k.name.clone()).collect();
        assert_eq!(names, ["FLICK_TIME", "GYRO_SENS", "STICK_POWER"], "the ACTIVE tab, in source order");

        // An out-of-range active tab falls back to the first, and doesn't panic.
        node.params.insert("jsm_active_tab".into(), serde_json::json!(9));
        let names: Vec<String> = super::knobs_of(&node).iter().map(|k| k.name.clone()).collect();
        assert_eq!(names, ["GYRO_SENS"]);

        // ...and a nudge lands in that same tab, not in whichever one is first.
        node.params.insert("jsm_active_tab".into(), serde_json::json!(1));
        assert!(super::nav_nudge_knob(&mut node, "STICK_POWER", 0.1));
        assert_eq!(
            node.params["jsm_tabs"][0]["text"].as_str().unwrap(),
            "GYRO_SENS = 1\n",
            "the inactive tab is untouched"
        );
        assert!(node.params["jsm_tabs"][1]["text"].as_str().unwrap().contains("STICK_POWER = 2.4"));
    }

    // A pad driving a pinned setting has to move the NUMBER IN THE TEXT, since
    // that is the only source of truth — not a param the engine would never read.
    #[test]
    fn a_gamepad_nudge_rewrites_the_setting_in_the_config_text() {
        use crate::canvas::node::{NodeData, NodeExtra};
        let mut node = NodeData {
            module_id: "module.jsm".into(),
            display_name: "JSM Config".into(),
            category: String::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            params: Default::default(),
            subpatch: None,
            extra: NodeExtra::default(),
        };
        node.params.insert(
            "jsm_tabs".into(),
            serde_json::json!([{ "name": "main", "text": "GYRO_SENS = 4   # feel\n" }]),
        );

        assert!(super::nav_nudge_knob(&mut node, "GYRO_SENS", 0.1), "it moved");
        let text = |n: &NodeData| {
            n.params["jsm_tabs"][0]["text"].as_str().unwrap_or_default().to_string()
        };
        // GYRO_SENS runs 0..32, so a tenth of the range is +3.2 on top of 4.
        assert_eq!(text(&node), "GYRO_SENS = 7.2   # feel\n", "spacing and comment survive");

        // A setting already at the top of its range doesn't "move", and saying so
        // lets the caller leave the text — and its undo history — alone.
        super::nav_nudge_knob(&mut node, "GYRO_SENS", 10.0);
        assert!(!super::nav_nudge_knob(&mut node, "GYRO_SENS", 1.0), "already at the end");
        assert_eq!(text(&node), "GYRO_SENS = 32   # feel\n");

        // A name that isn't in this tab is a no-op, not a panic or a stray write.
        let before = text(&node);
        assert!(!super::nav_nudge_knob(&mut node, "FLICK_TIME", 0.5));
        assert_eq!(text(&node), before);
    }

    #[test]
    fn where_the_tuning_strip_sits_round_trips_through_the_node() {
        for s in [Side::Bottom, Side::Top, Side::Left, Side::Right] {
            assert_eq!(Side::from_param(s.as_param()), s, "{}", s.label());
            assert!(!s.label().is_empty());
        }
        assert_eq!(Side::from_param("sideways"), Side::Bottom, "an unknown side is the default");
        assert_eq!(Side::from_param(""), Side::Bottom, "and so is none at all");
    }

    // The bug this exists to stop: a pinned editor showed a fixed handful of
    // sliders and making the widget taller changed nothing, because the editor
    // grew to eat every extra pixel and the strip stayed the same size — so the
    // faders past the first few simply ran off the bottom.
    #[test]
    fn a_taller_pinned_editor_gives_the_tuning_strip_more_room() {
        // Nine settings' worth of faders, plus the curve: more than a short pin
        // can show at once.
        let want = 110.0 + 9.0 * 27.0;
        let (_, short) = split_body(300.0, LINE, want);
        let (_, tall) = split_body(600.0, LINE, want);
        assert!(
            tall > short + 100.0,
            "the strip has to grow with the container: {short} then {tall}"
        );
    }

    // ...and the editor is still an editor. Three lines is the floor whatever the
    // strip asks for, so a config with a great many numeric settings can't leave
    // you with nowhere to type.
    #[test]
    fn the_editor_keeps_three_lines_however_much_the_strip_wants() {
        for want in [0.0, 200.0, 5_000.0] {
            let (rows, strip) = split_body(120.0, LINE, want);
            assert!(rows >= 3, "rows at want={want}: {rows}");
            assert!(strip >= 56.0, "a fader's worth of strip at want={want}: {strip}");
        }
    }

    // With nothing to tune the editor takes the lot — switching Tune off must not
    // leave a gap where the strip used to be.
    #[test]
    fn with_no_strip_the_editor_takes_the_whole_body() {
        let (rows, _) = split_body(280.0, LINE, 0.0);
        assert_eq!(rows, 20, "280 / 14");
    }

    // Adding tabs never collides with a name already in use — a binding that
    // loads a config by name has to land on exactly one tab.
    #[test]
    fn a_new_tab_gets_a_name_of_its_own() {
        let mut tabs = vec![JsmTab { name: "main".into(), text: String::new() }];
        assert_eq!(unique_name(&tabs, "config"), "config");
        tabs.push(JsmTab { name: "config".into(), text: String::new() });
        assert_eq!(unique_name(&tabs, "config"), "config2");
        tabs.push(JsmTab { name: "config2".into(), text: String::new() });
        assert_eq!(unique_name(&tabs, "config"), "config3");
    }
}

/// Header toggle: pass the pad's unmentioned inputs through, or emit only what
/// the config produces (how JSM behaves with the pad hidden from the game).
pub(crate) fn jsm_strict_header_toggle(
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    node: NodeId,
) {
    let mut strict = snarl.get_node(node)
        .and_then(|n| n.params.get("jsm_strict"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let resp = ui.checkbox(&mut strict, egui::RichText::new("Only what the config says").small());
    if resp.changed() {
        if let Some(n) = snarl.get_node_mut(node) {
            n.params.insert("jsm_strict".into(), Value::Bool(strict));
        }
    }
    resp.on_hover_text(
        "Off: inputs the config never mentions pass straight through, and the ones it \
         does are taken over.\n\
         On: nothing passes through — only what the config produces leaves this module, \
         which is how JoyShockMapper behaves with the pad hidden from the game.",
    );
}
