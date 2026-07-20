//! Layout-mode editing: toolbar/inspector controls shared by sub-patch
//! layouts and the screen overlay, decoration defaults, ItemStyle,
//! per-kind inspector strip items, decoration painting.

use super::*;



/// Render the full layout-editing controls row: snap toggle + grid step, the
/// "Add" decoration buttons, and the per-selection inspector strip. Shown at
/// the top of the Advanced sub-patch node body in Layout mode AND below the
/// Easy-mode preset bar, so both surfaces expose identical layout tools.
/// Caller is responsible for only invoking this while layout editing is active.
pub(crate) fn layout_editing_controls(
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    outer_id: NodeId,
) {
    let sel_module = subpatch_selected_module_info(snarl, outer_id);
    if let Some(sp) = snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) {
        layout_editing_controls_core(ui, &mut LayoutStateMut::of_subpatch(sp), sel_module);
    }
}

/// Mutable view over the layout-edit state shared by sub-patch layouts
/// (`UiSubPatch`) and the screen overlay (`OverlayLayout`). Lets the toolbar,
/// inspector, and (overlay) body machinery operate on either container.
pub(crate) struct LayoutStateMut<'a> {
    pub items: &'a mut Vec<LayoutItem>,
    pub snap_enabled: &'a mut bool,
    pub snap_grid_px: &'a mut u32,
    pub selected_item: &'a mut Option<usize>,
    pub selected_items: &'a mut Vec<usize>,
}

impl<'a> LayoutStateMut<'a> {
    pub fn of_subpatch(sp: &'a mut crate::canvas::node::UiSubPatch) -> Self {
        Self {
            items: &mut sp.items,
            snap_enabled: &mut sp.snap_enabled,
            snap_grid_px: &mut sp.snap_grid_px,
            selected_item: &mut sp.selected_item,
            selected_items: &mut sp.selected_items,
        }
    }
    pub fn of_overlay(ov: &'a mut crate::canvas::node::OverlayLayout) -> Self {
        Self {
            items: &mut ov.items,
            snap_enabled: &mut ov.snap_enabled,
            snap_grid_px: &mut ov.snap_grid_px,
            selected_item: &mut ov.selected_item,
            selected_items: &mut ov.selected_items,
        }
    }
}

/// Module info the inspector strip needs for the currently-selected item when
/// it's a module pin: (module_id, graph channel count). Precomputed by the
/// caller because resolving it requires the snarl that CONTAINS the pinned
/// node — which differs between sub-patch layouts (the owning sub-patch's
/// inner snarl) and the overlay (per-pin `source_path`).
pub(crate) type SelectedModuleInfo = Option<(String, usize)>;

/// Container-agnostic core of the layout-edit toolbar (snap controls +
/// decoration adders) and inspector strip. See `layout_editing_controls` for
/// the sub-patch wrapper; the overlay toolbar calls this directly.
pub(crate) fn layout_editing_controls_core(
    ui: &mut egui::Ui,
    state: &mut LayoutStateMut<'_>,
    sel_module: SelectedModuleInfo,
) {
    ui.horizontal(|ui| layout_toolbar_controls_core(ui, state));

    // Inspector strip for the currently selected item (+ bulk-style
    // propagation across a multi-selection).
    layout_inspector_strip_core(ui, state, sel_module);
}

/// The snap + decoration-adder controls, WITHOUT a row wrapper — the caller
/// supplies the layout (a `ui.horizontal` in the sub-patch wrapper; the
/// overlay toolbar keeps these on its main row while the inspector strip drops
/// to a second row below). Split out of `layout_editing_controls_core` so the
/// selected-item properties can always live on their own row.
pub(crate) fn layout_toolbar_controls_core(
    ui: &mut egui::Ui,
    state: &mut LayoutStateMut<'_>,
) {
    let mut add_kind: Option<&'static str> = None;
    ui.checkbox(state.snap_enabled, egui::RichText::new("Snap").small())
        .on_hover_text("Snap pinned-element positions and sizes to a grid in Layout mode");
    ui.add_enabled_ui(*state.snap_enabled, |ui| {
        ui.label(egui::RichText::new("grid").small().weak());
        let mut g = *state.snap_grid_px as i32;
        if ui.add(egui::DragValue::new(&mut g)
            .speed(0.5)
            .range(2i32..=64)
            .suffix("px"))
            .on_hover_text("Grid step in pixels (rounded to multiples of 2)")
            .changed()
        {
            *state.snap_grid_px = ((g.max(2)) / 2 * 2) as u32;
        }
    });
    ui.separator();
    ui.label(egui::RichText::new("Add:").small().weak());
    if ui.small_button("T").on_hover_text("Add Text label").clicked() {
        add_kind = Some("text");
    }
    if ui.small_button("▢").on_hover_text("Add Rectangle").clicked() {
        add_kind = Some("rect");
    }
    if ui.small_button("◯").on_hover_text("Add Ellipse").clicked() {
        add_kind = Some("ellipse");
    }
    if ui.small_button("╱").on_hover_text("Add Line").clicked() {
        add_kind = Some("line");
    }
    if ui.small_button("SVG").on_hover_text("Add SVG").clicked() {
        add_kind = Some("svg");
    }
    if let Some(kind) = add_kind {
        let deco = make_default_decoration(kind);
        state.items.push(LayoutItem::Deco(deco));
        let idx = state.items.len() - 1;
        *state.selected_item = Some(idx);
        *state.selected_items = vec![idx];
    }
}

/// Resolve `SelectedModuleInfo` for a sub-patch layout's current selection:
/// looks the pinned node up in the sub-patch's own inner snarl.
pub(crate) fn subpatch_selected_module_info(
    snarl: &Snarl<NodeData>,
    outer_id: NodeId,
) -> SelectedModuleInfo {
    let sp = snarl.get_node(outer_id).and_then(|n| n.subpatch.as_deref())?;
    let idx = sp.selected_item?;
    let LayoutItem::Module(m) = sp.items.get(idx)? else { return None };
    let inner = sp.snarl.get_node(egui_snarl::NodeId(m.inner_node_id))?;
    Some((inner.module_id.clone(), graph_channels_of_node(inner)))
}

/// Channel count for a graph pin's per-channel color row. Response curves
/// expose `min(inputs, outputs)` channels; scopes one per input; the
/// trigscope's index 0 is the trigger; envelope has a single trail color.
pub(crate) fn graph_channels_of_node(inner: &NodeData) -> usize {
    match inner.module_id.as_str() {
        "module.response_curve"
        | "module.vec_response_curve"
        | "module.twoway_response_curve" =>
            inner.inputs.len().min(inner.outputs.len()).max(1),
        "display.trigscope" => inner.inputs.len().saturating_sub(1).max(1),
        "generator.envelope" => 1,
        _ => inner.inputs.len().max(1),
    }
}

/// Container-agnostic core of `layout_inspector_strip`. `sel_module` carries
/// (module_id, graph_channels) for the selected item when it's a module pin —
/// resolved by the caller against the snarl that contains the pinned node.
pub(crate) fn layout_inspector_strip_core(
    ui: &mut egui::Ui,
    state: &mut LayoutStateMut<'_>,
    sel_module: SelectedModuleInfo,
) {
    let Some(idx) = *state.selected_item else { return };

    let inner_mid = sel_module.as_ref().map(|(mid, _)| mid.as_str());
    let is_text_pin   = inner_mid == Some("module.label");
    let is_switch_pin = inner_mid == Some("module.switch");
    let is_input_viewer_pin = inner_mid == Some("module.input_viewer");
    // The 3D controller viewer shares the Input Viewer's style-override STORAGE
    // but has its own inspector (adds view angle / opacity / highlight fade).
    let is_c3d_pin = inner_mid == Some("display.controller3d");
    // Touch Zones / Virtual Menu pins: pad colour + visibility overrides. The
    // override only affects the "field" element's render; on cards/options
    // pins the controls are inert.
    let is_zone_pad_pin = matches!(
        inner_mid,
        Some("module.touch_zones") | Some("module.menu")
    );
    let is_graph_pin = matches!(
        inner_mid,
        Some("module.response_curve")
            | Some("module.vec_response_curve")
            | Some("module.twoway_response_curve")
            | Some("display.oscilloscope")
            | Some("display.vectorscope")
            | Some("display.trigscope")
            | Some("generator.envelope")
    );
    let graph_channels = sel_module.as_ref().map(|(_, ch)| *ch).unwrap_or(1);

    // Snapshot the primary's style BEFORE the inspector edits it (only when a
    // multi-selection is active — single-select needs no propagation).
    let multi: Vec<usize> = state.selected_items.clone();
    let before: Option<ItemStyle> = if multi.len() > 1 {
        state.items.get(idx).map(item_style_of)
    } else { None };

    if idx < state.items.len() {
        match &mut state.items[idx] {
            LayoutItem::Deco(_) => {
                decoration_inspector_strip_item(ui, state.items, idx);
            }
            LayoutItem::Module(_) if is_text_pin => {
                text_pin_inspector_strip_item(ui, state.items, idx);
            }
            LayoutItem::Module(_) if is_switch_pin => {
                switch_pin_inspector_strip_item(ui, state.items, idx);
            }
            LayoutItem::Module(_) if is_input_viewer_pin => {
                input_viewer_pin_inspector_strip_item(ui, state.items, idx);
            }
            LayoutItem::Module(_) if is_zone_pad_pin => {
                menu_pin_inspector_strip_item(ui, state.items, idx);
            }
            LayoutItem::Module(_) if is_c3d_pin => {
                controller3d_pin_inspector_strip_item(ui, state.items, idx);
            }
            LayoutItem::Module(_) if is_graph_pin => {
                graph_pin_inspector_strip_item(ui, state.items, idx, graph_channels);
            }
            _ => {}
        }
    } else {
        *state.selected_item = None;
    }

    // Diff the primary against its pre-edit snapshot and apply the changed
    // style fields to the rest of the selection.
    if let Some(before) = before {
        if let Some(after) = state.items.get(idx).map(item_style_of) {
            let changed = before.diff(&after);
            if changed.any() {
                for &j in multi.iter() {
                    if j == idx { continue; }
                    if let Some(it) = state.items.get_mut(j) {
                        changed.apply_to(it);
                    }
                }
            }
        }
    }
}

/// ctx-data slot holding a copied layout-item style for paste-across.
pub(crate) fn layout_style_clipboard_key() -> egui::Id {
    egui::Id::new("flexinput::layout_style_clipboard")
}

/// Shared per-item right-click context menu for the layout editor. Used for
/// both the selected (primary) item and non-selected items. Sets the various
/// pending-action flags the caller commits after the loop.
#[allow(clippy::too_many_arguments)]
pub(crate) fn layout_item_context_menu(
    ui: &mut egui::Ui,
    idx: usize,
    is_deco: bool,
    has_style_clip: bool,
    n_selected: usize,
    zaction: &mut Option<(usize, &'static str)>,
    delete_idx: &mut Option<usize>,
    dup_request: &mut bool,
    copy_style_from: &mut Option<usize>,
    paste_style: &mut bool,
    menu_header: &str,
) {
    ui.label(egui::RichText::new(menu_header).small().strong());
    ui.separator();
    if ui.button("Send to surface").clicked()   { *zaction = Some((idx, "top"));    ui.close(); }
    if ui.button("Step Up").clicked()           { *zaction = Some((idx, "up"));     ui.close(); }
    if ui.button("Step Down").clicked()         { *zaction = Some((idx, "down"));   ui.close(); }
    if ui.button("Send to background").clicked(){ *zaction = Some((idx, "bottom")); ui.close(); }
    ui.separator();
    // Style copy/paste. Copy stashes THIS item's style; paste applies the
    // clipboard to the whole selection (or this item if nothing else selected).
    if ui.button("Copy style").clicked() { *copy_style_from = Some(idx); ui.close(); }
    ui.add_enabled_ui(has_style_clip, |ui| {
        let label = if n_selected > 1 {
            format!("Paste style to {} selected", n_selected)
        } else {
            "Paste style".to_string()
        };
        if ui.button(label).clicked() { *paste_style = true; ui.close(); }
    });
    // Duplicate — decorations only (module widgets reference an inner node and
    // can't be cloned as standalone layout items). Duplicates the whole
    // decoration selection when multiple are selected.
    if is_deco {
        ui.separator();
        let dlabel = if n_selected > 1 {
            "Duplicate selection".to_string()
        } else {
            "Duplicate".to_string()
        };
        if ui.button(dlabel).clicked() { *dup_request = true; ui.close(); }
    }
    ui.separator();
    if ui.button("Unpin").clicked() { *delete_idx = Some(idx); ui.close(); }
}

/// Clamp a layout-resize candidate size for a Module pin to its no-crop
/// envelope, derived from the cached natural (scale-1.0, minimum-flex-width)
/// content size: width can't go below what the scale floor needs for the
/// text, and height can't demand a larger text scale than the width can hold.
/// This makes the resize handle itself respect "contents never crop out of
/// frame" — to get a taller row (bigger text) you first have to give it
/// enough width for the enlarged labels plus the flexible parts' minimum.
/// Pins without a cached natural (graphs, whole-module) are unconstrained.
pub(crate) fn clamp_pin_frame_to_content(
    ui: &egui::Ui,
    outer_id: NodeId,
    it: &LayoutItem,
    w: f32,
    h: f32,
) -> (f32, f32) {
    let LayoutItem::Module(m) = it else { return (w, h) };
    let ws_key = egui::Id::new(("pin_ws_nat", outer_id.0, m.inner_node_id, m.element_id.as_str()));
    let Some(nat) = ui.ctx().data(|d| d.get_temp::<egui::Vec2>(ws_key)) else { return (w, h) };
    if nat.x < 1.0 || nat.y < 1.0 { return (w, h); }
    // 0.5 is the scale floor in `apply_widget_scale`: any narrower and the
    // text can no longer shrink to fit the frame.
    let w = w.max(nat.x * 0.5);
    // The largest text scale this width can hold (4.0 = the global ceiling —
    // taller than that only adds empty space).
    let s_max = (w / nat.x).min(4.0);
    let h = h.clamp(nat.y * 0.5, nat.y * s_max);
    (w, h)
}

/// Apply a Z-order action against a `Vec<LayoutItem>` (paint order).
pub(crate) fn apply_zorder_action_items(items: &mut Vec<LayoutItem>, idx: usize, act: &str, n: usize) {
    match act {
        "up"     if idx + 1 < n => { items.swap(idx, idx + 1); }
        "down"   if idx > 0     => { items.swap(idx, idx - 1); }
        "top"    if idx + 1 < n => { let it = items.remove(idx); items.push(it); }
        "bottom" if idx > 0     => { let it = items.remove(idx); items.insert(0, it); }
        _ => {}
    }
}

// ── Layout decorations ──────────────────────────────────────────────────────
//
// Decorations are static body items (text labels, SVGs, basic shapes) drawn
// under exposed-module pins. They live on `UiSubPatch::decorations` and are
// edited only in Layout mode via the toolbar + inspector strip.

pub(crate) const DECO_DEFAULT_FILL:    [u8; 4] = [200, 200, 200, 220];
pub(crate) const DECO_DEFAULT_STROKE:  [u8; 4] = [255, 255, 255, 220];

pub(crate) fn make_default_decoration(kind: &str) -> LayoutDecoration {
    match kind {
        "text" => LayoutDecoration::Text {
            pos: [16.0, 16.0],
            size: [160.0, 28.0],
            text: "Text".to_string(),
            font_size: 16.0,
            fill: DECO_DEFAULT_FILL,
            outline: [0, 0, 0, 0],
            outline_px: 0.0,
            align: TextAlign::Left,
            valign: crate::canvas::node::TextVAlign::Top,
        },
        "rect" => LayoutDecoration::Rect {
            pos: [16.0, 16.0],
            size: [120.0, 80.0],
            fill: [60, 60, 60, 180],
            stroke: DECO_DEFAULT_STROKE,
            stroke_px: 1.0,
            corner_radius: 4.0,
        },
        "ellipse" => LayoutDecoration::Ellipse {
            pos: [16.0, 16.0],
            size: [100.0, 100.0],
            fill: [60, 60, 60, 180],
            stroke: DECO_DEFAULT_STROKE,
            stroke_px: 1.0,
        },
        "line" => LayoutDecoration::Line {
            a: [16.0, 16.0],
            b: [136.0, 16.0],
            stroke: DECO_DEFAULT_STROKE,
            stroke_px: 1.5,
        },
        "svg" => LayoutDecoration::Svg {
            pos: [16.0, 16.0],
            size: [120.0, 120.0],
            svg_data: String::new(),
            rev: 0,
            tint: [255, 255, 255, 0],
            tint_mode: "override".to_string(),
            stroke: [0, 0, 0, 0],
            stroke_px: 0.0,
        },
        _ => LayoutDecoration::Rect {
            pos: [16.0, 16.0],
            size: [120.0, 80.0],
            fill: DECO_DEFAULT_FILL,
            stroke: DECO_DEFAULT_STROKE,
            stroke_px: 1.0,
            corner_radius: 4.0,
        },
    }
}

pub(crate) fn rgba_to_color32(c: [u8; 4]) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
}

pub(crate) fn color_button(ui: &mut egui::Ui, label: &str, rgba: &mut [u8; 4]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).small().weak());
        // Overlay decorations blend over a game, so alpha is always meaningful.
        changed = fxi_color_swatch(ui, rgba, label, true);
    });
    changed
}

/// Flattened style snapshot of a layout item, used for bulk-style propagation
/// across a multi-selection. Each field is `Option` so we can both (a) capture
/// only what an item kind actually has, and (b) diff to find what the user
/// changed. `apply_to` writes a changed field onto another item only where that
/// item kind exposes the same field — e.g. changing a Rect's fill while a Line
/// is also selected leaves the Line's (absent) fill untouched.
#[derive(Clone, Default, PartialEq)]
pub(crate) struct ItemStyle {
    fill: Option<[u8; 4]>,
    stroke: Option<[u8; 4]>,
    stroke_px: Option<f32>,
    corner_radius: Option<f32>,
    font_size: Option<f32>,
    // Graph-pin override (Response Curve / Oscilloscope / Vectorscope). Carried
    // whole so copy/paste-style and multi-select propagation transfer the
    // background, outline, and per-channel line/dot colors together. Applied
    // only onto other graph pins (see `apply_to`).
    graph: Option<crate::canvas::node::PinGraphOverride>,
    // Text-pin / switch-pin overrides are deliberately NOT bulk-propagated
    // here (they're per-module-kind and rarely span a mixed selection); the
    // decoration-level fill/stroke covers the common decoration case.
}

impl ItemStyle {
    pub(crate) fn any(&self) -> bool {
        self.fill.is_some() || self.stroke.is_some() || self.stroke_px.is_some()
            || self.corner_radius.is_some() || self.font_size.is_some()
            || self.graph.is_some()
    }
    /// Build the set of fields that differ between `self` (before) and `after`.
    pub(crate) fn diff(&self, after: &ItemStyle) -> ItemStyle {
        fn pick<T: PartialEq + Copy>(b: Option<T>, a: Option<T>) -> Option<T> {
            match (b, a) { (Some(bv), Some(av)) if bv != av => Some(av), _ => None }
        }
        ItemStyle {
            fill: pick(self.fill, after.fill),
            stroke: pick(self.stroke, after.stroke),
            stroke_px: pick(self.stroke_px, after.stroke_px),
            corner_radius: pick(self.corner_radius, after.corner_radius),
            font_size: pick(self.font_size, after.font_size),
            // Clone (not Copy) so picked by reference comparison.
            graph: match (&self.graph, &after.graph) {
                (b, Some(a)) if b.as_ref() != Some(a) => Some(a.clone()),
                _ => None,
            },
        }
    }
    /// Apply each changed field to `it` where that field exists on the item.
    pub(crate) fn apply_to(&self, it: &mut LayoutItem) {
        let d = match it {
            LayoutItem::Deco(d) => d,
            // Graph-pin styling rides on the `graph` payload only; copy/paste
            // and propagation between graph pins go through here.
            LayoutItem::Module(m) => {
                if let Some(g) = &self.graph {
                    let empty = g.background.is_none() && g.outline.is_none()
                        && g.outline_px.is_none() && g.gridline.is_none()
                        && g.channel_colors.iter().all(|c| c.is_none());
                    m.graph_override = if empty { None } else { Some(g.clone()) };
                }
                return;
            }
        };
        match d {
            LayoutDecoration::Text { fill, outline, outline_px, font_size, .. } => {
                if let Some(v) = self.fill { *fill = v; }
                if let Some(v) = self.stroke { *outline = v; }
                if let Some(v) = self.stroke_px { *outline_px = v; }
                if let Some(v) = self.font_size { *font_size = v; }
            }
            LayoutDecoration::Rect { fill, stroke, stroke_px, corner_radius, .. } => {
                if let Some(v) = self.fill { *fill = v; }
                if let Some(v) = self.stroke { *stroke = v; }
                if let Some(v) = self.stroke_px { *stroke_px = v; }
                if let Some(v) = self.corner_radius { *corner_radius = v; }
            }
            LayoutDecoration::Ellipse { fill, stroke, stroke_px, .. } => {
                if let Some(v) = self.fill { *fill = v; }
                if let Some(v) = self.stroke { *stroke = v; }
                if let Some(v) = self.stroke_px { *stroke_px = v; }
            }
            LayoutDecoration::Line { stroke, stroke_px, .. } => {
                if let Some(v) = self.stroke { *stroke = v; }
                if let Some(v) = self.stroke_px { *stroke_px = v; }
            }
            LayoutDecoration::Svg { stroke, stroke_px, .. } => {
                if let Some(v) = self.stroke { *stroke = v; }
                if let Some(v) = self.stroke_px { *stroke_px = v; }
            }
        }
    }
}

/// Snapshot the bulk-propagatable style of a layout item. For Text the
/// `outline`/`outline_px` map onto the generic `stroke`/`stroke_px` slots so a
/// stroke change made on, say, a Rect can carry to a Text outline.
pub(crate) fn item_style_of(it: &LayoutItem) -> ItemStyle {
    match it {
        LayoutItem::Deco(LayoutDecoration::Text { fill, outline, outline_px, font_size, .. }) => ItemStyle {
            fill: Some(*fill), stroke: Some(*outline), stroke_px: Some(*outline_px),
            corner_radius: None, font_size: Some(*font_size),
            ..ItemStyle::default()
        },
        LayoutItem::Deco(LayoutDecoration::Rect { fill, stroke, stroke_px, corner_radius, .. }) => ItemStyle {
            fill: Some(*fill), stroke: Some(*stroke), stroke_px: Some(*stroke_px),
            corner_radius: Some(*corner_radius), font_size: None,
            ..ItemStyle::default()
        },
        LayoutItem::Deco(LayoutDecoration::Ellipse { fill, stroke, stroke_px, .. }) => ItemStyle {
            fill: Some(*fill), stroke: Some(*stroke), stroke_px: Some(*stroke_px),
            corner_radius: None, font_size: None,
            ..ItemStyle::default()
        },
        LayoutItem::Deco(LayoutDecoration::Line { stroke, stroke_px, .. }) => ItemStyle {
            fill: None, stroke: Some(*stroke), stroke_px: Some(*stroke_px),
            corner_radius: None, font_size: None,
            ..ItemStyle::default()
        },
        LayoutItem::Deco(LayoutDecoration::Svg { stroke, stroke_px, .. }) => ItemStyle {
            fill: None, stroke: Some(*stroke), stroke_px: Some(*stroke_px),
            corner_radius: None, font_size: None,
            ..ItemStyle::default()
        },
        // Module pins only carry their graph override into the style snapshot
        // (text/switch pin overrides stay per-kind and aren't bulk-propagated).
        // `graph_override` is only ever populated on graph pins, so this is
        // inert for other module kinds.
        LayoutItem::Module(m) => ItemStyle {
            graph: m.graph_override.clone(),
            ..ItemStyle::default()
        },
    }
}

/// Render the contextual inspector strip for the selected decoration inside
/// a `LayoutItem::Deco`. Bails out cleanly if `idx` is stale or not a deco.
/// Z-order is controlled via the right-click menu, not this strip.
pub(crate) fn decoration_inspector_strip_item(
    ui: &mut egui::Ui,
    items: &mut Vec<LayoutItem>,
    idx: usize,
) {
    if idx >= items.len() { return; }
    let deco = match &mut items[idx] {
        LayoutItem::Deco(d) => d,
        _ => return,
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(deco.type_label()).small().strong());
        ui.separator();
        match deco {
            LayoutDecoration::Text { text, font_size, fill, outline, outline_px, align, valign, .. } => {
                use crate::canvas::node::TextVAlign;
                ui.add(egui::TextEdit::singleline(text).desired_width(140.0).hint_text("text"));
                ui.add(egui::DragValue::new(font_size).speed(0.25).range(6.0f32..=96.0).suffix("px"))
                    .on_hover_text("Font size");
                color_button(ui, "fill", fill);
                color_button(ui, "outline", outline);
                ui.add(egui::DragValue::new(outline_px).speed(0.1).range(0.0f32..=8.0).suffix("px"))
                    .on_hover_text("Outline thickness");
                egui::ComboBox::from_id_salt(("deco_align", idx))
                    .width(70.0)
                    .selected_text(match *align { TextAlign::Left => "Left", TextAlign::Center => "Center", TextAlign::Right => "Right" })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(align, TextAlign::Left, "Left");
                        ui.selectable_value(align, TextAlign::Center, "Center");
                        ui.selectable_value(align, TextAlign::Right, "Right");
                    });
                egui::ComboBox::from_id_salt(("deco_valign", idx))
                    .width(70.0)
                    .selected_text(match *valign { TextVAlign::Top => "Top", TextVAlign::Center => "Middle", TextVAlign::Bottom => "Bottom" })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(valign, TextVAlign::Top, "Top");
                        ui.selectable_value(valign, TextVAlign::Center, "Middle");
                        ui.selectable_value(valign, TextVAlign::Bottom, "Bottom");
                    })
                    .response.on_hover_text("Vertical alignment");
            }
            LayoutDecoration::Rect { fill, stroke, stroke_px, corner_radius, .. } => {
                color_button(ui, "fill", fill);
                color_button(ui, "stroke", stroke);
                ui.add(egui::DragValue::new(stroke_px).speed(0.1).range(0.0f32..=12.0).suffix("px"))
                    .on_hover_text("Stroke thickness");
                ui.add(egui::DragValue::new(corner_radius).speed(0.25).range(0.0f32..=64.0).suffix("r"))
                    .on_hover_text("Corner radius");
            }
            LayoutDecoration::Ellipse { fill, stroke, stroke_px, .. } => {
                color_button(ui, "fill", fill);
                color_button(ui, "stroke", stroke);
                ui.add(egui::DragValue::new(stroke_px).speed(0.1).range(0.0f32..=12.0).suffix("px"))
                    .on_hover_text("Stroke thickness");
            }
            LayoutDecoration::Line { stroke, stroke_px, .. } => {
                color_button(ui, "stroke", stroke);
                ui.add(egui::DragValue::new(stroke_px).speed(0.1).range(0.1f32..=12.0).suffix("px"))
                    .on_hover_text("Line thickness");
            }
            LayoutDecoration::Svg { tint, tint_mode, stroke, stroke_px, svg_data, rev, .. } => {
                if ui.small_button("Load…").on_hover_text("Load SVG file").clicked() {
                    // This inspector strip is reachable from the overlay's edit
                    // toolbar, where the always-on-top overlay would otherwise
                    // sit ON TOP of the (owner-less) native file dialog and hide
                    // it. Drop the overlay's topmost bit around the blocking
                    // pick_file so the dialog is reachable (no-op elsewhere).
                    let picked = crate::overlay::with_overlay_not_topmost(|| {
                        rfd::FileDialog::new().add_filter("SVG", &["svg"]).pick_file()
                    });
                    if let Some(path) = picked {
                        if let Ok(text) = std::fs::read_to_string(&path) {
                            *svg_data = text;
                            *rev = rev.wrapping_add(1);
                        }
                    }
                }
                color_button(ui, "tint", tint);
                egui::ComboBox::from_id_salt(("deco_svg_mode", idx))
                    .width(80.0)
                    .selected_text(tint_mode.as_str())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(tint_mode, "override".to_string(), "override");
                        ui.selectable_value(tint_mode, "additive".to_string(), "additive");
                    });
                color_button(ui, "stroke", stroke);
                ui.add(egui::DragValue::new(stroke_px).speed(0.1).range(0.0f32..=12.0).suffix("px"))
                    .on_hover_text("Frame stroke thickness");
            }
        }
    });
}

/// Inspector strip for the per-pin Text color override (when the selected
/// item is a Text module pin). Operates on `LayoutItem::Module`.
pub(crate) fn text_pin_inspector_strip_item(
    ui: &mut egui::Ui,
    items: &mut Vec<LayoutItem>,
    idx: usize,
) {
    if idx >= items.len() { return; }
    let exp = match &mut items[idx] {
        LayoutItem::Module(m) => m,
        _ => return,
    };
    let mut ov = exp.text_override.clone().unwrap_or_default();
    let mut clear = false;
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("Text pin override").small().strong());
        ui.separator();
        let mut fill_rgba = ov.fill.unwrap_or([220, 220, 220, 255]);
        if color_button(ui, "fill", &mut fill_rgba) { ov.fill = Some(fill_rgba); }
        let mut outline_rgba = ov.outline.unwrap_or([0, 0, 0, 0]);
        if color_button(ui, "outline", &mut outline_rgba) { ov.outline = Some(outline_rgba); }
        let mut opx = ov.outline_px.unwrap_or(0.0);
        if ui.add(egui::DragValue::new(&mut opx).speed(0.1).range(0.0f32..=8.0).suffix("px"))
            .on_hover_text("Outline thickness")
            .changed()
        {
            ov.outline_px = Some(opx);
        }
        if ui.small_button("Reset").on_hover_text("Use Text module's own colors").clicked() {
            clear = true;
        }
    });
    if clear {
        exp.text_override = None;
    } else if ov.fill.is_some() || ov.outline.is_some() || ov.outline_px.is_some() {
        exp.text_override = Some(ov);
    }
}

/// Inspector strip for a pinned Input Viewer board's style (when the selected
/// item is a `module.input_viewer` pin). Delegates to the board module's own
/// control cluster; stores the result on the pin's `iv_style_override`.
/// Inspector-strip controls for a pinned Touch Zones / Virtual Menu field pad:
/// main / highlight colour overrides (each falls back to the module's own
/// colour when unset) plus a visibility mode for live views (always / show on
/// touch / touched zones only). Stored per pin on `menu_style_override`.
pub(crate) fn menu_pin_inspector_strip_item(
    ui: &mut egui::Ui,
    items: &mut Vec<LayoutItem>,
    idx: usize,
) {
    use crate::canvas::node::{MenuStyleOverride, ZoneVisibility};
    if idx >= items.len() { return; }
    let exp = match &mut items[idx] {
        LayoutItem::Module(m) => m,
        _ => return,
    };
    let cleared = MenuStyleOverride { main: None, hi: None, visibility: ZoneVisibility::Always };
    let mut ov = exp.menu_style_override.unwrap_or(cleared);
    let before = ov;
    let mut reset = false;
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("Zone pad style").small().strong());
        ui.separator();
        ui.label(egui::RichText::new("main").small().weak());
        let mut mc = ov.main.unwrap_or(crate::canvas::menu_body::MENU_MAIN_DEFAULT);
        if fxi_color_swatch(ui, &mut mc,
            "Main colour for THIS pinned pad (alpha = pad opacity).\nUnset = the module's own colour.", true)
        {
            ov.main = Some(mc);
        }
        ui.label(egui::RichText::new("highlight").small().weak());
        let mut hc = ov.hi.unwrap_or(crate::canvas::menu_body::MENU_HIGHLIGHT_DEFAULT);
        if fxi_color_swatch(ui, &mut hc,
            "Highlight colour for THIS pinned pad (active zone / affordances).\nUnset = the module's own colour.", true)
        {
            ov.hi = Some(hc);
        }
        ui.separator();
        egui::ComboBox::from_id_salt(("menu_pin_vis", exp.inner_node_id, idx))
            .selected_text(match ov.visibility {
                ZoneVisibility::Always => "Always show",
                ZoneVisibility::OnTouch => "Show on touch",
                ZoneVisibility::TouchedZones => "Touched zones only",
            })
            .width(150.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut ov.visibility, ZoneVisibility::Always, "Always show");
                ui.selectable_value(&mut ov.visibility, ZoneVisibility::OnTouch, "Show on touch")
                    .on_hover_text("Hide the pad unless a touch point / zone is active.");
                ui.selectable_value(&mut ov.visibility, ZoneVisibility::TouchedZones, "Touched zones only")
                    .on_hover_text("Paint only the zone(s) currently touched — no pad chrome.\nRadial menus behave like Show on touch.");
            });
        if ui.small_button("Reset")
            .on_hover_text("Module colours, always shown")
            .clicked()
        {
            reset = true;
        }
    });
    if reset {
        exp.menu_style_override = None;
    } else if ov != before || exp.menu_style_override.is_some() {
        exp.menu_style_override = if ov == cleared { None } else { Some(ov) };
    }
}

pub(crate) fn input_viewer_pin_inspector_strip_item(
    ui: &mut egui::Ui,
    items: &mut Vec<LayoutItem>,
    idx: usize,
) {
    if idx >= items.len() { return; }
    let exp = match &mut items[idx] {
        LayoutItem::Module(m) => m,
        _ => return,
    };
    exp.iv_style_override = crate::canvas::input_viewer::iv_style_inspector(
        ui, exp.inner_node_id, exp.iv_style_override.as_ref());
}

/// Inspector strip for a pinned 3D controller viewer: frame style (bg /
/// highlight accent / outline) plus per-pin display settings — view angle,
/// model opacity, and highlight fade — each overriding the module's own params
/// for THIS pinned instance only. Reset clears the whole override.
pub(crate) fn controller3d_pin_inspector_strip_item(
    ui: &mut egui::Ui,
    items: &mut Vec<LayoutItem>,
    idx: usize,
) {
    use crate::canvas::node::IvStyleOverride;
    if idx >= items.len() { return; }
    let exp = match &mut items[idx] {
        LayoutItem::Module(m) => m,
        _ => return,
    };
    let mut ov = exp.iv_style_override.unwrap_or(IvStyleOverride {
        bg: [18, 18, 22, 255],
        accent: C3D_DEFAULT_ACCENT,
        tint: [255, 255, 255, 255],
        outline: [70, 70, 75, 255],
        outline_px: 0.0,
        c3d_pitch: None,
        c3d_alpha: None,
        c3d_fade: None,
        c3d_composite: None,
    });
    let before = ov;
    let mut reset = false;

    ui.vertical(|ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("3D viewer style").small().strong());
            ui.separator();
            ui.label(egui::RichText::new("bg").small().weak());
            fxi_color_swatch(ui, &mut ov.bg,
                "Frame fill — lower the alpha for a see-through frame over a game.", true);
            ui.label(egui::RichText::new("highlight").small().weak());
            fxi_color_swatch(ui, &mut ov.accent,
                "Active-input highlight: pressed buttons, trigger pull, stick tilt, touch dots, x-ray ghosts.", true);
            ui.label(egui::RichText::new("outline").small().weak());
            fxi_color_swatch(ui, &mut ov.outline, "Frame outline colour.", true);
            ui.add(egui::DragValue::new(&mut ov.outline_px).range(0.0..=6.0).speed(0.05))
                .on_hover_text("Frame outline width (0 = none).");
            if ui.small_button("Reset").on_hover_text("Use default style + the module's own display settings").clicked() {
                reset = true;
            }
        });
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            // Display overrides: initialized from the override, else the module
            // defaults; touching a slider pins it for this instance.
            let mut pitch = ov.c3d_pitch.unwrap_or(C3D_DEFAULT_PITCH_DEG);
            ui.label(egui::RichText::new("view").small().weak());
            if ui.add(egui::Slider::new(&mut pitch, 0.0..=85.0).suffix("°"))
                .on_hover_text("Camera elevation for this pinned instance.")
                .changed() { ov.c3d_pitch = Some(pitch); }
            let mut alpha = ov.c3d_alpha.unwrap_or(1.0);
            ui.label(egui::RichText::new("opacity").small().weak());
            if ui.add(egui::Slider::new(&mut alpha, 0.1..=1.0))
                .on_hover_text("Model see-through for this pinned instance.")
                .changed() { ov.c3d_alpha = Some(alpha); }
            let mut fade = ov.c3d_fade.unwrap_or(0.25);
            ui.label(egui::RichText::new("fade").small().weak());
            if ui.add(egui::Slider::new(&mut fade, 0.05..=2.0).suffix(" s"))
                .on_hover_text("Highlight fade-out time for this pinned instance.")
                .changed() { ov.c3d_fade = Some(fade); }
            let mut comp = ov.c3d_composite.unwrap_or(1.0);
            ui.label(egui::RichText::new("alpha").small().weak());
            if ui.add(egui::Slider::new(&mut comp, 0.05..=1.0))
                .on_hover_text("Widget composite alpha: fades the whole rendered controller as a 2D image (overlay), without the see-through look.")
                .changed() { ov.c3d_composite = Some(comp); }
        });

        // Material colours + model — edit the NODE (shared with the module
        // body and every pinned instance). Current values are published by the
        // pinned renderer each frame; edits ride temp memory back to it.
        let uid = exp.inner_node_id;
        let pub_data = ui.ctx().data(|d| {
            d.get_temp::<(String, crate::model::Scheme)>(egui::Id::new(("c3d_pub", uid)))
        });
        if let Some((model, cur)) = pub_data {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                // Model chooser (Auto + every available model folder).
                ui.label(egui::RichText::new("Model").small().strong());
                let mut sel = model.clone();
                egui::ComboBox::from_id_salt(("c3d_pin_model", uid))
                    .selected_text(sel.clone())
                    .show_ui(ui, |ui| {
                        if ui.selectable_value(&mut sel, "auto".to_string(), "Auto (device)").clicked() {
                            ui.ctx().data_mut(|d| {
                                d.insert_temp(egui::Id::new(("c3d_modeledit", uid)), String::new())
                            });
                        }
                        for m in crate::model::available_models() {
                            if ui.selectable_value(&mut sel, m.clone(), &m).clicked() {
                                ui.ctx().data_mut(|d| {
                                    d.insert_temp(egui::Id::new(("c3d_modeledit", uid)), m.clone())
                                });
                            }
                        }
                    });
                ui.separator();
                ui.label(egui::RichText::new("Colours").small().strong())
                    .on_hover_text("Model colours — shared with the module (not per-pin).");
                for (row_label, groups) in crate::model::material::ROWS {
                    ui.label(egui::RichText::new(*row_label).small().weak());
                    for &g in *groups {
                        let mut col = cur[g as usize];
                        if c3d_color_swatch(ui, &mut col, g.label()) {
                            ui.ctx().data_mut(|d| {
                                d.insert_temp(
                                    egui::Id::new(("c3d_matedit", uid)),
                                    (g.key().to_string(), col),
                                )
                            });
                        }
                    }
                }
                if ui
                    .small_button("Reset colours")
                    .on_hover_text("Reset the model colours to this model's defaults")
                    .clicked()
                {
                    ui.ctx().data_mut(|d| {
                        d.insert_temp(egui::Id::new(("c3d_matreset", uid)), true)
                    });
                }
                if ui
                    .small_button("Save…")
                    .on_hover_text("Save these colours to a .fxcol file")
                    .clicked()
                {
                    controller3d_scheme_save(&cur);
                }
                if ui
                    .small_button("Load…")
                    .on_hover_text("Load colours from a .fxcol file")
                    .clicked()
                {
                    controller3d_scheme_load_async(ui.ctx(), uid);
                }
            });
        }
    });

    if reset {
        exp.iv_style_override = None;
    } else if ov != before || exp.iv_style_override.is_some() {
        exp.iv_style_override = Some(ov);
    }
}

/// Inspector strip for the per-pin Switch color override (when the selected
/// item is a Switch module pin). Exposes fill, outline, and caption colors
/// for both ON and OFF states, plus a shared outline thickness.
pub(crate) fn switch_pin_inspector_strip_item(
    ui: &mut egui::Ui,
    items: &mut Vec<LayoutItem>,
    idx: usize,
) {
    use crate::canvas::node::PinSwitchOverride;
    if idx >= items.len() { return; }
    let exp = match &mut items[idx] {
        LayoutItem::Module(m) => m,
        _ => return,
    };
    let mut ov = exp.switch_override.clone().unwrap_or_default();
    let mut clear = false;

    ui.vertical(|ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Switch ON").small().strong());
            let mut fill = ov.fill_on.unwrap_or([80, 140, 200, 255]);
            if color_button(ui, "fill", &mut fill) { ov.fill_on = Some(fill); }
            let mut outl = ov.outline_on.unwrap_or([180, 200, 220, 255]);
            if color_button(ui, "outline", &mut outl) { ov.outline_on = Some(outl); }
            let mut txt = ov.text_on.unwrap_or([255, 255, 255, 255]);
            if color_button(ui, "text", &mut txt) { ov.text_on = Some(txt); }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Switch OFF").small().strong());
            let mut fill = ov.fill_off.unwrap_or([60, 60, 65, 255]);
            if color_button(ui, "fill", &mut fill) { ov.fill_off = Some(fill); }
            let mut outl = ov.outline_off.unwrap_or([100, 100, 110, 255]);
            if color_button(ui, "outline", &mut outl) { ov.outline_off = Some(outl); }
            let mut txt = ov.text_off.unwrap_or([200, 200, 200, 255]);
            if color_button(ui, "text", &mut txt) { ov.text_off = Some(txt); }
        });
        ui.horizontal_wrapped(|ui| {
            let mut opx = ov.outline_px.unwrap_or(1.0);
            if ui.add(egui::DragValue::new(&mut opx).speed(0.1).range(0.0f32..=8.0).suffix("px"))
                .on_hover_text("Outline thickness (both states)")
                .changed()
            {
                ov.outline_px = Some(opx);
            }
            if ui.small_button("Reset").on_hover_text("Use theme defaults").clicked() {
                clear = true;
            }
        });
    });
    if clear {
        exp.switch_override = None;
    } else {
        let any = ov.fill_on.is_some() || ov.fill_off.is_some()
            || ov.outline_on.is_some() || ov.outline_off.is_some()
            || ov.text_on.is_some() || ov.text_off.is_some()
            || ov.outline_px.is_some();
        exp.switch_override = if any { Some(PinSwitchOverride { ..ov }) } else { None };
    }
}

/// Inspector strip for the per-pin graph color override (Response Curve,
/// Oscilloscope, Vectorscope). Exposes background + outline + outline width,
/// then one color swatch per input channel for the line/dot color. All controls
/// share one wrapped row, so when there isn't enough width (e.g. Easy mode) the
/// channel swatches wrap onto a second row automatically. `n_channels` is the
/// inner module's resolved channel count.
pub(crate) fn graph_pin_inspector_strip_item(
    ui: &mut egui::Ui,
    items: &mut Vec<LayoutItem>,
    idx: usize,
    n_channels: usize,
) {
    use crate::canvas::node::PinGraphOverride;
    if idx >= items.len() { return; }
    let exp = match &mut items[idx] {
        LayoutItem::Module(m) => m,
        _ => return,
    };
    let mut ov = exp.graph_override.clone().unwrap_or_default();
    let mut clear = false;

    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("Graph pin override").small().strong());
        ui.separator();

        // Background: default is the shared 60%-opacity base.
        let mut bg = ov.background.unwrap_or([16, 16, 16, 153]);
        if color_button(ui, "bg", &mut bg) { ov.background = Some(bg); }

        // Outline: off by default (transparent / 0px).
        let mut outl = ov.outline.unwrap_or([180, 180, 180, 255]);
        if color_button(ui, "outline", &mut outl) { ov.outline = Some(outl); }
        let mut opx = ov.outline_px.unwrap_or(0.0);
        if ui.add(egui::DragValue::new(&mut opx).speed(0.1).range(0.0f32..=8.0).suffix("px"))
            .on_hover_text("Outline thickness")
            .changed()
        {
            ov.outline_px = Some(opx);
        }

        // Gridline / axis color. Default matches the brighter label hue
        // (unmultiplied 180,180,180,160 == GRAPH_GRID_DEFAULT premultiplied).
        let mut grid = ov.gridline.unwrap_or([180, 180, 180, 160]);
        if color_button(ui, "grid", &mut grid) { ov.gridline = Some(grid); }

        ui.separator();

        // One line/dot color swatch per channel. Each defaults to the built-in
        // palette entry for that channel.
        ov.channel_colors.resize(n_channels, None);
        for ch in 0..n_channels {
            let default = MULTI_COLORS[ch % MULTI_COLORS.len()];
            let mut col = ov.channel_colors[ch]
                .unwrap_or([default.r(), default.g(), default.b(), 255]);
            if color_button(ui, &format!("Ch{}", ch + 1), &mut col) {
                ov.channel_colors[ch] = Some(col);
            }
        }

        if ui.small_button("Reset").on_hover_text("Use the module's default colors").clicked() {
            clear = true;
        }
    });

    if clear {
        exp.graph_override = None;
    } else {
        let any = ov.background.is_some()
            || ov.outline.is_some()
            || ov.outline_px.is_some()
            || ov.gridline.is_some()
            || ov.channel_colors.iter().any(|c| c.is_some());
        // Drop trailing None channel slots so we don't serialize empty tails.
        while matches!(ov.channel_colors.last(), Some(None)) { ov.channel_colors.pop(); }
        exp.graph_override = if any { Some(PinGraphOverride { ..ov }) } else { None };
    }
}

/// Paint a single decoration into the given body painter. Coordinates are in
/// body-local space; caller already translated `origin` and provides absolute
/// `painter` and `offset` to add to local coords.
pub(crate) fn paint_decoration(painter: &egui::Painter, origin: egui::Pos2, deco: &LayoutDecoration) {
    match deco {
        LayoutDecoration::Rect { pos, size, fill, stroke, stroke_px, corner_radius } => {
            let r = egui::Rect::from_min_size(
                origin + egui::vec2(pos[0], pos[1]),
                egui::vec2(size[0].max(1.0), size[1].max(1.0)),
            );
            let fcol = rgba_to_color32(*fill);
            if fcol.a() > 0 {
                painter.rect_filled(r, *corner_radius, fcol);
            }
            let scol = rgba_to_color32(*stroke);
            if scol.a() > 0 && *stroke_px > 0.05 {
                painter.rect_stroke(r, *corner_radius,
                    egui::Stroke::new(*stroke_px, scol),
                    egui::StrokeKind::Inside);
            }
        }
        LayoutDecoration::Ellipse { pos, size, fill, stroke, stroke_px } => {
            let r = egui::Rect::from_min_size(
                origin + egui::vec2(pos[0], pos[1]),
                egui::vec2(size[0].max(1.0), size[1].max(1.0)),
            );
            let center = r.center();
            let radius = egui::vec2(r.width() * 0.5, r.height() * 0.5);
            // egui has no ellipse primitive; approximate with a polygon of 64 verts.
            let mut pts = Vec::with_capacity(64);
            for i in 0..64 {
                let t = (i as f32) / 64.0 * std::f32::consts::TAU;
                pts.push(egui::pos2(center.x + radius.x * t.cos(), center.y + radius.y * t.sin()));
            }
            let fcol = rgba_to_color32(*fill);
            if fcol.a() > 0 {
                painter.add(egui::Shape::convex_polygon(pts.clone(), fcol, egui::Stroke::NONE));
            }
            let scol = rgba_to_color32(*stroke);
            if scol.a() > 0 && *stroke_px > 0.05 {
                pts.push(pts[0]);
                painter.add(egui::Shape::line(pts, egui::Stroke::new(*stroke_px, scol)));
            }
        }
        LayoutDecoration::Line { a, b, stroke, stroke_px } => {
            let p1 = origin + egui::vec2(a[0], a[1]);
            let p2 = origin + egui::vec2(b[0], b[1]);
            painter.line_segment([p1, p2],
                egui::Stroke::new(*stroke_px, rgba_to_color32(*stroke)));
        }
        LayoutDecoration::Text { pos, size, text, font_size, fill, outline, outline_px, align, valign } => {
            use crate::canvas::node::TextVAlign;
            let r = egui::Rect::from_min_size(
                origin + egui::vec2(pos[0], pos[1]),
                egui::vec2(size[0].max(1.0), size[1].max(1.0)),
            );
            // Horizontal anchor + x from `align`; vertical anchor + y from
            // `valign`. We combine the two into a single Align2 so the glyph
            // run is positioned within the box per both axes.
            let (h_align2_kind, x) = match align {
                TextAlign::Left   => (0u8, r.min.x),
                TextAlign::Center => (1u8, r.center().x),
                TextAlign::Right  => (2u8, r.max.x),
            };
            let (v_kind, y) = match valign {
                TextVAlign::Top    => (0u8, r.min.y),
                TextVAlign::Center => (1u8, r.center().y),
                TextVAlign::Bottom => (2u8, r.max.y),
            };
            let ax = match h_align2_kind { 0 => egui::Align::LEFT, 1 => egui::Align::Center, _ => egui::Align::RIGHT };
            let ay = match v_kind        { 0 => egui::Align::TOP,  1 => egui::Align::Center, _ => egui::Align::BOTTOM };
            let anchor = egui::Align2([ax, ay]);
            let fcol = rgba_to_color32(*fill);
            let ocol = rgba_to_color32(*outline);
            // Cheap text outline: paint 8-direction offset copies first.
            if ocol.a() > 0 && *outline_px > 0.05 {
                for (dx, dy) in [(-1.0,0.0),(1.0,0.0),(0.0,-1.0),(0.0,1.0),(-1.0,-1.0),(1.0,-1.0),(-1.0,1.0),(1.0,1.0)] {
                    painter.text(
                        egui::pos2(x + dx * *outline_px, y + dy * *outline_px),
                        anchor, text,
                        egui::FontId::proportional(*font_size),
                        ocol,
                    );
                }
            }
            painter.text(
                egui::pos2(x, y),
                anchor, text,
                egui::FontId::proportional(*font_size),
                fcol,
            );
        }
        LayoutDecoration::Svg { pos, size, svg_data, rev, tint, tint_mode, stroke, stroke_px } => {
            let r = egui::Rect::from_min_size(
                origin + egui::vec2(pos[0], pos[1]),
                egui::vec2(size[0].max(8.0), size[1].max(8.0)),
            );
            let scol = rgba_to_color32(*stroke);
            if scol.a() > 0 && *stroke_px > 0.05 {
                painter.rect_stroke(r, 0.0,
                    egui::Stroke::new(*stroke_px, scol),
                    egui::StrokeKind::Inside);
            }
            if svg_data.is_empty() {
                painter.rect_stroke(r, 2.0,
                    egui::Stroke::new(1.0, egui::Color32::from_gray(80)),
                    egui::StrokeKind::Inside);
                painter.text(r.center(), egui::Align2::CENTER_CENTER, "SVG",
                    egui::FontId::proportional(11.0), egui::Color32::from_gray(140));
                return;
            }
            let pw = (r.width().round() as u32).max(1);
            let ph = (r.height().round() as u32).max(1);
            let ctx = painter.ctx();
            let cache_key = egui::Id::new((
                "deco_svg_tex", svg_data.as_ptr() as usize, *rev, pw, ph, tint_mode.as_str(),
                tint[0], tint[1], tint[2], tint[3],
            ));
            let cached = ctx.data(|d| d.get_temp::<egui::TextureHandle>(cache_key));
            let tex = match cached {
                Some(t) => Some(t),
                None => {
                    rasterize_svg_recolored(svg_data, pw, ph, tint_mode, rgba_to_color32(*tint))
                        .map(|img| {
                            let h = ctx.load_texture(
                                format!("deco-svg-{}-{}", *rev, pw),
                                img,
                                egui::TextureOptions::LINEAR,
                            );
                            ctx.data_mut(|d| d.insert_temp(cache_key, h.clone()));
                            h
                        })
                }
            };
            if let Some(tex) = tex {
                painter.image(
                    tex.id(), r,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        }
    }
}

// (Old `show_subpatch_decorations` has been folded into `show_subpatch_body`.
//  Decorations + module pins share one Z-order, one selection, and one
