//! The settings under a Response Curve's graph, for the whole family (Response Curve, Vec
//! Response Curve, Two-way, and the AutoMap curves that reuse their bodies).
//!
//! One row that is the Log/Exp slider, as wide as the graph it bends, with the curve's
//! switches as drawn squares at its right end; then the numbers as value pills - the
//! Envelope's - in groups that share a band while the width holds them and take a band each
//! when it doesn't. Each body used to lay these out itself as rows of checkboxes and bare
//! drag fields at whatever width they came to, three times over.

use super::glyphs::Glyph;
use super::*;

const GAP: f32 = 4.0;
/// The link toggle that sits between two pills.
const LINK_W: f32 = 18.0;
/// The narrowest a range pill reads at: `in↓` and `-1.00`.
const RANGE_PILL: f32 = 70.0;
/// The narrowest a count pill reads at: `H` and `16`.
const COUNT_PILL: f32 = 42.0;
const TRAIL_PILL: f32 = 96.0;

thread_local! {
    static ZOOM: std::cell::Cell<f32> = const { std::cell::Cell::new(1.0) };
    static ONE_BAND: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
    static FITTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the rows are being fitted to a pinned frame ([`with_zoom`]): there a band's
/// groups share all the width the frame has, where a body caps them at half as much again
/// as they asked for (see [`bands`]).
pub(crate) fn fitting() -> bool {
    FITTING.with(|f| f.get())
}

/// The scale the rows here are drawn at: 1 in a module's body, and whatever fits the frame
/// for a Style 2 pinned row ([`with_zoom`], set by `pin_style2::fit`). Every size these
/// helpers lay out - bands, gaps, the narrowest a group reads at, the pill font - is
/// multiplied by it, so a row drawn at 2× is the same row twice the size, not a row laid out
/// twice as wide at the same size. The pinned rows used to get their scale from
/// `apply_widget_scale`, which measures what was drawn and scales the next frame by it; these
/// rows don't shrink with a text style, so a narrower frame measured wider, scaled smaller,
/// drew wider again, and ran away (a sub-patch layout zoomed out to nothing, or crashed).
pub(crate) fn zoom() -> f32 {
    ZOOM.with(|z| z.get())
}

/// `v` at the current [`zoom`].
pub(crate) fn zoomed(v: f32) -> f32 {
    v * zoom()
}

fn z(v: f32) -> f32 {
    zoomed(v)
}

/// Say how wide one band of what is being drawn is at 1×, when it isn't laid out by
/// [`bands`] (which says so itself). The widest wins. Read by [`with_zoom`].
pub(crate) fn note_one_band(w: f32) {
    ONE_BAND.with(|c| c.set(c.get().max(w)));
}

/// Run `f` with the rows drawn at `zoom`, and return what it returned with how wide one band
/// of what it drew is at 1× (0 if it said nothing): a number that comes from the rows'
/// constants rather than from what was drawn, so it doesn't depend on the scale it was drawn
/// at. The previous zoom is back afterwards, whatever `f` did.
pub(crate) fn with_zoom<R>(zoom: f32, f: impl FnOnce() -> R) -> (R, f32) {
    struct Restore(f32, f32, bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            ZOOM.with(|z| z.set(self.0));
            ONE_BAND.with(|c| c.set(self.1));
            FITTING.with(|f| f.set(self.2));
        }
    }
    let restore = Restore(
        ZOOM.with(|z| z.replace(zoom)),
        ONE_BAND.with(|c| c.replace(0.0)),
        FITTING.with(|f| f.replace(true)),
    );
    let r = f();
    let one = ONE_BAND.with(|c| c.get());
    drop(restore);
    (r, one)
}

/// The height of a band of settings: one block of the layout grid, so every block the grip
/// gives the module goes to the graph and the rows land on block lines.
pub(crate) fn band_h(ui: &egui::Ui) -> f32 {
    crate::canvas::canvas_grid(ui.ctx()).map_or(28.0, |g| g.block) * zoom()
}

/// The In and Out ranges a curve shows.
pub(crate) enum CurveRange<'a> {
    /// Both ends of both: `[in↓, in↑, out↓, out↑]`. With `abs` only the upper ends apply,
    /// and only they are shown.
    Full { v: &'a mut [f32; 4], abs: bool },
    /// The upper ends only (Vec, Two-way), never below `floor`.
    Max { i1: &'a mut f32, o1: &'a mut f32, floor: f32 },
    /// A motion pick's view, in the sensor's own units: the fields it always had.
    Motion { unit: MotionUnit, abs: bool },
}

pub(crate) struct CurveSettings<'a> {
    pub sc_t: &'a mut f32,
    /// The Abs switch and whether it can be changed (Vec mode forces it on).
    pub abs: Option<(&'a mut bool, bool)>,
    /// The Two-way curve's Vec mode.
    pub vec: Option<&'a mut bool>,
    pub snap: &'a mut bool,
    pub range: CurveRange<'a>,
    pub gx: &'a mut f64,
    pub gy: &'a mut f64,
    pub grid_max: f64,
    pub trail: &'a mut i64,
    /// Which parts to draw: all of them in a body, one in a pinned element.
    pub parts: CurveParts,
}

/// The parts of a curve's settings, as its pinnable elements divide them.
#[derive(Clone, Copy)]
pub(crate) struct CurveParts {
    /// The Log/Exp row and its switches (`scale_row`).
    pub scale: bool,
    /// In and Out (`range_row`).
    pub range: bool,
    /// The grid and the trail (`grid_row`).
    pub grid: bool,
}

impl CurveParts {
    pub(crate) const ALL: CurveParts = CurveParts { scale: true, range: true, grid: true };
}

/// Where each group went, for the elements a layout can pin.
pub(crate) struct CurveRows {
    pub scale: egui::Rect,
    pub range: egui::Rect,
    pub grid: egui::Rect,
    /// The narrowest these rows can be drawn: the widest single group. The rows themselves
    /// are as wide as they were told to be, so their drawn width says nothing about it.
    pub min_w: f32,
    /// Each field's rect, by the param it sets (`scale_t`, `absolute`, `snap`, `in_min` …
    /// `out_max`, `grid_x`, `grid_y`, `trail_ms`), for a pinned row's gamepad focus.
    pub fields: Vec<(&'static str, egui::Rect)>,
}

/// Group rects for `mins`, left to right in bands `band` tall inside `width`: a group that
/// doesn't fit beside the ones before it starts the next band, and what a band has left over
/// is shared among its groups in proportion to what they asked for. Relative to (0, 0);
/// returns the rects and the height of all the bands.
pub(crate) fn bands(width: f32, mins: &[f32], band: f32, gap: f32) -> (Vec<egui::Rect>, f32) {
    // `mins` and `gap` are at 1×; `band` and `width` are already at the zoom.
    note_one_band(mins.iter().sum::<f32>() + gap * (mins.len().max(1) - 1) as f32);
    let mins: Vec<f32> = mins.iter().map(|m| z(*m)).collect();
    let mins = mins.as_slice();
    let gap = z(gap);
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut used = 0.0f32;
    for (i, m) in mins.iter().enumerate() {
        let fits = rows.last().is_some_and(|r| !r.is_empty()) && used + gap + m <= width + 0.5;
        if fits {
            used += gap + m;
            rows.last_mut().expect("a band").push(i);
        } else {
            rows.push(vec![i]);
            used = *m;
        }
    }
    let mut out = vec![egui::Rect::NOTHING; mins.len()];
    for (b, row) in rows.iter().enumerate() {
        let want: f32 = row.iter().map(|&i| mins[i]).sum::<f32>().max(1.0);
        // Up to half as much again as they asked for: a count of two digits stretched across
        // a whole band is a plank with a number on it. A pinned row fitted to its frame takes
        // the frame's whole width instead - the frame is the size the user chose, and a gap
        // at its right end is room the pills' names could have had.
        let cap = if fitting() { f32::INFINITY } else { want * 1.5 };
        let room = (width - gap * (row.len() as f32 - 1.0)).max(want).min(cap);
        let mut x = 0.0f32;
        for &i in row {
            let w = room * mins[i] / want;
            out[i] = egui::Rect::from_min_size(egui::pos2(x, b as f32 * band), egui::vec2(w, band));
            x += w + gap;
        }
    }
    (out, rows.len() as f32 * band)
}

/// A square switch with a drawing on it, at `rect`. Returns whether it was flipped.
pub(crate) fn icon_toggle(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    glyph: Glyph,
    on: &mut bool,
    enabled: bool,
    hover: &str,
    salt: impl std::hash::Hash,
) -> bool {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt(("curve_icon", salt)));
    if !enabled {
        child.disable();
    }
    let resp = child.put(
        rect,
        egui::Button::selectable(*on, "").frame_when_inactive(true).min_size(rect.size()),
    );
    let colour = if *on {
        child.visuals().strong_text_color()
    } else {
        child.visuals().text_color().linear_multiply(0.75)
    };
    glyphs::paint(child.painter(), rect, glyph, if enabled { colour } else { colour.linear_multiply(0.5) });
    if resp.on_hover_text(hover).clicked() {
        *on = !*on;
        true
    } else {
        false
    }
}

/// A body's settings as pill groups laid out in bands ([`bands`]), any of which can be left
/// out: a module's body draws all of them, a pinned element only its own (the Style 2 pins,
/// see `pin_style2`). Group `k` is the `k`th entry of the full list either way, so the code
/// that draws a group doesn't care which others are there.
pub(crate) struct Groups {
    cells: Vec<Option<egui::Rect>>,
    ctrl: f32,
    need: f32,
}

impl Groups {
    /// Lay out the groups `want` keeps, `width` wide (never narrower than the widest of
    /// them), and allocate their bands.
    pub(crate) fn layout(ui: &mut egui::Ui, width: f32, mins: &[f32], want: impl Fn(usize) -> bool) -> Self {
        let band = band_h(ui);
        let kept: Vec<usize> = (0..mins.len()).filter(|&k| want(k) && mins[k] > 0.0).collect();
        let kept_mins: Vec<f32> = kept.iter().map(|&k| mins[k]).collect();
        let need = kept_mins.iter().copied().fold(0.0f32, f32::max) * zoom();
        let width = width.max(need);
        let (rects, h) = if kept.is_empty() { (Vec::new(), 0.0) } else { bands(width, &kept_mins, band, 3.0 * GAP) };
        let (area, _) = ui.allocate_exact_size(egui::vec2(width, h), egui::Sense::hover());
        let mut cells = vec![None; mins.len()];
        for (i, &k) in kept.iter().enumerate() {
            cells[k] = Some(rects[i].translate(area.min.to_vec2()));
        }
        let _ = area;
        Self { cells, ctrl: band - 4.0, need }
    }

    /// Group `k`'s controls' rect - a pill's height, in the middle of its band - if it is
    /// drawn.
    pub(crate) fn get(&self, k: usize) -> Option<egui::Rect> {
        let c = self.cells.get(k).copied().flatten()?;
        Some(egui::Rect::from_min_size(egui::pos2(c.left(), c.center().y - self.ctrl * 0.5), egui::vec2(c.width(), self.ctrl)))
    }

    /// The widest group: what the rows need, whatever width they were drawn at.
    pub(crate) fn need(&self) -> f32 {
        self.need
    }
}

/// `rect` cut into slots in proportion to `weights`, `GAP` apart.
pub(crate) fn split(rect: egui::Rect, weights: &[f32]) -> Vec<egui::Rect> {
    let total: f32 = weights.iter().sum::<f32>().max(1e-3);
    let room = rect.width() - z(GAP) * (weights.len() as f32 - 1.0);
    let mut x = rect.left();
    weights
        .iter()
        .map(|w| {
            let r = egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(room * w / total, rect.height()));
            x = r.right() + z(GAP);
            r
        })
        .collect()
}

/// A category heading in a body: a chevron, the name and a hairline to the edge, which folds
/// and unfolds what is under it. The fold is kept on the node (`fold_<key>`), so it saves
/// with the patch. Returns whether the category is open.
pub(crate) fn section(ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, node_id: NodeId, key: &str, title: &str, default_open: bool) -> bool {
    let param = format!("fold_{key}");
    let open = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get(&param).and_then(|v| v.as_bool()))
        .map_or(default_open, |folded| !folded);
    let h = 20.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::click());
    let v = ui.visuals();
    let col = if resp.hovered() { v.strong_text_color() } else { v.text_color() };
    let c = egui::pos2(rect.left() + 6.0, rect.center().y);
    let pts = if open {
        vec![c + egui::vec2(-4.0, -2.0), c + egui::vec2(4.0, -2.0), c + egui::vec2(0.0, 3.0)]
    } else {
        vec![c + egui::vec2(-2.0, -4.0), c + egui::vec2(3.0, 0.0), c + egui::vec2(-2.0, 4.0)]
    };
    ui.painter().add(egui::Shape::convex_polygon(pts, col, egui::Stroke::NONE));
    let font = egui::TextStyle::Small.resolve(ui.style());
    let g = ui.painter().layout_no_wrap(title.to_string(), font, col);
    let text_w = g.size().x;
    ui.painter().galley(egui::pos2(rect.left() + 16.0, rect.center().y - g.size().y * 0.5), g, col);
    let line_x = rect.left() + 22.0 + text_w;
    if line_x < rect.right() {
        ui.painter().line_segment(
            [egui::pos2(line_x, rect.center().y), egui::pos2(rect.right(), rect.center().y)],
            egui::Stroke::new(1.0_f32, v.widgets.noninteractive.bg_stroke.color),
        );
    }
    if resp.on_hover_text(if open { "Fold" } else { "Unfold" }).clicked() {
        if let Some(n) = snarl.get_node_mut(node_id) {
            n.params.insert(param, Value::Bool(open));
        }
        return !open;
    }
    open
}

/// One choice out of a few, as one pill cut into segments at `rect`: the chosen segment lit,
/// a hairline between the others. The option rows that were loose words spread across a
/// row (a 3DOF's Pointer, an RWS view's Ruler / Room / Both) read as one control this way.
/// Returns the option clicked, if a different one was.
pub(crate) fn segmented<T: PartialEq + Clone>(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    value: &T,
    options: &[(T, &str, &str)],
    salt: impl std::hash::Hash + Copy,
) -> Option<T> {
    let n = options.len().max(1);
    let w = rect.width() / n as f32;
    let font = egui::TextStyle::Small.resolve(ui.style());
    let v = ui.visuals().clone();
    let enabled = ui.is_enabled();
    let mut picked = None;
    for (i, (option, text, hover)) in options.iter().enumerate() {
        let r = egui::Rect::from_min_size(egui::pos2(rect.left() + i as f32 * w, rect.top()), egui::vec2(w, rect.height()));
        let mut resp = ui.interact(r, egui::Id::new(("segmented", salt, i)), egui::Sense::click());
        if !hover.is_empty() {
            resp = resp.on_hover_text(*hover);
        }
        let on = value == option;
        let (fill, text_col) = if on {
            (v.selection.bg_fill, v.strong_text_color())
        } else if resp.hovered() && enabled {
            (v.widgets.hovered.weak_bg_fill, v.text_color())
        } else {
            (v.widgets.inactive.weak_bg_fill, v.text_color())
        };
        let rad = 3u8;
        let corners = egui::CornerRadius {
            nw: if i == 0 { rad } else { 0 },
            sw: if i == 0 { rad } else { 0 },
            ne: if i + 1 == n { rad } else { 0 },
            se: if i + 1 == n { rad } else { 0 },
        };
        let (fill, text_col) = if enabled { (fill, text_col) } else { (fill.gamma_multiply(0.6), text_col.gamma_multiply(0.5)) };
        ui.painter().rect_filled(r, corners, fill);
        let shown = simple_bodies::elide_to_width(ui, text, font.clone(), text_col, w - 4.0);
        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, shown, font.clone(), text_col);
        if i > 0 {
            ui.painter().line_segment(
                [egui::pos2(r.left(), r.top() + 3.0), egui::pos2(r.left(), r.bottom() - 3.0)],
                egui::Stroke::new(1.0_f32, v.extreme_bg_color),
            );
        }
        if resp.clicked() && enabled && !on {
            picked = Some(option.clone());
        }
    }
    picked
}

/// A switch with a word on it, at `rect`, in the pills' shape: lit while it is on. Returns
/// whether it flipped.
pub(crate) fn word_toggle(ui: &mut egui::Ui, rect: egui::Rect, on: &mut bool, text: &str, hover: &str, salt: impl std::hash::Hash + Copy) -> bool {
    let resp = ui.interact(rect, egui::Id::new(("word_toggle", salt)), egui::Sense::click()).on_hover_text(hover);
    let v = ui.visuals();
    let enabled = ui.is_enabled();
    let (fill, text_col) = if *on {
        (v.selection.bg_fill, v.strong_text_color())
    } else if resp.hovered() && enabled {
        (v.widgets.hovered.weak_bg_fill, v.text_color())
    } else {
        (v.widgets.inactive.weak_bg_fill, v.text_color())
    };
    let (fill, text_col) = if enabled { (fill, text_col) } else { (fill.gamma_multiply(0.6), text_col.gamma_multiply(0.5)) };
    let font = egui::TextStyle::Small.resolve(ui.style());
    ui.painter().rect_filled(rect, 3.0, fill);
    let shown = simple_bodies::elide_to_width(ui, text, font.clone(), text_col, rect.width() - 4.0);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, shown, font, text_col);
    if resp.clicked() && enabled {
        *on = !*on;
        true
    } else {
        false
    }
}

/// A dropdown at `rect` in the pills' shape: a coloured word cell, then the choice.
pub(crate) fn pill_combo<T: PartialEq + Clone>(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    label: &str,
    value: &T,
    options: &[(T, &str)],
    hover: &str,
    salt: impl std::hash::Hash + Copy,
) -> Option<T> {
    let shown = options.iter().find(|(o, _)| o == value).map_or("", |(_, t)| *t);
    let (label, label_w) = simple_bodies::pill_label_for(ui, label, pill_label_w(ui, label), shown, rect.width() - z(20.0));
    let v = ui.visuals().clone();
    let label_cell = egui::Rect::from_min_size(rect.min, egui::vec2(label_w, rect.height()));
    if label_w > 0.0 {
        ui.painter().rect_filled(label_cell, egui::CornerRadius { nw: 3, sw: 3, ne: 0, se: 0 }, v.selection.bg_fill);
        ui.painter().text(
            label_cell.center(),
            egui::Align2::CENTER_CENTER,
            label,
            simple_bodies::pill_font(),
            v.strong_text_color(),
        );
    }
    let combo = egui::Rect::from_min_max(egui::pos2(rect.left() + label_w, rect.top()), rect.max);
    let mut cell = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(combo)
            .layout(egui::Layout::left_to_right(egui::Align::Center))
            .id_salt(("pill_combo", salt)),
    );
    cell.spacing_mut().interact_size.y = rect.height();
    let mut picked = None;
    egui::ComboBox::from_id_salt(("pill_combo_box", salt))
        .selected_text(egui::RichText::new(shown).small())
        .width(combo.width() - 2.0 * cell.spacing().button_padding.x)
        .show_ui(&mut cell, |ui| {
            for (o, t) in options {
                if ui.selectable_label(o == value, egui::RichText::new(*t).small()).clicked() && o != value {
                    picked = Some(o.clone());
                }
            }
        })
        .response
        .on_hover_text(hover);
    picked
}

/// A value pill for a real number, at `rect`: dragged across (`per_px` a point) or typed.
#[allow(clippy::too_many_arguments)]
pub(crate) fn real_pill(
    ui: &mut egui::Ui,
    node: NodeId,
    salt: &str,
    rect: egui::Rect,
    label: &str,
    value: &mut f32,
    shown: &str,
    range: std::ops::RangeInclusive<f32>,
    per_px: f32,
    hover: &str,
) -> bool {
    let label_w = pill_label_w(ui, label);
    let (resp, typed) = simple_bodies::value_pill_number(
        ui,
        node,
        salt,
        rect,
        label,
        shown,
        label_w,
        f64::from(*range.start())..=f64::from(*range.end()),
    );
    let before = *value;
    if let Some(v) = typed {
        *value = v as f32;
    } else if resp.dragged() {
        *value = (*value + resp.drag_delta().x * per_px).max(*range.start()).min(*range.end());
    }
    resp.on_hover_text(hover);
    (*value - before).abs() > 1e-6
}

/// A value pill for a whole number. A slow drag still counts: the part of a step the
/// pointer has travelled is kept between frames rather than rounded away each one.
#[allow(clippy::too_many_arguments)]
pub(crate) fn count_pill(
    ui: &mut egui::Ui,
    node: NodeId,
    salt: &str,
    rect: egui::Rect,
    label: &str,
    value: &mut f64,
    shown: &str,
    range: std::ops::RangeInclusive<f64>,
    per_px: f32,
    hover: &str,
) -> bool {
    let label_w = pill_label_w(ui, label);
    let (resp, typed) = simple_bodies::value_pill_number(ui, node, salt, rect, label, shown, label_w, range.clone());
    let before = *value;
    let acc_id = egui::Id::new(("curve_pill_acc", node.0, salt));
    if let Some(v) = typed {
        *value = v.round();
    } else if resp.dragged() {
        let acc = ui.ctx().data(|d| d.get_temp::<f32>(acc_id)).unwrap_or(0.0) + resp.drag_delta().x * per_px;
        let steps = acc.trunc();
        ui.ctx().data_mut(|d| d.insert_temp(acc_id, acc - steps));
        *value = (*value + f64::from(steps)).max(*range.start()).min(*range.end());
    } else if resp.drag_stopped() {
        ui.ctx().data_mut(|d| d.remove_temp::<f32>(acc_id));
    }
    resp.on_hover_text(hover);
    (*value - before).abs() > 0.5
}

fn pill_label_w(ui: &egui::Ui, label: &str) -> f32 {
    let label = simple_bodies::pill_label_parts(label).0;
    if label.is_empty() {
        return 0.0;
    }
    // In the font the pill paints its label in, which is not the small one.
    let font = simple_bodies::pill_font();
    ui.painter().layout_no_wrap(label.to_string(), font, egui::Color32::PLACEHOLDER).size().x + z(10.0)
}

/// The link toggle, in the middle of `rect`.
fn link_at(ui: &mut egui::Ui, rect: egui::Rect, linked: bool, hover: &str, salt: impl std::hash::Hash) -> bool {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight))
            .id_salt(("curve_link", salt)),
    );
    link_toggle(&mut child, linked, hover).clicked()
}

/// `n` pills across `rect` with a link toggle in the middle of them.
fn pills_and_link(rect: egui::Rect, n: usize, ctrl: f32) -> (Vec<egui::Rect>, egui::Rect) {
    let pill_w = ((rect.width() - z(LINK_W) - n as f32 * z(GAP)) / n as f32).max(z(24.0));
    let top = rect.center().y - ctrl * 0.5;
    let mut x = rect.left();
    let mut pills = Vec::with_capacity(n);
    let mut link = egui::Rect::NOTHING;
    for i in 0..n {
        if i == n / 2 {
            link = egui::Rect::from_min_size(egui::pos2(x, top), egui::vec2(z(LINK_W), ctrl));
            x += z(LINK_W) + z(GAP);
        }
        pills.push(egui::Rect::from_min_size(egui::pos2(x, top), egui::vec2(pill_w, ctrl)));
        x += pill_w + z(GAP);
    }
    (pills, link)
}

/// Draw the settings `width` wide at the cursor. Returns whether a value changed, and where
/// the groups went.
pub(crate) fn curve_settings(
    ui: &mut egui::Ui,
    node_id: NodeId,
    snarl: &mut Snarl<NodeData>,
    width: f32,
    s: CurveSettings<'_>,
) -> (bool, CurveRows) {
    let band = band_h(ui);
    let ctrl = band - 4.0;
    let width = width.max(z(120.0));
    let mut changed = false;
    let mut fields: Vec<(&'static str, egui::Rect)> = Vec::new();
    let parts = s.parts;

    // ── The scale: Log ──●── Exp across the graph's width, the switches at its end ──
    let mut row = egui::Rect::NOTHING;
    if parts.scale {
        // Two squares, the words either side and a slider worth dragging.
        note_one_band(200.0);
        row = ui.allocate_exact_size(egui::vec2(width, band), egui::Sense::hover()).0;
        let mut right = row.right();
        let mut square = |w: f32| {
            let r = egui::Rect::from_min_size(egui::pos2(right - w, row.center().y - ctrl * 0.5), egui::vec2(w, ctrl));
            right -= w + z(GAP);
            r
        };
        let snap_r = square(ctrl);
        changed |= icon_toggle(
            ui,
            snap_r,
            Glyph::Magnet,
            s.snap,
            true,
            "Snap: dragged points settle on the nearest grid intersection",
            ("snap", node_id),
        );
        let mut abs_r = egui::Rect::NOTHING;
        if let Some((abs, enabled)) = s.abs {
            abs_r = square(ctrl);
            changed |= icon_toggle(
                ui,
                abs_r,
                Glyph::Abs,
                abs,
                enabled,
                "Abs: the curve reads the input's magnitude and gives the sign back, so one half \
                 of the graph serves both directions",
                ("abs", node_id),
            );
        }
        if let Some(vec) = s.vec {
            let r = square(z(34.0));
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(r)
                    .layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight))
                    .id_salt(("curve_vec", node_id)),
            );
            let resp = kit_bodies::toggle_button(&mut child, *vec, "vec", r.size())
                .on_hover_text("Vec2 mode: the curve shapes the vector's length. Forces Abs on.");
            if resp.clicked() {
                *vec = !*vec;
                changed = true;
            }
        }
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(egui::Rect::from_min_max(row.min, egui::pos2(right, row.bottom())))
                .layout(egui::Layout::left_to_right(egui::Align::Center))
                .id_salt(("curve_scale", node_id)),
        );
        child.spacing_mut().item_spacing.x = z(GAP);
        let font = egui::TextStyle::Small.resolve(child.style());
        let exp_w = child.painter().layout_no_wrap("Exp".into(), font, egui::Color32::PLACEHOLDER).size().x;
        let log = child.label(egui::RichText::new("Log").small().weak());
        let slider_w = (right - row.left() - log.rect.width() - exp_w - 3.0 * z(GAP)).max(z(40.0));
        let slider_id = egui::Id::new(("log_exp", node_id, child.layer_id()));
        let (slider_r, moved) = log_exp_slider(&mut child, slider_id, egui::vec2(slider_w, z(14.0)), s.sc_t);
        changed |= moved;
        child.label(egui::RichText::new("Exp").small().weak());
        fields.extend([("scale_t", slider_r), ("absolute", abs_r), ("snap", snap_r)]);
    }

    // ── The numbers ──
    // A motion pick's view is the sensor's own fields, on a row to themselves.
    let mut range_rect = egui::Rect::NOTHING;
    if parts.range {
        if let CurveRange::Motion { unit, abs } = &s.range {
            let r = ui.horizontal(|ui| motion_view_range_fields(ui, node_id, snarl, *unit, *abs)).response.rect;
            range_rect = r;
        }
    }
    let range_min = match &s.range {
        CurveRange::Full { abs: false, .. } => 4.0 * RANGE_PILL + LINK_W + 4.0 * GAP,
        CurveRange::Full { abs: true, .. } | CurveRange::Max { .. } => 2.0 * RANGE_PILL + LINK_W + 2.0 * GAP,
        CurveRange::Motion { .. } => 0.0,
    };
    let grid_min = 2.0 * COUNT_PILL + LINK_W + 2.0 * GAP;
    let mins = [range_min, grid_min, TRAIL_PILL];
    let groups = Groups::layout(ui, width, &mins, |k| if k == 0 { parts.range } else { parts.grid });

    // In / Out.
    if let Some(cell) = groups.get(0) {
        range_rect = cell;
        let linked = snarl.get_node(node_id).map_or(true, range_linked);
        let hover_in = "The input range the graph's width stands for";
        let hover_out = "The output range the graph's height stands for";
        let mut link_clicked = false;
        const NAMES: [&str; 4] = ["in_min", "in_max", "out_min", "out_max"];
        match s.range {
            CurveRange::Full { v, abs } => {
                let before = *v;
                let (labels, idx): (&[&str], &[usize]) = if abs {
                    (&["in↑", "out↑"], &[1, 3])
                } else {
                    (&["in↓", "in↑", "out↓", "out↑"], &[0, 1, 2, 3])
                };
                let (pills, link) = pills_and_link(cell, idx.len(), ctrl);
                for (k, &i) in idx.iter().enumerate() {
                    let shown = format!("{:.2}", v[i]);
                    changed |= real_pill(
                        ui,
                        node_id,
                        &format!("crv_range_{i}"),
                        pills[k],
                        labels[k],
                        &mut v[i],
                        &shown,
                        -100_000.0..=100_000.0,
                        0.01,
                        if i < 2 { hover_in } else { hover_out },
                    );
                    fields.push((NAMES[i], pills[k]));
                }
                link_clicked = link_at(ui, link, linked, range_link_hover(linked), ("range", node_id));
                if linked {
                    follow_range_link(before, v);
                }
            }
            CurveRange::Max { i1, o1, floor } => {
                let before = [0.0, *i1, 0.0, *o1];
                let (pills, link) = pills_and_link(cell, 2, ctrl);
                let shown = format!("{:.2}", *i1);
                changed |= real_pill(ui, node_id, "crv_range_1", pills[0], "in↑", i1, &shown, floor..=1000.0, 0.01, hover_in);
                let shown = format!("{:.2}", *o1);
                changed |= real_pill(ui, node_id, "crv_range_3", pills[1], "out↑", o1, &shown, floor..=1000.0, 0.01, hover_out);
                fields.extend([("in_max", pills[0]), ("out_max", pills[1])]);
                link_clicked = link_at(ui, link, linked, range_link_hover(linked), ("range", node_id));
                if linked {
                    let mut now = [0.0, *i1, 0.0, *o1];
                    follow_range_link(before, &mut now);
                    (*i1, *o1) = (now[1].max(floor), now[3].max(floor));
                }
            }
            CurveRange::Motion { .. } => {}
        }
        if link_clicked {
            toggle_range_link(snarl, node_id);
        }
    }

    // Grid.
    let mut grid_rect = egui::Rect::NOTHING;
    if let Some(grid_cell) = groups.get(1) {
        grid_rect = grid_cell;
        let unlinked = snarl
            .get_node(node_id)
            .and_then(|n| n.params.get(GRID_UNLINKED))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let (bx, by) = (*s.gx, *s.gy);
        let (pills, link) = pills_and_link(grid_cell, 2, ctrl);
        let shown = format!("{}", s.gx.round() as i64);
        let h_changed = count_pill(
            ui, node_id, "crv_grid_x", pills[0], "H", s.gx, &shown, 1.0..=s.grid_max, 0.08,
            "Grid divisions across the input. A dragged point settles on one when Snap is on.",
        );
        let shown = format!("{}", s.gy.round() as i64);
        let v_changed = count_pill(
            ui, node_id, "crv_grid_y", pills[1], "V", s.gy, &shown, 1.0..=s.grid_max, 0.08,
            "Grid divisions across the output.",
        );
        fields.extend([("grid_x", pills[0]), ("grid_y", pills[1])]);
        let hover = if unlinked {
            "Link H and V: change both together"
        } else {
            "Linked: H and V change together. Click to set them apart."
        };
        if link_at(ui, link, !unlinked, hover, ("grid", node_id)) {
            if let Some(n) = snarl.get_node_mut(node_id) {
                n.params.insert(GRID_UNLINKED.into(), Value::Bool(!unlinked));
            }
        }
        if !unlinked {
            if h_changed {
                *s.gy = (*s.gy + (*s.gx - bx)).max(1.0).min(s.grid_max);
            } else if v_changed {
                *s.gx = (*s.gx + (*s.gy - by)).max(1.0).min(s.grid_max);
            }
        }
        changed |= h_changed || v_changed;
    }

    // Trail.
    if let Some(r) = groups.get(2) {
        grid_rect = grid_rect.union(r);
        let mut t = *s.trail as f64;
        let shown = format!("{} ms", *s.trail);
        if count_pill(
            ui, node_id, "crv_trail", r, "trail", &mut t, &shown, 0.0..=TRAIL_MS_MAX as f64, 5.0,
            "How long the live input's trail stays on the graph. 0 turns it off.",
        ) {
            *s.trail = t.round() as i64;
            changed = true;
        }
        fields.push(("trail_ms", r));
    }

    let min_w = groups.need().max(if parts.scale { z(150.0) } else { 0.0 });
    (changed, CurveRows { scale: row, range: range_rect, grid: grid_rect, min_w, fields })
}

fn range_link_hover(linked: bool) -> &'static str {
    if linked {
        "Linked: scaling In scales Out by as much, and the other way round. Click to set them apart."
    } else {
        "Link In and Out: scale both together"
    }
}

/// A value pill in a flow row, `w` wide: for the rows that still mix pills with other
/// widgets (the Two-way curve's hysteresis and interpolation).
#[allow(clippy::too_many_arguments)]
pub(crate) fn flow_pill(
    ui: &mut egui::Ui,
    node: NodeId,
    salt: &str,
    w: f32,
    label: &str,
    value: &mut f32,
    shown: &str,
    range: std::ops::RangeInclusive<f32>,
    per_px: f32,
    hover: &str,
    enabled: bool,
) -> bool {
    let ctrl = band_h(ui) - 4.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, ctrl), egui::Sense::hover());
    if !enabled {
        // A value that is not this field's to set (the Hyst graph has taken it over): shown,
        // dimmed, and not dragged.
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt(("curve_flow_pill", salt)));
        child.disable();
        let mut v = *value;
        real_pill(&mut child, node, salt, rect, label, &mut v, shown, range, per_px, hover);
        return false;
    }
    real_pill(ui, node, salt, rect, label, value, shown, range, per_px, hover)
}

/// A pinned curve row in the new style - `scale_row`, `range_row` or `grid_row` - laid out
/// across the pin's frame, for the Response Curve family. See `pin_style2`.
pub(crate) fn render_curve_row_style2(
    inner_id: NodeId,
    module_id: &str,
    element: &str,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let Some(n) = snarl.get_node(inner_id) else { return };
    let f = |k: &str, d: f64| n.params.get(k).and_then(|v| v.as_f64()).unwrap_or(d) as f32;
    let b = |k: &str, d: bool| n.params.get(k).and_then(|v| v.as_bool()).unwrap_or(d);
    let vec_curve = module_id == "module.vec_response_curve";
    let twoway = module_id == "module.twoway_response_curve";
    let vec_mode = twoway && b("vec_mode", false);
    let mut abs = b("absolute", true);
    let mut range = [f("in_min", -1.0), f("in_max", 1.0), f("out_min", -1.0), f("out_max", 1.0)];
    let mut snap = b("snap", false);
    let mut sc_t = f("scale_t", 0.0);
    let mut gx = n.params.get("grid_x").and_then(|v| v.as_f64()).unwrap_or(4.0);
    let mut gy = n.params.get("grid_y").and_then(|v| v.as_f64()).unwrap_or(4.0);
    let mut trail = n.params.get("trail_ms").and_then(|v| v.as_i64()).unwrap_or(300);
    let unit = MotionUnit::of(n);
    let parts = CurveParts { scale: element == "scale_row", range: element == "range_row", grid: element == "grid_row" };
    // Drawn inside `pin_style2::fit`, which has set the zoom and handed over a ui the
    // frame's width: the rows fill it.
    let _ = container;
    let width = ui.available_width();
    let abs_now = abs;
    let sc_before = sc_t;
    let (mut i1, mut o1) = (range[1], range[3]);
    let (changed, rows) = curve_settings(
        ui,
        inner_id,
        snarl,
        width,
        CurveSettings {
            sc_t: &mut sc_t,
            abs: if vec_curve { None } else { Some((&mut abs, !vec_mode)) },
            vec: None,
            snap: &mut snap,
            range: match unit {
                Some(unit) => CurveRange::Motion { unit, abs: abs_now || vec_curve },
                None if vec_curve || twoway => CurveRange::Max { i1: &mut i1, o1: &mut o1, floor: 0.001 },
                None => CurveRange::Full { v: &mut range, abs: abs_now },
            },
            gx: &mut gx,
            gy: &mut gy,
            grid_max: if twoway { 32.0 } else { 20.0 },
            trail: &mut trail,
            parts,
        },
    );
    if vec_curve || twoway {
        (range[1], range[3]) = (i1, o1);
    }
    // The fields in the order the gamepad walks this element (`app::nav::fields`).
    let order: &[&str] = match element {
        "scale_row" if vec_curve => &["scale_t", "snap"],
        "scale_row" => &["scale_t", "absolute", "snap"],
        "range_row" if vec_curve => &["in_max", "out_max"],
        "range_row" => &["in_min", "in_max", "out_min", "out_max"],
        _ => &["grid_x", "grid_y", "trail_ms"],
    };
    let rects: Vec<egui::Rect> = order
        .iter()
        .map(|k| rows.fields.iter().find(|(f, _)| f == k).map_or(egui::Rect::NOTHING, |(_, r)| *r))
        .collect();
    publish_nav_field_rects(ui, inner_id, &rects);
    if changed {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            node.params.insert("absolute".into(), Value::Bool(abs));
            for (k, v) in ["in_min", "in_max", "out_min", "out_max"].into_iter().zip(range) {
                if let Some(num) = Number::from_f64(f64::from(v)) {
                    node.params.insert(k.into(), Value::Number(num));
                }
            }
            node.params.insert("snap".into(), Value::Bool(snap));
            if (sc_t - sc_before).abs() > 1e-6 {
                set_curve_scale(node, sc_t);
            }
            node.params.insert("grid_x".into(), serde_json::json!(gx.round() as i64));
            node.params.insert("grid_y".into(), serde_json::json!(gy.round() as i64));
            node.params.insert("trail_ms".into(), serde_json::json!(trail));
        }
        super::mark_overlay_param_write(ui.ctx());
    }
}

/// Move a curve's points so the response they describe is the same under a new Log/Exp
/// scale. The scale warps both axes of the graph (the engine reads `S⁻¹(curve(S(x)))`), so a
/// point's place on the graph and the input/output pair it stands for are two different
/// things: changing the scale used to leave the points where they were on the graph, which
/// changed every pair - the slider bent the curve the user had set. Each point is taken back
/// to the pair it stood for under `from` and put where that pair sits under `to`. The curve
/// between two points is still a line on the graph, so the shape between them follows the
/// scale; the points themselves do not move in input/output terms.
///
/// The Hyst lane's height is a band width, not an output: only its x is warped.
pub(crate) fn rescale_curve_points(node: &mut NodeData, from: f32, to: f32) {
    use flexinput_engine::eval::{curve_scale, curve_scale_inv};
    if (from - to).abs() < 1e-6 {
        return;
    }
    let warp = |v: f64| -> f64 {
        let real = curve_scale_inv((v.abs() as f32).min(1.0), from);
        v.signum() * f64::from(curve_scale(real, to))
    };
    for (key, warp_y) in [("points", true), ("points_dn", true), ("hyst_points", false)] {
        let Some(Value::Array(points)) = node.params.get_mut(key) else { continue };
        for p in points.iter_mut() {
            let Some(pair) = p.as_array_mut() else { continue };
            if let Some(x) = pair.first().and_then(|v| v.as_f64()) {
                pair[0] = serde_json::json!(warp(x));
            }
            if warp_y {
                if let Some(y) = pair.get(1).and_then(|v| v.as_f64()) {
                    pair[1] = serde_json::json!(warp(y));
                }
            }
        }
    }
}

/// Store a new Log/Exp scale, moving the points with it ([`rescale_curve_points`]).
pub(crate) fn set_curve_scale(node: &mut NodeData, scale_t: f32) {
    let from = node.params.get("scale_t").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    rescale_curve_points(node, from, scale_t);
    if let Some(n) = Number::from_f64(f64::from(scale_t)) {
        node.params.insert("scale_t".into(), Value::Number(n));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_share_a_band_while_they_fit_and_wrap_when_they_dont() {
        let (wide, h) = bands(400.0, &[200.0, 100.0, 60.0], 28.0, 12.0);
        assert_eq!(h, 28.0, "three groups that fit take one band");
        assert!((wide[2].right() - 400.0).abs() < 0.5, "the band is filled to its edge: {wide:?}");
        let (narrow, h) = bands(250.0, &[200.0, 100.0, 60.0], 28.0, 12.0);
        assert_eq!(h, 56.0, "the second and third wrap together");
        assert_eq!(narrow[0].width(), 250.0, "a group alone in a band has all of it it can use");
        assert_eq!((narrow[1].top(), narrow[2].top()), (28.0, 28.0));
        let (tiny, h) = bands(90.0, &[200.0, 100.0, 60.0], 28.0, 12.0);
        assert_eq!(h, 84.0, "each takes a band");
        assert!(tiny[0].width() >= 200.0, "a group is never given less than it asked for");
    }

    #[test]
    fn a_new_scale_moves_the_points_and_not_what_they_mean() {
        use flexinput_engine::eval::apply_curve;
        let desc = flexinput_modules::all_modules()
            .into_iter()
            .map(|r| r.descriptor)
            .find(|d| d.id == "module.response_curve")
            .expect("registered");
        let mut node = NodeData::from(&desc);
        let pts = [[0.0f32, 0.0], [0.3, 0.7], [0.6, 0.4], [1.0, 1.0]];
        node.params.insert("points".into(), serde_json::json!(pts.iter().map(|p| vec![p[0], p[1]]).collect::<Vec<_>>()));
        node.params.insert("biases".into(), serde_json::json!([0.0, 0.0, 0.0]));
        node.params.insert("scale_t".into(), serde_json::json!(0.0));
        let read = |n: &NodeData| -> Vec<[f32; 2]> {
            n.params["points"].as_array().unwrap().iter().map(|p| [p[0].as_f64().unwrap() as f32, p[1].as_f64().unwrap() as f32]).collect()
        };
        // What each point stands for before: the input that lands on it, and what comes out.
        let before: Vec<(f32, f32)> = pts.iter().map(|p| (p[0], apply_curve(p[0], &pts, &[0.0; 3], true, -1.0, 1.0, -1.0, 1.0, 0.0))).collect();
        set_curve_scale(&mut node, 0.6);
        let moved = read(&node);
        assert!((moved[1][0] - pts[1][0]).abs() > 0.02, "the points stayed where they were on the graph");
        for (input, output) in before {
            let now = apply_curve(input, &moved, &[0.0; 3], true, -1.0, 1.0, -1.0, 1.0, 0.6);
            assert!((now - output).abs() < 1e-3, "input {input} gave {output} and now gives {now}");
        }
        // And back: the points return to where they started.
        set_curve_scale(&mut node, 0.0);
        for (a, b) in read(&node).iter().zip(pts.iter()) {
            assert!((a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3, "{a:?} did not come back to {b:?}");
        }
    }
}
