//! Screen-overlay body: renders a tab's [`OverlayLayout`] items (pinned
//! module elements + decorations) onto the transparent overlay viewport, and
//! implements the overlay's edit-mode interactions (select / multi-select /
//! drag / resize / z-order / context menus).
//!
//! Interaction model note: this is intentionally NOT a copy of
//! `show_subpatch_body`'s manual-pointer machinery. That code fights
//! egui-snarl's node-frame interact (which claims primary drags before body
//! widgets see them); the overlay is a plain egui surface, so ordinary
//! `ui.interact` responses work — items are registered bottom→top in paint
//! order, so the topmost item naturally wins pointer contention.
//!
//! Item-level machinery (inspector strips, z-order, decorations, context
//! menu, pin natural-size clamping, the pinned-element render dispatch) is
//! shared with sub-patch layouts via the helpers in `viewer.rs`
//! (`LayoutStateMut`, `layout_editing_controls_core`, …).

use egui_snarl::{NodeId, Snarl};
use flexinput_core::Signal;

use super::node::{resolve_layout_deco, resolve_layout_rect, zone_axis, AnchorAxis, LayoutDecoration, LayoutItem, NodeData, OverlayLayout};
use super::viewer::{
    apply_zorder_action_items, clamp_pin_frame_to_content, graph_channels_of_node,
    item_style_of, layout_item_context_menu, layout_style_clipboard_key, make_default_decoration,
    paint_decoration, render_pinned_element, ItemStyle, SelectedModuleInfo,
};

/// Sentinel "outer node" id used for overlay pins whose module lives directly
/// on the tab canvas (`source_path == []`). Feeds the pin natural-size cache
/// key and is never looked up as a real node. Pins from inside a first-level
/// sub-patch use the sub-patch node id instead — deliberately the same key
/// the sub-patch's own layout uses, so both share one natural-size entry
/// (identical content ⇒ identical natural size).
pub(crate) const OVERLAY_TOP_OUTER: NodeId = NodeId(usize::MAX);

/// Ctx temp-data slot: while set, the overlay is in "pick an anchor target"
/// mode — the stored indices are the follower items awaiting a target. Set by
/// the toolbar's anchor picker; consumed by the next item-click in edit mode.
fn anchor_pick_id() -> egui::Id {
    egui::Id::new("fxi_overlay_anchor_pick")
}

/// Arm anchor-target pick mode for the given follower item indices.
pub(crate) fn arm_anchor_pick(ctx: &egui::Context, followers: Vec<usize>) {
    ctx.data_mut(|d| d.insert_temp(anchor_pick_id(), followers));
}

/// The follower indices awaiting an anchor target, if pick mode is armed.
pub(crate) fn anchor_pick_armed(ctx: &egui::Context) -> Option<Vec<usize>> {
    ctx.data(|d| d.get_temp::<Vec<usize>>(anchor_pick_id()))
        .filter(|v| !v.is_empty())
}

/// Cancel anchor-target pick mode.
pub(crate) fn clear_anchor_pick(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove_temp::<Vec<usize>>(anchor_pick_id()));
}

/// Resolve an overlay pin's node: `source_path == []` → node on the tab
/// canvas; `[sp]` → node inside first-level sub-patch `sp`. Deeper paths are
/// reserved and resolve to `None` (the pin shows as stale, never panics).
pub(crate) fn resolve_overlay_module<'s>(
    tab_snarl: &'s Snarl<NodeData>,
    path: &[usize],
    inner_node_id: usize,
) -> Option<&'s NodeData> {
    match path {
        [] => tab_snarl.get_node(NodeId(inner_node_id)),
        [sp] => tab_snarl
            .get_node(NodeId(*sp))?
            .subpatch
            .as_ref()?
            .snarl
            .get_node(NodeId(inner_node_id)),
        _ => None,
    }
}

/// `SelectedModuleInfo` for the overlay's current selection (for the shared
/// inspector strip). Mirrors `subpatch_selected_module_info`.
pub(crate) fn overlay_selected_module_info(
    tab_snarl: &Snarl<NodeData>,
    overlay: &OverlayLayout,
) -> SelectedModuleInfo {
    let idx = overlay.selected_item?;
    let LayoutItem::Module(m) = overlay.items.get(idx)? else { return None };
    let inner = resolve_overlay_module(tab_snarl, &m.source_path, m.inner_node_id)?;
    Some((inner.module_id.clone(), graph_channels_of_node(inner)))
}

/// Render + (in edit mode) run interactions for the overlay layout. `rect` is
/// the overlay's full screen area in the overlay viewport's coords; item
/// positions are relative to its origin.
#[allow(clippy::too_many_arguments)]
pub(crate) fn show_overlay_body(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    tab_snarl: &mut Snarl<NodeData>,
    overlay: &mut OverlayLayout,
    edit: bool,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    panic_shortcut: &crate::app::PanicShortcut,
) {
    let origin = rect.min;
    let cur = [rect.width().max(1.0), rect.height().max(1.0)];

    // Overlay layouts are screen-anchored: element positions are stored at the
    // size they were authored (`authored_size`) and re-anchored to the current
    // viewport for display (see `resolve_anchored_rect`). While EDITING, bake the
    // re-anchored positions into the items once (only when the editing viewport
    // differs from the authored one) and record the current size — so editing is
    // 1:1 with what's shown and anchoring resolves to identity during the edit.
    if edit {
        if let Some(a) = overlay.authored_size {
            if (a[0] - cur[0]).abs() > 0.5 || (a[1] - cur[1]).abs() > 0.5 {
                crate::canvas::node::reanchor_layout_items(&mut overlay.items, a, cur);
            }
        }
        overlay.authored_size = Some(cur);
    }
    let authored = overlay.authored_size.unwrap_or(cur);

    // Snapshot items for iteration (renderers need the tab snarl mutably).
    let items: Vec<LayoutItem> = overlay.items.clone();
    if items.is_empty() {
        return;
    }

    // Per-item module info: (module_id, display_name, exists).
    let infos: Vec<(String, String, bool)> = items
        .iter()
        .map(|it| {
            if let LayoutItem::Module(m) = it {
                let n = resolve_overlay_module(tab_snarl, &m.source_path, m.inner_node_id);
                (
                    n.map(|n| n.module_id.clone()).unwrap_or_default(),
                    n.map(|n| n.display_name.clone()).unwrap_or_default(),
                    n.is_some(),
                )
            } else {
                (String::new(), String::new(), true)
            }
        })
        .collect();

    // Snapshot of the tab snarl for pins inside sub-patches whose renderers
    // walk the AutoMap parent chain. Not gated per element id — a hardcoded
    // list here silently broke device resolution for every newly instrumented
    // element (the Input Viewer's "viewer" showed "No device" on the overlay
    // while working everywhere else).
    let needs_outer_snapshot = items.iter().any(|it| matches!(it,
        LayoutItem::Module(m) if !m.source_path.is_empty()
    ));
    let outer_snapshot: Option<Snarl<NodeData>> =
        if needs_outer_snapshot { Some(tab_snarl.clone()) } else { None };

    // Overlay snap divides the viewport into N equal cells PER AXIS (N a
    // multiple of 3, so grid lines nest inside the 3×3 anchor zones): 30 → 3.33%,
    // 18 → 5.56% — evenly divides the screen regardless of aspect, and the step
    // is resolution-independent.
    let snap_enabled = overlay.snap_enabled;
    // Cells per axis, kept a multiple of 3 (floor 9) so grid lines always nest
    // inside the 3×3 anchor zones. The toolbar constrains it on edit; floor here
    // guards legacy px values loaded into this field.
    let divisions = { let d = overlay.snap_grid_px.max(9); (d / 3) * 3 } as f32;
    let step_x = cur[0] / divisions;
    let step_y = cur[1] / divisions;
    let snap_x = |v: f32| -> f32 {
        if snap_enabled && step_x > 0.5 { (v / step_x).round() * step_x } else { v }
    };
    let snap_y = |v: f32| -> f32 {
        if snap_enabled && step_y > 0.5 { (v / step_y).round() * step_y } else { v }
    };
    let shift_held = ui.input(|i| i.modifiers.shift);
    const RESIZE_HANDLE: f32 = 14.0;
    const MIN_W: f32 = 32.0;
    const MIN_H: f32 = 18.0;

    // Edit-mode snap grid, drawn under everything.
    if edit && snap_enabled && step_x >= 2.0 && step_y >= 2.0 {
        let painter = ui.painter().with_clip_rect(rect);
        let stroke = egui::Stroke::new(0.5, egui::Color32::from_rgba_unmultiplied(150, 200, 255, 28));
        let mut x = 0.0f32;
        while x <= rect.width() {
            painter.line_segment(
                [egui::pos2(origin.x + x, rect.min.y), egui::pos2(origin.x + x, rect.max.y)],
                stroke,
            );
            x += step_x;
        }
        let mut y = 0.0f32;
        while y <= rect.height() {
            painter.line_segment(
                [egui::pos2(rect.min.x, origin.y + y), egui::pos2(rect.max.x, origin.y + y)],
                stroke,
            );
            y += step_y;
        }
    }

    // Edit-mode 3×3 anchor-zone guide: shows the regions that decide each
    // element's anchor (single zone → holds that corner/edge; spanning ≥2 →
    // stretches). Drawn brighter than the snap grid so the two read apart.
    if edit {
        let painter = ui.painter().with_clip_rect(rect);
        let stroke = egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(150, 200, 255, 60));
        for k in 1..=2 {
            let x = origin.x + rect.width() * (k as f32 / 3.0);
            painter.line_segment([egui::pos2(x, rect.min.y), egui::pos2(x, rect.max.y)], stroke);
            let y = origin.y + rect.height() * (k as f32 / 3.0);
            painter.line_segment([egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)], stroke);
        }
    }

    // Anchor-target pick hint banner (top-centre) while armed.
    if edit && anchor_pick_armed(ui.ctx()).is_some() {
        let painter = ui.painter().with_clip_rect(rect);
        let msg = "Click an element to anchor the selection to it — Esc to cancel";
        let font = egui::FontId::proportional(15.0);
        let galley = painter.layout_no_wrap(msg.to_string(), font, egui::Color32::WHITE);
        let pad = egui::vec2(12.0, 7.0);
        let center = egui::pos2(rect.center().x, rect.min.y + 28.0);
        let bg = egui::Rect::from_center_size(center, galley.size() + pad * 2.0);
        painter.rect_filled(bg, 8.0, egui::Color32::from_rgba_unmultiplied(40, 60, 90, 235));
        painter.rect_stroke(
            bg, 8.0,
            egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(150, 200, 255, 220)),
            egui::StrokeKind::Inside,
        );
        painter.galley(bg.center() - galley.size() * 0.5, galley, egui::Color32::WHITE);
    }

    // ── Background interactions (registered FIRST so items win on top) ──────
    let mut bg_add: Option<&'static str> = None;
    let mut clicked_empty = false;
    let mut marquee_commit: Option<egui::Rect> = None;
    if edit {
        let bg = ui.interact(
            rect,
            egui::Id::new("fxi_ov_bg"),
            egui::Sense::click_and_drag(),
        );
        if bg.clicked() {
            clicked_empty = true;
        }
        bg.context_menu(|ui| {
            ui.menu_button("Add", |ui| {
                if ui.button("Text").clicked()      { bg_add = Some("text");    ui.close(); }
                if ui.button("Rectangle").clicked() { bg_add = Some("rect");    ui.close(); }
                if ui.button("Ellipse").clicked()   { bg_add = Some("ellipse"); ui.close(); }
                if ui.button("Line").clicked()      { bg_add = Some("line");    ui.close(); }
                if ui.button("SVG").clicked()       { bg_add = Some("svg");     ui.close(); }
            });
        });
        // Marquee (rubber-band) multi-select on background drag.
        let marquee_key = egui::Id::new("fxi_ov_marquee");
        if bg.drag_started_by(egui::PointerButton::Primary) {
            if let Some(p) = bg.interact_pointer_pos() {
                ui.ctx().data_mut(|d| d.insert_temp(marquee_key, [p.x, p.y]));
            }
        }
        let start: Option<[f32; 2]> = ui.ctx().data(|d| d.get_temp(marquee_key));
        if let Some(start) = start {
            let cur = ui.ctx().input(|i| i.pointer.latest_pos());
            if bg.dragged_by(egui::PointerButton::Primary) {
                if let Some(p) = cur {
                    let r = egui::Rect::from_two_pos(egui::pos2(start[0], start[1]), p);
                    let painter = ui.painter().with_clip_rect(rect);
                    painter.rect_filled(r, 0.0, egui::Color32::from_rgba_unmultiplied(120, 180, 255, 30));
                    painter.rect_stroke(
                        r, egui::CornerRadius::ZERO,
                        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(150, 200, 255, 200)),
                        egui::StrokeKind::Inside,
                    );
                }
            }
            if bg.drag_stopped() {
                if let Some(p) = cur {
                    let r = egui::Rect::from_two_pos(egui::pos2(start[0], start[1]), p);
                    if r.area() >= 16.0 {
                        marquee_commit = Some(r);
                    }
                }
                ui.ctx().data_mut(|d| d.remove_temp::<[f32; 2]>(marquee_key));
            }
        }
    }

    // ── Render pass + edit interactions, in z-order (bottom → top) ──────────
    let mut new_pos:  Vec<Option<[f32; 2]>> = vec![None; items.len()];
    let mut new_size: Vec<Option<[f32; 2]>> = vec![None; items.len()];
    let mut new_line: Vec<Option<([f32; 2], [f32; 2])>> = vec![None; items.len()];
    let mut zaction: Option<(usize, &'static str)> = None;
    let mut delete_idx: Option<usize> = None;
    let mut stale_remove: Option<usize> = None;
    let mut dup_request = false;
    let mut copy_style_from: Option<usize> = None;
    let mut paste_style = false;
    let mut multi_drag_delta: Option<[f32; 2]> = None;
    // Selection changes requested by clicks this frame: (index, shift_held).
    let mut click_select: Option<(usize, bool)> = None;
    // Screen pos of a plain (non-drag) click this frame, so the apply step can
    // recompute the full hit-stack and cycle through overlapping items — parity
    // with the sub-patch layout editor. `None` for drag-select (no cycling).
    let mut click_pos: Option<egui::Pos2> = None;

    let selected_idx = overlay.selected_item;
    let selected_set: Vec<usize> = overlay.selected_items.clone();
    let is_multi = selected_set.len() > 1;

    let drag_pos_id  = |i: usize| egui::Id::new(("fxi_ov_drag_pos", i));
    let drag_size_id = |i: usize| egui::Id::new(("fxi_ov_drag_size", i));
    let drag_a_id    = |i: usize| egui::Id::new(("fxi_ov_drag_la", i));
    let drag_b_id    = |i: usize| egui::Id::new(("fxi_ov_drag_lb", i));
    // Per-drag-session resolved target: which item a drag STARTED on the
    // topmost item `i` should actually move (see the body drag block below).
    let drag_tgt_id  = |i: usize| egui::Id::new(("fxi_ov_drag_tgt", i));

    for (idx, it) in items.iter().enumerate() {
        // Resolve the item's rect for display — screen-anchored, or relative to
        // a linked target (anchor-to-object). Identity while editing, since
        // `authored == cur` after the bake above.
        let (lp, ls) = resolve_layout_rect(&items, idx, authored, cur);
        let item_rect = egui::Rect::from_min_size(
            origin + egui::vec2(lp[0], lp[1]),
            egui::vec2(ls[0].max(8.0), ls[1].max(8.0)),
        );

        // Paint the item itself.
        match it {
            LayoutItem::Module(m) => {
                let (mid, _disp, exists) = &infos[idx];
                if !*exists {
                    if edit {
                        // Ghost frame for a stale pin (source node deleted):
                        // visible + deletable in edit mode, skipped live.
                        let painter = ui.painter();
                        painter.rect_stroke(
                            item_rect, 4.0,
                            egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 120, 120, 180)),
                            egui::StrokeKind::Inside,
                        );
                        painter.text(
                            item_rect.center(), egui::Align2::CENTER_CENTER,
                            "missing", egui::FontId::proportional(12.0),
                            egui::Color32::from_rgba_unmultiplied(255, 150, 150, 200),
                        );
                    } else {
                        stale_remove = Some(idx);
                    }
                } else {
                    // Use the RESOLVED size (`ls`), not the raw `m.size`, so the
                    // widget fills `item_rect` — matching the selection outline and
                    // actually stretching when anchored across zones.
                    let mod_size = egui::vec2(ls[0].max(MIN_W), ls[1].max(MIN_H));
                    let inner_id = NodeId(m.inner_node_id);
                    let module_id = mid.clone();
                    let element_id = m.element_id.clone();
                    let graph_ov = m.graph_override.clone();
                    let iv_style_ov = m.iv_style_override.clone();
                    let menu_style_ov = m.menu_style_override;
                    let outer_snap_ref = outer_snapshot.as_ref();
                    let path = m.source_path.clone();
                    ui.scope_builder(egui::UiBuilder::new().max_rect(item_rect), |ui| {
                        let new_clip = ui.clip_rect().intersect(item_rect);
                        ui.set_clip_rect(new_clip);
                        // Salt widget IDs by item index so multiple pins of the
                        // same element don't collide.
                        ui.push_id(("fxi_ov_pin", idx), |ui| {
                            ui.add_enabled_ui(!edit, |ui| match path.as_slice() {
                                [] => {
                                    render_pinned_element(
                                        inner_id, &module_id, &element_id, ui, tab_snarl,
                                        mod_size, live_signals, panic_shortcut, None,
                                        None, OVERLAY_TOP_OUTER, edit, graph_ov, iv_style_ov,
                                        menu_style_ov,
                                    );
                                }
                                [sp] => {
                                    let sp_node = NodeId(*sp);
                                    if let Some(inner_snarl) = tab_snarl
                                        .get_node_mut(sp_node)
                                        .and_then(|n| n.subpatch.as_mut())
                                        .map(|sp| &mut sp.snarl)
                                    {
                                        render_pinned_element(
                                            inner_id, &module_id, &element_id, ui, inner_snarl,
                                            mod_size, live_signals, panic_shortcut, None,
                                            outer_snap_ref, sp_node, edit, graph_ov, iv_style_ov,
                                            menu_style_ov,
                                        );
                                    }
                                }
                                _ => {}
                            });
                        });
                    });
                }
            }
            LayoutItem::Deco(_) => {
                let painter = ui.painter().with_clip_rect(rect);
                // Re-anchor the decoration's geometry for display (following any
                // anchor-to link; identity while editing since authored == cur).
                paint_decoration(&painter, origin, &resolve_layout_deco(&items, idx, authored, cur));
            }
        }

        if !edit {
            continue;
        }

        // ── Edit-mode chrome + interactions for this item ───────────────────
        let in_set = selected_set.contains(&idx);
        let is_primary = selected_idx == Some(idx);
        let is_sel = in_set || is_primary;

        let outline_col = if is_sel {
            egui::Color32::from_rgba_unmultiplied(255, 220, 120, 230)
        } else {
            egui::Color32::from_rgba_unmultiplied(150, 220, 255, 140)
        };
        ui.painter().rect_stroke(
            item_rect, 0.0,
            egui::Stroke::new(1.0, outline_col),
            egui::StrokeKind::Inside,
        );

        // Anchor pivot marker on selected items: a dot at the point the element
        // is anchored to (corner / edge / centre), inset a little from the edges.
        if is_sel {
            let a = it.anchor();
            let (bp, bs) = it.bbox();
            let ex = if a.x == AnchorAxis::Auto { zone_axis(bp[0], bs[0], authored[0]).0 } else { a.x };
            let ey = if a.y == AnchorAxis::Auto { zone_axis(bp[1], bs[1], authored[1]).0 } else { a.y };
            let inset = 8.0_f32.min(item_rect.width() * 0.35).min(item_rect.height() * 0.35);
            let px = match ex {
                AnchorAxis::Start => item_rect.left() + inset,
                AnchorAxis::End => item_rect.right() - inset,
                _ => item_rect.center().x,
            };
            let py = match ey {
                AnchorAxis::Start => item_rect.top() + inset,
                AnchorAxis::End => item_rect.bottom() - inset,
                _ => item_rect.center().y,
            };
            let pivot = egui::pos2(px, py);
            let painter = ui.painter();
            painter.circle_stroke(pivot, 5.0, egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(20, 20, 20, 200)));
            painter.circle_filled(pivot, 3.5, egui::Color32::from_rgb(255, 200, 80));
            // Tiny crosshair so it reads as a pivot, not a handle.
            let t = egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(20, 20, 20, 160));
            painter.line_segment([pivot - egui::vec2(7.0, 0.0), pivot + egui::vec2(7.0, 0.0)], t);
            painter.line_segment([pivot - egui::vec2(0.0, 7.0), pivot + egui::vec2(0.0, 7.0)], t);
        }

        let menu_header = match it {
            LayoutItem::Module(m) => {
                let (mid, disp, _) = &infos[idx];
                if !disp.is_empty() { format!("{} — {}", disp, m.element_id) } else { mid.clone() }
            }
            LayoutItem::Deco(d) => d.type_label().to_string(),
        };
        let is_deco_idx = matches!(it, LayoutItem::Deco(_));
        let has_style_clip = ui.ctx().data(|d|
            d.get_temp::<ItemStyle>(layout_style_clipboard_key()).is_some());
        let n_selected = selected_set.len();

        if let LayoutItem::Deco(LayoutDecoration::Line { a, b, .. }) = it {
            // Line: whole-segment hit area for select/move; endpoint handles
            // for the primary in single-selection.
            let pa = origin + egui::vec2(a[0], a[1]);
            let pb = origin + egui::vec2(b[0], b[1]);
            let line_rect = egui::Rect::from_two_pos(pa, pb).expand(6.0);
            let resp = ui.interact(
                line_rect,
                egui::Id::new(("fxi_ov_item", idx)),
                egui::Sense::click_and_drag(),
            );
            if resp.clicked() {
                click_select = Some((idx, shift_held));
                click_pos = resp.interact_pointer_pos();
            }
            if in_set && resp.dragged_by(egui::PointerButton::Primary) {
                let dd = resp.drag_delta();
                let prev = multi_drag_delta.unwrap_or([0.0, 0.0]);
                multi_drag_delta = Some([prev[0] + dd.x, prev[1] + dd.y]);
            }
            resp.context_menu(|ui| {
                layout_item_context_menu(
                    ui, idx, is_deco_idx, has_style_clip, n_selected,
                    &mut zaction, &mut delete_idx,
                    &mut dup_request, &mut copy_style_from, &mut paste_style,
                    &menu_header,
                );
            });
            if is_primary && !is_multi {
                let h = 8.0;
                for (endpoint, id_of, other, is_a) in [
                    (pa, drag_a_id(idx), *b, true),
                    (pb, drag_b_id(idx), *a, false),
                ] {
                    let h_rect = egui::Rect::from_center_size(endpoint, egui::vec2(h, h));
                    ui.painter().rect_filled(h_rect, 1.0, outline_col);
                    let hr = ui.interact(
                        h_rect,
                        egui::Id::new(("fxi_ov_lh", idx, is_a)),
                        egui::Sense::click_and_drag(),
                    );
                    let cur = if is_a { *a } else { *b };
                    if hr.drag_started() {
                        ui.ctx().data_mut(|d| d.insert_temp(id_of, [cur[0], cur[1], 0.0f32, 0.0f32]));
                    }
                    if hr.dragged_by(egui::PointerButton::Primary) {
                        let prev = ui.ctx().data(|d| d.get_temp::<[f32; 4]>(id_of))
                            .unwrap_or([cur[0], cur[1], 0.0, 0.0]);
                        let dd = hr.drag_delta();
                        ui.ctx().data_mut(|d| d.insert_temp(
                            id_of, [prev[0], prev[1], prev[2] + dd.x, prev[3] + dd.y]));
                        let np = [snap_x(prev[0] + prev[2] + dd.x), snap_y(prev[1] + prev[3] + dd.y)];
                        new_line[idx] = Some(if is_a {
                            (np, new_line[idx].map(|l| l.1).unwrap_or(other))
                        } else {
                            (new_line[idx].map(|l| l.0).unwrap_or(other), np)
                        });
                    }
                }
            }
        } else {
            // Body select + drag.
            let resp = ui.interact(
                item_rect,
                egui::Id::new(("fxi_ov_item", idx)),
                egui::Sense::click_and_drag(),
            );
            if resp.clicked() {
                click_select = Some((idx, shift_held));
                click_pos = resp.interact_pointer_pos();
            }
            // egui hands the drag to the TOPMOST item under the pointer (`idx`),
            // but the user may have click-cycled the selection to a LOWER item in
            // a stack. Resolve which item a drag actually manipulates ONCE at
            // press time: the selected item when it sits under the press point
            // (honour the cycled selection), else the surface item. Without this,
            // dragging any stack always grabbed + re-selected the surface item,
            // ignoring the selection (the reported bug).
            let hit_at = |i: usize, p: egui::Pos2| -> bool {
                let (bp, bs) = items[i].bbox();
                egui::Rect::from_min_size(
                    origin + egui::vec2(bp[0], bp[1]),
                    egui::vec2(bs[0].max(8.0), bs[1].max(8.0)),
                )
                .contains(p)
            };
            if resp.drag_started() {
                let p = resp.interact_pointer_pos().unwrap_or(item_rect.center());
                let target = match selected_idx {
                    Some(sel) if sel != idx && hit_at(sel, p) => sel,
                    _ => idx,
                };
                let (tp, _) = items[target].bbox();
                ui.ctx().data_mut(|d| {
                    d.insert_temp(drag_tgt_id(idx), target);
                    d.insert_temp(drag_pos_id(target), [tp[0], tp[1], 0.0f32, 0.0f32]);
                });
            }
            if resp.dragged_by(egui::PointerButton::Primary) {
                let target = ui
                    .ctx()
                    .data(|d| d.get_temp::<usize>(drag_tgt_id(idx)))
                    .unwrap_or(idx);
                let dd = resp.drag_delta();
                // Move the whole multi-selection when the drag belongs to it
                // (topmost item is in the set, or the resolved target is a member).
                if is_multi && (in_set || selected_set.contains(&target)) {
                    let prev_acc = multi_drag_delta.unwrap_or([0.0, 0.0]);
                    multi_drag_delta = Some([prev_acc[0] + dd.x, prev_acc[1] + dd.y]);
                } else {
                    // Grabbing the surface item directly (target == the topmost,
                    // and it wasn't already selected) selects it; a redirect to a
                    // cycled selection leaves the selection untouched.
                    if target == idx && !in_set && !is_primary {
                        click_select = Some((idx, false));
                    }
                    let (tp, _) = items[target].bbox();
                    let prev = ui.ctx().data(|d| d.get_temp::<[f32; 4]>(drag_pos_id(target)))
                        .unwrap_or([tp[0], tp[1], 0.0, 0.0]);
                    ui.ctx().data_mut(|d| d.insert_temp(
                        drag_pos_id(target), [prev[0], prev[1], prev[2] + dd.x, prev[3] + dd.y]));
                    let tx = snap_x(prev[0] + prev[2] + dd.x).max(0.0);
                    let ty = snap_y(prev[1] + prev[3] + dd.y).max(0.0);
                    new_pos[target] = Some([tx, ty]);
                }
            }
            resp.context_menu(|ui| {
                layout_item_context_menu(
                    ui, idx, is_deco_idx, has_style_clip, n_selected,
                    &mut zaction, &mut delete_idx,
                    &mut dup_request, &mut copy_style_from, &mut paste_style,
                    &menu_header,
                );
            });

            // Corner resize handle — registered after the body so it wins in
            // its corner. Single-selection primary only (multi just moves).
            let handle_rect = egui::Rect::from_min_size(
                egui::pos2(item_rect.max.x - RESIZE_HANDLE, item_rect.max.y - RESIZE_HANDLE),
                egui::vec2(RESIZE_HANDLE, RESIZE_HANDLE),
            );
            if is_primary && !is_multi {
                let h_resp = ui.interact(
                    handle_rect,
                    egui::Id::new(("fxi_ov_resize", idx)),
                    egui::Sense::click_and_drag(),
                );
                let fill = if h_resp.hovered() || h_resp.dragged() {
                    egui::Color32::from_rgba_unmultiplied(255, 220, 120, 160)
                } else {
                    egui::Color32::from_rgba_unmultiplied(255, 220, 120, 90)
                };
                ui.painter().rect_filled(handle_rect, 2.0, fill);
                let s = egui::Stroke::new(1.2, egui::Color32::from_rgb(255, 220, 120));
                for k in 1..=3 {
                    let off = k as f32 * (RESIZE_HANDLE / 4.0);
                    ui.painter().line_segment(
                        [egui::pos2(handle_rect.max.x - off, handle_rect.max.y),
                         egui::pos2(handle_rect.max.x,       handle_rect.max.y - off)],
                        s,
                    );
                }
                if h_resp.drag_started() {
                    ui.ctx().data_mut(|d| d.insert_temp(drag_size_id(idx), [ls[0], ls[1], 0.0f32, 0.0f32]));
                }
                if h_resp.dragged_by(egui::PointerButton::Primary) {
                    let prev = ui.ctx().data(|d| d.get_temp::<[f32; 4]>(drag_size_id(idx)))
                        .unwrap_or([ls[0], ls[1], 0.0, 0.0]);
                    let dd = h_resp.drag_delta();
                    let mut ax = prev[2] + dd.x;
                    let mut ay = prev[3] + dd.y;
                    if shift_held {
                        let aspect = (prev[0] / prev[1].max(1.0)).max(0.0001);
                        if ax.abs() * (1.0 / aspect) > ay.abs() { ay = ax / aspect; } else { ax = ay * aspect; }
                    }
                    ui.ctx().data_mut(|d| d.insert_temp(drag_size_id(idx), [prev[0], prev[1], ax, ay]));
                    let tw = snap_x(prev[0] + ax).max(MIN_W.min(8.0));
                    let th = snap_y(prev[1] + ay).max(MIN_H.min(8.0));
                    // Clamp against the pin's cached natural size — same
                    // no-crop envelope as sub-patch layouts. The cache key
                    // matches what render_pinned_element used above.
                    let clamp_outer = match it {
                        LayoutItem::Module(m) => match m.source_path.as_slice() {
                            [sp] => NodeId(*sp),
                            _ => OVERLAY_TOP_OUTER,
                        },
                        _ => OVERLAY_TOP_OUTER,
                    };
                    let (tw, th) = clamp_pin_frame_to_content(ui, clamp_outer, it, tw, th);
                    new_size[idx] = Some([tw, th]);
                }
            } else if is_sel {
                let ghost = egui::Color32::from_rgba_unmultiplied(150, 220, 255, 40);
                ui.painter().rect_filled(handle_rect, 2.0, ghost);
            }
        }
    }

    if !edit {
        // Live mode: only stale-pin cleanup applies.
        if let Some(i) = stale_remove {
            if i < overlay.items.len() {
                overlay.items.remove(i);
            }
        }
        return;
    }

    // ── Anchor-to-object link guides + pick hover highlight (edit only) ───────
    {
        let painter = ui.painter().with_clip_rect(rect);
        let center = |p: [f32; 2], s: [f32; 2]| origin + egui::vec2(p[0] + s[0] * 0.5, p[1] + s[1] * 0.5);
        // A guide line from each follower to the target it is anchored to.
        for (i, it) in items.iter().enumerate() {
            if let Some(to) = it.anchor().to.filter(|&t| t != 0) {
                if let Some(j) = items.iter().position(|t| { let a = t.anchor(); a.id != 0 && a.id == to }) {
                    let (fp, fs) = resolve_layout_rect(&items, i, authored, cur);
                    let (tp, ts) = resolve_layout_rect(&items, j, authored, cur);
                    let col = egui::Color32::from_rgba_unmultiplied(120, 210, 160, 210);
                    painter.line_segment([center(fp, fs), center(tp, ts)], egui::Stroke::new(1.5, col));
                    painter.circle_filled(center(tp, ts), 4.0, col); // dot at the target
                    painter.circle_stroke(center(fp, fs), 3.5, egui::Stroke::new(1.5, col)); // ring at follower
                }
            }
        }
        // While picking a target, outline the element under the cursor.
        if anchor_pick_armed(ui.ctx()).is_some() {
            if let Some(c) = ui.ctx().pointer_hover_pos() {
                let local = [c.x - origin.x, c.y - origin.y];
                if let Some(i) = items.iter().enumerate().rev().find_map(|(i, it)| it.hit_test(local).then_some(i)) {
                    let (p, s) = resolve_layout_rect(&items, i, authored, cur);
                    let r = egui::Rect::from_min_size(
                        origin + egui::vec2(p[0], p[1]),
                        egui::vec2(s[0].max(8.0), s[1].max(8.0)),
                    );
                    painter.rect_stroke(
                        r, 4.0,
                        egui::Stroke::new(2.0, egui::Color32::from_rgb(150, 220, 120)),
                        egui::StrokeKind::Outside,
                    );
                }
            }
        }
    }

    // ── Anchor-to-object target pick ─────────────────────────────────────────
    // While armed, a plain click on an element links the awaiting follower(s) to
    // it instead of changing selection. Esc cancels.
    let anchor_pick = anchor_pick_armed(ui.ctx());
    if anchor_pick.is_some() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        clear_anchor_pick(ui.ctx());
    } else if let Some(followers) = anchor_pick.as_ref() {
        // Stay armed until an element is CLICKED (a miss on empty space keeps the
        // mode active; only a click or Esc ends it).
        if let Some((tgt, _)) = click_select {
            // Reject self-links and cycles (target already follows a follower).
            let ok = !followers.contains(&tgt)
                && !crate::canvas::node::anchor_chain_reaches(&overlay.items, tgt, followers);
            if ok {
                // Assign the target a stable id if it has none yet.
                let tid = {
                    let existing = overlay.items[tgt].anchor().id;
                    if existing != 0 {
                        existing
                    } else {
                        let nid = crate::canvas::node::next_anchor_link_id(&overlay.items);
                        let mut a = overlay.items[tgt].anchor();
                        a.id = nid;
                        overlay.items[tgt].set_anchor(a);
                        nid
                    }
                };
                for &f in followers {
                    if let Some(it) = overlay.items.get_mut(f) {
                        let mut a = it.anchor();
                        a.to = Some(tid);
                        it.set_anchor(a);
                    }
                }
            }
            clear_anchor_pick(ui.ctx());
        }
    }

    // ── Apply selection changes ──────────────────────────────────────────────
    if anchor_pick.is_none() {
    if let Some((idx, shift)) = click_select {
        if shift {
            // Shift+click toggles membership; primary follows.
            if let Some(pos) = overlay.selected_items.iter().position(|&i| i == idx) {
                overlay.selected_items.remove(pos);
                if overlay.selected_item == Some(idx) {
                    overlay.selected_item = overlay.selected_items.last().copied();
                }
            } else {
                overlay.selected_items.push(idx);
                overlay.selected_item = Some(idx);
            }
            overlay.cycle_pos = None;
        } else if let Some(cp) = click_pos {
            // Plain click: cycle through overlapping items at the same spot,
            // replacing the whole selection with the single chosen item — parity
            // with `show_subpatch_body`. `hits` is topmost-first (items paint
            // bottom→top, so `.rev()` yields top→bottom).
            let cur_local = [cp.x - origin.x, cp.y - origin.y];
            let hits: Vec<usize> = items.iter().enumerate().rev()
                .filter_map(|(i, it)| it.hit_test(cur_local).then_some(i))
                .collect();
            let hits = if hits.is_empty() { vec![idx] } else { hits };
            let near_prev = overlay.cycle_pos.map(|q| {
                ((q[0]-cur_local[0]).powi(2) + (q[1]-cur_local[1]).powi(2)).sqrt() < 6.0
            }).unwrap_or(false);
            // Only cycle when the current single selection is one of the hits (so
            // clicking a stack you already have selected steps through it).
            let single = overlay.selected_items.len() <= 1;
            let cur_sel = overlay.selected_item;
            let new_sel = if single && near_prev && hits.len() > 1 {
                match cur_sel.and_then(|s| hits.iter().position(|&h| h == s)) {
                    Some(pos) => hits[(pos + 1) % hits.len()],
                    None      => hits[0],
                }
            } else {
                hits[0]
            };
            overlay.selected_item = Some(new_sel);
            overlay.selected_items = vec![new_sel];
            overlay.cycle_pos = Some(cur_local);
        } else {
            overlay.selected_item = Some(idx);
            overlay.selected_items = vec![idx];
        }
    }
    if clicked_empty {
        overlay.selected_item = None;
        overlay.selected_items.clear();
        overlay.cycle_pos = None;
    }
    if let Some(r) = marquee_commit {
        let picked: Vec<usize> = items.iter().enumerate().filter_map(|(i, it)| {
            let (lp, ls) = it.bbox();
            let i_rect = egui::Rect::from_min_size(
                origin + egui::vec2(lp[0], lp[1]),
                egui::vec2(ls[0].max(1.0), ls[1].max(1.0)),
            );
            r.intersects(i_rect).then_some(i)
        }).collect();
        overlay.selected_item = picked.last().copied();
        overlay.selected_items = picked;
        overlay.cycle_pos = None;
    }
    } // end `if anchor_pick.is_none()` (selection changes suppressed during a target pick)

    // ── Commit pending mutations (mirrors show_subpatch_body's commit) ──────
    let style_clip_for_paste: Option<ItemStyle> = if paste_style {
        ui.ctx().data(|d| d.get_temp::<ItemStyle>(layout_style_clipboard_key()))
    } else { None };

    if let Some([ddx, ddy]) = multi_drag_delta {
        for &i in selected_set.iter() {
            if let Some(it) = overlay.items.get_mut(i) {
                match it {
                    LayoutItem::Module(m) => {
                        m.pos = [snap_x(m.pos[0] + ddx).max(0.0), snap_y(m.pos[1] + ddy).max(0.0)];
                    }
                    LayoutItem::Deco(d) => match d {
                        LayoutDecoration::Text { pos, .. }
                        | LayoutDecoration::Rect { pos, .. }
                        | LayoutDecoration::Ellipse { pos, .. }
                        | LayoutDecoration::Svg  { pos, .. } => {
                            *pos = [snap_x(pos[0] + ddx).max(0.0), snap_y(pos[1] + ddy).max(0.0)];
                        }
                        LayoutDecoration::Line { a, b, .. } => {
                            *a = [snap_x(a[0] + ddx).max(0.0), snap_y(a[1] + ddy).max(0.0)];
                            *b = [snap_x(b[0] + ddx).max(0.0), snap_y(b[1] + ddy).max(0.0)];
                        }
                    },
                }
            }
        }
    }
    let n_items = overlay.items.len();
    for (i, it) in overlay.items.iter_mut().enumerate() {
        if let Some(p) = new_pos.get(i).copied().flatten() {
            match it {
                LayoutItem::Module(m) => { m.pos = p; }
                LayoutItem::Deco(d) => match d {
                    LayoutDecoration::Text { pos, .. }
                    | LayoutDecoration::Rect { pos, .. }
                    | LayoutDecoration::Ellipse { pos, .. }
                    | LayoutDecoration::Svg  { pos, .. } => { *pos = p; }
                    _ => {}
                },
            }
        }
        if let Some(s) = new_size.get(i).copied().flatten() {
            match it {
                LayoutItem::Module(m) => {
                    m.size[0] = s[0].max(MIN_W);
                    m.size[1] = s[1].max(MIN_H);
                }
                LayoutItem::Deco(d) => match d {
                    LayoutDecoration::Text { size, .. }
                    | LayoutDecoration::Rect { size, .. }
                    | LayoutDecoration::Ellipse { size, .. }
                    | LayoutDecoration::Svg  { size, .. } => { *size = s; }
                    _ => {}
                },
            }
        }
        if let Some((na, nb)) = new_line.get(i).copied().flatten() {
            if let LayoutItem::Deco(LayoutDecoration::Line { a, b, .. }) = it {
                *a = na; *b = nb;
            }
        }
    }
    if let Some((i, act)) = zaction {
        let sel = overlay.selected_item;
        apply_zorder_action_items(&mut overlay.items, i, act, n_items);
        if sel == Some(i) {
            overlay.selected_item = match act {
                "up"     if i + 1 < n_items => Some(i + 1),
                "down"   if i > 0           => Some(i - 1),
                "top"    if i + 1 < n_items => Some(overlay.items.len() - 1),
                "bottom" if i > 0           => Some(0),
                _ => sel,
            };
        }
    }
    if let Some(i) = delete_idx.or(stale_remove) {
        if i < overlay.items.len() {
            overlay.items.remove(i);
            if let Some(s) = overlay.selected_item {
                if s == i { overlay.selected_item = None; }
                else if s > i { overlay.selected_item = Some(s - 1); }
            }
            overlay.selected_items.retain(|&j| j != i);
            for j in overlay.selected_items.iter_mut() {
                if *j > i { *j -= 1; }
            }
        }
    }
    if let Some(kind) = bg_add {
        let d = make_default_decoration(kind);
        overlay.items.push(LayoutItem::Deco(d));
        overlay.selected_item = Some(overlay.items.len() - 1);
        overlay.selected_items = vec![overlay.items.len() - 1];
    }
    if dup_request {
        let mut to_clone: Vec<usize> =
            if !selected_set.is_empty() { selected_set.clone() }
            else if let Some(p) = selected_idx { vec![p] }
            else { Vec::new() };
        to_clone.sort_unstable();
        let mut new_indices: Vec<usize> = Vec::new();
        for src in to_clone {
            if let Some(LayoutItem::Deco(d)) = overlay.items.get(src) {
                let mut nd = d.clone();
                match &mut nd {
                    LayoutDecoration::Text { pos, .. }
                    | LayoutDecoration::Rect { pos, .. }
                    | LayoutDecoration::Ellipse { pos, .. }
                    | LayoutDecoration::Svg  { pos, .. } => {
                        pos[0] += 12.0; pos[1] += 12.0;
                    }
                    LayoutDecoration::Line { a, b, .. } => {
                        a[0] += 12.0; a[1] += 12.0;
                        b[0] += 12.0; b[1] += 12.0;
                    }
                }
                // A duplicate must not inherit the original's link-target id —
                // otherwise followers of the original ambiguously (or wrongly)
                // retarget to the copy. Clear `id` so nothing is attached to the
                // copy; keep `to` so a copy of a FOLLOWER still follows its target.
                {
                    let mut a = nd.anchor();
                    a.id = 0;
                    nd.set_anchor(a);
                }
                overlay.items.push(LayoutItem::Deco(nd));
                new_indices.push(overlay.items.len() - 1);
            }
        }
        if !new_indices.is_empty() {
            overlay.selected_item = new_indices.last().copied();
            overlay.selected_items = new_indices;
            overlay.cycle_pos = None;
        }
    }
    if paste_style {
        if let Some(clip) = style_clip_for_paste.as_ref() {
            let targets: Vec<usize> =
                if !selected_set.is_empty() { selected_set.clone() }
                else if let Some(p) = selected_idx { vec![p] }
                else { Vec::new() };
            for t in targets {
                if let Some(it) = overlay.items.get_mut(t) {
                    clip.apply_to(it);
                }
            }
        }
    }
    if let Some(src) = copy_style_from {
        if let Some(style) = overlay.items.get(src).map(item_style_of) {
            ui.ctx().data_mut(|d| d.insert_temp(layout_style_clipboard_key(), style));
        }
    }
}
