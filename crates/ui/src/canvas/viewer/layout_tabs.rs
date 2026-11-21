//! A layout's tabs: each object is on one of them, or on every one, and one shows at a time.
//! With two or more, the layout has a row of tabs across its top and is laid out beneath it:
//! a click on a tab shows it, and while the layout is edited a double click renames a tab, the
//! tab showing has a × that takes it off and a + at the row's end adds one. The layout
//! editor's toolbar and the pad's layout-tab buttons (LB / RB unless rebound) pick tabs too.

use super::*;

/// The most tabs a layout can have.
pub(crate) const MAX_LAYOUT_TABS: usize = 32;

/// How tall a layout's row of tabs is, in the layout's own points.
pub(crate) const TAB_ROW_H: f32 = 30.0;

/// How much of the top of a layout with `tabs` tabs its row of tabs takes: none under two.
pub(crate) fn tab_row_h(tabs: usize) -> f32 {
    if tabs >= 2 { TAB_ROW_H } else { 0.0 }
}

/// What was done on a layout's row of tabs in a frame.
#[derive(Default, Debug, Clone, PartialEq)]
pub(crate) struct TabRowAct {
    /// A tab clicked: it shows.
    pub show: Option<usize>,
    /// A tab renamed (editing): its index and new name.
    pub rename: Option<(usize, String)>,
    /// A tab's × clicked (editing): it comes off.
    pub remove: Option<usize>,
    /// The + at the row's end clicked (editing): a tab is added, and shows.
    pub add: bool,
}

impl TabRowAct {
    /// Whether anything was done.
    pub(crate) fn any(&self) -> bool {
        self.show.is_some() || self.rename.is_some() || self.remove.is_some() || self.add
    }

    /// Do it to a layout - its tab names, its objects, the tab showing and its selection,
    /// which goes whenever another tab shows. Returns whether the layout itself changed
    /// (a tab renamed, added or taken off), as opposed to only another tab showing.
    pub(crate) fn apply(
        self,
        names: &mut Vec<String>,
        items: &mut [LayoutItem],
        active: &mut usize,
        selected_item: &mut Option<usize>,
        selected_items: &mut Vec<usize>,
    ) -> bool {
        let mut show = |k: usize, active: &mut usize| {
            if *active != k {
                *active = k;
                *selected_item = None;
                selected_items.clear();
            }
        };
        let mut changed = false;
        if let Some((k, name)) = self.rename {
            let name = name.trim().to_string();
            if let Some(n) = names.get_mut(k).filter(|n| !name.is_empty() && **n != name) {
                *n = name;
                changed = true;
            }
        }
        if let Some(k) = self.remove.filter(|&k| k < names.len()) {
            remove_layout_tab(names, items, k);
            show(k.saturating_sub(1).min(names.len().saturating_sub(1)), active);
            *active = (*active).min(names.len().saturating_sub(1));
            changed = true;
        }
        if self.add {
            if let Some(k) = add_layout_tab(names) {
                show(k, active);
                changed = true;
            }
        }
        if let Some(k) = self.show.filter(|&k| k < names.len()) {
            show(k, active);
        }
        changed
    }
}

/// Where a row of tabs at `rect` with `names` puts each tab, and its +: each tab as wide as
/// its name wants (the one showing room for its × while `edit`), all of them narrowed alike
/// where they'd run past the row.
fn tab_row_cells(ui: &egui::Ui, rect: egui::Rect, names: &[String], active: usize, edit: bool) -> (Vec<egui::Rect>, Option<egui::Rect>) {
    let font = tab_row_font(rect);
    let pad = 12.0;
    let close_w = 16.0;
    let plus = edit && names.len() < MAX_LAYOUT_TABS;
    let plus_w = if plus { rect.height() - 4.0 } else { 0.0 };
    let want: Vec<f32> = names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let w = ui.painter().layout_no_wrap(n.clone(), font.clone(), egui::Color32::PLACEHOLDER).size().x;
            (w + 2.0 * pad + if edit && i == active { close_w } else { 0.0 }).max(64.0)
        })
        .collect();
    let gap = 2.0;
    let room = (rect.width() - 8.0 - plus_w - gap * names.len() as f32).max(0.0);
    let total: f32 = want.iter().sum();
    let k = if total > room && total > 0.0 { room / total } else { 1.0 };
    let mut x = rect.left() + 4.0;
    let top = rect.top() + 4.0;
    let cells = want
        .iter()
        .map(|w| {
            let cell = egui::Rect::from_min_size(egui::pos2(x, top), egui::vec2(w * k, rect.bottom() - top));
            x += w * k + gap;
            cell
        })
        .collect();
    let plus = plus.then(|| egui::Rect::from_min_size(egui::pos2(x, top + 1.0), egui::vec2(plus_w, rect.bottom() - top - 3.0)));
    (cells, plus)
}

/// The type a row of tabs at `rect` writes its names in.
fn tab_row_font(rect: egui::Rect) -> egui::FontId {
    crate::settings::ui_font((rect.height() * 0.47).clamp(9.0, 20.0))
}

/// A layout's tabs as a solid row across `rect` - the layout sits beneath it: a tab per name,
/// the one showing (`active`) lit, a click showing another. While the layout is edited
/// (`edit`), a double click renames a tab in place, the tab showing has a × to take it off,
/// and a + at the end adds one. Nothing is drawn for fewer than two tabs.
pub(crate) fn tab_row(ui: &mut egui::Ui, rect: egui::Rect, names: &[String], active: usize, edit: bool, id: egui::Id) -> TabRowAct {
    let mut act = TabRowAct::default();
    if names.len() < 2 {
        return act;
    }
    let active = active.min(names.len() - 1);
    let v = ui.visuals().clone();
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    painter.rect_filled(rect, 0.0, v.panel_fill);
    painter.hline(rect.x_range(), rect.bottom() - 0.5, v.widgets.noninteractive.bg_stroke);
    let font = tab_row_font(rect);
    let (cells, plus) = tab_row_cells(ui, rect, names, active, edit);
    let rename_id = id.with("rename");
    // The tab being renamed and what's typed so far, and whether its box has had focus yet.
    let mut renaming: Option<(usize, String, bool)> = if edit { ui.ctx().data(|d| d.get_temp(rename_id)) } else { None };
    let top_round = egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 };
    for (i, (cell, name)) in cells.iter().zip(names).enumerate() {
        let lit = i == active;
        // Click and drag, so a press here is the tab's and not the canvas's under it.
        let resp = ui.interact(*cell, id.with(("tab", i)), egui::Sense::click_and_drag());
        let fill = if lit {
            v.selection.bg_fill
        } else if resp.hovered() {
            v.widgets.hovered.weak_bg_fill
        } else {
            v.widgets.inactive.weak_bg_fill
        };
        painter.rect_filled(*cell, top_round, fill);
        let text_col = if lit { v.strong_text_color() } else { v.text_color() };
        let close = (edit && lit).then(|| {
            let s = 14.0_f32.min(cell.height() - 4.0);
            egui::Rect::from_center_size(egui::pos2(cell.right() - 6.0 - s * 0.5, cell.center().y), egui::vec2(s, s))
        });
        let text_rect = egui::Rect::from_min_max(
            egui::pos2(cell.left() + 6.0, cell.top()),
            egui::pos2(close.map_or(cell.right() - 6.0, |c| c.left() - 2.0), cell.bottom()),
        );
        match renaming.as_mut().filter(|(k, _, _)| *k == i) {
            Some((_, text, focused)) => {
                let edit_resp = ui.put(
                    text_rect.shrink2(egui::vec2(0.0, 3.0)),
                    egui::TextEdit::singleline(text)
                        .id(id.with(("rename_box", i)))
                        .font(font.clone())
                        .margin(egui::vec2(2.0, 1.0)),
                );
                if !*focused {
                    edit_resp.request_focus();
                    *focused = true;
                } else if edit_resp.lost_focus() {
                    if !ui.input(|inp| inp.key_pressed(egui::Key::Escape)) {
                        act.rename = Some((i, text.clone()));
                    }
                    renaming = None;
                }
            }
            None => {
                let g = painter.layout_no_wrap(name.clone(), font.clone(), text_col);
                // A name wider than its tab is cut at the tab's edge; one that fits sits in the
                // middle of it.
                let x = if g.size().x <= text_rect.width() { text_rect.center().x - g.size().x * 0.5 } else { text_rect.left() };
                painter.with_clip_rect(text_rect.intersect(painter.clip_rect())).galley(
                    egui::pos2(x, text_rect.center().y - g.size().y * 0.5),
                    g,
                    text_col,
                );
            }
        }
        if let Some(c) = close {
            let cr = ui.interact(c, id.with(("tab_x", i)), egui::Sense::click_and_drag());
            if cr.hovered() {
                painter.circle_filled(c.center(), c.width() * 0.5 + 1.0, v.widgets.hovered.bg_fill);
            }
            let d = c.width() * 0.22;
            let xs = egui::Stroke::new(1.3_f32, text_col);
            painter.line_segment([c.center() + egui::vec2(-d, -d), c.center() + egui::vec2(d, d)], xs);
            painter.line_segment([c.center() + egui::vec2(d, -d), c.center() + egui::vec2(-d, d)], xs);
            if cr.on_hover_text("Take this tab off: what's on it goes to the tab before").clicked() {
                act.remove = Some(i);
                renaming = None;
            }
        }
        let resp = if edit { resp.on_hover_text("Double-click to rename") } else { resp };
        // A quick click after a click counts as a third (egui's triple click): rename on either.
        if edit && (resp.double_clicked() || resp.triple_clicked()) {
            renaming = Some((i, name.clone(), false));
        } else if resp.clicked() && !lit {
            act.show = Some(i);
        }
    }
    if let Some(p) = plus {
        let resp = ui.interact(p, id.with("tab_add"), egui::Sense::click_and_drag());
        if resp.hovered() {
            painter.rect_filled(p, 4.0, v.widgets.hovered.weak_bg_fill);
        }
        let c = p.center();
        let d = (p.height() * 0.22).max(3.0);
        let s = egui::Stroke::new(1.5_f32, v.text_color());
        painter.line_segment([c - egui::vec2(d, 0.0), c + egui::vec2(d, 0.0)], s);
        painter.line_segment([c - egui::vec2(0.0, d), c + egui::vec2(0.0, d)], s);
        if resp.on_hover_text("Another tab, at the end").clicked() {
            act.add = true;
        }
    }
    if edit {
        ui.ctx().data_mut(|d| match renaming {
            Some(r) => d.insert_temp(rename_id, r),
            None => {
                d.remove_temp::<(usize, String, bool)>(rename_id);
            }
        });
    }
    act
}

/// Add a tab to a layout's `names` and return its index. The first one added makes two -
/// what the layout had is on the first. `None` past [`MAX_LAYOUT_TABS`].
pub(crate) fn add_layout_tab(names: &mut Vec<String>) -> Option<usize> {
    if names.len() >= MAX_LAYOUT_TABS {
        return None;
    }
    if names.len() < 2 {
        let first = names.first().cloned().unwrap_or_else(|| "Tab 1".to_string());
        *names = vec![first];
    }
    let mut k = names.len() + 1;
    while names.iter().any(|n| *n == format!("Tab {k}")) {
        k += 1;
    }
    names.push(format!("Tab {k}"));
    Some(names.len() - 1)
}

/// Take tab `k` off a layout. What was on it goes to the tab before it (the one after, for
/// the first), the tabs after it move down one, and with one left the layout has no tabs.
pub(crate) fn remove_layout_tab(names: &mut Vec<String>, items: &mut [LayoutItem], k: usize) {
    if k >= names.len() {
        return;
    }
    names.remove(k);
    let to = k.saturating_sub(1);
    for it in items.iter_mut() {
        if let Some(t) = it.tab() {
            let t = t as usize;
            let nt = if t == k { to } else if t > k { t - 1 } else { t };
            it.set_tab(Some(nt as u8));
        }
    }
    if names.len() <= 1 {
        names.clear();
        for it in items.iter_mut().filter(|it| it.tab().is_some()) {
            it.set_tab(Some(0));
        }
    }
}

/// Which items of a layout show while tab `active` of `tabs` is shown (`LayoutItem::on_tab`).
pub(crate) fn shown_items(items: &[LayoutItem], active: usize, tabs: usize) -> Vec<bool> {
    items.iter().map(|it| it.on_tab(active, tabs)).collect()
}

/// The tab `active` moved `dir` along a layout's `n` tabs, stopping at the ends; `None` where
/// that's where it is, or the layout has no tabs.
pub(crate) fn stepped_tab(active: usize, n: usize, dir: i32) -> Option<usize> {
    if n < 2 {
        return None;
    }
    let to = (active as i64 + dir as i64).clamp(0, n as i64 - 1) as usize;
    (to != active.min(n - 1)).then_some(to)
}

impl LayoutStateMut<'_> {
    /// Show tab `k`: what was selected goes, as it isn't there any more.
    pub(crate) fn show_tab(&mut self, k: usize) {
        if *self.active_tab != k {
            *self.active_tab = k;
            *self.selected_item = None;
            self.selected_items.clear();
        }
    }

    /// The tab a new object goes on: the one showing, where the layout has tabs.
    pub(crate) fn new_item_tab(&self) -> u8 {
        if self.layout_tabs.len() >= 2 {
            (*self.active_tab).min(self.layout_tabs.len() - 1) as u8
        } else {
            0
        }
    }
}

/// What the layout editor's tab picker shows: the tab names (one, while the layout has none),
/// and adding one while there's room.
fn tab_picker_options(state: &LayoutStateMut<'_>) -> Vec<(usize, String)> {
    let names = if state.layout_tabs.len() >= 2 {
        state.layout_tabs.clone()
    } else {
        vec![state.layout_tabs.first().cloned().unwrap_or_else(|| "Tab 1".to_string())]
    };
    let mut options: Vec<(usize, String)> = names.into_iter().enumerate().collect();
    if state.layout_tabs.len() < MAX_LAYOUT_TABS {
        options.push((NEW_TAB, "+ new tab".to_string()));
    }
    options
}

/// The tab picker's option that adds a tab.
const NEW_TAB: usize = usize::MAX;

/// How wide the layout editor's tab picker is.
pub(crate) fn layout_tab_picker_w(ui: &egui::Ui, state: &LayoutStateMut<'_>) -> f32 {
    let widest = tab_picker_options(state).iter().map(|(_, n)| curve_rows::word_w(ui, n)).fold(0.0, f32::max);
    curve_rows::word_w(ui, "tab") + widest + 26.0
}

/// The layout editor's tab picker: the tab showing, the list to pick another or add one.
pub(crate) fn layout_tab_picker(ui: &mut egui::Ui, state: &mut LayoutStateMut<'_>, h: f32) {
    const NEW: usize = NEW_TAB;
    let owned = tab_picker_options(state);
    let options: Vec<(usize, &str)> = owned.iter().map(|(i, n)| (*i, n.as_str())).collect();
    let active = if state.layout_tabs.len() >= 2 { (*state.active_tab).min(state.layout_tabs.len() - 1) } else { 0 };
    let w = layout_tab_picker_w(ui, state);
    let (r, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    let hover = "Which tab of the layout is showing - each object is on one tab, or on all of them. \
                 Add a tab here; with two or more they're a row across the layout's top, where a \
                 double click renames one and its × takes it off.";
    if let Some(pick) = curve_rows::pill_combo(ui, r, "tab", &active, &options, hover, "layout_tab_picker") {
        if pick == NEW {
            if let Some(k) = add_layout_tab(state.layout_tabs) {
                state.show_tab(k);
            }
        } else {
            state.show_tab(pick);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect_on(tab: Option<u8>) -> LayoutItem {
        let mut it = LayoutItem::Deco(make_default_decoration("rect"));
        it.set_tab(tab);
        it
    }

    /// The first tab added makes two, with what was there on the first.
    #[test]
    fn the_first_tab_added_makes_two() {
        let mut names = Vec::new();
        let items = vec![rect_on(Some(0))];
        assert_eq!(add_layout_tab(&mut names), Some(1));
        assert_eq!(names, ["Tab 1", "Tab 2"]);
        assert_eq!(shown_items(&items, 1, 2), [false]);
        assert_eq!(shown_items(&items, 0, 2), [true]);
        assert_eq!(add_layout_tab(&mut names), Some(2));
        assert_eq!(names, ["Tab 1", "Tab 2", "Tab 3"]);
    }

    /// Removing a tab moves what was on it to the tab before (the one after, for the first)
    /// and the later tabs down; with one left the layout has no tabs.
    #[test]
    fn removing_a_tab_keeps_its_objects() {
        let mut names = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let mut items = vec![rect_on(Some(0)), rect_on(Some(1)), rect_on(Some(2)), rect_on(None)];
        remove_layout_tab(&mut names, &mut items, 1);
        assert_eq!(names, ["A", "C"]);
        let tabs: Vec<Option<u8>> = items.iter().map(|i| i.tab()).collect();
        assert_eq!(tabs, [Some(0), Some(0), Some(1), None]);
        remove_layout_tab(&mut names, &mut items, 0);
        assert!(names.is_empty(), "one tab left is none");
        let tabs: Vec<Option<u8>> = items.iter().map(|i| i.tab()).collect();
        assert_eq!(tabs, [Some(0), Some(0), Some(0), None]);
        assert_eq!(shown_items(&items, 0, 0), [true; 4]);
    }

    /// An object on a tab past the last shows on the last; the step stops at the ends.
    #[test]
    fn tabs_past_the_end_and_steps() {
        let it = rect_on(Some(9));
        assert!(it.on_tab(2, 3));
        assert!(!it.on_tab(1, 3));
        assert_eq!(stepped_tab(0, 3, -1), None);
        assert_eq!(stepped_tab(0, 3, 1), Some(1));
        assert_eq!(stepped_tab(2, 3, 1), None);
        assert_eq!(stepped_tab(0, 0, 1), None);
    }

    /// What's done on the row lands on the layout: a rename (not to nothing), a tab taken off
    /// with the one before showing, one added showing - the selection going whenever another
    /// tab shows - and the row only takes up room with two tabs or more.
    #[test]
    fn the_rows_acts_land_on_the_layout() {
        let mut names = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let mut items = vec![rect_on(Some(2))];
        let (mut active, mut sel, mut sels) = (2usize, Some(0usize), vec![0usize]);
        let changed = TabRowAct { rename: Some((1, "  Bee ".into())), ..Default::default() }
            .apply(&mut names, &mut items, &mut active, &mut sel, &mut sels);
        assert!(changed && names[1] == "Bee");
        assert_eq!((active, sel), (2, Some(0)), "a rename leaves what shows");
        assert!(!TabRowAct { rename: Some((1, " ".into())), ..Default::default() }
            .apply(&mut names, &mut items, &mut active, &mut sel, &mut sels), "never to nothing");
        TabRowAct { remove: Some(2), ..Default::default() }.apply(&mut names, &mut items, &mut active, &mut sel, &mut sels);
        assert_eq!(names, ["A", "Bee"]);
        assert_eq!((active, sel, items[0].tab()), (1, None, Some(1)), "the tab before shows, with what was on it");
        TabRowAct { add: true, ..Default::default() }.apply(&mut names, &mut items, &mut active, &mut sel, &mut sels);
        assert_eq!((names.len(), active), (3, 2));
        let shown = TabRowAct { show: Some(0), ..Default::default() }.apply(&mut names, &mut items, &mut active, &mut sel, &mut sels);
        assert!(!shown && active == 0, "only another tab showing");
        assert_eq!(tab_row_h(1), 0.0);
        assert_eq!(tab_row_h(2), TAB_ROW_H);
    }
}
