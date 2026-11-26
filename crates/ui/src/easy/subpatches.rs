//! Which of a tab's sub-patches Easy mode shows, and which one is primary.
//!
//! Every top-level sub-patch on a tab's canvas runs, shown or not. Easy mode
//! shows the ones marked for it, each as a section of the tab row above the
//! body (a section holds that sub-patch's own layout tabs), and its Devices
//! panel wires input devices into - and output devices out of - the PRIMARY
//! one only. The others keep whatever wiring Advanced mode gave them: a
//! secondary sub-patch on a tab of its own is another page of controls, not
//! another mode of the patch.
//!
//! Both marks are params on the outer sub-patch node, so they save with the
//! canvas and come back with undo, as is its icon (`easy_icon`): they belong
//! to the sub-patch's place in the patch, not its content, so loading a preset
//! into it keeps them and never makes the preset read "modified".
//! * `easy_primary` - the primary. With none marked, the first sub-patch on the
//!   canvas is, which is the one Easy mode always used.
//! * `easy_show` - shown in Easy mode. Unset, only the primary is, so a
//!   sub-patch added beside one that already exists stays out until it's
//!   ticked. The primary is always in.

use egui_snarl::{NodeId, Snarl};

use crate::canvas::NodeData;

/// The outer sub-patch node param marking the primary sub-patch.
pub(crate) const P_PRIMARY: &str = "easy_primary";
/// The outer sub-patch node param marking a sub-patch shown in Easy mode.
pub(crate) const P_SHOW: &str = "easy_show";
/// The outer sub-patch node param holding its icon.
pub(crate) const P_ICON: &str = "easy_icon";

fn flag(snarl: &Snarl<NodeData>, id: NodeId, key: &str) -> Option<bool> {
    snarl.get_node(id)?.params.get(key)?.as_bool()
}

/// The canvas's sub-patches, in the canvas's own order.
pub(crate) fn all(snarl: &Snarl<NodeData>) -> Vec<NodeId> {
    snarl
        .nodes_ids_data()
        .filter(|(_, n)| n.value.module_id == "subpatch")
        .map(|(id, _)| id)
        .collect()
}

/// The primary sub-patch: the one marked, else the first.
pub(crate) fn primary(snarl: &Snarl<NodeData>) -> Option<NodeId> {
    let all = all(snarl);
    all.iter()
        .copied()
        .find(|&id| flag(snarl, id, P_PRIMARY) == Some(true))
        .or_else(|| all.first().copied())
}

/// Whether Easy mode shows sub-patch `id`.
pub(crate) fn in_easy(snarl: &Snarl<NodeData>, id: NodeId) -> bool {
    primary(snarl) == Some(id) || flag(snarl, id, P_SHOW) == Some(true)
}

/// The sub-patches Easy mode shows, in the tab row's order: the primary, then
/// the others ticked for it in the canvas's order.
pub(crate) fn easy_list(snarl: &Snarl<NodeData>) -> Vec<NodeId> {
    let Some(p) = primary(snarl) else { return Vec::new() };
    let mut list = vec![p];
    list.extend(all(snarl).into_iter().filter(|&id| id != p && flag(snarl, id, P_SHOW) == Some(true)));
    list
}

/// The sub-patch Easy mode shows: `chosen` (an outer node id) while it's still
/// one of Easy mode's, else the primary.
pub(crate) fn shown(snarl: &Snarl<NodeData>, chosen: Option<usize>) -> Option<NodeId> {
    let list = easy_list(snarl);
    chosen
        .map(NodeId)
        .filter(|id| list.contains(id))
        .or_else(|| list.first().copied())
}

/// Make `id` the primary. The one that was keeps its place in Easy mode: it was
/// shown for being primary, and from now on is shown for its own mark. Only
/// the marks - moving the device wiring along is `wiring::move_primary`.
pub(crate) fn set_primary(snarl: &mut Snarl<NodeData>, id: NodeId) {
    let old = primary(snarl);
    for sid in all(snarl) {
        if let Some(n) = snarl.get_node_mut(sid) {
            n.params.remove(P_PRIMARY);
        }
    }
    if let Some(n) = snarl.get_node_mut(id) {
        n.params.insert(P_PRIMARY.into(), serde_json::Value::Bool(true));
    }
    if let Some(o) = old.filter(|&o| o != id) {
        if let Some(n) = snarl.get_node_mut(o) {
            n.params.insert(P_SHOW.into(), serde_json::Value::Bool(true));
        }
    }
}

/// Show sub-patch `id` in Easy mode or not. The primary is always shown.
pub(crate) fn set_in_easy(snarl: &mut Snarl<NodeData>, id: NodeId, on: bool) {
    if let Some(n) = snarl.get_node_mut(id) {
        n.params.insert(P_SHOW.into(), serde_json::Value::Bool(on));
    }
}

/// A copied sub-patch is a new one: in Easy mode only once ticked, and never
/// the primary by being pasted.
pub(crate) fn strip_marks(node: &mut NodeData) {
    if node.module_id == "subpatch" {
        node.params.remove(P_PRIMARY);
        node.params.remove(P_SHOW);
    }
}

/// The canvas is what Easy mode builds on its own - device nodes around at most
/// one sub-patch, and nothing else. Only then are the device nodes Easy mode's
/// to arrange; on any other canvas they stay where the user put them.
pub(crate) fn is_easy_shaped(snarl: &Snarl<NodeData>) -> bool {
    let mut subpatches = 0;
    for (_, n) in snarl.nodes_ids_data() {
        match n.value.module_id.as_str() {
            "device.source" | "device.sink" => {}
            "subpatch" => subpatches += 1,
            _ => return false,
        }
    }
    subpatches <= 1
}

/// The icon a sub-patch's tabs carry before their names ("" for none).
pub(crate) fn icon(snarl: &Snarl<NodeData>, id: NodeId) -> &str {
    snarl
        .get_node(id)
        .and_then(|n| n.params.get(P_ICON))
        .and_then(|v| v.as_str())
        .unwrap_or("")
}

/// A sub-patch's name as its tab shows it.
pub(crate) fn name(snarl: &Snarl<NodeData>, id: NodeId) -> String {
    let Some(n) = snarl.get_node(id) else { return String::new() };
    let sp_name = n.subpatch.as_ref().map_or("", |sp| sp.display_name.as_str());
    [n.display_name.trim(), sp_name.trim()]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or("Sub-patch")
        .to_string()
}

/// How many layout tabs sub-patch `id` shows as: one for a layout without tabs.
pub(crate) fn page_count(snarl: &Snarl<NodeData>, id: NodeId) -> usize {
    snarl
        .get_node(id)
        .and_then(|n| n.subpatch.as_ref())
        .map_or(1, |sp| if sp.layout_tabs.len() >= 2 { sp.layout_tabs.len() } else { 1 })
}

/// Every page of Easy mode's tab row in order: (sub-patch, its layout tab).
pub(crate) fn pages(snarl: &Snarl<NodeData>) -> Vec<(NodeId, usize)> {
    easy_list(snarl)
        .into_iter()
        .flat_map(|id| (0..page_count(snarl, id)).map(move |k| (id, k)))
        .collect()
}

/// The page showing: the shown sub-patch and its tab.
pub(crate) fn current_page(snarl: &Snarl<NodeData>, chosen: Option<usize>) -> Option<(NodeId, usize)> {
    let id = shown(snarl, chosen)?;
    let tab = snarl
        .get_node(id)
        .and_then(|n| n.subpatch.as_ref())
        .map_or(0, |sp| sp.active_tab.min(page_count(snarl, id) - 1));
    Some((id, tab))
}

/// The page `dir` along from the one showing, stopping at either end; `None`
/// with fewer than two pages or at an end.
pub(crate) fn stepped_page(snarl: &Snarl<NodeData>, chosen: Option<usize>, dir: i32) -> Option<(NodeId, usize)> {
    let pages = pages(snarl);
    if pages.len() < 2 {
        return None;
    }
    let at = current_page(snarl, chosen).and_then(|p| pages.iter().position(|q| *q == p))?;
    let to = (at as i64 + dir as i64).clamp(0, pages.len() as i64 - 1) as usize;
    (to != at).then(|| pages[to])
}

/// Show page `tab` of sub-patch `id`, recording it in `chosen`. Layout editing
/// carries over: the page left is locked and the one shown unlocked, so the
/// editor stays on and there's only ever one layout in it. What was selected on
/// the page left goes. Returns whether anything changed.
pub(crate) fn show_page(snarl: &mut Snarl<NodeData>, chosen: &mut Option<usize>, id: NodeId, tab: usize) -> bool {
    let before = current_page(snarl, *chosen);
    let old = shown(snarl, *chosen);
    if old != Some(id) {
        let editing = old
            .and_then(|o| snarl.get_node(o))
            .is_some_and(|n| n.extra.layout_unlocked);
        if let Some(n) = old.and_then(|o| snarl.get_node_mut(o)) {
            n.extra.layout_unlocked = false;
        }
        if editing {
            if let Some(n) = snarl.get_node_mut(id) {
                n.extra.layout_unlocked = true;
            }
        }
    }
    *chosen = Some(id.0);
    if let Some(sp) = snarl.get_node_mut(id).and_then(|n| n.subpatch.as_mut()) {
        let tab = if sp.layout_tabs.len() >= 2 { tab.min(sp.layout_tabs.len() - 1) } else { 0 };
        if sp.active_tab != tab {
            sp.active_tab = tab;
            sp.selected_item = None;
            sp.selected_items.clear();
        }
    }
    current_page(snarl, *chosen) != before
}

/// The emoji offered for a sub-patch's icon.
pub(crate) const ICONS: &[&str] = &[
    "🎮", "🕹", "🎯", "🖱", "⌨", "✋", "👆", "🔫", "⚔", "🛡", "🏹", "💣",
    "🚗", "🏎", "✈", "🚀", "⛵", "🐎", "⚽", "🏀", "🎵", "🎹", "🎧", "🎤",
    "⭐", "❤", "🔥", "💧", "⚡", "❄", "☀", "🌙", "🔧", "⚙", "🔒", "👁",
    "🅰", "🅱", "1⃣", "2⃣", "3⃣", "4⃣", "🔴", "🟢", "🔵", "🟡", "🟣", "⚫",
];

/// An icon picker: the emoji in a grid, the one in use lit, and taking it off.
/// Returns the pick - `Some("")` to take the icon off.
pub(crate) fn icon_grid(ui: &mut eframe::egui::Ui, current: &str) -> Option<String> {
    use eframe::egui;
    let mut pick = None;
    let size = crate::settings::ui_size(18.0);
    egui::Grid::new("subpatch_icon_grid").spacing([2.0, 2.0]).show(ui, |ui| {
        for (i, icon) in ICONS.iter().enumerate() {
            let b = egui::Button::selectable(*icon == current, egui::RichText::new(*icon).size(size));
            if ui.add_sized([size + 10.0, size + 8.0], b).clicked() {
                pick = Some(icon.to_string());
            }
            if i % 12 == 11 {
                ui.end_row();
            }
        }
    });
    ui.add_space(4.0);
    if ui.add_enabled(!current.is_empty(), egui::Button::new("No icon")).clicked() {
        pick = Some(String::new());
    }
    pick
}

/// Set sub-patch `id`'s icon.
pub(crate) fn set_icon(snarl: &mut Snarl<NodeData>, id: NodeId, icon: String) {
    if let Some(n) = snarl.get_node_mut(id) {
        if icon.is_empty() {
            n.params.remove(P_ICON);
        } else {
            n.params.insert(P_ICON.into(), serde_json::Value::String(icon));
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::canvas::node::UiSubPatch;

    pub(crate) fn sub(name: &str, tabs: &[&str]) -> NodeData {
        let mut sp = UiSubPatch::default();
        sp.display_name = name.into();
        sp.layout_tabs = tabs.iter().map(|t| t.to_string()).collect();
        NodeData {
            module_id: "subpatch".into(),
            display_name: name.into(),
            category: String::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            params: Default::default(),
            subpatch: Some(Box::new(sp)),
            extra: Default::default(),
        }
    }

    fn other(module_id: &str) -> NodeData {
        NodeData { module_id: module_id.into(), subpatch: None, ..sub("", &[]) }
    }

    fn canvas(n: usize) -> (Snarl<NodeData>, Vec<NodeId>) {
        let mut s = Snarl::new();
        let ids = (0..n).map(|i| s.insert_node(eframe::egui::pos2(0.0, 0.0), sub(&format!("S{i}"), &[]))).collect();
        (s, ids)
    }

    /// An old patch's lone sub-patch is the primary and in Easy mode, as it always
    /// was; one added beside it is in only once ticked.
    #[test]
    fn the_first_is_primary_and_the_rest_opt_in() {
        let (mut s, ids) = canvas(3);
        assert_eq!(primary(&s), Some(ids[0]));
        assert_eq!(easy_list(&s), vec![ids[0]]);
        set_in_easy(&mut s, ids[2], true);
        assert_eq!(easy_list(&s), vec![ids[0], ids[2]]);
        assert!(in_easy(&s, ids[2]) && !in_easy(&s, ids[1]));
        // The primary can't be taken out.
        set_in_easy(&mut s, ids[0], false);
        assert!(in_easy(&s, ids[0]));
    }

    /// A new primary leads the row, and the old one keeps its tab.
    #[test]
    fn a_new_primary_leads_and_the_old_one_stays() {
        let (mut s, ids) = canvas(3);
        set_primary(&mut s, ids[1]);
        assert_eq!(primary(&s), Some(ids[1]));
        assert_eq!(easy_list(&s), vec![ids[1], ids[0]]);
        set_primary(&mut s, ids[2]);
        assert_eq!(easy_list(&s), vec![ids[2], ids[0], ids[1]]);
    }

    /// A pasted primary is a plain sub-patch.
    #[test]
    fn a_pasted_subpatch_loses_its_marks() {
        let (mut s, ids) = canvas(2);
        set_primary(&mut s, ids[1]);
        let mut copy = s.get_node(ids[1]).unwrap().clone();
        strip_marks(&mut copy);
        let pasted = s.insert_node(eframe::egui::pos2(0.0, 0.0), copy);
        assert_eq!(primary(&s), Some(ids[1]));
        assert!(!in_easy(&s, pasted));
    }

    /// The shown sub-patch falls back to the primary once it leaves Easy mode.
    #[test]
    fn the_shown_one_falls_back_to_the_primary() {
        let (mut s, ids) = canvas(2);
        set_in_easy(&mut s, ids[1], true);
        assert_eq!(shown(&s, Some(ids[1].0)), Some(ids[1]));
        set_in_easy(&mut s, ids[1], false);
        assert_eq!(shown(&s, Some(ids[1].0)), Some(ids[0]));
        assert_eq!(shown(&s, None), Some(ids[0]));
    }

    /// Pages run through each sub-patch's layout tabs in turn; LB / RB step across
    /// sub-patches and stop at the ends.
    #[test]
    fn pages_step_across_subpatches() {
        let mut s = Snarl::new();
        let a = s.insert_node(eframe::egui::pos2(0.0, 0.0), sub("A", &["A1", "A2"]));
        let b = s.insert_node(eframe::egui::pos2(0.0, 0.0), sub("B", &[]));
        set_in_easy(&mut s, b, true);
        assert_eq!(pages(&s), vec![(a, 0), (a, 1), (b, 0)]);
        let mut chosen = None;
        assert_eq!(stepped_page(&s, chosen, -1), None);
        let (id, tab) = stepped_page(&s, chosen, 1).unwrap();
        assert!(show_page(&mut s, &mut chosen, id, tab));
        assert_eq!(current_page(&s, chosen), Some((a, 1)));
        let (id, tab) = stepped_page(&s, chosen, 1).unwrap();
        show_page(&mut s, &mut chosen, id, tab);
        assert_eq!(current_page(&s, chosen), Some((b, 0)));
        assert_eq!(stepped_page(&s, chosen, 1), None);
        // Back into A lands on its tab that was showing.
        assert_eq!(stepped_page(&s, chosen, -1), Some((a, 1)));
    }

    /// Layout editing follows the page into another sub-patch.
    #[test]
    fn layout_editing_follows_the_page() {
        let (mut s, ids) = canvas(2);
        set_in_easy(&mut s, ids[1], true);
        s.get_node_mut(ids[0]).unwrap().extra.layout_unlocked = true;
        let mut chosen = None;
        show_page(&mut s, &mut chosen, ids[1], 0);
        assert!(!s.get_node(ids[0]).unwrap().extra.layout_unlocked);
        assert!(s.get_node(ids[1]).unwrap().extra.layout_unlocked);
    }

    /// Device nodes are Easy mode's to arrange only around a lone sub-patch.
    #[test]
    fn only_a_device_only_canvas_is_easy_shaped() {
        let (mut s, _) = canvas(1);
        s.insert_node(eframe::egui::pos2(0.0, 0.0), other("device.source"));
        s.insert_node(eframe::egui::pos2(0.0, 0.0), other("device.sink"));
        assert!(is_easy_shaped(&s));
        s.insert_node(eframe::egui::pos2(0.0, 0.0), sub("Second", &[]));
        assert!(!is_easy_shaped(&s));
        let (mut s, _) = canvas(1);
        s.insert_node(eframe::egui::pos2(0.0, 0.0), other("module.response_curve"));
        assert!(!is_easy_shaped(&s));
    }
}
