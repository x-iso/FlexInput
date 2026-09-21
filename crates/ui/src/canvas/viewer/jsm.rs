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
    let points = flexinput_engine::eval::jsm_sens_curve(&compiled, 160);
    let dev = upstream_device(snarl, node_id, parent).unwrap_or_default();
    super::jsm_widgets::curve_graph(
        ui,
        size.x,
        size.y.max(40.0),
        &points,
        super::jsm_widgets::live_turn_speed(live, &dev),
        paint,
    );
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
    egui::vec2(p("jsm_editor_w", EDITOR_W).max(MIN_W), p("jsm_editor_h", EDITOR_H).max(MIN_H))
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
    if knobs_on && matches!(side, Side::Left | Side::Right) {
        let gap = ui.spacing().item_spacing.x;
        // The strip gets its share, but never so much that the editor stops
        // being one — below that the strip would be a column of faders next to a
        // sliver of text, which is not what "beside" is for.
        let strip_w = (size.x * 0.42).clamp(140.0, 320.0).min((size.x - 160.0).max(1.0));
        let col_w = (size.x - strip_w - gap).max(120.0);
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
        return;
    }

    ui.vertical(|ui| {
        ui.set_max_width(size.x);
        jsm_rows(node_id, ui, snarl, size, resizable, live, parent, paint, knobs_on);
    });
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
    let mut edited = None;
    ui.vertical(|ui| {
        ui.set_max_width(width);
        // Beside the editor the strip owns its column's full height, whether the
        // body is a node (which grows) or a pin (which does not) — either way it
        // has a column to fill rather than a leftover to squeeze into.
        edited = knob_rows(
            node_id, ui, snarl, width, &tabs[active].text, live, parent, paint,
            Some(height),
        );
    });
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
    let side = knob_side(snarl, node_id);

    // ── the strip, when it goes above the editor ─────────────────────────────
    // Drawn first, so the editor's own height budget (which measures what is
    // already in this column) accounts for it without being told.
    if owns_strip && side == Side::Top {
        let budget = (!resizable).then(|| (size.y * 0.5).max(56.0));
        if let Some(text) = knob_rows(
            node_id, ui, snarl, size.x, &tabs[active].text, live, parent, paint, budget,
        ) {
            tabs[active].text = text;
            changed = true;
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
    ui.horizontal(|ui| {
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
    let knobs_on = owns_strip && side == Side::Bottom;
    let strip_want = if knobs_on {
        let n = flexinput_engine::eval::jsm_knobs(&tabs[active].text).len() as f32;
        super::jsm_widgets::graph_height() + n * super::jsm_widgets::fader_height(ui)
    } else {
        0.0
    };
    let body_left = (size.y - used - SUMMARY_H).max(0.0);
    let (rows, strip_h) = split_body(body_left, line_h, strip_want);
    // Pinned, the container's height is all there is — so whatever the editor
    // leaves goes to the tuning strip, and the faders scroll inside it. Without a
    // budget the strip simply ran off the bottom of the widget, which looked like
    // a cap on how many settings a config could have. On the canvas the body grows
    // to fit instead, so there every fader is just drawn.
    let strip_budget = (!resizable && knobs_on).then_some(strip_h);
    let statuses: Vec<_> = compiled.lines.iter().map(|l| l.status.clone()).collect();
    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = wrap_width;
        let font = egui::FontId::monospace(12.0);
        let plain = ui.visuals().text_color();
        let raw = buf.as_str();
        for (i, line) in raw.split_inclusive('\n').enumerate() {
            let color = statuses.get(i).and_then(|s| status_color(ui, s)).unwrap_or(plain);
            job.append(line, 0.0, egui::TextFormat { font_id: font.clone(), color, ..Default::default() });
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
    let wheel_id = egui::Id::new(("jsm_wheel", node_id.0));
    let editor = crate::canvas::wheel::scrolling_body(
        ui,
        wheel_id,
        egui::ScrollArea::vertical()
            .id_salt(("jsm_editor", node_id.0))
            .max_height(rows as f32 * line_h)
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible),
        |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut text)
                    .code_editor()
                    .desired_rows(rows)
                    .desired_width(size.x - bar_w)
                    .clip_text(true)
                    .hint_text("Paste or load a JoyShockMapper config")
                    .layouter(&mut layouter),
            )
        },
    );
    // The box you see, bar included — not the text inside it, which is taller
    // than the box whenever the config is longer than the editor.
    let editor_rect = editor.inner_rect.with_max_x(editor.inner_rect.max.x + bar_w);
    let resp = editor.inner;
    if resp.changed() {
        tabs[active].text = text;
        changed = true;
    }
    // A binding under test must not type into the editor it is being edited in.
    flexinput_engine::eval::set_jsm_editor_focus(resp.has_focus());
    // The editor is what you pin to the config overlay.
    register_exposable_element(ui, node_id, "editor", editor_rect);

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
        if let Some(text) = knob_rows(
            node_id, ui, snarl, size.x, &tabs[active].text, live, parent, paint, strip_budget,
        ) {
            tabs[active].text = text;
            changed = true;
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
) -> Option<String> {
    // The curve first, and it stays put while the faders scroll: it is the thing
    // you are watching as you drag one.
    let curve_h = match budget {
        // Enough of the budget to read, but never so much that no fader is left
        // visible under it.
        Some(h) => (h * 0.45).clamp(44.0, super::jsm_widgets::graph_height()),
        None => super::jsm_widgets::graph_height(),
    };
    let compiled = flexinput_engine::eval::jsm_compile(text, &[]);
    let points = flexinput_engine::eval::jsm_sens_curve(&compiled, 96);
    let dev = upstream_device(snarl, node_id, parent).unwrap_or_default();
    let rect = super::jsm_widgets::curve_graph(
        ui,
        width,
        curve_h,
        &points,
        super::jsm_widgets::live_turn_speed(live, &dev),
        paint,
    );
    register_exposable_element(ui, node_id, "curve", rect);

    let knobs = flexinput_engine::eval::jsm_knobs(text);
    if knobs.is_empty() {
        ui.label(
            egui::RichText::new("no numeric settings to tune yet — add one (GYRO_SENS = 2) and a slider appears")
                .small()
                .weak(),
        );
        return None;
    }

    // Inside a scroll area the bar takes a lane off the right — without allowing
    // for it the fader ran under the bar and the value printed above its right
    // end was half-hidden behind it.
    let fader_w = match budget {
        Some(_) => (width - ui.style().spacing.scroll.allocated_width()).max(24.0),
        None => width,
    };
    let faders = |ui: &mut egui::Ui| {
        let mut edited = None;
        for knob in &knobs {
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

    match budget {
        Some(h) => super::jsm_widgets::scrolling_faders(
            ui,
            node_id,
            width,
            (h - curve_h - ui.spacing().item_spacing.y).max(super::jsm_widgets::fader_height(ui)),
            faders,
        ),
        None => faders(ui),
    }
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
