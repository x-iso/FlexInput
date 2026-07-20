//! Sub-patch node body (pinned-element layout rendering) + Inlet/Outlet
//! type selector.

use super::*;

// ── Sub-patch body ────────────────────────────────────────────────────────────

/// Type selector body for Inlet and Outlet nodes inside a sub-patch.
/// The selected type is stored in `params["signal_type"]` and propagated to the node's pin.
pub(crate) fn show_inlet_outlet_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (current, is_inlet) = snarl.get_node(node_id)
        .map(|n| {
            let t = n.params.get("signal_type")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or(SignalType::Any);
            (t, n.module_id == "subpatch.inlet")
        })
        .unwrap_or((SignalType::Any, true));

    let mut t = current;
    egui::ComboBox::from_id_salt(egui::Id::new(("sp_io_type", node_id.0)))
        .selected_text(format!("{:?}", t))
        .width(74.0)
        .show_ui(ui, |ui| {
            for opt in [SignalType::Float, SignalType::Bool, SignalType::Vec2,
                        SignalType::Int, SignalType::Any, SignalType::AutoMap]
            {
                ui.selectable_value(&mut t, opt, format!("{:?}", opt));
            }
        });

    if t != current {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if let Ok(v) = serde_json::to_value(t) {
                node.params.insert("signal_type".to_string(), v);
            }
            // Update the visible pin type immediately.
            if is_inlet {
                if let Some(pin) = node.outputs.get_mut(0) { pin.signal_type = t; }
            } else {
                if let Some(pin) = node.inputs.get_mut(0) { pin.signal_type = t; }
            }
        }
    }
}

// ── Sub-patch body: bare pinned UI elements ───────────────────────────────────

/// Renders pinned UI elements at their stored 2D positions, **without** any
/// surrounding container/title — each element is a free-floating widget.
/// In Layout mode (`outer_node.extra.layout_unlocked`) each element gets a
/// dashed selection outline + corner resize handle and the underlying widget
/// is disabled so the user can drag/resize without accidentally operating it.
/// In Lock mode the widget is fully interactive and no chrome is drawn.
pub(crate) fn show_subpatch_body(
    outer_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    panic_shortcut: &crate::app::PanicShortcut,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) -> bool {
    // ── One-time legacy migrations ──────────────────────────────────────────
    if let Some(sp) = snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) {
        // Drain legacy exposed_modules / decorations into unified `items` Vec.
        sp.migrate_into_items();
        // Rewrite Remapper / Map Action "default" → "whole_module" element id.
        let module_ids: std::collections::HashMap<usize, String> = sp.snarl
            .nodes_ids_data()
            .map(|(id, info)| (id.0, info.value.module_id.clone()))
            .collect();
        for it in sp.items.iter_mut() {
            if let LayoutItem::Module(m) = it {
                if m.element_id == "default" {
                    if let Some(mid) = module_ids.get(&m.inner_node_id) {
                        if mid == "module.remapper" || mid == "module.map_action" {
                            m.element_id = "whole_module".to_string();
                        }
                    }
                }
            }
        }
    }

    let is_empty = snarl.get_node(outer_id)
        .and_then(|n| n.subpatch.as_ref())
        .map(|sp| sp.items.is_empty())
        .unwrap_or(true);
    if is_empty { return false; }

    // Bail when the sub-patch node is collapsed (or animating to/from it).
    //
    // Snarl renders the body each frame regardless of openness — when
    // collapsed, it shifts the body cursor UP by `payload_offset(openness)`
    // so the body widgets draw above the visible node and get clipped by the
    // header frame. But our whole-module pin renderers paint into separate
    // transform layers via `set_transform_layer` + `set_sublayer`, which
    // bypass the parent clip rect; the result is a pinned Remapper / Map
    // Action widget that floats above the collapsed header.
    //
    // Detect this by comparing the body cursor to the parent clip rect:
    // when collapsed, the cursor sits well ABOVE the clip_rect's top edge
    // (because snarl moved it up by payload_offset). Bail in that case.
    // Distinguish "collapsed" (skip body) from "scrolled out of view" (still
    // render normally so the body comes back when the user scrolls down).
    //
    // Snarl exposes the target open/closed state directly on the node. If
    // the sub-patch is collapsed (or animating to collapsed), `open == false`
    // and we skip the body entirely — otherwise the whole-module renderers'
    // separate transform layers would keep painting at their last position
    // because they bypass the parent body clip rect.
    let is_open = snarl.get_node_info(outer_id).map(|n| n.open).unwrap_or(true);
    if !is_open {
        return false;
    }

    let is_unlocked = snarl.get_node(outer_id)
        .map(|n| n.extra.layout_unlocked)
        .unwrap_or(false);

    // Clear runtime selection on Layout-mode exit — UNLESS gamepad UI nav owns
    // the selection this frame (it drives `selected_item` outside layout-edit
    // mode; the app stamps a pass-numbered flag when a nav device is active).
    let nav_owns_selection = {
        let stamp: Option<u64> = ui.ctx().data(|d|
            d.get_temp(egui::Id::new("gp_nav_owns_selection")));
        stamp == Some(ui.ctx().cumulative_pass_nr())
    };
    if !is_unlocked && !nav_owns_selection {
        if let Some(sp) = snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) {
            sp.selected_item = None;
            sp.selected_items.clear();
            sp.cycle_pos = None;
        }
    }

    // Snapshot items so we can iterate without holding a mutable borrow on
    // the subpatch (renderers need to borrow `sp.snarl` mutably).
    let items: Vec<LayoutItem> = snarl.get_node(outer_id)
        .and_then(|n| n.subpatch.as_ref())
        .map(|sp| sp.items.clone())
        .unwrap_or_default();

    // Snapshot the outer snarl for whole-module / Text pin renderers that
    // need AutomapGlowParent chain or per-pin override lookup. Touch Zones'
    // "field"/"cards" pins need the chain too: live-dot device resolution and
    // the Special-picker `outer` addressing both walk it — without a snapshot
    // the pinned widget shows no dots and the picker writes to the wrong snarl.
    let needs_outer_snapshot = !is_unlocked && items.iter().any(|it| matches!(it,
        LayoutItem::Module(m) if m.element_id == "whole_module" || m.element_id == "text"
            || m.element_id == "field" || m.element_id == "cards"
    ));
    let outer_snapshot: Option<Snarl<NodeData>> = if needs_outer_snapshot {
        Some(snarl.clone())
    } else { None };

    let (snap_enabled, snap_grid) = snarl.get_node(outer_id)
        .and_then(|n| n.subpatch.as_ref())
        .map(|sp| (sp.snap_enabled, sp.snap_grid_px.max(2) as f32))
        .unwrap_or((false, 8.0));

    // Per-item inner-module info (display name etc), for stale-pin cleanup
    // and right-click menu labels.
    let infos: Vec<(String, String, bool)> = items.iter().map(|it| {
        if let LayoutItem::Module(m) = it {
            let inner = snarl.get_node(outer_id)
                .and_then(|n| n.subpatch.as_ref())
                .and_then(|sp| sp.snarl.get_node(egui_snarl::NodeId(m.inner_node_id)));
            (
                inner.map(|n| n.module_id.clone()).unwrap_or_default(),
                inner.map(|n| n.display_name.clone()).unwrap_or_default(),
                inner.is_some(),
            )
        } else {
            (String::new(), String::new(), true)
        }
    }).collect();

    // ── Auto-fit body to item bbox (grow + shrink) ──────────────────────────
    const PAD: f32 = 16.0;
    let mut bbox_max = [0.0f32, 0.0f32];
    for it in items.iter() {
        let (lp, ls) = it.bbox();
        bbox_max[0] = bbox_max[0].max(lp[0] + ls[0]);
        bbox_max[1] = bbox_max[1].max(lp[1] + ls[1]);
    }
    let body_w = (bbox_max[0] + PAD).max(120.0);
    let body_h = (bbox_max[1] + PAD).max(40.0);
    let (body_rect, _) = ui.allocate_exact_size(egui::vec2(body_w, body_h), egui::Sense::hover());
    let origin = body_rect.min;

    // Layout mode: faint snap grid for visual feedback. Drawn under widgets.
    if is_unlocked && snap_enabled && snap_grid >= 2.0 {
        let painter = ui.painter().with_clip_rect(body_rect);
        let stroke = egui::Stroke::new(0.5, egui::Color32::from_rgba_unmultiplied(150, 200, 255, 28));
        let mut x = 0.0f32;
        while x <= body_w {
            painter.line_segment(
                [egui::pos2(origin.x + x, origin.y),
                 egui::pos2(origin.x + x, origin.y + body_h)],
                stroke,
            );
            x += snap_grid;
        }
        let mut y = 0.0f32;
        while y <= body_h {
            painter.line_segment(
                [egui::pos2(origin.x,          origin.y + y),
                 egui::pos2(origin.x + body_w, origin.y + y)],
                stroke,
            );
            y += snap_grid;
        }
    }

    // Drag/resize accumulator buckets (one entry per item).
    let mut new_pos:  Vec<Option<[f32; 2]>> = vec![None; items.len()];
    let mut new_size: Vec<Option<[f32; 2]>> = vec![None; items.len()];
    let mut new_line: Vec<Option<([f32;2],[f32;2])>> = vec![None; items.len()];
    let mut zaction: Option<(usize, &'static str)> = None;
    let mut delete_idx: Option<usize> = None;
    let mut stale_remove: Option<usize> = None;
    // Right-click actions for the selection. `dup_request` duplicates every
    // selected DECORATION (module pins are excluded). `copy_style_from`
    // stashes one item's style to the ctx clipboard; `paste_style` applies it
    // to all selected items where the field exists.
    let mut dup_request = false;
    let mut copy_style_from: Option<usize> = None;
    let mut paste_style = false;

    let snap = |v: f32| -> f32 {
        if snap_enabled && snap_grid > 0.5 { (v / snap_grid).round() * snap_grid } else { v }
    };
    let shift_held = ui.input(|i| i.modifiers.shift);
    // Kept small: the handle sits inside the item's corner, so its footprint
    // is effectively the smallest frame you can still comfortably resize.
    const RESIZE_HANDLE: f32 = 14.0;
    const MIN_W: f32 = 32.0;
    const MIN_H: f32 = 18.0;

    let drag_pos_id  = |i: usize| egui::Id::new(("li_drag_pos",  outer_id.0, i));
    let drag_size_id = |i: usize| egui::Id::new(("li_drag_size", outer_id.0, i));
    let drag_a_id    = |i: usize| egui::Id::new(("li_drag_la",   outer_id.0, i));
    let drag_b_id    = |i: usize| egui::Id::new(("li_drag_lb",   outer_id.0, i));

    // ── Render pass (paint widgets / decorations in z-order) ────────────────
    // All items render disabled in Layout mode (clicks fall through to the
    // unified select layer below). In Lock mode modules are interactive.
    //
    // Overlay pick: each PIN is itself a pick target (path = this sub-patch
    // node on the tab canvas) — that's what "pin the same element to the
    // overlay" means for a layout the user already curated. The pinned
    // bodies' own `register_exposable_element` calls are suppressed during
    // the render: their node ids live in the INNER snarl, which the overlay's
    // one-level path schema can't address from here (and the top-level drain
    // would mislabel them as tab-canvas nodes).
    let pick_raw = ui.ctx().data(|d| {
        d.get_temp::<bool>(egui::Id::new(OVERLAY_PICK_ACTIVE_KEY)).unwrap_or(false)
    });
    // Armed for THIS body: pick active, not suppressed by an outer context
    // (nested editors), body sits on the tab canvas (one-level path), and
    // not in layout-edit mode (whose manual pointer machinery would fight
    // the pick targets).
    let pick_here = overlay_pick_active(ui.ctx())
        && automap_parent.is_none()
        && !is_unlocked;
    for (idx, it) in items.iter().enumerate() {
        match it {
            LayoutItem::Module(m) => {
                let (mid, _disp, exists) = &infos[idx];
                if !*exists { stale_remove = Some(idx); continue; }
                let mod_pos = origin + egui::vec2(m.pos[0], m.pos[1]);
                let mod_size = egui::vec2(m.size[0].max(MIN_W), m.size[1].max(MIN_H));
                let element_rect = egui::Rect::from_min_size(mod_pos, mod_size);
                let inner_id = egui_snarl::NodeId(m.inner_node_id);
                let module_id = mid.clone();
                let element_id = m.element_id.clone();
                let graph_ov = m.graph_override.clone();
                let iv_style_ov = m.iv_style_override;
                let menu_style_ov = m.menu_style_override;
                let outer_snap_ref = outer_snapshot.as_ref();
                let prev_suppressed = ui.ctx().data(|d| {
                    d.get_temp::<bool>(egui::Id::new(OVERLAY_PICK_SUPPRESSED_KEY)).unwrap_or(false)
                });
                if pick_raw {
                    set_overlay_pick_suppressed(ui.ctx(), true);
                }
                ui.scope_builder(egui::UiBuilder::new().max_rect(element_rect), |ui| {
                    let new_clip = ui.clip_rect().intersect(element_rect);
                    ui.set_clip_rect(new_clip);
                    // Salt widget IDs by layout-item index so multiple pins of
                    // the same inner module don't collide on their per-mapping
                    // widget IDs (DragValue, Button, etc.).
                    ui.push_id(("fxi_layout_pin", idx), |ui| {
                        ui.add_enabled_ui(!is_unlocked, |ui| {
                            if let Some(sp) = snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) {
                                render_pinned_element(
                                    inner_id, &module_id, &element_id, ui, &mut sp.snarl,
                                    mod_size, live_signals, panic_shortcut, automap_parent,
                                    outer_snap_ref, outer_id, is_unlocked, graph_ov, iv_style_ov,
                                    menu_style_ov,
                                );
                            }
                        });
                    });
                });
                if pick_raw {
                    set_overlay_pick_suppressed(ui.ctx(), prev_suppressed);
                }
                if pick_here {
                    let id = egui::Id::new(("fxi_overlay_pick_pin", outer_id.0, idx));
                    let resp = ui.interact(element_rect, id, egui::Sense::click());
                    let painter = ui.painter().with_clip_rect(body_rect);
                    let (fill, outline) = if resp.hovered() {
                        (egui::Color32::from_rgba_unmultiplied(255, 200, 90, 90),
                         egui::Color32::from_rgb(255, 220, 140))
                    } else {
                        (egui::Color32::from_rgba_unmultiplied(230, 160, 40, 35),
                         egui::Color32::from_rgb(230, 160, 40))
                    };
                    painter.rect_filled(element_rect, 4.0, fill);
                    painter.rect_stroke(element_rect, 4.0,
                        egui::Stroke::new(1.5, outline), egui::StrokeKind::Inside);
                    if resp.clicked() {
                        put_overlay_pick_result(
                            ui.ctx(), vec![outer_id.0], m.inner_node_id,
                            m.element_id.clone(), m.size,
                        );
                    }
                }
            }
            LayoutItem::Deco(d) => {
                let painter = ui.painter().with_clip_rect(body_rect);
                paint_decoration(&painter, origin, d);
            }
        }
    }

    if !is_unlocked {
        // Lock mode: no further interaction. Stale-pin cleanup still applies.
        if let Some(i) = stale_remove {
            if let Some(sp) = snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) {
                if i < sp.items.len() { sp.items.remove(i); }
                if sp.items.is_empty() {
                    if let Some(node) = snarl.get_node_mut(outer_id) {
                        node.extra.layout_unlocked = false;
                    }
                }
            }
        }
        return false;
    }

    // ── Layout mode interaction model ───────────────────────────────────────
    // For SELECTION we cannot rely on egui's `Response::clicked()` because
    // egui-snarl's node-frame interact (registered before the body) claims
    // primary drag on pointer-down, preventing `clicked()` from firing on
    // items inside the frame. So we do MANUAL hit-testing on the raw primary
    // pointer-release inside `body_rect` instead — independent of any
    // interact response.
    let mut bg_add: Option<&'static str> = None;

    // Detect primary press WITHOUT drag completion using raw pointer state.
    // We capture press position on press-down, then check on release whether
    // the pointer didn't travel beyond a small threshold (so a real drag of
    // the underlying snarl node doesn't get mistaken for a click).
    //
    // CRITICAL: the pointer position returned by `latest_pos()` is in GLOBAL
    // screen coords, but `body_rect.min` (and per-item `origin`) live in the
    // current UI layer's local coords. The canvas layer has a TSTransform
    // applied (zoom/pan), so we must convert via the inverse layer xform.
    let press_state_key = egui::Id::new(("li_press_track", outer_id.0));
    // (press_local, primary_was_down_last_frame, press_was_on_higher_layer)
    type PressState = ([f32; 2], bool, bool);
    let parent_to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
        .unwrap_or(egui::emath::TSTransform::IDENTITY);
    let from_global = parent_to_global.inverse();
    let pointer_local = ui.ctx().input(|i| i.pointer.latest_pos()).map(|p| from_global * p);
    let pointer_global = ui.ctx().input(|i| i.pointer.latest_pos());
    // Visible viewport in body-local coords. In Easy mode the body lives on a
    // scrolled+scaled sublayer whose clip rect is the panel window — content
    // scrolled out of view (and anything ABOVE the panel, like the layout
    // inspector strip) maps to a body-local point OUTSIDE this rect. Guarding
    // selection on it stops inspector clicks from registering as clicks on
    // scrolled-away items, whose body-local bbox the inverse transform can
    // otherwise alias onto. `body_rect.contains` alone is insufficient because
    // it spans the whole (unscrolled) body, not just the visible window.
    let visible_rect = ui.clip_rect();
    let in_view = |p: egui::Pos2| body_rect.contains(p) && visible_rect.contains(p);
    let primary_down   = ui.ctx().input(|i| i.pointer.primary_down());
    let secondary_down = ui.ctx().input(|i| i.pointer.secondary_down());
    let prev: Option<PressState> = ui.ctx().data(|d| d.get_temp(press_state_key));
    let primary_was_down = prev.map(|p| p.1).unwrap_or(false);
    let prev_press: Option<[f32; 2]> = prev.map(|p| p.0);
    let prev_higher_layer: bool = prev.map(|p| p.2).unwrap_or(false);

    // "Cursor is over a popup / area above our canvas layer." This is true
    // whenever the topmost layer at the pointer is in a higher Order than
    // our canvas layer (which lives in Order::Middle). Context menus open as
    // Foreground areas, so a click on a menu item flips this to true.
    let our_order = ui.layer_id().order;
    let pointer_on_higher_layer: bool = if let Some(p) = pointer_global {
        ui.ctx().layer_id_at(p)
            .map(|l| (l.order as u8) > (our_order as u8))
            .unwrap_or(false)
    } else { false };

    // Marquee (rubber-band) selection state: body-local start point, set when
    // a Shift+primary press begins in empty space. While the marquee is active
    // we paint the rect each frame and, on release, select every item whose
    // bbox the rect overlaps.
    let marquee_key = egui::Id::new(("li_marquee", outer_id.0));
    let marquee_start: Option<[f32; 2]> = ui.ctx().data(|d| d.get_temp(marquee_key));

    // Track press-down: stash local press pos. If the cursor is over a
    // higher layer (popup / menu / window) at press time, mark the press as
    // "ignore" so the release doesn't trigger background-click logic.
    if primary_down && !primary_was_down {
        if let Some(p) = pointer_local {
            if in_view(p) && !pointer_on_higher_layer {
                let local = [p.x - origin.x, p.y - origin.y];
                ui.ctx().data_mut(|d| d.insert_temp(press_state_key, (local, true, false)));
                // Begin a marquee when Shift is held and the press landed in
                // empty space (no item under the cursor).
                if shift_held {
                    let on_item = items.iter().any(|it| it.hit_test(local));
                    if !on_item {
                        ui.ctx().data_mut(|d| d.insert_temp(marquee_key, local));
                    }
                }
            } else {
                ui.ctx().data_mut(|d| d.insert_temp(press_state_key, ([f32::NAN, f32::NAN], true, true)));
            }
        }
    } else if primary_down {
        // While held, OR in the higher-layer flag so any popup that
        // overlapped the cursor during the press window causes suppression.
        let higher_seen_so_far = prev_higher_layer || pointer_on_higher_layer;
        ui.ctx().data_mut(|d| d.insert_temp(
            press_state_key,
            (prev_press.unwrap_or([f32::NAN, f32::NAN]), true, higher_seen_so_far),
        ));
    }

    // On release, suppress if the cursor was over a higher layer at any
    // point during the press window OR is over one right now (handles
    // the case where the popup hasn't quite dismissed yet by release time).
    let primary_just_released = !primary_down && primary_was_down;
    let suppress = prev_higher_layer || pointer_on_higher_layer;
    let mut click_local: Option<[f32; 2]> = None;
    let mut click_in_empty = false;
    if primary_just_released {
        if !suppress {
            if let (Some(p), Some(start)) = (pointer_local, prev_press) {
                if in_view(p) && !start[0].is_nan() {
                    let cur_local = [p.x - origin.x, p.y - origin.y];
                    let dx = cur_local[0] - start[0];
                    let dy = cur_local[1] - start[1];
                    if (dx*dx + dy*dy).sqrt() < 6.0 {
                        let hits: Vec<usize> = items.iter().enumerate().rev()
                            .filter_map(|(i, it)| it.hit_test(cur_local).then_some(i))
                            .collect();
                        if hits.is_empty() {
                            click_in_empty = true;
                        } else {
                            click_local = Some(cur_local);
                            if let Some(sp) = snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) {
                                if shift_held {
                                    // Shift+click toggles the topmost hit in/out
                                    // of the multi-selection set. Primary follows
                                    // the toggled item (or a remaining member).
                                    let target = hits[0];
                                    if let Some(pos) = sp.selected_items.iter().position(|&i| i == target) {
                                        sp.selected_items.remove(pos);
                                        if sp.selected_item == Some(target) {
                                            sp.selected_item = sp.selected_items.last().copied();
                                        }
                                    } else {
                                        sp.selected_items.push(target);
                                        sp.selected_item = Some(target);
                                    }
                                    sp.cycle_pos = None;
                                } else {
                                    // Plain click: cycle through overlapping
                                    // items at the same spot; replace the whole
                                    // selection with the single chosen item.
                                    let near_prev = sp.cycle_pos.map(|q| {
                                        ((q[0]-cur_local[0]).powi(2) + (q[1]-cur_local[1]).powi(2)).sqrt() < 6.0
                                    }).unwrap_or(false);
                                    // Only cycle when the current single selection
                                    // is one of the hits (so clicking a stack you
                                    // already have selected steps through it).
                                    let single = sp.selected_items.len() <= 1;
                                    let cur_sel = sp.selected_item;
                                    let new_sel = if single && near_prev && hits.len() > 1 {
                                        let cur_pos = cur_sel.and_then(|s| hits.iter().position(|&h| h == s));
                                        match cur_pos {
                                            Some(pos) => hits[(pos + 1) % hits.len()],
                                            None      => hits[0],
                                        }
                                    } else {
                                        hits[0]
                                    };
                                    sp.selected_item = Some(new_sel);
                                    sp.selected_items = vec![new_sel];
                                    sp.cycle_pos = Some(cur_local);
                                }
                            }
                        }
                    }
                }
            }
        }
        // Marquee release: if a rubber-band was active, select every item
        // whose bbox overlaps the dragged rectangle (replaces the current
        // selection). A near-zero rect (no real drag) just clears below via
        // click_in_empty. Done regardless of `suppress` so a marquee that
        // briefly passed under a popup still commits.
        if let (Some(start), Some(p)) = (marquee_start, pointer_local) {
            let cur = [p.x - origin.x, p.y - origin.y];
            let min = [start[0].min(cur[0]), start[1].min(cur[1])];
            let max = [start[0].max(cur[0]), start[1].max(cur[1])];
            let area = (max[0] - min[0]) * (max[1] - min[1]);
            if area >= 16.0 {
                let picked: Vec<usize> = items.iter().enumerate().filter_map(|(i, it)| {
                    let (lp, ls) = it.bbox();
                    let i_min = lp;
                    let i_max = [lp[0] + ls[0].max(1.0), lp[1] + ls[1].max(1.0)];
                    // AABB overlap test.
                    let overlap = i_min[0] <= max[0] && i_max[0] >= min[0]
                        && i_min[1] <= max[1] && i_max[1] >= min[1];
                    overlap.then_some(i)
                }).collect();
                if let Some(sp) = snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) {
                    sp.selected_item = picked.last().copied();
                    sp.selected_items = picked;
                    sp.cycle_pos = None;
                }
                // A committed marquee is not an empty click.
                click_in_empty = false;
            }
            ui.ctx().data_mut(|d| d.remove_temp::<[f32; 2]>(marquee_key));
        }
        // Always clear press state after release (suppressed or not).
        ui.ctx().data_mut(|d| d.remove_temp::<PressState>(press_state_key));
    }
    let _ = click_local;

    // Paint the live marquee rectangle while the rubber-band drag is in
    // progress (Shift held, primary down, started in empty space).
    if let (Some(start), Some(p), true) = (marquee_start, pointer_local, primary_down) {
        let cur = [p.x - origin.x, p.y - origin.y];
        let r = egui::Rect::from_two_pos(
            origin + egui::vec2(start[0], start[1]),
            origin + egui::vec2(cur[0], cur[1]),
        );
        let painter = ui.painter().with_clip_rect(body_rect);
        painter.rect_filled(r, 0.0, egui::Color32::from_rgba_unmultiplied(120, 180, 255, 30));
        painter.rect_stroke(
            r, egui::CornerRadius::ZERO,
            egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(150, 200, 255, 200)),
            egui::StrokeKind::Inside,
        );
        request_repaint_throttled(ui.ctx());
    }
    // If the primary button is no longer down but a stale marquee slot
    // remains (e.g. release happened off-window), clear it.
    if !primary_down && marquee_start.is_some() {
        ui.ctx().data_mut(|d| d.remove_temp::<[f32; 2]>(marquee_key));
    }

    // Background interact for right-click "Add ▶" menu only (left-clicks
    // are handled manually above; this catches secondary clicks in the
    // background — per-item context menus override on item rects).
    let bg_resp = ui.interact(
        body_rect,
        egui::Id::new(("li_bg_hit", outer_id.0)),
        egui::Sense::click(),
    );
    let _ = secondary_down;
    bg_resp.context_menu(|ui| {
        ui.menu_button("Add", |ui| {
            if ui.button("Text").clicked()      { bg_add = Some("text");    ui.close(); }
            if ui.button("Rectangle").clicked() { bg_add = Some("rect");    ui.close(); }
            if ui.button("Ellipse").clicked()   { bg_add = Some("ellipse"); ui.close(); }
            if ui.button("Line").clicked()      { bg_add = Some("line");    ui.close(); }
            if ui.button("SVG").clicked()       { bg_add = Some("svg");     ui.close(); }
        });
    });

    // Per-item hits in z-order (bottom→top) — used ONLY for the right-click
    // context menu (left-click selection is done manually above).
    let mut hit_responses: Vec<(usize, egui::Response)> = Vec::with_capacity(items.len());
    for (idx, it) in items.iter().enumerate() {
        let (lp, ls) = it.bbox();
        let mut rect = egui::Rect::from_min_size(
            origin + egui::vec2(lp[0], lp[1]),
            egui::vec2(ls[0].max(8.0), ls[1].max(8.0)),
        );
        if matches!(it, LayoutItem::Deco(LayoutDecoration::Line { .. })) {
            rect = rect.expand(6.0);
        }
        let resp = ui.interact(
            rect,
            egui::Id::new(("li_hit", outer_id.0, idx)),
            egui::Sense::click_and_drag(),
        );
        hit_responses.push((idx, resp));
    }

    if click_in_empty {
        if let Some(sp) = snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) {
            sp.selected_item = None;
            sp.selected_items.clear();
            sp.cycle_pos = None;
        }
    }

    // Refresh selected_idx after potential mutation.
    let selected_idx = snarl.get_node(outer_id)
        .and_then(|n| n.subpatch.as_ref())
        .and_then(|sp| sp.selected_item);
    // Snapshot the full multi-selection set for the paint/drag pass.
    let selected_set: Vec<usize> = snarl.get_node(outer_id)
        .and_then(|n| n.subpatch.as_ref())
        .map(|sp| sp.selected_items.clone())
        .unwrap_or_default();

    // Multi-drag accumulator: when the user drags a member of a multi-
    // selection, we record the delta here and apply it to EVERY selected
    // item in the commit pass (so they move together).
    let mut multi_drag_delta: Option<[f32; 2]> = None;
    let is_multi = selected_set.len() > 1;

    // Per-item paint pass + drag/resize. Outline = any item in the selection
    // set; resize handle / line endpoints / inspector = the PRIMARY only.
    for (idx, it) in items.iter().enumerate() {
        let (lp, ls) = it.bbox();
        let rect = egui::Rect::from_min_size(
            origin + egui::vec2(lp[0], lp[1]),
            egui::vec2(ls[0].max(8.0), ls[1].max(8.0)),
        );
        let in_set = selected_set.contains(&idx);
        let is_primary = selected_idx == Some(idx);
        // `is_sel` drives the selection outline color: highlight every member.
        let is_sel = in_set || is_primary;

        // Outline.
        let outline_col = if is_sel {
            egui::Color32::from_rgba_unmultiplied(255, 220, 120, 230)
        } else {
            egui::Color32::from_rgba_unmultiplied(150, 220, 255, 140)
        };
        let stroke = egui::Stroke::new(1.0, outline_col);
        ui.painter().line_segment([rect.left_top(),     rect.right_top()],    stroke);
        ui.painter().line_segment([rect.right_top(),    rect.right_bottom()], stroke);
        ui.painter().line_segment([rect.right_bottom(), rect.left_bottom()],  stroke);
        ui.painter().line_segment([rect.left_bottom(),  rect.left_top()],     stroke);

        let menu_header = match it {
            LayoutItem::Module(m) => {
                let (mid, disp, _) = &infos[idx];
                if !disp.is_empty() { format!("{} — {}", disp, m.element_id) }
                else { mid.clone() }
            }
            LayoutItem::Deco(d) => d.type_label().to_string(),
        };
        // Context-menu state flags (computed once per item).
        let is_deco_idx = matches!(it, LayoutItem::Deco(_));
        let has_style_clip = ui.ctx().data(|d|
            d.get_temp::<ItemStyle>(layout_style_clipboard_key()).is_some());
        let n_selected = selected_set.len();

        // Line: two endpoint handles instead of corner resize.
        if let LayoutItem::Deco(LayoutDecoration::Line { a, b, .. }) = it {
            let pa = origin + egui::vec2(a[0], a[1]);
            let pb = origin + egui::vec2(b[0], b[1]);
            let h = 8.0;
            let ha_rect = egui::Rect::from_center_size(pa, egui::vec2(h, h));
            let hb_rect = egui::Rect::from_center_size(pb, egui::vec2(h, h));
            let handle_col = if is_sel {
                outline_col
            } else {
                egui::Color32::from_rgba_unmultiplied(outline_col.r(), outline_col.g(), outline_col.b(), 60)
            };
            ui.painter().rect_filled(ha_rect, 1.0, handle_col);
            ui.painter().rect_filled(hb_rect, 1.0, handle_col);
            // Endpoint editing is single-item only (the PRIMARY). In a
            // multi-selection the line moves as a whole via multi-drag.
            if is_primary && !is_multi {
                let ar = ui.interact(ha_rect, egui::Id::new(("li_la", outer_id.0, idx)), egui::Sense::click_and_drag());
                let br = ui.interact(hb_rect, egui::Id::new(("li_lb", outer_id.0, idx)), egui::Sense::click_and_drag());
                if ar.drag_started() {
                    ui.ctx().data_mut(|d| d.insert_temp(drag_a_id(idx), [a[0], a[1], 0.0f32, 0.0f32]));
                }
                if ar.dragged_by(egui::PointerButton::Primary) {
                    let prev = ui.ctx().data(|d| d.get_temp::<[f32;4]>(drag_a_id(idx))).unwrap_or([a[0],a[1],0.0,0.0]);
                    let dd = ar.drag_delta();
                    ui.ctx().data_mut(|d| d.insert_temp(drag_a_id(idx), [prev[0],prev[1], prev[2]+dd.x, prev[3]+dd.y]));
                    let na = [snap(prev[0]+prev[2]+dd.x), snap(prev[1]+prev[3]+dd.y)];
                    new_line[idx] = Some((na, *b));
                }
                if br.drag_started() {
                    ui.ctx().data_mut(|d| d.insert_temp(drag_b_id(idx), [b[0], b[1], 0.0f32, 0.0f32]));
                }
                if br.dragged_by(egui::PointerButton::Primary) {
                    let prev = ui.ctx().data(|d| d.get_temp::<[f32;4]>(drag_b_id(idx))).unwrap_or([b[0],b[1],0.0,0.0]);
                    let dd = br.drag_delta();
                    ui.ctx().data_mut(|d| d.insert_temp(drag_b_id(idx), [prev[0],prev[1], prev[2]+dd.x, prev[3]+dd.y]));
                    let nb = [snap(prev[0]+prev[2]+dd.x), snap(prev[1]+prev[3]+dd.y)];
                    let cur_a = new_line[idx].map(|p| p.0).unwrap_or(*a);
                    new_line[idx] = Some((cur_a, nb));
                }
            }
            // In a multi-selection a selected line translates as a whole
            // (endpoint editing is disabled above). Hit-test along the
            // segment so the whole line is grabbable for the group move.
            if is_multi && in_set {
                let line_rect = egui::Rect::from_two_pos(pa, pb).expand(6.0);
                let lresp = ui.interact(
                    line_rect,
                    egui::Id::new(("li_line_move", outer_id.0, idx)),
                    egui::Sense::click_and_drag(),
                );
                if lresp.dragged_by(egui::PointerButton::Primary) {
                    let dd = lresp.drag_delta();
                    let prev_acc = multi_drag_delta.unwrap_or([0.0, 0.0]);
                    multi_drag_delta = Some([prev_acc[0] + dd.x, prev_acc[1] + dd.y]);
                }
            }
        } else {
            // ── Body drag for the SELECTED non-Line item — registered BEFORE
            //    the resize handle so the handle (registered later) wins
            //    inside its corner area, while the body drag wins everywhere
            //    else inside the rect. Using the full rect (no corner cut)
            //    means narrow rectangles remain fully draggable.
            if in_set {
                let body = ui.interact(
                    rect,
                    egui::Id::new(("li_body_drag", outer_id.0, idx)),
                    egui::Sense::click_and_drag(),
                );
                if body.drag_started() {
                    ui.ctx().data_mut(|d| d.insert_temp(drag_pos_id(idx), [lp[0], lp[1], 0.0f32, 0.0f32]));
                }
                if body.dragged_by(egui::PointerButton::Primary) {
                    let prev = ui.ctx().data(|d| d.get_temp::<[f32;4]>(drag_pos_id(idx))).unwrap_or([lp[0],lp[1],0.0,0.0]);
                    let dd = body.drag_delta();
                    if is_multi {
                        // Multi-selection: record the raw (snapped) delta and
                        // apply it to EVERY selected item in the commit pass so
                        // they translate together. Don't write new_pos[idx]
                        // here — the commit pass handles all members uniformly.
                        let prev_acc = multi_drag_delta.unwrap_or([0.0, 0.0]);
                        multi_drag_delta = Some([
                            prev_acc[0] + dd.x,
                            prev_acc[1] + dd.y,
                        ]);
                    } else {
                        ui.ctx().data_mut(|d| d.insert_temp(drag_pos_id(idx), [prev[0], prev[1], prev[2]+dd.x, prev[3]+dd.y]));
                        let tx = snap(prev[0] + prev[2] + dd.x).max(0.0);
                        let ty = snap(prev[1] + prev[3] + dd.y).max(0.0);
                        new_pos[idx] = Some([tx, ty]);
                    }
                }
            }

            // Corner resize handle (ghost when unselected) — registered AFTER
            // the body drag so it wins inside its small corner area. Resize is
            // single-item only (the PRIMARY); a multi-selection only moves.
            let handle_rect = egui::Rect::from_min_size(
                egui::pos2(rect.max.x - RESIZE_HANDLE, rect.max.y - RESIZE_HANDLE),
                egui::vec2(RESIZE_HANDLE, RESIZE_HANDLE),
            );
            if is_primary && !is_multi {
                let h_resp = ui.interact(
                    handle_rect,
                    egui::Id::new(("li_resize", outer_id.0, idx)),
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
                    let prev = ui.ctx().data(|d| d.get_temp::<[f32;4]>(drag_size_id(idx))).unwrap_or([ls[0],ls[1],0.0,0.0]);
                    let dd = h_resp.drag_delta();
                    let mut ax = prev[2] + dd.x;
                    let mut ay = prev[3] + dd.y;
                    if shift_held {
                        let aspect = (prev[0] / prev[1].max(1.0)).max(0.0001);
                        if ax.abs() * (1.0 / aspect) > ay.abs() { ay = ax / aspect; } else { ax = ay * aspect; }
                    }
                    ui.ctx().data_mut(|d| d.insert_temp(drag_size_id(idx), [prev[0], prev[1], ax, ay]));
                    let tw = snap(prev[0] + ax).max(MIN_W.min(8.0));
                    let th = snap(prev[1] + ay).max(MIN_H.min(8.0));
                    let (tw, th) = clamp_pin_frame_to_content(ui, outer_id, it, tw, th);
                    new_size[idx] = Some([tw, th]);
                }
            } else {
                // Ghost handle for discoverability — paint only.
                let ghost = egui::Color32::from_rgba_unmultiplied(150, 220, 255, 40);
                ui.painter().rect_filled(handle_rect, 2.0, ghost);
            }
        }

        // Per-item context menu. For the selected item, attach to a fresh
        // SECONDARY-only interact registered LAST (so it wins right-click
        // even on top of the body_drag handler). For non-selected items,
        // attach to the hit_responses entry.
        if is_sel {
            // Secondary-only sense → does not steal primary drag from the
            // body_drag interact registered earlier in this iteration.
            let ctx_resp = ui.interact(
                rect,
                egui::Id::new(("li_ctx", outer_id.0, idx)),
                egui::Sense::click(),
            );
            ctx_resp.context_menu(|ui| {
                layout_item_context_menu(
                    ui, idx, is_deco_idx, has_style_clip, n_selected,
                    &mut zaction, &mut delete_idx,
                    &mut dup_request, &mut copy_style_from, &mut paste_style,
                    &menu_header,
                );
            });
        } else if let Some((_, resp)) = hit_responses.iter().find(|(i, _)| *i == idx) {
            resp.clone().context_menu(|ui| {
                layout_item_context_menu(
                    ui, idx, is_deco_idx, has_style_clip, n_selected,
                    &mut zaction, &mut delete_idx,
                    &mut dup_request, &mut copy_style_from, &mut paste_style,
                    &menu_header,
                );
            });
        }
    }

    // Read the style clipboard BEFORE the mutable `sp` borrow below so the
    // paste-style path can apply it without re-borrowing ctx data mid-borrow.
    let style_clip_for_paste: Option<ItemStyle> = if paste_style {
        ui.ctx().data(|d| d.get_temp::<ItemStyle>(layout_style_clipboard_key()))
    } else { None };
    // Set inside the `sp` borrow when the layout becomes empty; applied after.
    let mut clear_unlocked = false;

    // ── Commit pending mutations ─────────────────────────────────────────────
    if let Some(sp) = snarl.get_node_mut(outer_id).and_then(|n| n.subpatch.as_mut()) {
        // Multi-drag: translate every selected item by the shared delta. Done
        // first (before the single-item new_pos pass, which is skipped while
        // is_multi). Each member is clamped to ≥0 and snapped independently so
        // the group keeps its relative layout.
        if let Some([ddx, ddy]) = multi_drag_delta {
            for &i in selected_set.iter() {
                if let Some(it) = sp.items.get_mut(i) {
                    match it {
                        LayoutItem::Module(m) => {
                            m.pos = [snap(m.pos[0] + ddx).max(0.0), snap(m.pos[1] + ddy).max(0.0)];
                        }
                        LayoutItem::Deco(d) => match d {
                            LayoutDecoration::Text { pos, .. }
                            | LayoutDecoration::Rect { pos, .. }
                            | LayoutDecoration::Ellipse { pos, .. }
                            | LayoutDecoration::Svg  { pos, .. } => {
                                *pos = [snap(pos[0] + ddx).max(0.0), snap(pos[1] + ddy).max(0.0)];
                            }
                            LayoutDecoration::Line { a, b, .. } => {
                                *a = [snap(a[0] + ddx).max(0.0), snap(a[1] + ddy).max(0.0)];
                                *b = [snap(b[0] + ddx).max(0.0), snap(b[1] + ddy).max(0.0)];
                            }
                        },
                    }
                }
            }
        }
        let n_items = sp.items.len();
        for (i, it) in sp.items.iter_mut().enumerate() {
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
            let sel = sp.selected_item;
            apply_zorder_action_items(&mut sp.items, i, act, n_items);
            if sel == Some(i) {
                sp.selected_item = match act {
                    "up"     if i + 1 < n_items => Some(i + 1),
                    "down"   if i > 0           => Some(i - 1),
                    "top"    if i + 1 < n_items => Some(sp.items.len() - 1),
                    "bottom" if i > 0           => Some(0),
                    _ => sel,
                };
            }
        }
        if let Some(i) = delete_idx.or(stale_remove) {
            if i < sp.items.len() {
                sp.items.remove(i);
                if let Some(s) = sp.selected_item {
                    if s == i { sp.selected_item = None; }
                    else if s > i { sp.selected_item = Some(s - 1); }
                }
            }
            // Deferred so we don't re-borrow `snarl` inside this `sp` borrow.
            if sp.items.is_empty() { clear_unlocked = true; }
        }
        if let Some(kind) = bg_add {
            let d = make_default_decoration(kind);
            sp.items.push(LayoutItem::Deco(d));
            sp.selected_item = Some(sp.items.len() - 1);
            sp.selected_items = vec![sp.items.len() - 1];
        }

        // ── Duplicate selection (decorations only) ──────────────────────
        // Clone every selected DECORATION, offset by [12,12] so the copy is
        // visible, append in the original z-order, and reselect the new
        // clones. Module pins are skipped (they reference an inner node).
        if dup_request {
            // Take the selection set to clone from (paint order preserved).
            let mut to_clone: Vec<usize> =
                if !selected_set.is_empty() { selected_set.clone() }
                else if let Some(p) = selected_idx { vec![p] }
                else { Vec::new() };
            to_clone.sort_unstable();
            let mut new_indices: Vec<usize> = Vec::new();
            for src in to_clone {
                if let Some(LayoutItem::Deco(d)) = sp.items.get(src) {
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
                    sp.items.push(LayoutItem::Deco(nd));
                    new_indices.push(sp.items.len() - 1);
                }
            }
            if !new_indices.is_empty() {
                sp.selected_item = new_indices.last().copied();
                sp.selected_items = new_indices;
                sp.cycle_pos = None;
            }
        }

        // ── Paste style across the selection ────────────────────────────
        // Apply the ctx style clipboard to every selected item (or the
        // primary if there's only a single selection) where the field
        // exists. `copy_style_from` is handled outside the `sp` borrow below.
        if paste_style {
            if let Some(clip) = style_clip_for_paste.as_ref() {
                let targets: Vec<usize> =
                    if !selected_set.is_empty() { selected_set.clone() }
                    else if let Some(p) = selected_idx { vec![p] }
                    else { Vec::new() };
                for t in targets {
                    if let Some(it) = sp.items.get_mut(t) {
                        clip.apply_to(it);
                    }
                }
            }
        }
    }

    // Deferred empty-layout cleanup (outside the `sp` borrow above).
    if clear_unlocked {
        if let Some(node) = snarl.get_node_mut(outer_id) {
            node.extra.layout_unlocked = false;
        }
    }

    // ── Copy style → ctx clipboard (outside the `sp` borrow) ────────────
    if let Some(src) = copy_style_from {
        let style = snarl.get_node(outer_id)
            .and_then(|n| n.subpatch.as_ref())
            .and_then(|sp| sp.items.get(src))
            .map(item_style_of);
        if let Some(style) = style {
            ui.ctx().data_mut(|d| d.insert_temp(layout_style_clipboard_key(), style));
        }
    }

    false
}
