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
    /// True for screen-anchored overlay layouts (info / config / menu overlays):
    /// the snap grid is a % of the viewport and elements can keep their aspect
    /// when stretched. False for px-based sub-patch body layouts.
    pub is_overlay: bool,
    /// Overlay layouts: the viewport size elements are stored at (used by the
    /// anchor picker to show what an `Auto` axis resolves to). `None` for
    /// sub-patch layouts and un-baked overlays.
    pub authored_size: Option<[f32; 2]>,
    /// The layout's tabs (`UiSubPatch::layout_tabs`) and the one showing.
    pub layout_tabs: &'a mut Vec<String>,
    pub active_tab: &'a mut usize,
}

impl<'a> LayoutStateMut<'a> {
    pub fn of_subpatch(sp: &'a mut crate::canvas::node::UiSubPatch) -> Self {
        Self {
            items: &mut sp.items,
            snap_enabled: &mut sp.snap_enabled,
            snap_grid_px: &mut sp.snap_grid_px,
            selected_item: &mut sp.selected_item,
            selected_items: &mut sp.selected_items,
            is_overlay: false,
            authored_size: None,
            layout_tabs: &mut sp.layout_tabs,
            active_tab: &mut sp.active_tab,
        }
    }
    pub fn of_overlay(ov: &'a mut crate::canvas::node::OverlayLayout) -> Self {
        let authored_size = ov.authored_size;
        Self {
            items: &mut ov.items,
            snap_enabled: &mut ov.snap_enabled,
            snap_grid_px: &mut ov.snap_grid_px,
            selected_item: &mut ov.selected_item,
            selected_items: &mut ov.selected_items,
            is_overlay: true,
            authored_size,
            layout_tabs: &mut ov.layout_tabs,
            active_tab: &mut ov.active_tab,
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

/// How tall the layout editor's tool buttons are.
const TOOL_H: f32 = 24.0;

/// The layout editor's tools, in a row at the cursor (the caller's layout runs it across):
/// snap - the magnet - and its grid as a value pill, then a drawing for each thing to add. On
/// an overlay, the selection's anchor and whether a stretched pin keeps its shape too. No row
/// wrapper: Easy mode puts these in its header row, an overlay on its toolbar's first row.
/// A word button in the layout toolbar's look: as tall as its squares, in the control font,
/// `accent` lit like a switch that's on (the toolbar's way out).
pub(crate) fn layout_tool_word(ui: &mut egui::Ui, word: &str, hover: &str, accent: bool) -> bool {
    let w = curve_rows::word_w(ui, word) + 6.0;
    let v = ui.visuals();
    let mut text = egui::RichText::new(word).font(simple_bodies::control_font());
    if accent {
        text = text.color(crate::widgets::text_on(v.selection.bg_fill));
    }
    let mut button = egui::Button::new(text).min_size(egui::vec2(w, TOOL_H));
    if accent {
        button = button.fill(v.selection.bg_fill);
    }
    ui.add(button).on_hover_text(hover).clicked()
}

pub(crate) fn layout_toolbar_controls_core(
    ui: &mut egui::Ui,
    state: &mut LayoutStateMut<'_>,
) {
    use super::glyphs::Glyph;
    let mut add_kind: Option<&'static str> = None;
    let node = NodeId(egui::Id::new("layout_toolbar").value() as usize);
    let square = |ui: &mut egui::Ui| ui.allocate_exact_size(egui::vec2(TOOL_H, TOOL_H), egui::Sense::hover()).0;

    let r = square(ui);
    curve_rows::icon_toggle(ui, r, Glyph::Magnet, state.snap_enabled, true,
        "Snap: positions and sizes land on the grid", "layout_snap");
    let overlay = state.is_overlay;
    let enabled = *state.snap_enabled;
    ui.add_enabled_ui(enabled, |ui| {
        let (r, _) = ui.allocate_exact_size(egui::vec2(104.0, TOOL_H), egui::Sense::hover());
        // The grid shows while its pill is under the pointer, so a change to it can be seen.
        if ui.rect_contains_pointer(r) {
            let ctx = ui.ctx().clone();
            // Both read before `data_mut` takes the context: asking it again inside deadlocks.
            let (id, pass) = (grid_peek_id(&ctx), ctx.cumulative_pass_nr());
            ctx.data_mut(|d| d.insert_temp(id, pass));
        }
        if overlay {
            // Overlays snap on a share of the screen: the pill is the cell's size, quantized
            // to cell counts that nest in the 3x3 anchor zones (multiples of 3).
            let cells = (((*state.snap_grid_px).max(9) / 3) * 3).clamp(9, 99);
            let mut pct = 100.0 / cells as f32;
            let shown = format!("{pct:.1}%");
            if curve_rows::real_pill(ui, node, "grid_pct", r, "grid", &mut pct, &shown, 100.0 / 99.0..=100.0 / 9.0, 0.02,
                "The grid's cell, as a share of the screen each way (smaller is finer). It lines up with the 3x3 anchor zones.", 100.0 / crate::canvas::node::default_overlay_divisions() as f32)
            {
                *state.snap_grid_px = (((100.0 / pct.max(0.01)) / 3.0).round() * 3.0).clamp(9.0, 99.0) as u32;
            }
        } else {
            let mut g = f64::from(*state.snap_grid_px);
            let shown = format!("{g:.0} px");
            if curve_rows::count_pill(ui, node, "grid_px", r, "grid", &mut g, &shown, 2.0..=64.0, 0.25,
                "The grid's step, in points (even numbers)", f64::from(crate::canvas::node::default_snap_grid_px()))
            {
                *state.snap_grid_px = ((g.round() as u32).max(2) / 2) * 2;
            }
        }
    });

    // Overlay-only: every element's anchor shown, or only the selection's.
    if overlay {
        let r = square(ui);
        let mut all = overlay_all_anchors(ui.ctx());
        if curve_rows::icon_toggle(ui, r, Glyph::Anchors, &mut all, true,
            "Anchors: show what every element is anchored to, not only the selected one", "layout_all_anchors")
        {
            ui.ctx().data_mut(|d| d.insert_persisted(overlay_all_anchors_id(), all));
        }
    }

    // Overlay-only: anchoring and keep-aspect for the selected element(s).
    if overlay {
        let sel: Vec<usize> = if state.selected_items.is_empty() {
            state.selected_item.iter().copied().collect()
        } else {
            state.selected_items.clone()
        };
        if let Some(followers) = crate::canvas::overlay_body::anchor_pick_armed(ui.ctx()) {
            ui.label(
                egui::RichText::new(format!("⚓ Click a target for {} element(s)…", followers.len()))
                    .small()
                    .color(egui::Color32::from_rgb(150, 200, 255)),
            );
            if layout_tool_word(ui, "Cancel", "Stop picking a target", false) {
                crate::canvas::overlay_body::clear_anchor_pick(ui.ctx());
            }
        } else if !sel.is_empty() {
            // The anchor's menu button as tall as the toolbar's squares, in the control font.
            ui.scope(|ui| {
                ui.spacing_mut().interact_size.y = TOOL_H;
                ui.style_mut().override_font_id = Some(simple_bodies::control_font());
                anchor_picker(ui, state, &sel);
            });
        }
        let modules: Vec<usize> = sel.iter().copied()
            .filter(|&i| matches!(state.items.get(i), Some(LayoutItem::Module(_))))
            .collect();
        if !modules.is_empty() {
            let mut ka = modules.iter().any(|&i| matches!(
                state.items.get(i), Some(LayoutItem::Module(m)) if m.keep_aspect));
            let (r, _) = ui.allocate_exact_size(egui::vec2(curve_rows::word_w(ui, "keep shape") + 8.0, TOOL_H), egui::Sense::hover());
            if curve_rows::word_toggle(ui, r, &mut ka, "keep shape",
                "Stretched across the screen's zones, the pin scales evenly instead of distorting", "layout_keep_aspect")
            {
                for &i in &modules {
                    if let Some(LayoutItem::Module(m)) = state.items.get_mut(i) {
                        m.keep_aspect = ka;
                    }
                }
            }
        }
    }
    ui.separator();
    for (glyph, kind, hover) in [
        (Glyph::AddText, "text", "Add a text"),
        (Glyph::AddRect, "rect", "Add a rectangle"),
        (Glyph::AddEllipse, "ellipse", "Add an ellipse"),
        (Glyph::AddLine, "line", "Add a line"),
        (Glyph::AddPicture, "svg", "Add a picture (an SVG, an icon, a pad's button)"),
    ] {
        // What doesn't fit whole is left out, not cut at the row's end.
        if ui.available_width() < TOOL_H {
            break;
        }
        let r = square(ui);
        if kit::header_icon_button(ui, glyph, hover, r, ("layout_add", kind)) {
            add_kind = Some(kind);
        }
    }
    if ui.available_width() >= layout_tab_picker_w(ui, state) + 12.0 {
        ui.separator();
        layout_tab_picker(ui, state, TOOL_H);
        if layout_tool_word(ui, "duplicate",
            "A new tab with copies of what's on this one - to swap some of it for others on the \
             copy, for a layer, say. What's on every tab stays on every tab.", false)
        {
            duplicate_layout_tab(state);
        }
    }
    if let Some(kind) = add_kind {
        let mut deco = make_default_decoration(kind);
        deco.set_tab(state.new_item_tab());
        state.items.push(LayoutItem::Deco(deco));
        let idx = state.items.len() - 1;
        *state.selected_item = Some(idx);
        *state.selected_items = vec![idx];
    }
}

/// A compact glyph for the toolbar's Anchor button showing the effective anchor
/// point (`Auto` axes shown as their resolved zone). Stretched axes get a ↔/↕.
fn anchor_label(
    a: crate::canvas::node::Anchor,
    px: crate::canvas::node::AnchorAxis,
    sx_auto: bool,
    py: crate::canvas::node::AnchorAxis,
    sy_auto: bool,
) -> String {
    use crate::canvas::node::AnchorAxis::*;
    let link = if a.to.is_some() { "🔗" } else { "" };
    if a.is_auto() {
        return format!("{link}Auto");
    }
    let col = |ax| match ax { Center => 1usize, End => 2, _ => 0 };
    let ex = if a.x == Auto { px } else { a.x };
    let ey = if a.y == Auto { py } else { a.y };
    let sx = if a.x == Auto { sx_auto } else { a.stretch_x };
    let sy = if a.y == Auto { sy_auto } else { a.stretch_y };
    let base = [["↖", "↑", "↗"], ["←", "•", "→"], ["↙", "↓", "↘"]][col(ey)][col(ex)];
    let stretch = match (sx, sy) {
        (true, true) => "⤢",
        (true, false) => "↔",
        (false, true) => "↕",
        (false, false) => "",
    };
    format!("{link}{base}{stretch}")
}

/// Overlay-only anchor picker: a 3×3 grid that picks the anchor POINT (a specific
/// zone / corner / edge / centre) and two INDEPENDENT Stretch toggles. The point
/// fixes which edge/centre stays put; Stretch X/Y scale the size proportionally
/// so an element spanning zones keeps the same relative width/height across
/// aspect ratios. They combine freely (e.g. bottom-centre + Stretch X). `Auto`
/// derives both from placement; its resolved cell + stretch are shown.
/// Changes propagate to every selected element (modules AND decorations).
fn anchor_picker(ui: &mut egui::Ui, state: &mut LayoutStateMut<'_>, sel: &[usize]) {
    use crate::canvas::node::{Anchor, AnchorAxis};
    let primary = state.selected_item.filter(|i| sel.contains(i)).or_else(|| sel.first().copied());
    let (cur, bbox) = primary
        .and_then(|i| state.items.get(i))
        .map(|it| (it.anchor(), it.bbox()))
        .unwrap_or((Anchor::default(), ([0.0, 0.0], [0.0, 0.0])));
    // What each Auto axis resolves to right now (needs the authored viewport).
    let authored = state.authored_size.unwrap_or([1920.0, 1080.0]);
    let (dpx, dsx) = crate::canvas::node::zone_axis(bbox.0[0], bbox.1[0], authored[0]);
    let (dpy, dsy) = crate::canvas::node::zone_axis(bbox.0[1], bbox.1[1], authored[1]);
    // Effective point/stretch (Auto → derived), used to seed the controls.
    let ex = if cur.x == AnchorAxis::Auto { dpx } else { cur.x };
    let ey = if cur.y == AnchorAxis::Auto { dpy } else { cur.y };

    crate::touch::menu_button(ui, format!("⚓ {}", anchor_label(cur, dpx, dsx, dpy, dsy)), |ui| {
        ui.label(egui::RichText::new("Anchor to screen").small().weak());

        let mut sx = if cur.x == AnchorAxis::Auto { dsx } else { cur.stretch_x };
        let mut sy = if cur.y == AnchorAxis::Auto { dsy } else { cur.stretch_y };
        let col = |ax| match ax { AnchorAxis::Center => 1usize, AnchorAxis::End => 2, _ => 0 };
        let mut gx = col(ex);
        let mut gy = col(ey);
        let is_auto = cur.x == AnchorAxis::Auto && cur.y == AnchorAxis::Auto;
        let mut changed = false;
        let mut set_auto = false;

        // 3×3 point grid — a single cell (the anchor point) is highlighted.
        let glyphs = [["↖", "↑", "↗"], ["←", "•", "→"], ["↙", "↓", "↘"]];
        egui::Grid::new("overlay_anchor_grid").spacing([3.0, 3.0]).show(ui, |ui| {
            for row in 0..3usize {
                for c in 0..3usize {
                    let lit = c == gx && row == gy;
                    if ui.selectable_label(lit, egui::RichText::new(glyphs[row][c]).monospace()).clicked() {
                        gx = c;
                        gy = row;
                        changed = true;
                    }
                }
                ui.end_row();
            }
        });
        ui.horizontal(|ui| {
            if ui.checkbox(&mut sx, egui::RichText::new("Stretch X").small())
                .on_hover_text("Scale width with the viewport (keep the same relative width)")
                .changed() { changed = true; }
            if ui.checkbox(&mut sy, egui::RichText::new("Stretch Y").small())
                .on_hover_text("Scale height with the viewport (keep the same relative height)")
                .changed() { changed = true; }
        });
        ui.separator();
        if ui.selectable_label(is_auto, egui::RichText::new("Auto (from placement)").small())
            .on_hover_text("Derive point + stretch from which 3×3 zone the element sits in")
            .clicked()
        {
            set_auto = true;
            changed = true;
        }

        // Apply edits before the object-link section (which mutates the same
        // items) so both can happen in one menu pass. Preserve id/to.
        if changed {
            let points = [AnchorAxis::Start, AnchorAxis::Center, AnchorAxis::End];
            for &i in sel {
                if let Some(it) = state.items.get_mut(i) {
                    let mut a = it.anchor();
                    if set_auto {
                        a.x = AnchorAxis::Auto;
                        a.y = AnchorAxis::Auto;
                        a.stretch_x = false;
                        a.stretch_y = false;
                    } else {
                        a.x = points[gx];
                        a.y = points[gy];
                        a.stretch_x = sx;
                        a.stretch_y = sy;
                    }
                    it.set_anchor(a);
                }
            }
        }

        ui.separator();
        // ── Anchor to another OBJECT ────────────────────────────────────────
        // A linked element follows the target's resolved rect (the X/Y anchors
        // above then act WITHIN that target frame), so framed groups move and
        // stretch together.
        let linked = cur.to.is_some();
        if linked {
            ui.label(egui::RichText::new("Following another element").small().weak());
            if ui.button(egui::RichText::new("Unlink from object").small())
                .on_hover_text("Anchor to the screen again instead of to another element")
                .clicked()
            {
                for &i in sel {
                    if let Some(it) = state.items.get_mut(i) {
                        let mut a = it.anchor();
                        a.to = None;
                        it.set_anchor(a);
                    }
                }
                ui.close();
            }
        } else if ui.button(egui::RichText::new("⚓ Anchor to object…").small())
            .on_hover_text("Then click the element to anchor the selection to")
            .clicked()
        {
            crate::canvas::overlay_body::arm_anchor_pick(ui.ctx(), sel.to_vec());
            ui.close();
        }
    })
    .response
    .on_hover_text("Anchor the selected element(s) to a screen region or another element");
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
    Some((curve_ui_module_id(inner).to_string(), graph_channels_of_node(inner)))
}

/// Channel count for a graph pin's per-channel color row. Response curves
/// expose `min(inputs, outputs)` channels; scopes one per input; the
/// trigscope's index 0 is the trigger; envelope has a single trail color.
pub(crate) fn graph_channels_of_node(inner: &NodeData) -> usize {
    match curve_ui_module_id(inner) {
        "module.response_curve"
        | "module.vec_response_curve"
        | "module.twoway_response_curve" => curve_channels(inner),
        "display.trigscope" => inner.inputs.len().saturating_sub(1).max(1),
        "generator.envelope" => 1,
        // Sensitivity, and the camera speed it produces.
        "module.jsm" => 2,
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
    let room = ui.available_width();
    layout_inspector(ui, state, sel_module, None, Some(room));
}

/// An overlay toolbar's Inspector strip: one slim line, up to about two thirds of the screen
/// - past that its sections go to tabs.
pub(crate) fn overlay_inspector_strip(
    ui: &mut egui::Ui,
    state: &mut LayoutStateMut<'_>,
    sel_module: SelectedModuleInfo,
) {
    let screen = ui.ctx().content_rect().width();
    let room = (screen * 0.7).min(screen - 64.0).max(240.0);
    layout_inspector(ui, state, sel_module, None, Some(room));
}

/// Whether an overlay's editor shows every element's anchor - its point, and the line to what
/// it is anchored to - rather than only the selected elements' (the toolbar's anchor switch).
/// Remembered across runs.
pub(crate) fn overlay_all_anchors_id() -> egui::Id {
    egui::Id::new("fxi_overlay_all_anchors")
}

pub(crate) fn overlay_all_anchors(ctx: &egui::Context) -> bool {
    ctx.data_mut(|d| *d.get_persisted_mut_or_default::<bool>(overlay_all_anchors_id()))
}

/// How much of a layout's snap grid (and an overlay's anchor zones) shows, 0 to 1: it eases
/// in while an object of the layout is moved or resized - `layout_set_moving`, from the frame
/// before, since the grid goes under the objects - or the pointer is on the grid's pill in the
/// toolbar, and out again after. It was there the whole time an object could be moved.
pub(crate) fn layout_grid_t(ctx: &egui::Context, id: egui::Id) -> f32 {
    let moving: bool = ctx.data(|d| d.get_temp(id.with("moving"))).unwrap_or(false);
    let peek_id = grid_peek_id(ctx);
    let peek = ctx.data(|d| d.get_temp::<u64>(peek_id)).is_some_and(|pass| pass + 1 >= ctx.cumulative_pass_nr());
    ctx.animate_bool_with_time(id.with("grid_fade"), moving || peek, 0.15)
}

/// Whether an object of the layout `id` is being moved or resized this frame: the grid shows
/// while one is (`layout_grid_t`).
pub(crate) fn layout_set_moving(ctx: &egui::Context, id: egui::Id, moving: bool) {
    ctx.data_mut(|d| d.insert_temp(id.with("moving"), moving));
}

/// Where the toolbar notes the pass its grid pill was last under the pointer, per viewport (an
/// overlay's toolbar counts its own passes).
fn grid_peek_id(ctx: &egui::Context) -> egui::Id {
    egui::Id::new(("layout_grid_peek", ctx.viewport_id()))
}

/// How opaque a layout object is drawn while its layout is edited: whole, easing to
/// see-through while the pointer is over it (`hovered`) so whatever it covers shows, and
/// back once the pointer leaves. Keyed per object by `id`.
pub(crate) fn layout_hover_opacity(ctx: &egui::Context, id: egui::Id, hovered: bool) -> f32 {
    const SEE_THROUGH: f32 = 0.4;
    1.0 - (1.0 - SEE_THROUGH) * layout_hover_t(ctx, id, hovered)
}

/// How far a layout object being edited has eased into its hovered look - see-through, its
/// outline showing - 0 to 1, by the same easing every frame however often it's asked.
pub(crate) fn layout_hover_t(ctx: &egui::Context, id: egui::Id, hovered: bool) -> f32 {
    ctx.animate_bool_with_time(id, hovered, 0.15)
}

/// The pointer in `ui`'s layer's own coordinates, while it's over `area` there and not over
/// a popup or window above the layer: where a layout being edited looks for what's under it.
pub(crate) fn layout_pointer_local(ui: &egui::Ui, area: egui::Rect) -> Option<egui::Pos2> {
    let global = ui.ctx().input(|i| i.pointer.hover_pos())?;
    let above = ui.ctx().layer_id_at(global).is_some_and(|l| (l.order as u8) > (ui.layer_id().order as u8));
    if above {
        return None;
    }
    let to_global = ui.ctx().layer_transform_to_global(ui.layer_id()).unwrap_or(egui::emath::TSTransform::IDENTITY);
    let local = to_global.inverse() * global;
    (area.contains(local) && ui.clip_rect().contains(local)).then_some(local)
}

/// Which of a dragged layout object's lines catch on the other objects': its edges on theirs
/// (Shift held), or its centre on theirs (Ctrl held).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObjectSnap {
    Edges,
    Centres,
}

impl ObjectSnap {
    /// What the held modifiers ask a move for, if anything: Shift edges, else Ctrl centres.
    pub(crate) fn held(ui: &egui::Ui) -> Option<Self> {
        let m = ui.input(|i| i.modifiers);
        if m.shift {
            Some(Self::Edges)
        } else if m.ctrl || m.mac_cmd {
            Some(Self::Centres)
        } else {
            None
        }
    }
}

/// How far to move a dragged object at `rect` so its edges - or its centre - land on the
/// nearest of the `others`', each axis on its own, within `reach`; and the lines it landed on,
/// across then down, for the editor to draw as guides. All in the same coordinates.
pub(crate) fn snap_to_objects(
    rect: egui::Rect,
    others: &[egui::Rect],
    mode: ObjectSnap,
    reach: f32,
) -> (egui::Vec2, Option<f32>, Option<f32>) {
    let lines = |r: egui::Rect, across: bool| -> [Option<f32>; 2] {
        match (mode, across) {
            (ObjectSnap::Edges, true) => [Some(r.left()), Some(r.right())],
            (ObjectSnap::Edges, false) => [Some(r.top()), Some(r.bottom())],
            (ObjectSnap::Centres, true) => [Some(r.center().x), None],
            (ObjectSnap::Centres, false) => [Some(r.center().y), None],
        }
    };
    let nearest = |across: bool| -> Option<(f32, f32)> {
        let mine = lines(rect, across);
        others
            .iter()
            .flat_map(|o| lines(*o, across).into_iter().flatten())
            .flat_map(|line| mine.into_iter().flatten().map(move |m| (line - m, line)))
            .filter(|(d, _)| d.abs() <= reach)
            .min_by(|a, b| a.0.abs().total_cmp(&b.0.abs()))
    };
    let (x, y) = (nearest(true), nearest(false));
    (egui::vec2(x.map_or(0.0, |b| b.0), y.map_or(0.0, |b| b.0)), x.map(|b| b.1), y.map(|b| b.1))
}

/// A resize with Ctrl held: the object at `pos`, `size` big, has its moving edges - its right
/// and bottom - caught on the nearest of the `others`' edges or centres within `reach`, each
/// axis on its own. About its centre (Alt), the opposite edges move as far the other way.
/// Keeping its shape (Shift, `aspect` = width over height), only the nearer catch holds and
/// the other side follows the shape. Returns the place and size it ends at and the lines it
/// caught on, across then down, for guides. All in the layout's coordinates.
pub(crate) fn snap_resize_to_objects(
    pos: [f32; 2],
    size: [f32; 2],
    others: &[egui::Rect],
    reach: f32,
    about_centre: bool,
    aspect: Option<f32>,
) -> ([f32; 2], [f32; 2], Option<f32>, Option<f32>) {
    let catch = |edge: f32, across: bool| -> Option<(f32, f32)> {
        others
            .iter()
            .filter(|o| o.is_positive())
            .flat_map(|o| if across { [o.left(), o.center().x, o.right()] } else { [o.top(), o.center().y, o.bottom()] })
            .map(|line| (line - edge, line))
            .filter(|(d, _)| d.abs() <= reach)
            .min_by(|a, b| a.0.abs().total_cmp(&b.0.abs()))
    };
    let mut x = catch(pos[0] + size[0], true);
    let mut y = catch(pos[1] + size[1], false);
    if aspect.is_some() {
        match (x, y) {
            (Some(a), Some(b)) if a.0.abs() <= b.0.abs() => y = None,
            (Some(_), Some(_)) => x = None,
            _ => {}
        }
    }
    let k = if about_centre { 2.0 } else { 1.0 };
    let (mut pos, mut size) = (pos, size);
    if let Some((d, _)) = x {
        size[0] += k * d;
        if about_centre {
            pos[0] -= d;
        }
    }
    if let Some((d, _)) = y {
        size[1] += k * d;
        if about_centre {
            pos[1] -= d;
        }
    }
    if let Some(aspect) = aspect.filter(|a| *a > 0.0) {
        if x.is_some() {
            let h = size[0] / aspect;
            if about_centre {
                pos[1] -= (h - size[1]) * 0.5;
            }
            size[1] = h;
        } else if y.is_some() {
            let w = size[1] * aspect;
            if about_centre {
                pos[0] -= (w - size[0]) * 0.5;
            }
            size[0] = w;
        }
    }
    (pos, size, x.map(|c| c.1), y.map(|c| c.1))
}

/// While a layout is edited and a modifier is held, what it does there, in a small panel at
/// the window's bottom left - as the canvas does for its own. Once a pass per viewport, however
/// many layouts are being edited in it.
pub(crate) fn layout_modifier_hint(ctx: &egui::Context, overlay: bool) {
    let m = ctx.input(|i| i.modifiers);
    let ctrl = m.ctrl || m.mac_cmd;
    let mut lines: Vec<&'static str> = Vec::new();
    if m.shift && m.alt {
        lines.push("Shift+Alt+Resize   Keep the shape, about the centre");
    } else if m.shift {
        lines.push("Shift+Drag         Edges catch on other objects' edges");
        lines.push("Shift+Resize       Keep the shape");
        lines.push("Shift+Click        Add to or take from the selection");
        if !overlay {
            lines.push("Shift+Drag empty   Select a region");
        }
        lines.push("Shift+Gradient     An end catches on edges");
    } else if ctrl {
        lines.push("Ctrl+Drag          Centre catches on other objects' centres");
        lines.push("Ctrl+Resize        Edges catch on other objects' edges or centres");
        lines.push("Ctrl+Gradient      An end catches on centres");
    } else if m.alt {
        lines.push("Alt+Resize         Resize about the centre");
        lines.push("Alt+Shift+Resize   ... keeping the shape");
    }
    if lines.is_empty() {
        return;
    }
    // Both read before `data_mut` takes the context: asking it again inside deadlocks.
    let (key, pass) = (egui::Id::new(("layout_modifier_hint", ctx.viewport_id())), ctx.cumulative_pass_nr());
    let first = ctx.data_mut(|d| {
        let shown = d.get_temp::<u64>(key) == Some(pass);
        d.insert_temp(key, pass);
        !shown
    });
    if !first {
        return;
    }
    egui::Area::new(egui::Id::new("layout_modifier_tooltip"))
        .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(12.0, -12.0))
        .order(egui::Order::Tooltip)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
                ui.set_min_width(180.0);
                for line in &lines {
                    ui.label(egui::RichText::new(*line).small().monospace());
                }
            });
        });
}

/// Whether a layout is being edited in `ctx`'s viewport this pass ([`layout_modifier_hint`]
/// ran): the canvas leaves the modifiers' hint to it.
pub(crate) fn layout_hint_shown(ctx: &egui::Context) -> bool {
    let (key, pass) = (egui::Id::new(("layout_modifier_hint", ctx.viewport_id())), ctx.cumulative_pass_nr());
    ctx.data(|d| d.get_temp::<u64>(key)) == Some(pass)
}

/// Draw the guides a snapped drag landed on (`snap_to_objects`), local `across` and `down`
/// lines placed by `origin`, through `area`.
pub(crate) fn paint_snap_guides(painter: &egui::Painter, origin: egui::Pos2, area: egui::Rect, across: &[f32], down: &[f32]) {
    let stroke = egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(255, 110, 200, 220));
    for x in across {
        let x = origin.x + x;
        painter.line_segment([egui::pos2(x, area.top()), egui::pos2(x, area.bottom())], stroke);
    }
    for y in down {
        let y = origin.y + y;
        painter.line_segment([egui::pos2(area.left(), y), egui::pos2(area.right(), y)], stroke);
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
    let ws_key = pin_ws_nat_key(ui.ctx(), outer_id.0, m.inner_node_id, &m.element_id);
    // A card list pinned whole keeps its top part and one card in view: no shorter than
    // that at the scale its width draws it at.
    if let Some(min_body) = ui.ctx().data(|d| d.get_temp::<f32>(super::pinned::pin_min_body_h_id(ws_key))) {
        let scale = (w.max(40.0) / super::pinned::REMAP_DESIGN_W).clamp(0.25, 4.0);
        return (w, h.max(min_body * scale));
    }
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
            tab: 0,
            pos: [16.0, 16.0],
            size: [160.0, 28.0],
            text: "Text".to_string(),
            font_size: 16.0,
            fill: DECO_DEFAULT_FILL,
            outline: [0, 0, 0, 0],
            outline_px: 0.0,
            align: TextAlign::Left,
            valign: crate::canvas::node::TextVAlign::Top,
            anchor: Default::default(),
        },
        "rect" => LayoutDecoration::Rect { tab: 0, fill_gradient: None,
            pos: [16.0, 16.0],
            size: [120.0, 80.0],
            fill: [60, 60, 60, 180],
            stroke: DECO_DEFAULT_STROKE,
            stroke_px: 1.0,
            corner_radius: 4.0,
            anchor: Default::default(),
        },
        "ellipse" => LayoutDecoration::Ellipse { tab: 0, fill_gradient: None,
            pos: [16.0, 16.0],
            size: [100.0, 100.0],
            fill: [60, 60, 60, 180],
            stroke: DECO_DEFAULT_STROKE,
            stroke_px: 1.0,
            anchor: Default::default(),
        },
        "line" => LayoutDecoration::Line {
            tab: 0,
            a: [16.0, 16.0],
            b: [136.0, 16.0],
            stroke: DECO_DEFAULT_STROKE,
            stroke_px: 1.5,
            anchor: Default::default(),
        },
        "svg" => LayoutDecoration::Svg { tab: 0, tint_gradient: None,
            pos: [16.0, 16.0],
            size: [120.0, 120.0],
            svg_data: String::new(),
            rev: 0,
            tint: [255, 255, 255, 0],
            tint_mode: "override".to_string(),
            stroke: [0, 0, 0, 0],
            stroke_px: 0.0,
            icon_key: String::new(),
            anchor: Default::default(),
        },
        _ => LayoutDecoration::Rect { tab: 0, fill_gradient: None,
            pos: [16.0, 16.0],
            size: [120.0, 80.0],
            fill: DECO_DEFAULT_FILL,
            stroke: DECO_DEFAULT_STROKE,
            stroke_px: 1.0,
            corner_radius: 4.0,
            anchor: Default::default(),
        },
    }
}

pub(crate) fn rgba_to_color32(c: [u8; 4]) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
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
    /// A shape's fill gradient (`Some(None)` for a solid fill), carried with its fill.
    fill_gradient: Option<Option<crate::canvas::node::Gradient>>,
    // Graph-pin override (Response Curve / Oscilloscope / Vectorscope). Carried
    // whole so copy/paste-style and multi-select propagation transfer the
    // background, outline, and per-channel line/dot colors together. Applied
    // only onto other graph pins (see `apply_to`).
    graph: Option<crate::canvas::node::PinGraphOverride>,
    /// A module pin's text cap (`ExposedModule::text_max`; `Some(None)`: none).
    text_max: Option<Option<f32>>,
    // Text-pin / switch-pin overrides are deliberately NOT bulk-propagated
    // here (they're per-module-kind and rarely span a mixed selection); the
    // decoration-level fill/stroke covers the common decoration case.
}

impl ItemStyle {
    pub(crate) fn any(&self) -> bool {
        self.fill.is_some() || self.stroke.is_some() || self.stroke_px.is_some()
            || self.corner_radius.is_some() || self.font_size.is_some()
            || self.fill_gradient.is_some() || self.graph.is_some() || self.text_max.is_some()
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
            fill_gradient: match (&self.fill_gradient, &after.fill_gradient) {
                (b, Some(a)) if b.as_ref() != Some(a) => Some(a.clone()),
                _ => None,
            },
            // Clone (not Copy) so picked by reference comparison.
            graph: match (&self.graph, &after.graph) {
                (b, Some(a)) if b.as_ref() != Some(a) => Some(a.clone()),
                _ => None,
            },
            text_max: pick(self.text_max, after.text_max),
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
                if let Some(v) = self.text_max {
                    m.text_max = v;
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
            LayoutDecoration::Rect { fill, fill_gradient, stroke, stroke_px, corner_radius, .. } => {
                if let Some(v) = self.fill { *fill = v; }
                if let Some(v) = &self.fill_gradient { *fill_gradient = v.clone(); }
                if let Some(v) = self.stroke { *stroke = v; }
                if let Some(v) = self.stroke_px { *stroke_px = v; }
                if let Some(v) = self.corner_radius { *corner_radius = v; }
            }
            LayoutDecoration::Ellipse { fill, fill_gradient, stroke, stroke_px, .. } => {
                if let Some(v) = self.fill { *fill = v; }
                if let Some(v) = &self.fill_gradient { *fill_gradient = v.clone(); }
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
        LayoutItem::Deco(LayoutDecoration::Rect { fill, fill_gradient, stroke, stroke_px, corner_radius, .. }) => ItemStyle {
            fill: Some(*fill), stroke: Some(*stroke), stroke_px: Some(*stroke_px),
            corner_radius: Some(*corner_radius), font_size: None,
            fill_gradient: Some(fill_gradient.clone()),
            ..ItemStyle::default()
        },
        LayoutItem::Deco(LayoutDecoration::Ellipse { fill, fill_gradient, stroke, stroke_px, .. }) => ItemStyle {
            fill: Some(*fill), stroke: Some(*stroke), stroke_px: Some(*stroke_px),
            corner_radius: None, font_size: None,
            fill_gradient: Some(fill_gradient.clone()),
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
            text_max: Some(m.text_max),
            ..ItemStyle::default()
        },
    }
}









/// Paint a single decoration into the given body painter. Coordinates are in
/// body-local space; caller already translated `origin` and provides absolute
/// `painter` and `offset` to add to local coords.
pub(crate) fn paint_decoration(painter: &egui::Painter, origin: egui::Pos2, deco: &LayoutDecoration) {
    match deco {
        LayoutDecoration::Rect { pos, size, fill, fill_gradient, stroke, stroke_px, corner_radius, .. } => {
            let r = egui::Rect::from_min_size(
                origin + egui::vec2(pos[0], pos[1]),
                egui::vec2(size[0].max(1.0), size[1].max(1.0)),
            );
            let fcol = rgba_to_color32(*fill);
            if let Some(g) = fill_gradient {
                let shape = egui::epaint::RectShape::filled(r, *corner_radius, egui::Color32::WHITE);
                paint_gradient_fill(painter, egui::Shape::Rect(shape), r, g);
            } else if fcol.a() > 0 {
                painter.rect_filled(r, *corner_radius, fcol);
            }
            let scol = rgba_to_color32(*stroke);
            if scol.a() > 0 && *stroke_px > 0.05 {
                painter.rect_stroke(r, *corner_radius,
                    egui::Stroke::new(*stroke_px, scol),
                    egui::StrokeKind::Inside);
            }
        }
        LayoutDecoration::Ellipse { pos, size, fill, fill_gradient, stroke, stroke_px, .. } => {
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
            if let Some(g) = fill_gradient {
                let shape = egui::Shape::convex_polygon(pts.clone(), egui::Color32::WHITE, egui::Stroke::NONE);
                paint_gradient_fill(painter, shape, r, g);
            } else if fcol.a() > 0 {
                painter.add(egui::Shape::convex_polygon(pts.clone(), fcol, egui::Stroke::NONE));
            }
            let scol = rgba_to_color32(*stroke);
            if scol.a() > 0 && *stroke_px > 0.05 {
                pts.push(pts[0]);
                painter.add(egui::Shape::line(pts, egui::Stroke::new(*stroke_px, scol)));
            }
        }
        LayoutDecoration::Line { a, b, stroke, stroke_px, .. } => {
            let p1 = origin + egui::vec2(a[0], a[1]);
            let p2 = origin + egui::vec2(b[0], b[1]);
            painter.line_segment([p1, p2],
                egui::Stroke::new(*stroke_px, rgba_to_color32(*stroke)));
        }
        LayoutDecoration::Text { pos, size, text, font_size, fill, outline, outline_px, align, valign, .. } => {
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
                        crate::settings::ui_font(*font_size),
                        ocol,
                    );
                }
            }
            painter.text(
                egui::pos2(x, y),
                anchor, text,
                crate::settings::ui_font(*font_size),
                fcol,
            );
        }
        LayoutDecoration::Svg { pos, size, svg_data, rev, tint, tint_mode, tint_gradient, stroke, stroke_px, icon_key, .. } => {
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
            let ctx = painter.ctx();
            // A chosen set/`gp:` icon key wins over a loaded custom SVG and resolves
            // dynamically (gp glyphs restyle to the current pad); else the raw
            // svg_data. `salt` keys the texture cache and, for gp icons, differs per
            // skin so a pad change re-rasterizes.
            let (svg_src, salt): (&str, usize) = if !icon_key.is_empty() {
                match crate::macro_icons::icon_key_svg_bytes(
                    icon_key, crate::macro_icons::current_gp_skin(ctx))
                    .and_then(|b| std::str::from_utf8(b).ok().map(|s| (s, b.as_ptr() as usize)))
                {
                    Some(pair) => pair,
                    None => (svg_data.as_str(), svg_data.as_ptr() as usize),
                }
            } else {
                (svg_data.as_str(), svg_data.as_ptr() as usize)
            };
            if svg_src.is_empty() {
                painter.rect_stroke(r, 2.0,
                    egui::Stroke::new(1.0_f32, crate::settings::tinted_gray(80)),
                    egui::StrokeKind::Inside);
                painter.text(r.center(), egui::Align2::CENTER_CENTER, "SVG",
                    crate::settings::ui_font(11.0), crate::settings::tinted_gray(140));
                return;
            }
            let pw = (r.width().round() as u32).max(1);
            let ph = (r.height().round() as u32).max(1);
            let cache_key = egui::Id::new((
                "deco_svg_tex", salt, *rev, pw, ph, tint_mode.as_str(),
                tint[0], tint[1], tint[2], tint[3],
                tint_gradient.as_ref().map(|g| g.cache_key()),
            ));
            let cached = crate::widgets::tex_cache::get(&ctx, cache_key);
            let tex = match cached {
                Some(t) => Some(t),
                None => {
                    let img = match tint_gradient {
                        // Each pixel tinted the gradient's colour there, as much as its alpha.
                        Some(g) => {
                            let box_size = egui::vec2(pw as f32, ph as f32);
                            rasterize_svg_recolored_by(svg_src, pw, ph, tint_mode, |x, y| {
                                let c = g.sample(egui::vec2(x as f32 + 0.5, y as f32 + 0.5), box_size)
                                    .to_srgba_unmultiplied();
                                [c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32 / 255.0]
                            })
                        }
                        None => rasterize_svg_recolored(svg_src, pw, ph, tint_mode, rgba_to_color32(*tint)),
                    };
                    img
                        .map(|img| {
                            let h = ctx.load_texture(
                                format!("deco-svg-{}-{}", *rev, pw),
                                img,
                                egui::TextureOptions::LINEAR,
                            );
                            crate::widgets::tex_cache::put(&ctx, cache_key, h.clone());
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

#[cfg(test)]
mod object_snap_tests {
    use super::*;

    fn r(x: f32, y: f32, w: f32, h: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h))
    }

    /// Shift: a dragged object's edge catches on another's nearby edge - the same side or
    /// the facing one - across and down on their own, with the line it caught on.
    #[test]
    fn edges_catch_on_the_other_objects_edges() {
        let others = [r(100.0, 100.0, 50.0, 50.0)];
        // Left edge 3 points right of the other's left: pulled onto it, nothing down.
        let (off, x, y) = snap_to_objects(r(103.0, 300.0, 20.0, 20.0), &others, ObjectSnap::Edges, 6.0);
        assert_eq!((off, x, y), (egui::vec2(-3.0, 0.0), Some(100.0), None));
        // Its right edge 4 short of the other's left: abutting.
        let (off, x, _) = snap_to_objects(r(76.0, 300.0, 20.0, 20.0), &others, ObjectSnap::Edges, 6.0);
        assert_eq!((off.x, x), (4.0, Some(100.0)));
        // Out of reach: left alone.
        let (off, x, y) = snap_to_objects(r(120.0, 300.0, 5.0, 5.0), &others, ObjectSnap::Edges, 6.0);
        assert_eq!((off, x, y), (egui::Vec2::ZERO, None, None));
    }

    /// Ctrl: the centre catches on another's centre, and edges don't.
    #[test]
    fn centres_catch_on_the_other_objects_centres() {
        let others = [r(100.0, 100.0, 50.0, 50.0)];
        let (off, x, y) = snap_to_objects(r(117.0, 113.0, 20.0, 20.0), &others, ObjectSnap::Centres, 6.0);
        assert_eq!((off, x, y), (egui::vec2(-2.0, 2.0), Some(125.0), Some(125.0)));
        let (off, _, _) = snap_to_objects(r(103.0, 400.0, 20.0, 20.0), &others, ObjectSnap::Centres, 6.0);
        // Resizing with Ctrl: the right edge catches on the other's left, the bottom on its
        // centre; about the centre, the left moves as far the other way.
        let other = [r(200.0, 100.0, 100.0, 100.0)];
        let (pos, size, x, y) = snap_resize_to_objects([100.0, 100.0], [97.0, 52.0], &other, 6.0, false, None);
        assert_eq!((pos, size, x, y), ([100.0, 100.0], [100.0, 50.0], Some(200.0), Some(150.0)));
        let (pos, size, _, _) = snap_resize_to_objects([100.0, 100.0], [97.0, 20.0], &other, 6.0, true, None);
        assert_eq!((pos, size), ([97.0, 100.0], [103.0, 20.0]));
        // Keeping the shape, the nearer catch holds and the other side follows.
        let (_, size, x, y) = snap_resize_to_objects([100.0, 100.0], [98.0, 48.0], &other, 6.0, false, Some(2.0));
        assert_eq!((size, x, y), ([100.0, 50.0], Some(200.0), None));
        assert_eq!(off.x, 0.0, "an edge on an edge is not a centre on a centre");
    }
}
