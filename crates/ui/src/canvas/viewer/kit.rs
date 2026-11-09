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
pub(crate) const ROW_GAP: f32 = 4.0;
/// Space above a section caption.
#[allow(dead_code)] // As `Kit::section`.
pub(crate) const SECTION_GAP: f32 = 6.0;
/// The narrowest a row's flexible part gets.
pub(crate) const MIN_FLEX: f32 = 52.0;
/// The narrowest body: a label, the narrowest rail, and a value box.
pub(crate) const MIN_W: f32 = LABEL_W + GAP + MIN_FLEX + GAP + VALUE_W;

/// The params key holding the frame width the user dragged a kit node to.
pub(crate) const WIDTH_PARAM: &str = "ui_w";

/// Modules whose bodies are built from the kit: they get a width grip, and
/// their body width comes from [`body_width`].
pub(crate) fn uses_kit(module_id: &str) -> bool {
    matches!(
        module_id,
        "module.delay"
            | "module.average"
            | "module.dc_filter"
            | "math.negate"
            | "math.quantize"
            | "logic.delay"
            | "logic.counter"
            | "logic.has_changed"
            | "logic.greater_than"
            | "logic.less_than"
            | "module.vec_to_deflection"
    )
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
) -> f32 {
    let Some(frame_w) = frame_w else { return avail.max(min_w).floor() };
    let whole = |w: f32| block.map_or(w, |b| ((w - 0.5) / b).ceil().max(1.0) * b);
    // Everything around the body: the pin columns, their spacing, the frame.
    let around = frame_w - avail;
    let narrowest = whole(around + min_w);
    let wanted = node.params.get(WIDTH_PARAM).and_then(|v| v.as_f64()).map(|w| whole(w as f32));
    let frame = wanted.unwrap_or(frame_w).max(narrowest);
    (frame - around).max(min_w).floor()
}

/// A body of rows `w` wide.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Kit {
    pub(crate) w: f32,
}

/// Lays out a kit body `w` wide; `add` draws its rows.
///
/// The rows go in a child of `ui`, and `ui` is told the body takes exactly
/// `w`, whatever the rows drew: a value or an option too long for its cell
/// can't make the node measure wider, and so can't tip it over a block and
/// back every other frame.
pub(crate) fn body<R>(ui: &mut egui::Ui, w: f32, add: impl FnOnce(&mut egui::Ui, Kit) -> R) -> R {
    let origin = ui.cursor().min;
    let mut rows = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("kit_body")
            .max_rect(egui::Rect::from_min_size(origin, egui::vec2(w, 1.0e5)))
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    rows.spacing_mut().item_spacing.y = ROW_GAP;
    let r = add(&mut rows, Kit { w });
    let h = rows.min_rect().height();
    ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    r
}

fn row_h(ui: &egui::Ui) -> f32 {
    ui.spacing().interact_size.y
}

/// A label cell: small, weak, left-aligned, clipped to the cell.
fn label_cell(ui: &mut egui::Ui, text: &str, w: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, row_h(ui)), egui::Sense::hover());
    let font = egui::TextStyle::Small.resolve(ui.style());
    let color = if ui.is_enabled() {
        ui.visuals().weak_text_color()
    } else {
        ui.visuals().weak_text_color().gamma_multiply(0.5)
    };
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    let pos = egui::pos2(rect.left(), rect.center().y - galley.size().y * 0.5);
    ui.painter().with_clip_rect(rect).galley(pos, galley, color);
    resp
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

impl Kit {
    /// What's left of the row for its control after the label cell.
    pub(crate) fn rest(self) -> f32 {
        self.w - LABEL_W - GAP
    }

    /// A row: the label cell, then `add` lays out the rest, given its width.
    pub(crate) fn row<R>(
        self,
        ui: &mut egui::Ui,
        label: &str,
        add: impl FnOnce(&mut egui::Ui, f32) -> R,
    ) -> (R, egui::Rect) {
        let r = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            label_cell(ui, label, LABEL_W);
            ui.add_space(GAP);
            add(ui, self.rest())
        });
        (r.inner, r.response.rect)
    }

    /// A row with no label cell: `add` gets the whole width.
    pub(crate) fn full_row<R>(self, ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui, f32) -> R) -> (R, egui::Rect) {
        let r = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            add(ui, self.w)
        });
        (r.inner, r.response.rect)
    }

    /// A value box `VALUE_W` wide at the right edge of what's left of the row,
    /// after `before` points of other cells.
    fn value_box(ui: &mut egui::Ui, rest: f32, before: f32, drag: egui::DragValue<'_>) -> egui::Response {
        ui.add_space((rest - before - VALUE_W).max(0.0));
        ui.add_sized([VALUE_W, row_h(ui)], drag)
    }

    /// Label, a slider rail across the row, and the value's box on the right
    /// edge. `drag` builds the box's `DragValue` (speed, range, suffix…).
    /// `log` spaces the rail logarithmically, for times and other values whose
    /// small settings matter as much as their large ones.
    pub(crate) fn slider(
        self,
        ui: &mut egui::Ui,
        label: &str,
        value: &mut f32,
        range: RangeInclusive<f32>,
        log: bool,
        drag: impl FnOnce(&mut f32) -> egui::DragValue<'_>,
    ) -> (bool, egui::Rect) {
        self.row(ui, label, |ui, rest| {
            let rail = (rest - GAP - VALUE_W).max(MIN_FLEX);
            ui.spacing_mut().slider_width = rail;
            let slider = egui::Slider::new(value, range).show_value(false).logarithmic(log);
            let mut changed = ui.add(slider).changed();
            ui.add_space(GAP);
            changed |= ui.add_sized([VALUE_W, row_h(ui)], drag(value)).changed();
            changed
        })
    }

    /// Label and the value's box on the right edge, for values with no useful
    /// range to slide across.
    pub(crate) fn value(
        self,
        ui: &mut egui::Ui,
        label: &str,
        value: &mut f32,
        drag: impl FnOnce(&mut f32) -> egui::DragValue<'_>,
    ) -> (bool, egui::Rect) {
        self.row(ui, label, |ui, rest| Self::value_box(ui, rest, 0.0, drag(value)).changed())
    }

    /// Label, then two value boxes sharing the row, each with its own small
    /// caption (Min / Max and the like).
    pub(crate) fn pair(
        self,
        ui: &mut egui::Ui,
        label: &str,
        a: (&str, &mut f32, bool),
        b: (&str, &mut f32, bool),
        drag: impl Fn(&mut f32) -> egui::DragValue<'_>,
    ) -> (bool, egui::Rect) {
        self.row(ui, label, |ui, rest| {
            let half = (rest - GAP) * 0.5;
            let mut changed = false;
            for (i, (caption, v, enabled)) in [a, b].into_iter().enumerate() {
                if i > 0 {
                    ui.add_space(GAP);
                }
                let cap_w = (half - VALUE_W - GAP).max(0.0);
                ui.add_enabled_ui(enabled, |ui| {
                    label_cell(ui, caption, cap_w);
                    ui.add_space(GAP);
                    changed |= ui.add_sized([VALUE_W, row_h(ui)], drag(v)).changed();
                });
            }
            changed
        })
    }

    /// Label, then the options as a strip of equal cells across the row. Each
    /// option is (value, text, hover text — empty for none).
    pub(crate) fn choice<T: PartialEq + Clone>(
        self,
        ui: &mut egui::Ui,
        label: Option<&str>,
        value: &mut T,
        options: &[(T, &str, &str)],
    ) -> (bool, egui::Rect) {
        match label {
            Some(label) => self.row(ui, label, |ui, w| strip(ui, w, value, options)),
            None => self.full_row(ui, |ui, w| strip(ui, w, value, options)),
        }
    }

    /// Label, then a checkbox with its own text.
    pub(crate) fn toggle(self, ui: &mut egui::Ui, label: &str, value: &mut bool, text: &str) -> (egui::Response, egui::Rect) {
        self.row(ui, label, |ui, _| ui.checkbox(value, egui::RichText::new(text).small()))
    }

    /// Label, then − count + at the right edge. Returns the step taken.
    pub(crate) fn stepper(
        self,
        ui: &mut egui::Ui,
        label: &str,
        count: usize,
        range: RangeInclusive<usize>,
        hover: (&str, &str),
    ) -> (Option<isize>, egui::Rect) {
        self.row(ui, label, |ui, rest| {
            let h = row_h(ui);
            let count_w = (VALUE_W - 2.0 * (h + GAP)).max(h);
            ui.add_space((rest - (2.0 * (h + GAP) + count_w)).max(0.0));
            let mut step = None;
            let minus = ui.add_enabled(count > *range.start(), egui::Button::new("−").min_size(egui::vec2(h, h)));
            if minus.on_hover_text(hover.0).clicked() {
                step = Some(-1);
            }
            ui.add_space(GAP);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(count_w, h), egui::Sense::hover());
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                count.to_string(),
                egui::TextStyle::Body.resolve(ui.style()),
                ui.visuals().text_color(),
            );
            ui.add_space(GAP);
            let plus = ui.add_enabled(count < *range.end(), egui::Button::new("+").min_size(egui::vec2(h, h)));
            if plus.on_hover_text(hover.1).clicked() {
                step = Some(1);
            }
            step
        })
    }

    /// A section caption: small text followed by a hairline to the right edge.
    #[allow(dead_code)] // For bodies with more than one group of rows; none moved over yet.
    pub(crate) fn section(self, ui: &mut egui::Ui, title: &str) {
        ui.add_space(SECTION_GAP - ROW_GAP);
        let font = egui::TextStyle::Small.resolve(ui.style());
        let color = ui.visuals().weak_text_color();
        let galley = ui.painter().layout_no_wrap(title.to_owned(), font, color);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(self.w, galley.size().y), egui::Sense::hover());
        let y = rect.center().y;
        ui.painter().galley(egui::pos2(rect.left(), rect.top()), galley.clone(), color);
        let x0 = rect.left() + galley.size().x + GAP;
        if x0 < rect.right() {
            let line = ui.visuals().widgets.noninteractive.bg_stroke;
            ui.painter().line_segment([egui::pos2(x0, y), egui::pos2(rect.right(), y)], line);
        }
    }

    /// A short note under a row: small, weak, wrapped to the body's width.
    #[allow(dead_code)] // As `section`.
    pub(crate) fn note(self, ui: &mut egui::Ui, text: &str) {
        ui.add(egui::Label::new(egui::RichText::new(text).small().weak()).wrap());
    }
}
