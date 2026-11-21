//! The layout editor's Inspector: what the selected object is, where it sits and how big it
//! is, and its options - in the canvas's own style: sections under headings, value pills,
//! colour pills, segmented choices and word switches, in bands that wrap to the width given.
//! A narrow panel (Easy mode's) stacks the sections; a wide row (an overlay's toolbar, a
//! sub-patch node's header) lays them side by side in columns.

use super::*;
use crate::canvas::node::{ExposedModule, Gradient, GradientKind};

/// How wide one column of the Inspector is where there's room for several side by side.
const COLUMN_W: f32 = 270.0;
/// Room between two columns.
const COLUMN_GAP: f32 = 14.0;

/// Where the Inspector's sections go: a column `w` wide, or - `compact` - inline in a row
/// that wraps, each control as narrow as it reads, for a toolbar that has to stay slim.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Col {
    pub w: f32,
    pub compact: bool,
}

/// How tall a compact Inspector's controls are.
const COMPACT_H: f32 = 22.0;
/// The room between a compact Inspector's controls.
const COMPACT_GAP: f32 = 3.0;
/// What a rule between two compact sections takes, its spacing with it.
const COMPACT_RULE: f32 = 6.0 + COMPACT_GAP;

/// What a compact Inspector's sections do this time through: draw, measure how wide each would
/// be in a line (drawing nothing), or draw only the one its tabs show.
#[derive(Default)]
enum Pass {
    #[default]
    Draw,
    Measure(Vec<(String, f32)>),
    Tab { show: usize, at: usize },
}

thread_local! {
    static PASS: std::cell::RefCell<Pass> = std::cell::RefCell::new(Pass::Draw);
}

/// One section's controls: a column `w` wide, its rows laid out in bands at the cursor - or,
/// compact, each control inline at the cursor.
pub(crate) struct Insp<'u> {
    pub ui: &'u mut egui::Ui,
    pub w: f32,
    compact: bool,
    /// Keys the pills' drag and typing state: one per object.
    node: NodeId,
    salt: egui::Id,
    /// Measuring (compact): how wide the controls so far are, nothing drawn.
    measure: Option<f32>,
}

impl<'u> Insp<'u> {
    fn new(ui: &'u mut egui::Ui, col: Col, salt: egui::Id) -> Self {
        Self { ui, w: col.w, compact: col.compact, node: NodeId(salt.value() as usize), salt, measure: None }
    }

    /// Measuring: count `w` more and say so - the caller draws nothing then.
    pub fn measured(&mut self, w: f32) -> bool {
        match self.measure.as_mut() {
            Some(acc) => {
                *acc += w.ceil() + COMPACT_GAP;
                true
            }
            None => false,
        }
    }

    /// The rects of a band (or several, as the width needs) of groups `mins` wide at 1×, each
    /// band stretched across the whole column, so every row ends on the same edge.
    pub fn row(&mut self, mins: &[f32]) -> Vec<egui::Rect> {
        if self.compact {
            // Inline: nothing yet. Each control takes the room its own words need as it's
            // drawn (`fit`) - a share of a column's width cut words short ("col...") and
            // drew values smaller, and a control left out (a colour's x while there are
            // only two) left a hole.
            return vec![egui::Rect::NOTHING; mins.len()];
        }
        let band = curve_rows::band_h(self.ui);
        let ctrl = band - 4.0;
        let gap = 3.0 * kit::GAP;
        let (mut cells, h) = curve_rows::bands(self.w, mins, band, gap);
        // Each band's cells as shares of what they take, spread to the column's width.
        let mut tops: Vec<f32> = cells.iter().map(|c| c.top()).collect();
        tops.dedup_by(|a, b| (*a - *b).abs() < 0.5);
        for top in tops {
            let row: Vec<usize> = (0..cells.len()).filter(|&i| (cells[i].top() - top).abs() < 0.5).collect();
            let used: f32 = row.iter().map(|&i| cells[i].width()).sum();
            let room = self.w - gap * (row.len() as f32 - 1.0);
            let mut x = 0.0;
            for &i in &row {
                let w = cells[i].width() * room / used.max(1.0);
                cells[i] = egui::Rect::from_min_size(egui::pos2(x, top), egui::vec2(w, band));
                x += w + gap;
            }
        }
        let (area, _) = self.ui.allocate_exact_size(egui::vec2(self.w, h), egui::Sense::hover());
        cells
            .into_iter()
            .map(|c| {
                let c = c.translate(area.min.to_vec2());
                egui::Rect::from_min_size(egui::pos2(c.left(), c.center().y - ctrl * 0.5), egui::vec2(c.width(), ctrl))
            })
            .collect()
    }

    /// Where a control goes: `r`, in a column; compact, a cell `w` wide at the cursor - or
    /// nowhere while measuring, where `w` is counted instead.
    fn fit(&mut self, r: egui::Rect, w: f32) -> Option<egui::Rect> {
        if !self.compact {
            return Some(r);
        }
        if self.measured(w) {
            return None;
        }
        Some(self.ui.allocate_exact_size(egui::vec2(w.ceil(), COMPACT_H), egui::Sense::hover()).0)
    }

    /// How wide `text` is in the control font.
    fn word(&self, text: &str) -> f32 {
        let font = simple_bodies::control_font();
        self.ui.painter().layout_no_wrap(text.to_string(), font, egui::Color32::PLACEHOLDER).size().x
    }

    /// A value pill for a real number. Compact, as wide as its word and its value need - the
    /// value given room for four figures, so the row doesn't shift as it's dragged.
    #[allow(clippy::too_many_arguments)]
    pub fn number(&mut self, r: egui::Rect, key: &str, label: &str, value: &mut f32, shown: &str, range: std::ops::RangeInclusive<f32>, per_px: f32, hover: &str) -> bool {
        let short = simple_bodies::pill_label_parts(label).0;
        let label_w = if short.is_empty() { 0.0 } else { self.word(short) + 12.0 };
        let Some(r) = self.fit(r, label_w + self.word(shown).max(self.word("8888")) + 16.0) else { return false };
        curve_rows::real_pill(self.ui, self.node, key, r, label, value, shown, range, per_px, hover)
    }

    /// [`Self::colour`] whose word a compact Inspector leaves off, where the section's caption
    /// already says what it colours: the swatch alone, its hover still saying.
    pub fn swatch(&mut self, r: egui::Rect, key: &str, label: &str, rgba: &mut [u8; 4], hover: &str) -> bool {
        let label = if self.compact { "" } else { label };
        self.colour(r, key, label, rgba, hover)
    }

    /// A pill saying what it colours, with its colour's swatch at its right end: click the
    /// swatch for the picker (right-click it to copy or paste a colour).
    pub fn colour(&mut self, r: egui::Rect, key: &str, label: &str, rgba: &mut [u8; 4], hover: &str) -> bool {
        let swatch = COMPACT_H * 1.15 + 6.0;
        let Some(r) = self.fit(r, if label.is_empty() { swatch } else { self.word(label) + 14.0 + swatch }) else { return false };
        let ui = &mut *self.ui;
        let fill = ui.visuals().widgets.inactive.weak_bg_fill;
        let text = ui.visuals().text_color();
        let edge = ui.visuals().weak_text_color();
        ui.painter().rect_filled(r, 4.0, fill);
        // A swatch with no word beside it fills its pill.
        let swatch_w = if label.is_empty() { (r.width() - 6.0).max(4.0) } else { (r.height() * 1.15).min(r.width() * 0.5) };
        let sr = egui::Rect::from_min_size(
            egui::pos2(r.right() - swatch_w - 3.0, r.top() + 3.0),
            egui::vec2(swatch_w, (r.height() - 6.0).max(4.0)),
        );
        let font = simple_bodies::control_font();
        if !label.is_empty() {
            let shown = simple_bodies::elide_to_width(ui, label, font.clone(), text, sr.left() - r.left() - 10.0);
            ui.painter().text(egui::pos2(r.left() + 7.0, r.center().y), egui::Align2::LEFT_CENTER, shown, font, text);
        }
        let mut cell = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(sr)
                .layout(egui::Layout::left_to_right(egui::Align::Center))
                .id_salt((self.salt, key)),
        );
        cell.spacing_mut().interact_size = sr.size();
        let changed = crate::widgets::fxi_color_swatch(&mut cell, rgba, hover, true);
        ui.painter().rect_stroke(sr, 2.0, egui::Stroke::new(1.0_f32, edge), egui::StrokeKind::Outside);
        ui.interact(egui::Rect::from_min_max(r.min, egui::pos2(sr.left(), r.bottom())), self.salt.with((key, "hover")), egui::Sense::hover())
            .on_hover_text(hover);
        changed
    }

    /// One choice out of a few, as one pill cut into segments.
    pub fn choice<T: PartialEq + Clone>(&mut self, r: egui::Rect, key: &str, value: &T, options: &[(T, &str, &str)]) -> Option<T> {
        // Compact, every segment as wide as the widest word: the segments share the pill evenly.
        let widest = options.iter().map(|(_, w, _)| self.word(w)).fold(0.0, f32::max);
        let r = self.fit(r, options.len() as f32 * (widest + 16.0))?;
        curve_rows::segmented(self.ui, r, value, options, (self.salt, key))
    }

    /// A choice that's a list: a pill with its word, then the choice, the list dropping down.
    pub fn combo<T: PartialEq + Clone>(&mut self, r: egui::Rect, key: &str, label: &str, value: &T, options: &[(T, &str)], hover: &str) -> Option<T> {
        let short = simple_bodies::pill_label_parts(label).0;
        let label_w = if short.is_empty() { 0.0 } else { self.word(short) + 12.0 };
        let widest = options.iter().map(|(_, w)| self.word(w)).fold(0.0, f32::max);
        let r = self.fit(r, label_w + widest + 34.0)?;
        curve_rows::pill_combo(self.ui, r, label, value, options, hover, (self.salt, key))
    }


    /// A button in the house style, filling `r`.
    pub fn button(&mut self, r: egui::Rect, word: &str, hover: &str) -> bool {
        let Some(r) = self.fit(r, (self.word(word) + 16.0).max(COMPACT_H)) else { return false };
        let font = simple_bodies::control_font();
        self.ui
            .put(r, remapper_bodies::action_button_font(word, r.width(), font).min_size(r.size()))
            .on_hover_text(hover)
            .clicked()
    }

    /// A line of text, the column's width.
    pub fn text(&mut self, text: &mut String, hint: &str) -> bool {
        let w = if self.compact { 120.0 } else { self.w };
        if self.measured(w) {
            return false;
        }
        self.ui
            .add(egui::TextEdit::singleline(text).desired_width(w).hint_text(hint).font(simple_bodies::control_font()))
            .changed()
    }

    /// A short note in the hint colour, wrapping to the column.
    pub fn note(&mut self, text: &str) {
        if self.compact {
            let font = egui::TextStyle::Small.resolve(self.ui.style());
            let w = self.ui.painter().layout_no_wrap(text.to_string(), font, egui::Color32::PLACEHOLDER).size().x;
            if !self.measured(w) {
                self.ui.label(egui::RichText::new(text).small().weak());
            }
            return;
        }
        let w = self.w;
        self.ui.scope(|ui| {
            ui.set_max_width(w);
            ui.label(egui::RichText::new(text).small().weak());
        });
    }
}

/// A section heading in the modules' own look: its title, then a hairline to the column's
/// edge.
fn heading(ui: &mut egui::Ui, w: f32, title: &str) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 22.0), egui::Sense::hover());
    let v = ui.visuals();
    let col = v.strong_text_color();
    let font = egui::TextStyle::Small.resolve(ui.style());
    let g = ui.painter().layout_no_wrap(title.to_string(), font, col);
    let text_w = g.size().x;
    ui.painter().galley(egui::pos2(rect.left(), rect.center().y - g.size().y * 0.5), g, col);
    let line_x = rect.left() + text_w + 6.0;
    if line_x < rect.right() {
        ui.painter().line_segment(
            [egui::pos2(line_x, rect.center().y), egui::pos2(rect.right(), rect.center().y)],
            egui::Stroke::new(1.0_f32, v.widgets.noninteractive.bg_stroke.color),
        );
    }
}

/// Lay out one section: a column `w` wide - beside the last where the Inspector is wide, under
/// it where it's narrow - its heading, then what `body` puts in it.
fn section(ui: &mut egui::Ui, col: Col, salt: egui::Id, title: &str, body: impl FnOnce(&mut Insp<'_>)) {
    if col.compact {
        let font = egui::TextStyle::Small.resolve(ui.style());
        let col_text = ui.visuals().weak_text_color();
        let g = ui.painter().layout_no_wrap(title.to_string(), font, col_text);
        // What this pass does with it (`Pass`).
        let (measure, shown) = PASS.with(|p| match &mut *p.borrow_mut() {
            Pass::Draw => (false, true),
            Pass::Measure(_) => (true, false),
            Pass::Tab { show, at } => {
                let mine = *at == *show;
                *at += 1;
                (false, mine)
            }
        });
        if measure {
            let mut insp = Insp::new(ui, col, salt.with(title));
            insp.measure = Some(0.0);
            body(&mut insp);
            let w = COMPACT_RULE + g.size().x + COMPACT_GAP + insp.measure.unwrap_or(0.0);
            PASS.with(|p| {
                if let Pass::Measure(v) = &mut *p.borrow_mut() {
                    v.push((title.to_string(), w));
                }
            });
            return;
        }
        if !shown {
            return;
        }
        let tabbed = PASS.with(|p| matches!(*p.borrow(), Pass::Tab { .. }));
        if !tabbed {
            // Inline: a rule before it - unless it starts a line - its title as a caption (on
            // the controls' middle, not their top), then its controls. Tabbed, its tab names it.
            if ui.cursor().min.x > ui.max_rect().min.x + 1.0 {
                ui.separator();
            }
            let (r, _) = ui.allocate_exact_size(egui::vec2(g.size().x, COMPACT_H), egui::Sense::hover());
            ui.painter().galley(egui::pos2(r.left(), r.center().y - g.size().y * 0.5), g, col_text);
        }
        body(&mut Insp::new(ui, col, salt.with(title)));
        return;
    }
    let w = col.w;
    ui.allocate_ui_with_layout(egui::vec2(w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.set_width(w);
        ui.spacing_mut().item_spacing.y = 2.0;
        heading(ui, w, title);
        body(&mut Insp::new(ui, col, salt.with(title)));
        ui.add_space(6.0);
    });
}

/// The fold chevron at the start of a compact Inspector: pointing down while it's open, right
/// while it's folded. Returns whether it was clicked.
fn fold_chevron(ui: &mut egui::Ui, open: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(16.0, COMPACT_H), egui::Sense::click());
    let v = ui.visuals();
    let col = if resp.hovered() { v.strong_text_color() } else { v.text_color() };
    let c = rect.center();
    let pts = if open {
        vec![c + egui::vec2(-5.0, -2.5), c + egui::vec2(5.0, -2.5), c + egui::vec2(0.0, 3.5)]
    } else {
        vec![c + egui::vec2(-2.5, -5.0), c + egui::vec2(3.5, 0.0), c + egui::vec2(-2.5, 5.0)]
    };
    ui.painter().add(egui::Shape::convex_polygon(pts, col, egui::Stroke::NONE));
    let hover = if open {
        "Fold the settings away, so the object can be moved without them in the way"
    } else {
        "Show the object's settings"
    };
    resp.on_hover_text(hover).clicked()
}

/// How wide a compact Inspector's tab for `title` is.
fn tab_w(ui: &egui::Ui, title: &str) -> f32 {
    let font = simple_bodies::control_font();
    ui.painter().layout_no_wrap(title.to_string(), font, egui::Color32::PLACEHOLDER).size().x + 14.0
}

/// A compact Inspector's tabs, one per section: the shown one's word lit and underlined, the
/// others dim. Returns the one clicked.
fn section_tabs(ui: &mut egui::Ui, titles: &[&str], shown: usize) -> Option<usize> {
    let mut picked = None;
    let font = simple_bodies::control_font();
    for (i, title) in titles.iter().enumerate() {
        let (r, resp) = ui.allocate_exact_size(egui::vec2(tab_w(ui, title), COMPACT_H), egui::Sense::click());
        let v = ui.visuals();
        let on = i == shown;
        let col = if on {
            v.strong_text_color()
        } else if resp.hovered() {
            v.text_color()
        } else {
            v.weak_text_color()
        };
        if resp.hovered() && !on {
            ui.painter().rect_filled(r, 3.0, v.widgets.hovered.weak_bg_fill);
        }
        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, *title, font.clone(), col);
        if on {
            let y = r.bottom() - 1.0;
            ui.painter().line_segment([egui::pos2(r.left() + 4.0, y), egui::pos2(r.right() - 4.0, y)], egui::Stroke::new(2.0_f32, v.selection.bg_fill));
        }
        if resp.on_hover_text(format!("The {} settings", title.to_lowercase())).clicked() {
            picked = Some(i);
        }
    }
    picked
}

/// The Inspector for the selected object of a layout: what it is (`title`, else its kind),
/// where it sits and how big it is, how a redesigned row is drawn, and its own options - one
/// section each. With several selected, a change made to the first goes to the rest where
/// they have the same setting.
///
/// `compact` - with the room a line has - lays it out as one slim line, for an overlay's
/// toolbar or a sub-patch node's header: a chevron that folds it away, then the sections in a
/// line; or, when they'd take more than the room, a tab for each and the shown one's controls.
/// Otherwise in a column, or columns side by side where the width holds them.
pub(crate) fn layout_inspector(
    ui: &mut egui::Ui,
    state: &mut LayoutStateMut<'_>,
    sel_module: SelectedModuleInfo,
    title: Option<&str>,
    compact: Option<f32>,
) {
    let Some(idx) = (*state.selected_item).filter(|&i| i < state.items.len()) else {
        *state.selected_item = None;
        return;
    };
    let inner_mid = sel_module.as_ref().map(|(mid, _)| mid.clone());
    let channels = sel_module.as_ref().map_or(1, |(_, ch)| *ch);
    let multi: Vec<usize> = state.selected_items.clone();
    let before = (multi.len() > 1).then(|| item_style_of(&state.items[idx]));

    let avail = ui.available_width();
    let wide = avail >= 2.0 * COLUMN_W + COLUMN_GAP;
    let col_w = Col { w: if wide { COLUMN_W } else { avail.max(120.0) }, compact: compact.is_some() };
    let salt = egui::Id::new(("layout_inspector", idx));
    let kind = match &state.items[idx] {
        LayoutItem::Deco(d) => d.type_label().to_string(),
        LayoutItem::Module(m) => m.element_id.replace('_', " "),
    };
    let title = title.map_or(kind, str::to_string);
    let kind_title = title.clone();

    let tab_names: Vec<String> = state.layout_tabs.clone();
    // The tab picked for the object, which goes to the rest of a selection too.
    let mut moved_to: Option<Option<u8>> = None;
    let mut draw = |ui: &mut egui::Ui| {
        let item = &mut state.items[idx];
        section(ui, col_w, salt, &title, |insp| {
            if multi.len() > 1 {
                insp.note(&format!("{} selected: a change here goes to all of them.", multi.len()));
            }
            place_rows(insp, item);
            let n = tab_names.len();
            if n >= 2 {
                let mut options: Vec<(Option<u8>, &str)> = tab_names.iter().enumerate().map(|(i, t)| (Some(i as u8), t.as_str())).collect();
                options.push((None, "all tabs"));
                let r = insp.row(&[200.0])[0];
                let cur = item.tab().map(|t| t.min(n as u8 - 1));
                if let Some(v) = insp.combo(r, "tab", "tab", &cur, &options, "Which of the layout's tabs this is on - or all of them") {
                    item.set_tab(v);
                    moved_to = Some(v);
                }
            }
        });
        let LayoutItem::Module(m) = item else {
            let LayoutItem::Deco(d) = item else { return };
            decoration_sections(ui, col_w, salt, d, idx);
            return;
        };
        let Some(mid) = inner_mid.as_deref() else { return };
        if m.element_id == "whole_module" && module_widget_zooms(mid) {
            section(ui, col_w, salt, "Widget", |insp| {
                let r = insp.row(&[160.0])[0];
                let mut z = m.zoom.unwrap_or(1.0);
                let shown = format!("{:.0}%", z * 100.0);
                let range = crate::canvas::node::WIDGET_ZOOM;
                if insp.number(r, "zoom", "zoom", &mut z, &shown, range, 0.002,
                    "How large the module is drawn in the widget: under 100% its buttons and values come out \
                     smaller and the rest - a graph - takes the room; over 100% everything is larger")
                {
                    m.zoom = ((z - 1.0).abs() > 0.005).then_some(z);
                }
            });
        }
        if pin_style2::supported(mid, &m.element_id) {
            section(ui, col_w, salt, "Look", |insp| {
                let cur = m.pin_style.unwrap_or(1);
                let r = insp.row(&[200.0])[0];
                if let Some(v) = insp.choice(r, "style", &cur, &[
                    (1u8, "style 1", "The row as it was before the module redesign"),
                    (2u8, "style 2", "The module's own look: value pills and joined switches"),
                ]) {
                    m.pin_style = Some(v);
                }
            });
        }
        match mid {
            "module.label" => text_pin_section(ui, col_w, salt, m),
            "module.switch" => switch_pin_sections(ui, col_w, salt, m),
            "module.input_viewer" => board_pin_section(ui, col_w, salt, m),
            "module.touch_zones" | "module.menu" => zone_pad_section(ui, col_w, salt, m),
            "display.controller3d" => controller3d_pin_sections(ui, col_w, salt, m),
            "processing.gyro_3dof" if m.element_id == "preview" => gyro_preview_sections(ui, col_w, salt, m),
            "module.response_curve"
            | "module.vec_response_curve"
            | "module.twoway_response_curve"
            | "module.automap_response_curve"
            | "module.automap_vec_response_curve"
            | "module.automap_twoway_response_curve" => graph_pin_sections(ui, col_w, salt, m, channels, 2),
            "generator.envelope" => graph_pin_sections(ui, col_w, salt, m, channels, 3),
            "display.oscilloscope" | "display.vectorscope" | "display.trigscope" | "module.audio_stream_haptics" | "module.jsm" => {
                graph_pin_sections(ui, col_w, salt, m, channels, 0)
            }
            _ => {}
        }
    };
    if let Some(room) = compact {
        // How wide each section would be in a line, before anything is drawn.
        PASS.with(|p| *p.borrow_mut() = Pass::Measure(Vec::new()));
        draw(ui);
        let widths = PASS.with(|p| match std::mem::take(&mut *p.borrow_mut()) {
            Pass::Measure(v) => v,
            _ => Vec::new(),
        });
        let folded_id = egui::Id::new("layout_inspector_folded");
        let tab_id = egui::Id::new("layout_inspector_tab");
        let folded: bool = ui.ctx().data(|d| d.get_temp(folded_id)).unwrap_or(false);
        let line: f32 = widths.iter().map(|(_, w)| w).sum();
        let tabbed = widths.len() > 1 && 16.0 + line > room;
        let titles: Vec<&str> = widths.iter().map(|(t, _)| t.as_str()).collect();
        let wanted: Option<String> = ui.ctx().data(|d| d.get_temp(tab_id));
        let shown = wanted.and_then(|t| titles.iter().position(|x| *x == t)).unwrap_or(0);
        // As wide as it needs, up to the room: the toolbar an overlay's Inspector sits in lays
        // its content out at last frame's size, and wouldn't grow for a line that wraps.
        let need = 16.0 + COMPACT_GAP + if folded {
            ui.painter().layout_no_wrap(kind_title.clone(), egui::TextStyle::Small.resolve(ui.style()), egui::Color32::PLACEHOLDER).size().x
        } else if tabbed {
            titles.iter().map(|t| tab_w(ui, t) + COMPACT_GAP).sum::<f32>() + widths.get(shown).map_or(0.0, |(_, w)| *w)
        } else {
            line
        };
        ui.set_min_width(need.min(room).max(0.0));
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(COMPACT_GAP, COMPACT_GAP);
            if fold_chevron(ui, !folded) {
                ui.ctx().data_mut(|d| d.insert_temp(folded_id, !folded));
            }
            if folded {
                // Folded: what the object is, nothing more.
                let font = egui::TextStyle::Small.resolve(ui.style());
                let col_text = ui.visuals().weak_text_color();
                let g = ui.painter().layout_no_wrap(kind_title.clone(), font, col_text);
                let (r, _) = ui.allocate_exact_size(egui::vec2(g.size().x, COMPACT_H), egui::Sense::hover());
                ui.painter().galley(egui::pos2(r.left(), r.center().y - g.size().y * 0.5), g, col_text);
                return;
            }
            if !tabbed {
                draw(ui);
                return;
            }
            if let Some(i) = section_tabs(ui, &titles, shown) {
                ui.ctx().data_mut(|d| d.insert_temp(tab_id, titles[i].to_string()));
            }
            ui.separator();
            PASS.with(|p| *p.borrow_mut() = Pass::Tab { show: shown, at: 0 });
            draw(ui);
            PASS.with(|p| *p.borrow_mut() = Pass::Draw);
        });
    } else if wide {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = COLUMN_GAP;
            draw(ui);
        });
    } else {
        ui.vertical(|ui| draw(ui));
    }

    if let Some(t) = moved_to {
        for &j in &multi {
            if let Some(it) = state.items.get_mut(j) {
                it.set_tab(t);
            }
        }
    }
    if let Some(before) = before {
        let after = item_style_of(&state.items[idx]);
        let changed = before.diff(&after);
        if changed.any() {
            for &j in multi.iter().filter(|&&j| j != idx) {
                if let Some(it) = state.items.get_mut(j) {
                    changed.apply_to(it);
                }
            }
        }
    }
}

/// Whether a module pinned whole is drawn as its zoomable widget (`pinned::render_module_widget`)
/// rather than a card view of its own (the mapping modules, the network ones).
fn module_widget_zooms(mid: &str) -> bool {
    !matches!(
        mid,
        "module.remapper" | "module.map_action" | "module.automap_combiner" | "module.network_send" | "module.network_recv"
    )
}

/// Where the object sits and how big it is: x and y, w and h - a line's two ends instead.
fn place_rows(insp: &mut Insp<'_>, item: &mut LayoutItem) {
    let pair = |insp: &mut Insp<'_>, keys: [&str; 2], v: &mut [f32; 2], min: f32, hover: [&str; 2]| {
        let rs = insp.row(&[110.0, 110.0]);
        for i in 0..2 {
            let shown = format!("{:.0}", v[i]);
            insp.number(rs[i], keys[i], keys[i], &mut v[i], &shown, min..=100_000.0, 1.0, hover[i]);
        }
    };
    match item {
        LayoutItem::Deco(LayoutDecoration::Line { a, b, .. }) => {
            pair(insp, ["x1", "y1"], a, -100_000.0, ["Where the line starts, across", "Where the line starts, down"]);
            pair(insp, ["x2", "y2"], b, -100_000.0, ["Where the line ends, across", "Where the line ends, down"]);
        }
        LayoutItem::Deco(d) => {
            let (pos, size) = match d {
                LayoutDecoration::Text { pos, size, .. }
                | LayoutDecoration::Svg { pos, size, .. }
                | LayoutDecoration::Rect { pos, size, .. }
                | LayoutDecoration::Ellipse { pos, size, .. } => (pos, size),
                LayoutDecoration::Line { .. } => return,
            };
            pair(insp, ["x", "y"], pos, -100_000.0, ["How far from the left", "How far from the top"]);
            pair(insp, ["w", "h"], size, 1.0, ["How wide", "How tall"]);
        }
        LayoutItem::Module(m) => {
            pair(insp, ["x", "y"], &mut m.pos, -100_000.0, ["How far from the left", "How far from the top"]);
            pair(insp, ["w", "h"], &mut m.size, 1.0, ["How wide", "How tall"]);
        }
    }
}

/// A decoration's own sections: a text's words and type, a shape's fill and stroke, an SVG's
/// picture, tint and frame, a line's stroke.
fn decoration_sections(ui: &mut egui::Ui, w: Col, salt: egui::Id, d: &mut LayoutDecoration, idx: usize) {
    match d {
        LayoutDecoration::Text { text, font_size, fill, outline, outline_px, align, valign, .. } => {
            section(ui, w, salt, "Text", |insp| {
                insp.text(text, "text");
                let rs = insp.row(&[100.0, 130.0]);
                let shown = format!("{font_size:.0} px");
                insp.number(rs[0], "size", "size", font_size, &shown, 6.0..=96.0, 0.25, "How big the type is");
                if let Some(v) = insp.choice(rs[1], "align", align, &[
                    (TextAlign::Left, "left", "From the left edge"),
                    (TextAlign::Center, "mid", "In the middle"),
                    (TextAlign::Right, "right", "From the right edge"),
                ]) {
                    *align = v;
                }
                use crate::canvas::node::TextVAlign;
                let r = insp.row(&[180.0])[0];
                if let Some(v) = insp.choice(r, "valign", valign, &[
                    (TextVAlign::Top, "top", "Along the top"),
                    (TextVAlign::Center, "middle", "In the middle, down"),
                    (TextVAlign::Bottom, "bottom", "Along the bottom"),
                ]) {
                    *valign = v;
                }
                let rs = insp.row(&[110.0, 110.0]);
                insp.colour(rs[0], "fill", "colour", fill, "The text's colour");
                insp.colour(rs[1], "outline", "outline", outline, "The colour round each letter");
                let r = insp.row(&[130.0])[0];
                let shown = format!("{outline_px:.1} px");
                insp.number(r, "outline_px", "outline", outline_px, &shown, 0.0..=8.0, 0.02, "How thick the outline round the letters is");
            });
        }
        LayoutDecoration::Rect { fill, fill_gradient, stroke, stroke_px, corner_radius, .. } => {
            section(ui, w, salt, "Fill", |insp| fill_rows(insp, "fill", fill, fill_gradient));
            section(ui, w, salt, "Stroke", |insp| {
                stroke_rows(insp, stroke, stroke_px);
                let r = insp.row(&[130.0])[0];
                let shown = format!("{corner_radius:.0}");
                insp.number(r, "corner", "corners", corner_radius, &shown, 0.0..=64.0, 0.1, "How round the corners are");
            });
        }
        LayoutDecoration::Ellipse { fill, fill_gradient, stroke, stroke_px, .. } => {
            section(ui, w, salt, "Fill", |insp| fill_rows(insp, "fill", fill, fill_gradient));
            section(ui, w, salt, "Stroke", |insp| stroke_rows(insp, stroke, stroke_px));
        }
        LayoutDecoration::Line { stroke, stroke_px, .. } => {
            section(ui, w, salt, "Line", |insp| stroke_rows(insp, stroke, stroke_px));
        }
        LayoutDecoration::Svg { tint, tint_mode, tint_gradient, stroke, stroke_px, svg_data, rev, icon_key, .. } => {
            section(ui, w, salt, "Picture", |insp| {
                // The shared set, the pad's own glyphs, or an SVG of your own.
                if insp.measured(160.0) {
                    return;
                }
                insp.ui.scope(|ui| {
                    ui.set_max_width(w.w);
                    if let Some((k, s)) = crate::canvas::menu_body::icon_picker_button(
                        ui, egui::Id::new(("deco_svg_icon", idx)), icon_key, svg_data)
                    {
                        *icon_key = k;
                        *svg_data = s;
                        *rev = rev.wrapping_add(1);
                    }
                });
            });
            section(ui, w, salt, "Tint", |insp| {
                let r = insp.row(&[180.0])[0];
                let cur = tint_mode.clone();
                if let Some(v) = insp.choice(r, "tint_mode", &cur, &[
                    ("override".to_string(), "override", "The picture takes the tint's colour, as much as its alpha"),
                    ("additive".to_string(), "additive", "The tint's colour is added to the picture's own"),
                ]) {
                    *tint_mode = v;
                }
                fill_rows(insp, "tint", tint, tint_gradient);
            });
            section(ui, w, salt, "Frame", |insp| stroke_rows(insp, stroke, stroke_px));
        }
    }
}

/// A stroke's colour and width.
fn stroke_rows(insp: &mut Insp<'_>, stroke: &mut [u8; 4], stroke_px: &mut f32) {
    let rs = insp.row(&[110.0, 110.0]);
    insp.swatch(rs[0], "stroke", "colour", stroke, "The stroke's colour");
    let shown = format!("{stroke_px:.1} px");
    insp.number(rs[1], "stroke_px", "width", stroke_px, &shown, 0.0..=12.0, 0.02, "How thick the stroke is (0 for none)");
}

/// A fill or a tint: one colour, or a gradient - linear or radial - with its colours, each
/// where it sits along the line (one added in the widest gap, one taken off while there are
/// more than two), and where the middle of them falls.
fn fill_rows(insp: &mut Insp<'_>, word: &str, solid: &mut [u8; 4], gradient: &mut Option<Gradient>) {
    let cur = gradient.as_ref().map(|g| g.kind);
    let r = insp.row(&[200.0])[0];
    if let Some(kind) = insp.choice(r, "kind", &cur, &[
        (None, "solid", "One colour"),
        (Some(GradientKind::Linear), "linear", "Colours along a line: drag its ends on the object"),
        (Some(GradientKind::Radial), "radial", "Colours out from a centre: drag the centre and the edge on the object"),
    ]) {
        *gradient = match kind {
            None => {
                if let Some(first) = gradient.as_ref().and_then(|g| g.stops.first()) {
                    *solid = first.color;
                }
                None
            }
            Some(kind) => Some(match gradient.take() {
                Some(mut g) => {
                    (g.start, g.end) = Gradient::default_line(kind);
                    g.kind = kind;
                    g
                }
                None => Gradient::new(kind, *solid, lighter(*solid)),
            }),
        };
    }
    let Some(g) = gradient else {
        let r = insp.row(&[150.0])[0];
        insp.swatch(r, word, word, solid, &format!("The {word}'s colour"));
        return;
    };
    let n = g.stops.len();
    let mut remove = None;
    let mut moved = false;
    for (i, stop) in g.stops.iter_mut().enumerate() {
        let rs = insp.row(&[110.0, 80.0, 30.0]);
        insp.swatch(rs[0], &format!("stop{i}"), &format!("colour {}", i + 1), &mut stop.color, "One of the gradient's colours");
        let mut pct = stop.at * 100.0;
        let shown = format!("{pct:.0}%");
        if insp.number(rs[1], &format!("at{i}"), "at", &mut pct, &shown, 0.0..=100.0, 0.25, "Where this colour sits along the line") {
            stop.at = pct / 100.0;
            moved = true;
        }
        if n > 2 && insp.button(rs[2], "×", "Take this colour off") {
            remove = Some(i);
        }
    }
    if let Some(i) = remove {
        g.stops.remove(i);
    }
    let rs = insp.row(&[110.0, 110.0]);
    if insp.button(rs[0], "+ colour", "Another colour, in the widest gap") {
        g.add_stop_in_widest_gap();
    }
    let mut pct = g.bias * 100.0;
    let shown = format!("{pct:.0}%");
    if insp.number(rs[1], "bias", "middle", &mut pct, &shown, 2.0..=98.0, 0.25, "Where along the line the middle of the colours falls: 50% for evenly") {
        g.bias = pct / 100.0;
    }
    if moved {
        g.sort_stops();
    }
}

/// A colour lightened toward white, for a gradient's second colour.
fn lighter(c: [u8; 4]) -> [u8; 4] {
    let up = |v: u8| (v as f32 + (255.0 - v as f32) * 0.6).round() as u8;
    [up(c[0]), up(c[1]), up(c[2]), c[3].max(200)]
}

/// A reset button the column's width, its hover saying what it puts back.
fn reset_row(insp: &mut Insp<'_>, hover: &str) -> bool {
    let r = insp.row(&[120.0])[0];
    insp.button(r, "Reset", hover)
}

/// A pinned Text module's colours for this pin alone.
fn text_pin_section(ui: &mut egui::Ui, w: Col, salt: egui::Id, m: &mut ExposedModule) {
    let mut ov = m.text_override.clone().unwrap_or_default();
    let mut clear = false;
    section(ui, w, salt, "Text colours", |insp| {
        let rs = insp.row(&[110.0, 110.0]);
        let mut fill = ov.fill.unwrap_or([220, 220, 220, 255]);
        if insp.colour(rs[0], "fill", "colour", &mut fill, "This pin's text colour") {
            ov.fill = Some(fill);
        }
        let mut outline = ov.outline.unwrap_or([0, 0, 0, 0]);
        if insp.colour(rs[1], "outline", "outline", &mut outline, "The colour round this pin's letters") {
            ov.outline = Some(outline);
        }
        let r = insp.row(&[130.0])[0];
        let mut px = ov.outline_px.unwrap_or(0.0);
        let shown = format!("{px:.1} px");
        if insp.number(r, "outline_px", "outline", &mut px, &shown, 0.0..=8.0, 0.02, "How thick the outline round the letters is") {
            ov.outline_px = Some(px);
        }
        clear = reset_row(insp, "The Text module's own colours");
    });
    if clear {
        m.text_override = None;
    } else if ov.fill.is_some() || ov.outline.is_some() || ov.outline_px.is_some() {
        m.text_override = Some(ov);
    }
}

/// A pinned Switch's colours, on and off, for this pin alone.
fn switch_pin_sections(ui: &mut egui::Ui, w: Col, salt: egui::Id, m: &mut ExposedModule) {
    use crate::canvas::node::PinSwitchOverride;
    let mut ov = m.switch_override.clone().unwrap_or_default();
    let mut clear = false;
    for (title, on) in [("Switch on", true), ("Switch off", false)] {
        section(ui, w, salt, title, |insp| {
            let (fill, outline, text, defaults) = if on {
                (&mut ov.fill_on, &mut ov.outline_on, &mut ov.text_on, [[80, 140, 200, 255], [180, 200, 220, 255], [255, 255, 255, 255]])
            } else {
                (&mut ov.fill_off, &mut ov.outline_off, &mut ov.text_off, [[60, 60, 65, 255], [100, 100, 110, 255], [200, 200, 200, 255]])
            };
            let rs = insp.row(&[110.0, 110.0, 110.0]);
            for (i, (slot, word)) in [(fill, "fill"), (outline, "outline"), (text, "text")].into_iter().enumerate() {
                let mut c = slot.unwrap_or(defaults[i]);
                if insp.colour(rs[i], word, word, &mut c, &format!("The switch's {word} while it is {}", if on { "on" } else { "off" })) {
                    *slot = Some(c);
                }
            }
        });
    }
    section(ui, w, salt, "Outline", |insp| {
        let r = insp.row(&[130.0])[0];
        let mut px = ov.outline_px.unwrap_or(1.0);
        let shown = format!("{px:.1} px");
        if insp.number(r, "outline_px", "width", &mut px, &shown, 0.0..=8.0, 0.02, "How thick the outline is, on and off") {
            ov.outline_px = Some(px);
        }
        clear = reset_row(insp, "The theme's colours");
    });
    if clear {
        m.switch_override = None;
    } else {
        let any = ov.fill_on.is_some() || ov.fill_off.is_some() || ov.outline_on.is_some() || ov.outline_off.is_some()
            || ov.text_on.is_some() || ov.text_off.is_some() || ov.outline_px.is_some();
        m.switch_override = if any { Some(PinSwitchOverride { ..ov }) } else { None };
    }
}

/// A pinned Input Viewer board's style for this pin alone.
fn board_pin_section(ui: &mut egui::Ui, w: Col, salt: egui::Id, m: &mut ExposedModule) {
    let mut o = m.iv_style_override.clone().unwrap_or_else(|| crate::canvas::input_viewer::IvStyle::default().to_override());
    let before = o.clone();
    let mut reset = false;
    section(ui, w, salt, "Board", |insp| {
        let rs = insp.row(&[110.0, 110.0]);
        insp.colour(rs[0], "bg", "plate", &mut o.bg, "The board's plate: lower the alpha for a see-through board over a game");
        insp.colour(rs[1], "accent", "pressed", &mut o.accent, "What's pressed: halos, a trigger's fill, the sticks' and touches' dots");
        let rs = insp.row(&[110.0, 110.0]);
        insp.colour(rs[0], "tint", "tint", &mut o.tint, "Multiplies the button art; it still brightens on a press");
        insp.colour(rs[1], "outline", "outline", &mut o.outline, "The board's outline colour");
        let r = insp.row(&[130.0])[0];
        let shown = format!("{:.1} px", o.outline_px);
        insp.number(r, "outline_px", "outline", &mut o.outline_px, &shown, 0.0..=6.0, 0.02, "How thick the board's outline is (0 for none)");
        reset = reset_row(insp, "The default board style");
    });
    if reset {
        m.iv_style_override = None;
    } else if o != before {
        m.iv_style_override = Some(o);
    }
}

/// A pinned Touch Zones or Virtual Menu field pad: its colours and when it shows, for this
/// pin alone.
fn zone_pad_section(ui: &mut egui::Ui, w: Col, salt: egui::Id, m: &mut ExposedModule) {
    use crate::canvas::node::{MenuStyleOverride, ZoneVisibility};
    let cleared = MenuStyleOverride { main: None, hi: None, visibility: ZoneVisibility::Always };
    let mut ov = m.menu_style_override.unwrap_or(cleared);
    let before = ov;
    let mut reset = false;
    section(ui, w, salt, "Zone pad", |insp| {
        let rs = insp.row(&[110.0, 110.0]);
        let mut main = ov.main.unwrap_or(crate::canvas::menu_body::MENU_MAIN_DEFAULT);
        if insp.colour(rs[0], "main", "main", &mut main, "This pad's main colour (alpha is the pad's opacity); unset, the module's own") {
            ov.main = Some(main);
        }
        let mut hi = ov.hi.unwrap_or(crate::canvas::menu_body::MENU_HIGHLIGHT_DEFAULT);
        if insp.colour(rs[1], "hi", "highlight", &mut hi, "This pad's highlight: the zone in use and what it offers; unset, the module's own") {
            ov.hi = Some(hi);
        }
        let r = insp.row(&[230.0])[0];
        if let Some(v) = insp.combo(r, "visibility", "shows", &ov.visibility, &[
            (ZoneVisibility::Always, "always"),
            (ZoneVisibility::OnTouch, "on touch"),
            (ZoneVisibility::TouchedZones, "touched zones"),
        ], "When the pad shows: always, only while touched, or only the zones touched (a radial menu as on touch)") {
            ov.visibility = v;
        }
        reset = reset_row(insp, "The module's colours, always shown");
    });
    if reset {
        m.menu_style_override = None;
    } else if ov != before || m.menu_style_override.is_some() {
        m.menu_style_override = if ov == cleared { None } else { Some(ov) };
    }
}

/// An override seeded for a pinned 3D picture: its frame's fill `bg`, the default accent,
/// tint and outline, and no display settings of its own yet.
fn picture_override(bg: [u8; 4]) -> crate::canvas::node::IvStyleOverride {
    crate::canvas::node::IvStyleOverride {
        bg,
        accent: C3D_DEFAULT_ACCENT,
        tint: [255, 255, 255, 255],
        outline: [70, 70, 75, 255],
        outline_px: 0.0,
        c3d_pitch: None,
        c3d_alpha: None,
        c3d_fade: None,
        c3d_composite: None,
        c3d_xray: None,
        c3d_materials: None,
        c3d_model: None,
        gyro_look: None,
    }
}

/// A pinned 3DOF preview's frame and how it draws the pad, for this pin alone.
fn gyro_preview_sections(ui: &mut egui::Ui, w: Col, salt: egui::Id, m: &mut ExposedModule) {
    let dark = ui.visuals().extreme_bg_color.to_srgba_unmultiplied();
    let mut ov = m.iv_style_override.clone().unwrap_or_else(|| picture_override(dark));
    let before = ov.clone();
    let mut reset = false;
    section(ui, w, salt, "Frame", |insp| {
        let rs = insp.row(&[110.0, 110.0]);
        insp.colour(rs[0], "bg", "fill", &mut ov.bg, "The frame's fill: lower the alpha for a see-through frame over a game");
        insp.colour(rs[1], "outline", "outline", &mut ov.outline, "The frame's outline colour");
        let r = insp.row(&[130.0])[0];
        let shown = format!("{:.1} px", ov.outline_px);
        insp.number(r, "outline_px", "outline", &mut ov.outline_px, &shown, 0.0..=6.0, 0.02, "How thick the frame's outline is (0 for none)");
    });
    section(ui, w, salt, "Pad", |insp| {
        let r = insp.row(&[240.0])[0];
        let cur = ov.gyro_look.clone();
        if let Some(v) = insp.choice(r, "look", &cur, &[
            (None, "module's", "As the module's own preview draws it"),
            (Some("icon".to_string()), "icon", "The pad's icon"),
            (Some("outline".to_string()), "outline", "The outline of the pad's icon"),
            (Some("model".to_string()), "3D", "The pad's 3D model, half see-through"),
        ]) {
            ov.gyro_look = v;
        }
        let r = insp.row(&[150.0])[0];
        let mut a = ov.c3d_composite.unwrap_or(0.5) * 100.0;
        let shown = format!("{a:.0}%");
        if insp.number(r, "opacity", "model", &mut a, &shown, 5.0..=100.0, 0.25, "How opaque the pad's 3D model is in this pin (until set, the module's own)") {
            ov.c3d_composite = Some(a / 100.0);
        }
        reset = reset_row(insp, "The pane's own fill, no outline, the pad as the module draws it");
    });
    if reset {
        m.iv_style_override = None;
    } else if ov != before || m.iv_style_override.is_some() {
        m.iv_style_override = Some(ov);
    }
}

/// A pinned 3D viewer's frame, how it's drawn, its model and its colours, for this pin alone.
fn controller3d_pin_sections(ui: &mut egui::Ui, w: Col, salt: egui::Id, m: &mut ExposedModule) {
    let mut ov = m.iv_style_override.clone().unwrap_or_else(|| picture_override([0, 0, 0, 0]));
    let before = ov.clone();
    let mut reset = false;
    let uid = m.inner_node_id;
    section(ui, w, salt, "Frame", |insp| {
        let rs = insp.row(&[110.0, 110.0]);
        insp.colour(rs[0], "bg", "fill", &mut ov.bg, "The frame's fill: lower the alpha for a see-through frame over a game");
        insp.colour(rs[1], "accent", "highlight", &mut ov.accent, "What's pressed: buttons, a trigger's pull, a stick's tilt, touches, x-ray ghosts");
        let rs = insp.row(&[110.0, 110.0]);
        insp.colour(rs[0], "outline", "outline", &mut ov.outline, "The frame's outline colour");
        let shown = format!("{:.1} px", ov.outline_px);
        insp.number(rs[1], "outline_px", "width", &mut ov.outline_px, &shown, 0.0..=6.0, 0.02, "How thick the frame's outline is (0 for none)");
    });
    section(ui, w, salt, "Display", |insp| {
        let rs = insp.row(&[110.0, 110.0]);
        let mut pitch = ov.c3d_pitch.unwrap_or(C3D_DEFAULT_PITCH_DEG);
        let shown = format!("{pitch:.0}°");
        if insp.number(rs[0], "pitch", "view", &mut pitch, &shown, 0.0..=85.0, 0.25, "How far above the pad this pin's camera looks from") {
            ov.c3d_pitch = Some(pitch);
        }
        let mut alpha = ov.c3d_alpha.unwrap_or(1.0) * 100.0;
        let shown = format!("{alpha:.0}%");
        if insp.number(rs[1], "alpha", "opacity", &mut alpha, &shown, 10.0..=100.0, 0.25, "How see-through the model is in this pin") {
            ov.c3d_alpha = Some(alpha / 100.0);
        }
        let rs = insp.row(&[110.0, 110.0]);
        let mut fade = ov.c3d_fade.unwrap_or(0.25);
        let shown = format!("{fade:.2} s");
        if insp.number(rs[0], "fade", "fade", &mut fade, &shown, 0.05..=2.0, 0.005, "How long a highlight takes to fade out in this pin") {
            ov.c3d_fade = Some(fade);
        }
        let mut comp = ov.c3d_composite.unwrap_or(1.0) * 100.0;
        let shown = format!("{comp:.0}%");
        if insp.number(rs[1], "composite", "fade all", &mut comp, &shown, 5.0..=100.0, 0.25, "Fades the whole picture of the pad as one image, without the see-through look") {
            ov.c3d_composite = Some(comp / 100.0);
        }
        let r = insp.row(&[150.0])[0];
        let mut xray = ov.c3d_xray.unwrap_or(crate::model::callback::GHOST_VIS_LOW) * 100.0;
        let shown = format!("{xray:.0}%");
        if insp.number(r, "xray", "x-ray", &mut xray, &shown, 0.0..=100.0, 0.25, "A pressed input shows through the shell once less than this much of it is in view") {
            ov.c3d_xray = Some(xray / 100.0);
        }
    });
    // The model and its colours: what the pinned renderer last drew (its effective values).
    if let Some(map) = ui.ctx().data_mut(|d| {
        d.remove_temp::<serde_json::Map<String, serde_json::Value>>(egui::Id::new(("c3d_pin_matload", uid)))
    }) {
        ov.c3d_materials = Some(map);
    }
    let published = ui.ctx().data(|d| d.get_temp::<(String, crate::model::Scheme)>(egui::Id::new(("c3d_pub", uid))));
    if let Some((model, mut cur)) = published {
        if let Some(mats) = ov.c3d_materials.as_ref() {
            c3d_apply_materials(&mut cur, mats);
        }
        section(ui, w, salt, "Model", |insp| {
            let mut options: Vec<(Option<String>, String)> = vec![(None, format!("{model} (module's)")), (Some(String::new()), "auto (device)".into())];
            options.extend(crate::model::available_models().into_iter().map(|m| (Some(m.clone()), m)));
            let shown: Vec<(Option<String>, &str)> = options.iter().map(|(k, l)| (k.clone(), l.as_str())).collect();
            let r = insp.row(&[230.0])[0];
            if let Some(v) = insp.combo(r, "model", "model", &ov.c3d_model, &shown, "Which model this pin draws: the module's, the device's own, or one chosen") {
                ov.c3d_model = v;
            }
        });
        section(ui, w, salt, "Colours", |insp| {
            for (row_label, groups) in crate::model::material::ROWS {
                let _ = row_label;
                let mins: Vec<f32> = groups.iter().map(|_| 110.0).collect();
                let rs = insp.row(&mins);
                for (&g, r) in groups.iter().zip(rs) {
                    let mut col = cur[g as usize];
                    if insp.colour(r, g.key(), g.label(), &mut col, g.label()) {
                        ov.c3d_materials.get_or_insert_with(Default::default)
                            .insert(g.key().to_string(), serde_json::json!([col[0], col[1], col[2], col[3]]));
                    }
                }
            }
            let rs = insp.row(&[80.0, 80.0, 80.0]);
            if insp.button(rs[0], "Reset", "Drop this pin's colours and follow the module's again") {
                ov.c3d_materials = None;
            }
            if insp.button(rs[1], "Save…", "Save this pin's colours to a .fxcol file") {
                controller3d_scheme_save(&cur);
            }
            if insp.button(rs[2], "Load…", "Load colours from a .fxcol file into this pin") {
                controller3d_scheme_load_async(insp.ui.ctx(), "c3d_pin_matload", uid);
            }
        });
    }
    section(ui, w, salt, "Defaults", |insp| {
        reset = reset_row(insp, "The default style and the module's own display settings");
    });
    if reset {
        m.iv_style_override = None;
    } else if ov != before || m.iv_style_override.is_some() {
        m.iv_style_override = Some(ov);
    }
}

/// A pinned graph's colours - its pane, grid and lines - and the switches its module keeps
/// in its header (`switches` of grid, labels and snap), for this copy alone.
fn graph_pin_sections(ui: &mut egui::Ui, w: Col, salt: egui::Id, m: &mut ExposedModule, channels: usize, switches: usize) {
    use crate::canvas::node::PinGraphOverride;
    let mut ov = m.graph_override.clone().unwrap_or_default();
    let mut clear = false;
    section(ui, w, salt, "Graph", |insp| {
        let rs = insp.row(&[110.0, 110.0]);
        let mut bg = ov.background.unwrap_or([16, 16, 16, 153]);
        if insp.colour(rs[0], "bg", "pane", &mut bg, "The graph's pane: lower the alpha for a see-through graph over a game") {
            ov.background = Some(bg);
        }
        let mut grid = ov.gridline.unwrap_or([180, 180, 180, 160]);
        if insp.colour(rs[1], "grid", "grid", &mut grid, "The grid's lines and the axes") {
            ov.gridline = Some(grid);
        }
        let rs = insp.row(&[110.0, 110.0]);
        let mut outline = ov.outline.unwrap_or([180, 180, 180, 255]);
        if insp.colour(rs[0], "outline", "outline", &mut outline, "The pane's outline colour") {
            ov.outline = Some(outline);
        }
        let mut px = ov.outline_px.unwrap_or(0.0);
        let shown = format!("{px:.1} px");
        if insp.number(rs[1], "outline_px", "width", &mut px, &shown, 0.0..=8.0, 0.02, "How thick the pane's outline is (0 for none)") {
            ov.outline_px = Some(px);
        }
        // The module's header switches as this copy's own: `module` follows the module on
        // the canvas, `on` and `off` set it for this copy alone.
        for (name, hover, field) in [
            ("grid", "The grid's lines: as the module shows them, or on or off in this copy", &mut ov.show_grid),
            ("labels", "The numbers along the grid: as the module shows them, or on or off in this copy", &mut ov.show_labels),
            ("snap", "Dragged points settle on the nearest grid crossing: as the module does, or on or off in this copy", &mut ov.snap),
        ]
        .into_iter()
        .take(switches)
        {
            let r = insp.row(&[200.0])[0];
            if let Some(v) = insp.combo(r, name, name, &*field, &[(None, "module's"), (Some(true), "on"), (Some(false), "off")], hover) {
                *field = v;
            }
        }
    });
    section(ui, w, salt, "Lines", |insp| {
        ov.channel_colors.resize(channels, None);
        let mins: Vec<f32> = (0..channels).map(|_| 90.0).collect();
        let rs = insp.row(&mins);
        for (ch, r) in rs.into_iter().enumerate() {
            let default = MULTI_COLORS[ch % MULTI_COLORS.len()];
            let mut col = ov.channel_colors[ch].unwrap_or([default.r(), default.g(), default.b(), 255]);
            if insp.colour(r, &format!("ch{ch}"), &format!("ch {}", ch + 1), &mut col, &format!("Channel {}'s line and dots", ch + 1)) {
                ov.channel_colors[ch] = Some(col);
            }
        }
        clear = reset_row(insp, "The module's own colours and switches");
    });
    if clear {
        m.graph_override = None;
    } else {
        let any = ov.background.is_some() || ov.outline.is_some() || ov.outline_px.is_some() || ov.gridline.is_some()
            || ov.show_grid.is_some() || ov.show_labels.is_some() || ov.snap.is_some()
            || ov.channel_colors.iter().any(|c| c.is_some());
        while matches!(ov.channel_colors.last(), Some(None)) {
            ov.channel_colors.pop();
        }
        m.graph_override = if any { Some(PinGraphOverride { ..ov }) } else { None };
    }
}
