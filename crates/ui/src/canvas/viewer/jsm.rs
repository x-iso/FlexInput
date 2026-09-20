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
) {
    jsm_body(node_id, ui, snarl, None, live);
}

/// The pins the pad feeding this node actually reports, so the editor can say
/// which buttons a config asks for that this pad hasn't got. Empty when there is
/// no device yet — nothing is claimed on a guess.
fn pins_this_pad_reports(
    snarl: &Snarl<NodeData>,
    node_id: NodeId,
    live: &std::collections::HashMap<(String, String), Signal>,
) -> std::collections::HashSet<String> {
    let dev = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("_automap_device_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
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
) {
    jsm_body(node_id, ui, snarl, Some(size), live);
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
) {
    // A node body is laid out BETWEEN the pin columns, so its parent is a
    // horizontal layout with no width to inherit: the body picks its own size
    // (stored on the node, dragged by the handle below) and stacks its rows in
    // its own vertical layout. Pinned, the container's size wins.
    let size = pinned.unwrap_or_else(|| editor_size(snarl, node_id));
    ui.vertical(|ui| {
        ui.set_max_width(size.x);
        jsm_rows(node_id, ui, snarl, size, pinned.is_none(), live);
    });
}

fn jsm_rows(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    size: egui::Vec2,
    resizable: bool,
    live: &std::collections::HashMap<(String, String), Signal>,
) {
    let mut tabs = read_tabs(snarl, node_id);
    let mut active = snarl.get_node(node_id)
        .and_then(|n| n.params.get("jsm_active_tab"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    if active >= tabs.len() { active = 0; }
    let mut changed = false;

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
        &pins_this_pad_reports(snarl, node_id, live),
    );
    let mut text = tabs[active].text.clone();
    let line_h = ui.text_style_height(&egui::TextStyle::Monospace).max(10.0);
    // Whatever height is left under the two rows above, in whole lines.
    let used = ui.min_rect().height();
    let rows = (((size.y - used - SUMMARY_H) / line_h).floor() as i32).max(3) as usize;
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
        // the node body keeps the line-by-line reasons.
        if resizable {
            show_line_notes(ui, &compiled);
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
    use super::{unique_name, JsmTab};

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
