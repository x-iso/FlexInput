//! Touch Zones machinery: zone-tree editing, field painting, mapping
//! cards, learn capture, divider overlays. Shared with the Virtual Menu
//! body (menu_body.rs) which drives the same card/field renderers.

use super::*;

// ── Touch Zones body ──────────────────────────────────────────────────────────

/// Read a field's divider edges from a node. Field 0 uses `col_edges`/`row_edges`;
/// field N>0 uses `col_edges{N}`/`row_edges{N}`.
pub(crate) fn tz_node_edges(node: &NodeData, field: usize, which: &str) -> Vec<f32> {
    let key = if field == 0 { which.to_string() } else { format!("{which}{field}") };
    node.params.get(&key).and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect())
        .unwrap_or_default()
}

pub(crate) fn tz_read_field_edges(snarl: &Snarl<NodeData>, node_id: NodeId, field: usize, which: &str) -> Vec<f32> {
    snarl.get_node(node_id).map(|n| tz_node_edges(n, field, which)).unwrap_or_default()
}

pub(crate) fn tz_write_field_edges(node: &mut NodeData, field: usize, which: &str, edges: &[f32]) {
    let key = if field == 0 { which.to_string() } else { format!("{which}{field}") };
    node.params.insert(key, Value::Array(edges.iter().map(|&v| Value::from(v as f64)).collect()));
}

/// Number of touch fields on the node (2 in split mode, else 1).
pub(crate) fn tz_n_fields(snarl: &Snarl<NodeData>, node_id: NodeId) -> usize {
    let split = snarl.get_node(node_id)
        .and_then(|n| n.params.get("field_mode").and_then(|v| v.as_str())) == Some("split");
    if split { 2 } else { 1 }
}

/// Reconstruct per-(field,zone) live state from a node's OWN computed outputs
/// (`extra.last_out`). Source-agnostic — works for physical, network, and
/// collector touch. Returns `(field, zone) → (local_x, local_y, active)`.
pub(crate) fn tz_zone_live(node: &NodeData) -> std::collections::HashMap<(usize, usize), (f32, f32, bool)> {
    use flexinput_core::touchzones as tz;
    let mut m = std::collections::HashMap::new();
    if let Some(ids) = node.params.get("output_pin_ids").and_then(|v| v.as_array()) {
        for (i, idv) in ids.iter().enumerate() {
            let Some(tz::Pin::Zone { field, idx, comp }) = idv.as_str().and_then(tz::parse_pin) else { continue };
            let Some(sig) = node.extra.last_out.get(i).copied().flatten() else { continue };
            let e = m.entry((field, idx)).or_insert((0.0, 0.0, false));
            match comp {
                tz::ZoneComp::X => e.0 = sig.as_float(),
                tz::ZoneComp::Y => e.1 = sig.as_float(),
                tz::ZoneComp::Active => e.2 = sig.as_bool(),
            }
        }
    }
    m
}

/// Live per-(field,zone) finger state resolved from the upstream device's touch
/// pins in `live_signals` — used by MAPPING mode, which has no zone output ports
/// for [`tz_zone_live`] to read. Mirrors the engine's zone resolution: single
/// mode folds both fingers onto field 0 (touch1 last so it wins); split mode maps
/// touch1→field0, touch2→field1. Returns local (x,y,active) per occupied zone.
/// Live per-(field,zone) finger state for the mapping-mode field: which zone each
/// finger is ACTIVATING (hold-aware) + its local position. Under "Hold" a finger
/// stays attributed to its ORIGIN zone even after sliding into a neighbour (the
/// neighbour reports no hit), mirroring the eval — so the glow / analog preview
/// track ACTUAL output, not mere presence. Local coords are relative to the
/// effective (origin-if-held) zone, clamped to 0..1 (a held finger dragged out
/// saturates at the zone edge). Per-finger start zones persist in ctx temp,
/// advanced once per pass so multiple widgets sharing a node don't double-step.
pub(crate) fn tz_live_hits(
    snarl: &Snarl<NodeData>,
    node_id: NodeId,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    automap_parent: Option<&AutomapGlowParent<'_>>,
    ctx: &egui::Context,
) -> std::collections::HashMap<(usize, usize), (f32, f32, bool)> {
    use flexinput_core::touchzones as tz;
    let mut m = std::collections::HashMap::new();
    let Some(dev) = remapper_upstream_device_id(snarl, node_id, 0, automap_parent) else { return m; };
    let node = snarl.get_node(node_id);
    let split = node.and_then(|n| n.params.get("field_mode").and_then(|v| v.as_str())) == Some("split");
    let hold_zones: std::collections::HashSet<(usize, usize)> = node
        .and_then(|n| n.params.get("hold_zones").and_then(|v| v.as_array()))
        .map(|a| a.iter().filter_map(|p| {
            let q = p.as_array()?;
            Some((q.first()?.as_u64()? as usize, q.get(1)?.as_u64()? as usize))
        }).collect())
        .unwrap_or_default();
    let readf = |pin: &str| live_signals.get(&(dev.clone(), pin.to_string())).map(|s| s.as_float()).unwrap_or(0.0);
    let readb = |pin: &str| live_signals.get(&(dev.clone(), pin.to_string())).map(|s| s.as_bool()).unwrap_or(false);
    // Per-finger [active, start_zone, centre_x, centre_y] × 2, advanced once per
    // pass. The centre is the adaptive relative origin captured at touchdown
    // (mirrors eval's analog_by_zone): a landing inside the zone's inner region
    // becomes the centre (relative), otherwise the zone centre (absolute). It lets
    // the live vectorscope + curve preview reflect the zone's relative/absolute
    // setting instead of a raw absolute position.
    let pass = ctx.cumulative_pass_nr();
    let track_id = egui::Id::new(("tz_live_track", node_id.0));
    let (stored_pass, mut track): (u64, Vec<f32>) =
        ctx.data(|d| d.get_temp(track_id)).unwrap_or((0, vec![0.0; 8]));
    if track.len() < 8 { track.resize(8, 0.0); }
    let advance = stored_pass != pass;
    let mut just_down: Option<(usize, usize)> = None;
    // Adaptive-centre deflection per START zone (unit space, +Y DOWN — callers
    // flip to +Y up). Keyed like eval's analog_by_zone and published in ctx so the
    // vectorscope + curve preview read the SAME value the engine emits.
    let mut defl: std::collections::HashMap<(usize, usize), (f32, f32)> = std::collections::HashMap::new();
    let adaptive_cards: Vec<Value> = node
        .and_then(|n| n.params.get("zone_maps").and_then(|v| v.as_array()).cloned())
        .unwrap_or_default();
    for finger in 0..2usize {
        let (px, py, pa) = [("touch1_x", "touch1_y", "touch1_active"),
                            ("touch2_x", "touch2_y", "touch2_active")][finger];
        let field = if split { finger } else { 0 };
        let base = finger * 4;
        let active = readb(pa);
        let prev_active = track[base] > 0.5;
        if !active {
            if advance { track[base] = 0.0; }
            continue;
        }
        let tree = tz_field_tree(snarl, node_id, field);
        let (x, y) = tz::pad_point_to_unit(readf(px), readf(py));
        let (cur_id, _, _) = tree.locate(x, y);
        let cur_idx = cur_id as usize;
        let start_zone = if !prev_active { cur_idx } else { track[base + 1] as usize };
        // START zone geometry drives both the absolute centre and the deflection
        // scale (matches eval: a half-zone move = full deflection).
        let [sx0, sy0, sx1, sy1] = tree.zone_rect(start_zone as u32).unwrap_or([0.0, 0.0, 1.0, 1.0]);
        let (scx, scy) = ((sx0 + sx1) * 0.5, (sy0 + sy1) * 0.5);
        let (shw, shh) = (((sx1 - sx0) * 0.5).max(1e-3), ((sy1 - sy0) * 0.5).max(1e-3));
        if advance {
            if !prev_active {
                track[base + 1] = cur_idx as f32;
                let inner = tz_zone_adaptive(&adaptive_cards, field, cur_idx);
                let (cx, cy) = if (x - scx).abs() <= inner * shw && (y - scy).abs() <= inner * shh {
                    (x, y)
                } else { (scx, scy) };
                track[base + 2] = cx;
                track[base + 3] = cy;
                // Newest touchdown wins the tab-follow (see render_touch_zones_*),
                // so two fingers don't flicker the cards panel between zones.
                just_down = Some((field, cur_idx));
            }
            track[base] = 1.0;
        }
        let eff = if hold_zones.contains(&(field, start_zone)) { start_zone } else { cur_idx };
        let [x0, y0, x1, y1] = tree.zone_rect(eff as u32).unwrap_or([0.0, 0.0, 1.0, 1.0]);
        let lx = if x1 > x0 { ((x - x0) / (x1 - x0)).clamp(0.0, 1.0) } else { 0.5 };
        let ly = if y1 > y0 { ((y - y0) / (y1 - y0)).clamp(0.0, 1.0) } else { 0.5 };
        m.insert((field, eff), (lx, ly, true));
        // Adaptive deflection about the captured centre, scaled by the START zone.
        let (cx, cy) = (track[base + 2], track[base + 3]);
        let dfx = ((x - cx) / shw).clamp(-1.0, 1.0);
        let dfy = ((y - cy) / shh).clamp(-1.0, 1.0);
        defl.insert((field, start_zone), (dfx, dfy));
    }
    if advance {
        ctx.data_mut(|d| d.insert_temp(track_id, (pass, track)));
        // Publish the last touched-down origin (pass-stamped) so the tab-follow
        // locks to it, and a pick-mode tap can act on a FRESH touchdown only.
        if let Some((f, z)) = just_down {
            ctx.data_mut(|d| d.insert_temp(
                egui::Id::new(("tz_last_origin", node_id.0)), (pass, f, z)));
        }
    }
    // Publish the adaptive deflection map for the live vectorscope + curve preview
    // (pass-stamped so a stale frame doesn't leak a phantom deflection).
    ctx.data_mut(|d| d.insert_temp(
        egui::Id::new(("tz_live_defl", node_id.0)), (pass, defl.clone())));

    // Per-zone ACTIVE output pins for the on-pad activation glow: a finger is in
    // the zone (hold-aware, from `m`) AND the card's trigger is satisfied this
    // frame (touch = finger present; click = pad also pressed). Swipes are
    // transient and skipped. Stashed in ctx so `tz_paint_zone_mapping` can light
    // the exact icons that are firing — including a click's button alongside the
    // analog vectorscope. Keyed by node so both fields/widgets read their own.
    let cards = node.and_then(|n| n.params.get("zone_maps").and_then(|v| v.as_array()).cloned())
        .unwrap_or_default();
    let mut active_out: std::collections::HashMap<(usize, usize), Vec<String>> = std::collections::HashMap::new();
    for (&(f, z), &(_, _, act)) in &m {
        if !act { continue; }
        let clicked = readb(if f == 0 { "btn_touchpad" } else { "btn_touchpad2" });
        for c in cards.iter().filter(|c|
            c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) == f as u64 &&
            c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) == z as u64)
        {
            let trig = c.get("in").and_then(|v| v.as_array()).and_then(|a| a.first())
                .and_then(|v| v.as_str()).unwrap_or("tz_touch");
            let fire = match trig {
                "tz_click" => clicked,
                t if t.starts_with("tz_swipe") => false, // transient — not shown
                _ => true, // tz_touch
            };
            if !fire { continue; }
            let e = active_out.entry((f, z)).or_default();
            for p in c.get("out").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|v| v.as_str()) {
                if !e.iter().any(|x| x == p) { e.push(p.to_string()); }
            }
        }
    }
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(("tz_active_out", node_id.0)), active_out));

    m
}

/// The BSP zone tree for a field: an explicit `zone_tree`/`zone_tree{field}` param
/// (once the user has added partial dividers), else derived from the legacy grid
/// (`col_edges`/`row_edges`). Single source of truth shared with the eval so zone
/// hit-testing, drawing and mapping stay in lock-step.
pub(crate) fn tz_field_tree(snarl: &Snarl<NodeData>, node_id: NodeId, field: usize)
    -> flexinput_core::touchzones::ZoneNode
{
    use flexinput_core::touchzones as tz;
    let key = if field == 0 { "zone_tree".to_string() } else { format!("zone_tree{field}") };
    if let Some(t) = snarl.get_node(node_id)
        .and_then(|n| n.params.get(&key)).and_then(tz::ZoneNode::from_value)
    {
        return t;
    }
    let col = tz_read_field_edges(snarl, node_id, field, "col_edges");
    let row = tz_read_field_edges(snarl, node_id, field, "row_edges");
    tz::ZoneNode::from_grid(&col, &row)
}

/// Write a field's zone tree back to its param, dropping the legacy grid edges for
/// that field so the tree becomes authoritative.
pub(crate) fn tz_set_field_tree(snarl: &mut Snarl<NodeData>, node_id: NodeId,
    field: usize, tree: &flexinput_core::touchzones::ZoneNode)
{
    let key = if field == 0 { "zone_tree".to_string() } else { format!("zone_tree{field}") };
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.params.insert(key, tree.to_value());
    }
}

/// Cards (this field) bound to any of `zones`.
pub(crate) fn tz_cards_in_zones(snarl: &Snarl<NodeData>, node_id: NodeId, field: usize, zones: &[u32]) -> usize {
    snarl.get_node(node_id).and_then(|n| n.params.get("zone_maps").and_then(|v| v.as_array()))
        .map(|cards| cards.iter().filter(|c|
            c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) == field as u64 &&
            zones.contains(&(c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) as u32))).count())
        .unwrap_or(0)
}

/// Remove the divider at `path`. If the zones it would merge away carry no
/// mappings, apply immediately; otherwise stash a pending-merge so the module
/// shows a confirm popup (`tz_render_merge_popup`).
pub(crate) fn tz_request_or_apply_merge(snarl: &mut Snarl<NodeData>, node_id: NodeId,
    field: usize, tree: &flexinput_core::touchzones::ZoneNode, path: &[u8])
{
    let mut probe = tree.clone();
    let Some((_, removed)) = probe.remove_split(path, None) else { return; };
    if tz_cards_in_zones(snarl, node_id, field, &removed) == 0 {
        tz_set_field_tree(snarl, node_id, field, &probe); // nothing to lose — merge now
    } else if let Some(node) = snarl.get_node_mut(node_id) {
        node.params.insert("_tz_merge".into(), Value::Object(serde_json::Map::from_iter([
            ("field".to_string(), Value::from(field as u64)),
            ("path".to_string(), Value::Array(path.iter().map(|&b| Value::from(b as u64)).collect())),
        ])));
    }
}

/// If a merge is pending (`_tz_merge`), draw the confirm popup: the removed
/// zone(s) carry mappings, so the user picks whether to DELETE those mappings,
/// keep them by re-homing onto the surviving zone, or CANCEL.
pub(crate) fn tz_render_merge_popup(ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>, node_id: NodeId) {
    let Some(m) = snarl.get_node(node_id).and_then(|n| n.params.get("_tz_merge").cloned()) else { return; };
    let field = m.get("field").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let path: Vec<u8> = m.get("path").and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_u64().map(|b| b as u8)).collect()).unwrap_or_default();
    let mut tree = tz_field_tree(snarl, node_id, field);
    let mut probe = tree.clone();
    let Some((kept, removed)) = probe.remove_split(&path, None) else {
        if let Some(n) = snarl.get_node_mut(node_id) { n.params.remove("_tz_merge"); }
        return;
    };
    let _ = kept;
    let n_cards = tz_cards_in_zones(snarl, node_id, field, &removed);
    let mut choice: Option<&'static str> = None;
    egui::Window::new("Remove divider")
        .id(egui::Id::new(("tz_merge_popup", node_id.0)))
        .collapsible(false).resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ui.ctx(), |ui| {
            ui.label(format!("Merging removes {} zone(s) that carry {} mapping(s).",
                removed.len(), n_cards));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("Choose inheritor (tap a zone)")
                    .on_hover_text("Pick which zone the mappings should move to — then the divider is removed.")
                    .clicked() { choice = Some("pick"); }
                if ui.button("Delete mappings").clicked() { choice = Some("delete"); }
                if ui.button("Cancel").clicked() { choice = Some("cancel"); }
            });
        });
    let Some(choice) = choice else { return; };
    match choice {
        // Enter merge-pick mode; the zone tap handler runs remove + re-home.
        "pick" => {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("_tz_pick".into(), serde_json::json!({
                    "kind": "merge", "field": field,
                    "path": path.iter().map(|&b| b as u64).collect::<Vec<_>>(),
                }));
            }
        }
        "delete" => {
            tree.remove_split(&path, None);
            tz_set_field_tree(snarl, node_id, field, &tree);
            if let Some(node) = snarl.get_node_mut(node_id) {
                if let Some(cards) = node.params.get_mut("zone_maps").and_then(|v| v.as_array_mut()) {
                    cards.retain(|c| !(c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) == field as u64 &&
                        removed.contains(&(c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) as u32))));
                }
            }
        }
        _ => {} // cancel
    }
    if let Some(node) = snarl.get_node_mut(node_id) { node.params.remove("_tz_merge"); }
}

// ── "Pick a destination zone" mode — shared by Migrate (move a zone's mappings)
// and the delete-inheritor flow (merge, then re-home the removed zones' cards).
// While active, clicking / tapping a zone applies the action instead of selecting.

pub(crate) fn tz_pick_kind(snarl: &Snarl<NodeData>, node_id: NodeId) -> Option<String> {
    snarl.get_node(node_id).and_then(|n| n.params.get("_tz_pick"))
        .and_then(|v| v.get("kind")).and_then(|v| v.as_str()).map(String::from)
}

pub(crate) fn tz_cancel_pick(snarl: &mut Snarl<NodeData>, node_id: NodeId) {
    if let Some(n) = snarl.get_node_mut(node_id) { n.params.remove("_tz_pick"); }
}

/// Begin a Migrate: the SELECTED zone's mappings will move onto the next zone the
/// user clicks / taps.
pub(crate) fn tz_start_migrate(snarl: &mut Snarl<NodeData>, node_id: NodeId) {
    let (field, zone) = tz_read_selection(snarl, node_id);
    if let Some(n) = snarl.get_node_mut(node_id) {
        n.params.insert("_tz_pick".into(), serde_json::json!({
            "kind": "migrate", "field": field, "src": zone,
        }));
    }
}

/// Re-home every card in `from_zones` (this field) onto `dest`.
pub(crate) fn tz_move_cards(snarl: &mut Snarl<NodeData>, node_id: NodeId, field: usize,
    from_zones: &[u32], dest: u32)
{
    if let Some(node) = snarl.get_node_mut(node_id) {
        if let Some(cards) = node.params.get_mut("zone_maps").and_then(|v| v.as_array_mut()) {
            for c in cards.iter_mut() {
                let f = c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let z = c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                if f == field && from_zones.contains(&z) {
                    if let Some(o) = c.as_object_mut() { o.insert("z".into(), Value::from(dest as u64)); }
                }
            }
        }
    }
}

/// Apply the pending pick to destination zone id `dest`, then clear the mode.
pub(crate) fn tz_apply_pick(snarl: &mut Snarl<NodeData>, node_id: NodeId, dest: usize) {
    let Some(pick) = snarl.get_node(node_id).and_then(|n| n.params.get("_tz_pick").cloned()) else { return; };
    let kind = pick.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    let field = pick.get("field").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    match kind {
        "migrate" => {
            let src = pick.get("src").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            if dest as u32 != src { tz_move_cards(snarl, node_id, field, &[src], dest as u32); }
        }
        "merge" => {
            let path: Vec<u8> = pick.get("path").and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_u64().map(|b| b as u8)).collect()).unwrap_or_default();
            let mut tree = tz_field_tree(snarl, node_id, field);
            if let Some((kept, removed)) = tree.remove_split(&path, None) {
                tz_set_field_tree(snarl, node_id, field, &tree);
                // Tapping one of the merged-away zones means "the survivor".
                let dest = if removed.contains(&(dest as u32)) { kept } else { dest as u32 };
                tz_move_cards(snarl, node_id, field, &removed, dest);
            }
        }
        _ => {}
    }
    tz_cancel_pick(snarl, node_id);
}

/// While picking, draw a banner over the pad prompting the user to choose a zone,
/// with a Cancel. Returns nothing; the zone click/tap handlers do the applying.
pub(crate) fn tz_pick_banner(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>,
    painter: &egui::Painter, rect: egui::Rect, accent: egui::Color32)
{
    let Some(kind) = tz_pick_kind(snarl, node_id) else { return; };
    painter.rect_stroke(rect, 4.0, egui::Stroke::new(2.0, accent), egui::epaint::StrokeKind::Inside);
    let msg = if kind == "migrate" { "Tap a zone to MOVE the mappings there" }
              else { "Tap a zone to INHERIT the mappings, then merge" };
    let br = egui::Rect::from_center_size(egui::pos2(rect.center().x, rect.top() + 14.0),
        egui::vec2(rect.width().min(300.0), 22.0));
    painter.rect_filled(br, 4.0, egui::Color32::from_black_alpha(180));
    painter.text(br.center(), egui::Align2::CENTER_CENTER, msg,
        egui::FontId::proportional(12.0), accent);
    let cancel = egui::Rect::from_min_size(egui::pos2(rect.right() - 60.0, rect.bottom() - 24.0), egui::vec2(52.0, 18.0));
    if ui.interact(cancel, ui.id().with((node_id, "tzpickcancel")), egui::Sense::click()).clicked() {
        tz_cancel_pick(snarl, node_id);
    }
    painter.rect_filled(cancel, 3.0, egui::Color32::from_black_alpha(180));
    painter.text(cancel.center(), egui::Align2::CENTER_CENTER, "Cancel",
        egui::FontId::proportional(11.0), egui::Color32::WHITE);
    ui.ctx().request_repaint();
}

/// Subdivide the zone under unit point `(ux, uy)` at its own centre along `axis`.
/// `new_low` puts the new EMPTY cell on the low side (left/top) so a "+" on a
/// zone's left/top edge adds the empty cell there and pushes the mapping the
/// other way.
pub(crate) fn tz_subdivide_at(snarl: &mut Snarl<NodeData>, node_id: NodeId, field: usize,
    ux: f32, uy: f32, axis: flexinput_core::touchzones::Axis, new_low: bool)
{
    use flexinput_core::touchzones::Axis;
    let mut tree = tz_field_tree(snarl, node_id, field);
    let (id, _, _) = tree.locate(ux, uy);
    let center = tree.zone_rect(id).map(|[x0, y0, x1, y1]| match axis {
        Axis::V => (x0 + x1) * 0.5,
        Axis::H => (y0 + y1) * 0.5,
    }).unwrap_or(0.5);
    if tree.subdivide_side(id, axis, center, new_low).is_some() {
        tz_set_field_tree(snarl, node_id, field, &tree);
    }
}

/// Tree version of the hover-revealed +/- overlay (mapping mode): a "−" on each
/// divider removes/merges it (raising the mapped-zone confirm popup when needed),
/// and — for the zone under the cursor — a "+" on the NEAREST edge splits that
/// zone, adding the new empty cell on THAT side (so the "+" you reach for is the
/// side the cell appears). The "+" tracks each zone's edges, not just the pad
/// border.
#[allow(clippy::too_many_arguments)]
pub(crate) fn tz_tree_line_overlay(
    node_id: NodeId,
    field: usize,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    painter: &egui::Painter,
    rect: egui::Rect,
    accent: egui::Color32,
    visuals: &egui::Visuals,
) {
    use flexinput_core::touchzones::Axis;
    let tree = tz_field_tree(snarl, node_id, field);
    let to_x = |u: f32| rect.left() + u * rect.width();
    let to_y = |u: f32| rect.top() + u * rect.height();
    let edge = 28.0;     // edge-proximity threshold (px) that reveals the "+"
    let inset = 12.0;    // "+" inset from the zone edge so it sits inside the cell
    let from_global = ui.ctx().layer_transform_to_global(ui.layer_id())
        .unwrap_or(egui::emath::TSTransform::IDENTITY)
        .inverse();
    let ptr = ui.input(|i| i.pointer.hover_pos()).map(|p| from_global * p);
    // (ux, uy, axis, new_low)
    let mut sub: Option<(f32, f32, Axis, bool)> = None;
    let mut rem: Option<Vec<u8>> = None;
    let divs = tree.dividers();

    // Nearest divider under the pointer (within a few px of its line) → the "−"
    // shows there dynamically, like the "+", instead of one marker per divider.
    let near_div: Option<usize> = ptr.filter(|p| rect.contains(*p)).and_then(|p| {
        divs.iter().enumerate().filter_map(|(di, div)| {
            let (d, on_span) = match div.axis {
                Axis::V => ((p.x - to_x(div.pos)).abs(),
                    p.y >= to_y(div.span_lo) - 6.0 && p.y <= to_y(div.span_hi) + 6.0),
                Axis::H => ((p.y - to_y(div.pos)).abs(),
                    p.x >= to_x(div.span_lo) - 6.0 && p.x <= to_x(div.span_hi) + 6.0),
            };
            (on_span && d <= 10.0).then_some((di, d))
        }).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(di, _)| di)
    });

    if let Some(di) = near_div {
        // "−" on the hovered divider → remove/merge.
        let div = &divs[di];
        let mid = (div.span_lo + div.span_hi) * 0.5;
        let c = match div.axis {
            Axis::V => egui::pos2(to_x(div.pos), to_y(mid)),
            Axis::H => egui::pos2(to_x(mid), to_y(div.pos)),
        };
        if tz_mini_button(ui, painter, ui.id().with((node_id, "tztm", field, di)),
            c, "−", accent, visuals) { rem = Some(div.path.clone()); }
    } else if let Some(p) = ptr.filter(|p| rect.contains(*p)) {
        // "+" on the hovered zone's nearest edge.
        for (id, [x0, y0, x1, y1]) in tree.zones() {
            let zr = egui::Rect::from_min_max(egui::pos2(to_x(x0), to_y(y0)), egui::pos2(to_x(x1), to_y(y1)));
            if !zr.contains(p) { continue; }
            let (dl, dr, dt, db) = (p.x - zr.left(), zr.right() - p.x, p.y - zr.top(), zr.bottom() - p.y);
            let m = dl.min(dr).min(dt).min(db);
            if m <= edge {
                let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
                let (pos, axis, new_low) = if m == dl {
                    (egui::pos2(zr.left() + inset, zr.center().y), Axis::V, true)
                } else if m == dr {
                    (egui::pos2(zr.right() - inset, zr.center().y), Axis::V, false)
                } else if m == dt {
                    (egui::pos2(zr.center().x, zr.top() + inset), Axis::H, true)
                } else {
                    (egui::pos2(zr.center().x, zr.bottom() - inset), Axis::H, false)
                };
                if tz_mini_button(ui, painter, ui.id().with((node_id, "tztp", field, id)),
                    pos, "+", accent, visuals) { sub = Some((cx, cy, axis, new_low)); }
            }
            let _ = id;
            break; // only the hovered zone
        }
    }

    if let Some((ux, uy, axis, new_low)) = sub {
        tz_subdivide_at(snarl, node_id, field, ux, uy, axis, new_low);
    }
    if let Some(path) = rem { tz_request_or_apply_merge(snarl, node_id, field, &tree, &path); }
}

/// The centred square used for a zone's analog viz (response-curve graph when
/// idle, vectorscope when active) — shared by the painter and the interactive
/// curve editor so their geometry matches exactly.
pub(crate) fn tz_zone_scope_rect(zr: egui::Rect) -> egui::Rect {
    let sz = (zr.width().min(zr.height()) * 0.62).clamp(20.0, 64.0);
    egui::Rect::from_center_size(zr.center(), egui::vec2(sz, sz))
}

/// The response-curve control points for a zone's analog card (over the 0..1
/// deflection magnitude). Defaults to linear when none stored.
pub(crate) fn tz_zone_curve(zone_maps: &[Value], field: usize, idx: usize) -> Vec<[f32; 2]> {
    for c in zone_maps.iter().filter(|c|
        c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) == field as u64
            && c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) == idx as u64)
    {
        if let Some(arr) = c.get("curve").and_then(|v| v.as_array()) {
            let pts: Vec<[f32; 2]> = arr.iter().filter_map(|p| {
                let q = p.as_array()?;
                Some([q.first()?.as_f64()? as f32, q.get(1)?.as_f64()? as f32])
            }).collect();
            if pts.len() >= 2 { return pts; }
        }
    }
    vec![[0.0, 0.0], [1.0, 1.0]]
}

/// True when a zone has at least one analog (mouse / stick) output card.
/// Whether a Touch Zones OUT pin receives the zone's analog deflection —
/// the gate for the response curve, the Relative-center slider, and the
/// on-zone vectorscope. Static analog bus pins always do; a macro pin does
/// when its port's declared type carries the deflection (Vec2 / Float / Any —
/// Bool ports only take the gate). Resolved through the per-frame macro
/// registry, so a dangling id counts as digital.
pub(crate) fn tz_out_pin_is_analog(pin: &str) -> bool {
    if matches!(pin,
        "mouse" | "mouse_x" | "mouse_y" | "left_stick" | "right_stick" | "scroll_x" | "scroll_y")
    {
        return true;
    }
    if flexinput_core::macros::parse_macro_pin(pin).is_some() {
        return crate::macro_icons::registry_entry(pin).is_some_and(|e| matches!(
            e.signal_type,
            SignalType::Vec2 | SignalType::Float | SignalType::Any
        ));
    }
    false
}

pub(crate) fn tz_zone_is_analog(zone_maps: &[Value], field: usize, idx: usize) -> bool {
    zone_maps.iter().any(|c|
        c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) == field as u64
            && c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) == idx as u64
            && c.get("out").and_then(|v| v.as_array())
                .map(|a| a.iter().any(|p| p.as_str().map(tz_out_pin_is_analog).unwrap_or(false)))
                .unwrap_or(false))
}

/// Store `pts` as the response `curve` on the first analog card of (field, zone).
pub(crate) fn tz_set_zone_curve(snarl: &mut Snarl<NodeData>, node_id: NodeId,
    field: usize, idx: usize, pts: &[[f32; 2]])
{
    let is_analog = tz_out_pin_is_analog;
    let Some(node) = snarl.get_node_mut(node_id) else { return };
    let Some(cards) = node.params.get_mut("zone_maps").and_then(|v| v.as_array_mut()) else { return };
    for c in cards.iter_mut() {
        let f = c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let z = c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        if f != field || z != idx { continue; }
        let analog = c.get("out").and_then(|v| v.as_array())
            .map(|a| a.iter().any(|p| p.as_str().map(is_analog).unwrap_or(false)))
            .unwrap_or(false);
        if !analog { continue; }
        if let Some(obj) = c.as_object_mut() {
            obj.insert("curve".to_string(), Value::Array(pts.iter()
                .map(|p| Value::Array(vec![Value::from(p[0] as f64), Value::from(p[1] as f64)]))
                .collect()));
        }
        return;
    }
}

/// The zone's adaptive-centre inner fraction (0..1): how much of the zone acts as
/// a RELATIVE centre for analog deflection (0 = absolute from zone centre, 1 =
/// wherever you touch is the centre). Stored on the analog card. Default 0.30.
pub(crate) fn tz_zone_adaptive(zone_maps: &[Value], field: usize, idx: usize) -> f32 {
    zone_maps.iter().filter(|c|
        c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) == field as u64 &&
        c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) == idx as u64)
        .find_map(|c| c.get("adaptive").and_then(|v| v.as_f64()))
        .map(|v| (v as f32).clamp(0.0, 1.0)).unwrap_or(0.30)
}

// (tz_set_zone_adaptive removed: the relative/absolute setting is now edited
//  per-card directly on each analog card via the card's own `adaptive` key.)

/// True when zone `(field, zone)` is marked "hold" (a gesture starting there
/// stays bound to it even if the finger slides into a neighbouring zone).
pub(crate) fn tz_zone_held(snarl: &Snarl<NodeData>, node_id: NodeId, field: usize, zone: usize) -> bool {
    snarl.get_node(node_id)
        .and_then(|n| n.params.get("hold_zones").and_then(|v| v.as_array()))
        .map(|a| a.iter().any(|p| p.as_array().map(|q|
            q.first().and_then(|v| v.as_u64()) == Some(field as u64)
                && q.get(1).and_then(|v| v.as_u64()) == Some(zone as u64)).unwrap_or(false)))
        .unwrap_or(false)
}

/// Set/clear the "hold" flag for zone `(field, zone)` in the `hold_zones` param
/// (a list of `[field, zone]` pairs).
pub(crate) fn tz_set_zone_held(snarl: &mut Snarl<NodeData>, node_id: NodeId,
    field: usize, zone: usize, held: bool)
{
    let Some(node) = snarl.get_node_mut(node_id) else { return };
    let mut list: Vec<Value> = node.params.get("hold_zones")
        .and_then(|v| v.as_array()).cloned().unwrap_or_default();
    list.retain(|p| p.as_array().map(|q| !(
        q.first().and_then(|v| v.as_u64()) == Some(field as u64)
            && q.get(1).and_then(|v| v.as_u64()) == Some(zone as u64))).unwrap_or(true));
    if held {
        list.push(Value::Array(vec![Value::from(field as u64), Value::from(zone as u64)]));
    }
    node.params.insert("hold_zones".into(), Value::Array(list));
}

/// A full-size interactive response-curve editor for a zone's analog output,
/// shown in the CARD ROW (below the pad) where there's room — the tiny on-zone
/// graph is a read-only preview. Behaves like the Response Curve module's graph:
/// drag points (endpoints move in Y, interior in X+Y), double-click empty space
/// to add a point, right-click a point to remove it. X = deflection magnitude
/// 0..1, Y = output 0..1. Writes the first analog card's `curve`; `live_mag`
/// draws the current input→output dot when the zone is active.
/// Default (identity) card curve — a card carrying exactly this stores no
/// `curve` param at all, keeping saved patches lean.
pub(crate) fn identity_curve() -> Vec<[f32; 2]> {
    vec![[0.0, 0.0], [1.0, 1.0]]
}

/// Normalize points pasted/loaded from elsewhere so the card sampler's
/// invariants hold: clamp to the 0..1 unit box, sort by x, pin the endpoints
/// to x=0 / x=1, and guarantee at least two points.
pub(crate) fn sanitize_card_curve(pts: &mut Vec<[f32; 2]>) {
    for p in pts.iter_mut() {
        p[0] = p[0].clamp(0.0, 1.0);
        p[1] = p[1].clamp(0.0, 1.0);
    }
    pts.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap_or(std::cmp::Ordering::Equal));
    if pts.len() < 2 {
        *pts = identity_curve();
        return;
    }
    pts.first_mut().unwrap()[0] = 0.0;
    pts.last_mut().unwrap()[0] = 1.0;
}

/// Save a card curve as a `.fxc` file — same format the Response Curve
/// module writes, so files are interchangeable (card curves live in the
/// 0..1 magnitude space, hence the 0-based ranges).
pub(crate) fn card_curve_save(pts: &[[f32; 2]]) {
    let cf = CurveFile {
        points: pts.iter().map(|p| [p[0] as f64, p[1] as f64]).collect(),
        biases: vec![],
        absolute: true,
        in_min: 0.0,
        in_max: 1.0,
        out_min: 0.0,
        out_max: 1.0,
        grid_x: 4,
        grid_y: 4,
        snap: false,
        scale_t: 0.0,
        trail_ms: 300,
        show_scaled_grid: false,
        show_grid_labels: false,
    };
    if let Some(path) = crate::overlay::with_overlay_not_topmost(|| {
        rfd::FileDialog::new()
            .add_filter("FlexInput Curve", &["fxc"])
            .set_file_name("curve.fxc")
            .save_file()
    }) {
        if let Ok(json) = serde_json::to_string_pretty(&cf) {
            let _ = std::fs::write(path, json);
        }
    }
}

/// Load ONLY the points from a `.fxc` file (module settings in the file are
/// ignored, mirroring the module-side "Load only the curve" semantics).
pub(crate) fn card_curve_load() -> Option<Vec<[f32; 2]>> {
    let path = crate::overlay::with_overlay_not_topmost(|| {
        rfd::FileDialog::new()
            .add_filter("FlexInput Curve", &["fxc"])
            .pick_file()
    })?;
    let text = std::fs::read_to_string(path).ok()?;
    let cf: CurveFile = serde_json::from_str(&text).ok()?;
    let mut pts: Vec<[f32; 2]> = cf.points.iter().map(|p| [p[0] as f32, p[1] as f32]).collect();
    sanitize_card_curve(&mut pts);
    Some(pts)
}

/// Per-mapping-card response-curve editor. Drag points, double-click to add,
/// right-click a point to remove; right-click the background for the shared
/// curve menu (Reset / Copy / Paste / Save… / Load… — same clipboard and
/// `.fxc` files as the Response Curve module).
///
/// `threshold`: `Some(slot)` shows the manual-activation controls — a
/// HORIZONTAL line over the curve's OUTPUT plus a checkbox row. While set,
/// a digital binding is held whenever the shaped magnitude sits on/above the
/// line and releases the moment it dips below (see the matching engine
/// logic in `eval.rs`). Drag the line vertically to tune it.
///
/// `nav_uid`: publishes the gamepad-nav graph geometry / selection rings on
/// the channels the TZ zone editor used (`gp_nav_curve_geom` etc.) — passed
/// only by the Touch Zones first-analog card so controller curve editing
/// keeps working; Remapper/Lean card curves are mouse-edited for now.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn mapping_curve_editor(
    ui: &mut egui::Ui,
    id_salt: egui::Id,
    pts: &mut Vec<[f32; 2]>,
    threshold: Option<&mut Option<f32>>,
    live_mag: Option<f32>,
    accent: egui::Color32,
    visuals: &egui::Visuals,
    nav_uid: Option<usize>,
    // Gamepad-focused curve field on this card: Some(6) = threshold (highlight the
    // line + enable row so the user sees what up/down / South act on).
    nav_curve_field: Option<u64>,
) -> bool {
    let mut changed = false;
    let w = ui.available_width().clamp(140.0, 360.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 104.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, visuals.extreme_bg_color);
    painter.rect_stroke(rect, 3.0, egui::Stroke::new(1.0, visuals.weak_text_color()), egui::StrokeKind::Inside);
    let g = rect.shrink(8.0);
    let to = |x: f32, y: f32| egui::pos2(
        g.left() + x.clamp(0.0, 1.0) * g.width(),
        g.bottom() - y.clamp(0.0, 1.0) * g.height());
    let unto = |p: egui::Pos2| (
        ((p.x - g.left()) / g.width()).clamp(0.0, 1.0),
        ((g.bottom() - p.y) / g.height()).clamp(0.0, 1.0));

    // ── Gamepad-nav integration (nav_uid channels only) ───────────────────
    // Publish the graph geometry (GLOBAL space, 0..1 both axes) so the shared
    // curve driver (`nav_drive_curve_dots`/`_dot`) can add/move/delete dots here
    // exactly like the Response Curve module's graph. Read back the selected dot
    // (while entered) + a focus flag (while the curve row is highlighted but not
    // yet entered) to draw the matching rings.
    let pass = ui.ctx().cumulative_pass_nr();
    let mut nav_sel_dot: Option<usize> = None;
    let mut nav_editing = false;
    if let Some(uid) = nav_uid {
        let to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
            .unwrap_or(egui::emath::TSTransform::IDENTITY);
        ui.ctx().data_mut(|d| d.insert_temp(
            egui::Id::new(("gp_nav_curve_geom", uid)),
            (pass, to_global * g, 0.0f32, 1.0f32, 0.0f32, 1.0f32)));
        let nav_sel: Option<(u64, usize, bool)> = ui.ctx()
            .data(|d| d.get_temp(egui::Id::new(("gp_nav_curve_sel", uid))));
        let nav_sel = nav_sel.filter(|(p, _, _)| pass.saturating_sub(*p) <= 1);
        nav_sel_dot = nav_sel.map(|(_, i, _)| i);
        nav_editing = nav_sel.map(|(_, _, e)| e).unwrap_or(false);
        let nav_focused: bool = ui.ctx()
            .data(|d| d.get_temp::<u64>(egui::Id::new(("gp_nav_tz_curve_focus", uid))))
            .map(|p| pass.saturating_sub(p) <= 1).unwrap_or(false);
        if nav_focused || nav_sel_dot.is_some() {
            painter.rect_stroke(rect, 3.0, egui::Stroke::new(1.5, accent), egui::StrokeKind::Inside);
        }
        // Gamepad users can't scroll manually, and the whole-module wrapper uses a
        // MANUAL scroll offset (not an egui ScrollArea, so `scroll_to_rect` is a
        // no-op). Publish a body-space scroll delta on the same channel the cards
        // use (`gp_nav_remap_scroll`) so the wrapper keeps the focused element in
        // view — the graph while editing, and the threshold enable-row (which sits
        // ~30px BELOW the graph) while the threshold field is focused.
        if nav_focused || nav_sel_dot.is_some() || nav_curve_field.is_some() {
            let extra = if nav_curve_field == Some(6) { 32.0 } else { 0.0 };
            let target = egui::Rect::from_min_max(rect.min, rect.max + egui::vec2(0.0, extra));
            let clip = ui.clip_rect();
            let mut need = 0.0f32;
            if target.top() < clip.top() + 4.0 {
                need = target.top() - (clip.top() + 4.0);
            } else if target.bottom() > clip.bottom() - 4.0 {
                need = target.bottom() - (clip.bottom() - 4.0);
            }
            if need.abs() > 1.0 {
                ui.ctx().data_mut(|d| d.insert_temp(
                    egui::Id::new(("gp_nav_remap_scroll", uid)), (pass, need)));
                request_repaint_throttled(ui.ctx());
            }
        }
    }

    // Grid + identity reference.
    let grid = visuals.weak_text_color().gamma_multiply(0.35);
    for k in 1..4 {
        let t = k as f32 / 4.0;
        painter.line_segment([to(t, 0.0), to(t, 1.0)], egui::Stroke::new(0.5, grid));
        painter.line_segment([to(0.0, t), to(1.0, t)], egui::Stroke::new(0.5, grid));
    }
    painter.line_segment([to(0.0, 0.0), to(1.0, 1.0)], egui::Stroke::new(0.5, grid));

    // Add-area first (bottom of z-order); handles after so they win their rects.
    // NOTE: `interact_pointer_pos()` is ALREADY in this UI's local space (even
    // inside the whole-module scale layer), so it maps directly through `unto` —
    // do NOT apply the layer transform here (that double-transforms and scatters
    // the points).
    let bg = ui.interact(g, id_salt.with("bg"), egui::Sense::click());
    // Threshold-line drag band — registered BEFORE the point handles so a
    // handle sitting on the line still wins its own 16px rect.
    let mut thr_val: Option<f32> = threshold.as_ref().and_then(|s| **s);
    if let Some(t) = thr_val {
        let ly = to(0.0, t).y;
        let band = egui::Rect::from_min_max(
            egui::pos2(g.left(), ly - 6.0), egui::pos2(g.right(), ly + 6.0));
        let tr = ui.interact(band, id_salt.with("thr"), egui::Sense::drag());
        if tr.hovered() || tr.dragged() {
            tr.clone().on_hover_cursor(egui::CursorIcon::ResizeVertical);
        }
        if tr.dragged() {
            if let Some(p) = tr.interact_pointer_pos() {
                let (_, ny) = unto(p);
                thr_val = Some(ny.clamp(0.01, 1.0));
                changed = true;
            }
        }
    }
    let mut remove: Option<usize> = None;
    let n = pts.len();
    for i in 0..n {
        let hp = to(pts[i][0], pts[i][1]);
        let r = ui.interact(egui::Rect::from_center_size(hp, egui::vec2(16.0, 16.0)),
            id_salt.with(("pt", i)), egui::Sense::click_and_drag());
        let hot = r.hovered() || r.dragged();
        if hot { r.clone().on_hover_cursor(egui::CursorIcon::Grab); }
        if r.dragged() {
            if let Some(p) = r.interact_pointer_pos() {
                let (nx, ny) = unto(p);
                // Interior points stay ordered; guard the clamp so a crowded pair
                // (lo > hi) can't panic — pin to the midpoint of the neighbours.
                let x = if i == 0 { 0.0 } else if i + 1 == n { 1.0 } else {
                    let lo = pts[i - 1][0] + 0.03;
                    let hi = pts[i + 1][0] - 0.03;
                    if lo <= hi { nx.clamp(lo, hi) } else { (pts[i - 1][0] + pts[i + 1][0]) * 0.5 }
                };
                pts[i] = [x, ny];
                changed = true;
            }
        }
        if r.secondary_clicked() && i != 0 && i + 1 != n { remove = Some(i); }
    }
    if let Some(i) = remove {
        pts.remove(i);
        changed = true;
    } else if bg.double_clicked() {
        if let Some(p) = bg.interact_pointer_pos() {
            let (nx, ny) = unto(p);
            let at = pts.iter().position(|q| q[0] > nx).unwrap_or(pts.len());
            if at > 0 && at < pts.len() {
                pts.insert(at, [nx.clamp(0.02, 0.98), ny]);
                changed = true;
            }
        }
    }

    // Threshold line (under the curve): dashed, warm color, right-edge knob.
    // Thickens + gains an accent halo while the gamepad has the threshold field
    // focused (field 6) so the user sees it's the drag target.
    let thr_focused = nav_curve_field == Some(6);
    if let Some(t) = thr_val {
        let ly = to(0.0, t).y;
        let thr_col = egui::Color32::from_rgb(255, 170, 60);
        painter.add(egui::Shape::dashed_line(
            &[egui::pos2(g.left(), ly), egui::pos2(g.right(), ly)],
            egui::Stroke::new(if thr_focused { 2.2 } else { 1.2 }, thr_col), 5.0, 4.0));
        let knob = egui::pos2(g.right() - 4.0, ly);
        painter.circle_filled(knob, if thr_focused { 4.5 } else { 3.5 }, thr_col);
        if thr_focused {
            painter.circle_stroke(knob, 6.5, egui::Stroke::new(1.5, accent));
        }
    } else if thr_focused {
        // Threshold OFF but focused → hint the line at the default so South-to-
        // enable has an anchor.
        let ly = to(0.0, 0.5).y;
        painter.add(egui::Shape::dashed_line(
            &[egui::pos2(g.left(), ly), egui::pos2(g.right(), ly)],
            egui::Stroke::new(1.0, accent.gamma_multiply(0.6)), 4.0, 4.0));
    }

    // Curve polyline (over the possibly-just-edited points) + handles.
    for wnd in pts.windows(2) {
        painter.line_segment([to(wnd[0][0], wnd[0][1]), to(wnd[1][0], wnd[1][1])],
            egui::Stroke::new(1.8, accent));
    }
    for (i, p) in pts.iter().enumerate() {
        painter.circle_filled(to(p[0], p[1]), 4.0, accent);
        painter.circle_stroke(to(p[0], p[1]), 4.0, egui::Stroke::new(1.0, visuals.extreme_bg_color));
        // Gamepad-selected dot: accent ring (thicker while being moved/entered).
        if nav_sel_dot == Some(i) {
            painter.circle_stroke(to(p[0], p[1]), if nav_editing { 8.0 } else { 6.5 },
                egui::Stroke::new(2.0, accent));
        }
    }
    // Live input→output dot; green while it would hold a thresholded binding.
    if let Some(m) = live_mag {
        let m = m.clamp(0.0, 1.0);
        let y = flexinput_engine::sample_curve(pts, m, &[]).clamp(0.0, 1.0);
        let on = thr_val.map(|t| y >= t).unwrap_or(false);
        let col = if on { egui::Color32::from_rgb(110, 230, 130) }
                  else  { egui::Color32::from_rgb(90, 200, 255) };
        painter.circle_filled(to(m, y), 3.0, col);
        request_repaint_throttled(ui.ctx());
    }

    // Shared curve menu — same clipboard and .fxc files as the Response
    // Curve module, so shapes travel freely between modules and cards.
    bg.context_menu(|ui| {
        if ui.button("Reset").clicked() {
            *pts = identity_curve();
            changed = true;
            ui.close();
        }
        ui.separator();
        if ui.button("Copy").clicked() {
            curve_clipboard_set(ui.ctx(), CurveClip { points: pts.clone(), biases: vec![] });
            ui.close();
        }
        let has_clip = curve_clipboard_get(ui.ctx()).is_some();
        if ui.add_enabled(has_clip, egui::Button::new("Paste")).clicked() {
            if let Some(clip) = curve_clipboard_get(ui.ctx()) {
                *pts = clip.points;
                sanitize_card_curve(pts);
                changed = true;
            }
            ui.close();
        }
        ui.separator();
        if ui.button("Save…").clicked() {
            card_curve_save(pts);
            ui.close();
        }
        if ui.button("Load…").clicked() {
            if let Some(p) = card_curve_load() {
                *pts = p;
                changed = true;
            }
            ui.close();
        }
    });

    // Threshold enable + readout row (shown only where a manual activation
    // point is meaningful — the caller decides via `threshold`).
    if let Some(slot) = threshold {
        let mut on = thr_val.is_some();
        let row = ui.horizontal(|ui| {
            if ui.checkbox(&mut on, egui::RichText::new("Activation threshold").small())
                .on_hover_text("Manual activation point for digital outputs: the binding is held while the curve's output sits on/above the orange line and releases the moment it dips below. Off = default behaviour (freq-modulated taps for analog mode, built-in stick threshold otherwise). Drag the line on the graph to tune it.")
                .changed()
            {
                thr_val = if on { Some(0.5) } else { None };
                changed = true;
            }
            if let Some(t) = thr_val.as_mut() {
                let mut pct = *t * 100.0;
                if ui.add(egui::DragValue::new(&mut pct).speed(1.0).range(1.0..=100.0)
                    .suffix("%").fixed_decimals(0)).changed()
                {
                    *t = pct / 100.0;
                    changed = true;
                }
            }
        });
        // Accent ring around the enable row while the gamepad has it focused, so
        // the "toggle / adjust threshold" target is unmistakable.
        if thr_focused {
            ui.painter().rect_stroke(row.response.rect.expand(2.0), 3.0,
                egui::Stroke::new(1.5, accent), egui::StrokeKind::Outside);
        }
        *slot = thr_val;
    }

    changed
}

/// `Some(node.0)` when card `idx` (scope) is the gamepad-nav ENTERED card, so its
/// curve section should publish geometry to the shared curve nav channel and ring
/// while being dot-edited. Only the entered card publishes (one per node), so the
/// per-node geometry channel never collides across cards.
pub(crate) fn curve_nav_uid(ctx: &egui::Context, node_id: NodeId, scope: &str, idx: usize) -> Option<usize> {
    let pass = ctx.cumulative_pass_nr();
    ctx.data(|d| d.get_temp::<(u64, usize, bool)>(
            egui::Id::new(("gp_nav_remap_card", node_id.0, scope))))
        .filter(|(p, sel, ent)| *ent && *sel == idx && pass.saturating_sub(*p) <= 1)
        .map(|_| node_id.0)
}

/// Slim expander strip + (when open) the curve editor for one mapping card.
/// Reads/writes `curve` + `threshold` on the card's `working` map; returns
/// true when the card changed. Identity curve + no threshold stores nothing,
/// so untouched cards stay lean and take the engine's default paths.
///
/// `nav_uid`: forwarded to the editor AND force-opens the section while the
/// gamepad-nav curve row is focused/entered (the Touch Zones controller flow
/// needs the graph visible to ring it).
#[allow(clippy::too_many_arguments)]
pub(crate) fn mapping_card_curve_section(
    ui: &mut egui::Ui,
    node_id: NodeId,
    scope: &str,
    idx: usize,
    working: &mut serde_json::Map<String, Value>,
    show_threshold: bool,
    live_mag: Option<f32>,
    nav_uid: Option<usize>,
) -> bool {
    // Render as a flush continuation of the header card above: zero the inter-item
    // gap (on THIS child ui only) and wrap the content in a frame with the card's
    // body fill + black border, bottom corners rounded and top square — so the
    // card (which squares its bottom when a section follows) and this section
    // share ONE continuous border and read as a single mapping card.
    ui.spacing_mut().item_spacing.y = 0.0;
    const C_BODY_BG: egui::Color32 = egui::Color32::from_rgb(0x3C, 0x3C, 0x3C);
    const C_BORDER:  egui::Color32 = egui::Color32::BLACK;
    let accent = ui.visuals().selection.stroke.color;
    // Gamepad selection/focus. `selected_here` = this card is the nav selection
    // (used to extend the card GLOW around this section so header + section share
    // ONE ring, drawn by the nav driver — not a second border here). `nav_field` =
    // the focused curve field on the ENTERED card (4 toggle / 5 graph / 6
    // threshold), driving the per-element highlights inside.
    let pass = ui.ctx().cumulative_pass_nr();
    let card_sel: Option<(usize, bool)> = ui.ctx()
        .data(|d| d.get_temp::<(u64, usize, bool)>(
            egui::Id::new(("gp_nav_remap_card", node_id.0, scope))))
        .filter(|(p, _, _)| pass.saturating_sub(*p) <= 1)
        .map(|(_, sel, ent)| (sel, ent));
    let selected_here = card_sel.map(|(sel, _)| sel == idx).unwrap_or(false);
    let entered_here = card_sel.map(|(sel, ent)| ent && sel == idx).unwrap_or(false);
    let nav_field: Option<u64> = if !entered_here { None } else {
        ui.ctx().data(|d| d.get_temp::<(u64, u64)>(
                egui::Id::new(("gp_nav_remap_card_field", node_id.0, scope))))
            .filter(|(p, f)| *f >= 4 && pass.saturating_sub(*p) <= 1)
            .map(|(_, f)| f)
    };
    let out = egui::Frame::default()
        .fill(C_BODY_BG)
        .stroke(egui::Stroke::new(1.0, C_BORDER))
        .corner_radius(egui::CornerRadius { nw: 0, ne: 0, sw: 5, se: 5 })
        .inner_margin(egui::Margin { left: 0, right: 0, top: 1, bottom: 2 })
        .show(ui, |ui| {
    let open_id = egui::Id::new(("card_curve_open", node_id.0, scope.to_string(), idx));
    let mut open = ui.ctx().data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
    if let Some(uid) = nav_uid {
        let pass = ui.ctx().cumulative_pass_nr();
        let focus = ui.ctx()
            .data(|d| d.get_temp::<u64>(egui::Id::new(("gp_nav_tz_curve_focus", uid))))
            .map(|p| pass.saturating_sub(p) <= 1).unwrap_or(false);
        let entered = ui.ctx()
            .data(|d| d.get_temp::<(u64, usize, bool)>(egui::Id::new(("gp_nav_curve_sel", uid))))
            .map(|(p, _, _)| pass.saturating_sub(p) <= 1).unwrap_or(false);
        if focus || entered { open = true; }
    }

    let has_custom = working.contains_key("curve") || working.contains_key("threshold");
    let (row, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 15.0), egui::Sense::click());
    {
        let painter = ui.painter_at(row);
        let col = if nav_field == Some(4) {
            accent
        } else if resp.hovered() {
            ui.visuals().text_color()
        } else {
            ui.visuals().weak_text_color()
        };
        let tri = if open { "⏷" } else { "⏵" };
        painter.text(row.left_center() + egui::vec2(6.0, 0.0), egui::Align2::LEFT_CENTER,
            format!("{tri} Response curve"), egui::FontId::proportional(10.5), col);
        // Accent dot: this card carries a custom curve and/or threshold.
        if has_custom {
            painter.circle_filled(row.left_center() + egui::vec2(96.0, 0.0), 2.5, accent);
        }
    }
    if resp.clicked() { open = !open; }
    ui.ctx().data_mut(|d| d.insert_temp(open_id, open));
    if !open {
        return false;
    }

    let mut pts: Vec<[f32; 2]> = working.get("curve").and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|p| {
            let q = p.as_array()?;
            Some([q.first()?.as_f64()? as f32, q.get(1)?.as_f64()? as f32])
        }).collect())
        .unwrap_or_default();
    if pts.len() < 2 {
        pts = identity_curve();
    }
    let mut thr: Option<f32> = working.get("threshold").and_then(|v| v.as_f64()).map(|v| v as f32);

    let vis = ui.visuals().clone();
    let changed = mapping_curve_editor(
        ui, open_id.with("ed"), &mut pts,
        if show_threshold { Some(&mut thr) } else { None },
        live_mag, accent, &vis, nav_uid, nav_field,
    );
    if changed {
        if pts == identity_curve() {
            working.remove("curve");
        } else {
            working.insert("curve".into(),
                Value::Array(pts.iter().map(|p| serde_json::json!([p[0], p[1]])).collect()));
        }
        match thr.and_then(|t| Number::from_f64(t as f64)) {
            Some(t) => { working.insert("threshold".into(), Value::Number(t)); }
            None => { working.remove("threshold"); }
        }
    }
    changed
    });
    // Publish this section's GLOBAL rect for the selected card so the nav driver's
    // card glow expands to wrap header + section as ONE ring (instead of a separate
    // border here). Keyed per node+scope; only the selected card publishes.
    if selected_here {
        let to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
            .unwrap_or(egui::emath::TSTransform::IDENTITY);
        ui.ctx().data_mut(|d| d.insert_temp(
            egui::Id::new(("gp_nav_card_section_rect", node_id.0, scope.to_string())),
            (pass, to_global * out.response.rect)));
    }
    out.inner
}

/// Largest live analog-input magnitude across a mapping's in pins, read from
/// the upstream device's live signals — drives the editor's preview dot.
pub(crate) fn live_analog_in_mag(
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    dev: Option<&str>,
    in_pins: &[String],
) -> Option<f32> {
    let dev = dev?;
    let mut best: Option<f32> = None;
    for p in in_pins {
        let v = if let Some((axis, sign)) = flexinput_engine::analog_axis_for_cardinal(p) {
            live_signals.get(&(dev.to_string(), axis.to_string()))
                .map(|s| s.as_float())
                .or_else(|| {
                    // Vec2 fallback: some sources publish the stick as a Vec2
                    // ("left_stick") rather than separate axis floats.
                    let (vpin, comp) = axis.rsplit_once('_')?;
                    match live_signals.get(&(dev.to_string(), vpin.to_string()))? {
                        Signal::Vec2(v) => Some(if comp == "x" { v.x } else { v.y }),
                        _ => None,
                    }
                })
                .map(|r| (r * sign).clamp(0.0, 1.0))
        } else if matches!(p.as_str(), "left_trigger" | "right_trigger") {
            live_signals.get(&(dev.to_string(), p.to_string()))
                .map(|s| s.as_float().clamp(0.0, 1.0))
        } else {
            None
        };
        if let Some(v) = v {
            best = Some(best.map_or(v, |b: f32| b.max(v)));
        }
    }
    best
}

/// Paint one zone's MAPPING content (mapping mode): the output icon(s) of the
/// zone's cards, or — for an analog output (mouse / stick) — the response-curve
/// graph (idle) that swaps to a live vectorscope while the zone is active, with
/// the output icon in the corner. Empty zones show a faint index. The zone's
/// active highlight (drawn by the caller) is the "lit when activated" cue.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn tz_paint_zone_mapping(
    painter: &egui::Painter,
    ctx: &egui::Context,
    node_id: NodeId,
    zr: egui::Rect,
    field: usize,
    idx: usize,
    zone_maps: &[Value],
    skin: crate::canvas::remapper_icons::Skin,
    deflect: Option<(f32, f32)>,
    accent: egui::Color32,
    visuals: &egui::Visuals,
    // Virtual Menu per-zone overrides (empty for Touch Zones). A zone icon
    // override REPLACES the destination icons; a zone name reserves a bottom
    // band so the icon lifts clear of it (the label text is drawn by the
    // menu's own post-pass, which knows the same band).
    zone_meta: &std::collections::HashMap<u32, crate::canvas::menu_body::ZoneMeta>,
) {
    // Icon override (menu only): drawn instead of the mapping destination
    // icons, lifted above the zone-name band when the zone is also named.
    if let Some(m) = zone_meta.get(&(idx as u32)) {
        if !m.icon.is_empty() || !m.svg.is_empty() {
            let band = if m.label.is_empty() { 0.0 } else { (zr.height() * 0.22).clamp(10.0, 15.0) + 3.0 };
            let region = egui::Rect::from_min_max(zr.min, egui::pos2(zr.max.x, zr.max.y - band));
            let ic = (region.height() * 0.6).clamp(14.0, 34.0).min(region.width() - 6.0).max(10.0);
            if let Some(tex) = crate::macro_icons::macro_port_icon_texture(ctx, &m.icon, &m.svg, ic) {
                painter.image(
                    tex.id(),
                    egui::Rect::from_center_size(region.center(), egui::vec2(ic, ic)),
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
            return;
        }
    }
    let is_analog = tz_out_pin_is_analog;
    // Output pins across every card bound to this (field, zone), split by kind so
    // a click's button icon shows ALONGSIDE the analog vectorscope (not hidden by
    // it). Order preserved (first-seen) so it matches the card list.
    let mut analog_pins: Vec<String> = Vec::new();
    let mut digital_pins: Vec<String> = Vec::new();
    for c in zone_maps.iter().filter(|c|
        c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) == field as u64
            && c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) == idx as u64)
    {
        for p in c.get("out").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|v| v.as_str()) {
            let bucket = if is_analog(p) { &mut analog_pins } else { &mut digital_pins };
            if !bucket.iter().any(|x| x == p) { bucket.push(p.to_string()); }
        }
    }

    if analog_pins.is_empty() && digital_pins.is_empty() {
        // Unmapped zone: faint index so it's still identifiable as a target.
        painter.text(zr.center(), egui::Align2::CENTER_CENTER, format!("{idx}"),
            egui::FontId::proportional(11.0), visuals.weak_text_color().gamma_multiply(0.5));
        return;
    }

    // Which output pins are actually FIRING this frame (per-trigger; computed in
    // tz_live_hits). Drives the per-icon activation glow — a click lights its
    // button independently of the analog deflection.
    let active_set: Vec<String> = ctx.data(|d| d
        .get_temp::<std::collections::HashMap<(usize, usize), Vec<String>>>(
            egui::Id::new(("tz_active_out", node_id.0))))
        .and_then(|mm| mm.get(&(field, idx)).cloned())
        .unwrap_or_default();
    let is_on = |p: &str| active_set.iter().any(|x| x == p);
    // Small icon in a rect with an optional activation glow behind it.
    let icon = |pos: egui::Pos2, ic: f32, pin: &str, on: bool| {
        if on {
            painter.rect_filled(egui::Rect::from_min_size(pos, egui::vec2(ic, ic)).expand(2.5),
                3.0, accent.gamma_multiply(0.55));
        }
        paint_chord_chip_to_rect(painter, ctx, pos, ic, pin, skin);
    };

    if !analog_pins.is_empty() {
        let bx = tz_zone_scope_rect(zr);
        painter.rect_filled(bx, 2.0, visuals.extreme_bg_color.gamma_multiply(0.7));
        let grid = visuals.weak_text_color().gamma_multiply(0.5);
        painter.rect_stroke(bx, 2.0, egui::Stroke::new(1.0, visuals.weak_text_color()), egui::StrokeKind::Inside);
        if let Some((dx, dy)) = deflect {
            // ACTIVE → vectorscope: crosshair + live dot.
            painter.line_segment([egui::pos2(bx.center().x, bx.top()), egui::pos2(bx.center().x, bx.bottom())],
                egui::Stroke::new(0.5, grid));
            painter.line_segment([egui::pos2(bx.left(), bx.center().y), egui::pos2(bx.right(), bx.center().y)],
                egui::Stroke::new(0.5, grid));
            let p = egui::pos2(
                bx.center().x + dx.clamp(-1.0, 1.0) * 0.5 * bx.width(),
                bx.center().y - dy.clamp(-1.0, 1.0) * 0.5 * bx.height()); // +Y up
            painter.line_segment([bx.center(), p], egui::Stroke::new(1.5, accent.gamma_multiply(0.6)));
            painter.circle_filled(p, 3.0, accent);
        } else {
            // IDLE → response curve over the 0..1 deflection magnitude.
            let pts = tz_zone_curve(zone_maps, field, idx);
            let to = |x: f32, y: f32| egui::pos2(
                bx.left() + x.clamp(0.0, 1.0) * bx.width(),
                bx.bottom() - y.clamp(0.0, 1.5) * bx.height());
            // Faint linear reference (identity).
            painter.line_segment([to(0.0, 0.0), to(1.0, 1.0)], egui::Stroke::new(0.5, grid));
            for w in pts.windows(2) {
                painter.line_segment([to(w[0][0], w[0][1]), to(w[1][0], w[1][1])],
                    egui::Stroke::new(1.5, accent));
            }
            for p in &pts {
                painter.circle_filled(to(p[0], p[1]), 2.0, accent);
            }
        }
        // Analog output icon in the scope's bottom-right corner (glows while it
        // drives — deflect present ⇒ a live hold-aware hit).
        if let Some(ap) = analog_pins.first() {
            let ic = (bx.width() * 0.42).clamp(12.0, 22.0);
            let pos = egui::pos2(bx.right() - ic - 1.0, bx.bottom() - ic - 1.0);
            icon(pos, ic, ap, deflect.is_some() || is_on(ap));
        }
        // Digital outputs (e.g. a touchpad-click button) share the zone: a small
        // row across the TOP, each lighting when its own trigger fires.
        if !digital_pins.is_empty() {
            let n = digital_pins.len();
            let ic = (zr.width() / (n as f32 + 0.5)).clamp(10.0, 18.0);
            let total_w = n as f32 * ic + (n as f32 - 1.0) * 2.0;
            let mut x = zr.center().x - total_w * 0.5;
            let y = zr.top() + 2.0;
            for p in &digital_pins {
                icon(egui::pos2(x, y), ic, p, is_on(p));
                x += ic + 2.0;
            }
        }
    } else {
        // Digital-only zone: icon(s) centred in a row, each lit by its own trigger.
        let n = digital_pins.len();
        let ic = (zr.height() * 0.46).clamp(14.0, 30.0).min(zr.width() / n.max(1) as f32 - 2.0).max(10.0);
        let total_w = n as f32 * ic + (n as f32 - 1.0) * 3.0;
        let mut x = zr.center().x - total_w * 0.5;
        for p in &digital_pins {
            let pos = egui::pos2(x, zr.center().y - ic * 0.5);
            // Fall back to the finger-active cue if the fine-grained set is absent.
            icon(pos, ic, p, is_on(p) || (active_set.is_empty() && deflect.is_some()));
            x += ic + 3.0;
        }
    }
}

/// Draw one field's pad into `rect`: background, zone cells + index labels, the
/// active-zone highlight, live finger dots, the frame, and draggable dividers
/// (line MOVING — never changes the zone count, so it's wiring-safe). Persists
/// any divider drag. Shared by the in-canvas body and the pinned widget;
/// `id_salt` keeps their interaction ids distinct.
#[allow(clippy::too_many_arguments)]
pub(crate) fn tz_draw_field(
    node_id: NodeId,
    field: usize,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    painter: &egui::Painter,
    rect: egui::Rect,
    col_edges: &[f32],
    row_edges: &[f32],
    zone_live: &std::collections::HashMap<(usize, usize), (f32, f32, bool)>,
    visuals: &egui::Visuals,
    accent: egui::Color32,
    main_override: Option<egui::Color32>,
    // `Some(a)` = "touched zones only": non-active zones (plate + content) paint
    // at opacity `a`, active zones stay full. `None` = normal full render.
    inactive_alpha: Option<f32>,
    id_salt: &'static str,
) {
    use flexinput_core::touchzones as tz;
    let to_x = |u: f32| rect.left() + u * rect.width();
    let to_y = |u: f32| rect.top() + u * rect.height();

    // Dimmed painter clone for non-active zone content in "touched zones only"
    // mode (`multiply_opacity` fades fills/text/icons alike). `None` → the real
    // painter, so the normal path is byte-for-byte unchanged.
    let dim = inactive_alpha.map(|a| {
        let mut p = painter.clone();
        p.set_opacity(a);
        p
    });

    // Optional `main_color` theming (menu + Touch Zones): tints the plate
    // (additively) and the frame. Absent → the plain themed look, unchanged.
    // `main_override` (per-pin style) wins over the module's own param.
    let main_col = main_override.or_else(|| snarl.get_node(node_id)
        .filter(|n| n.params.contains_key("main_color"))
        .map(|n| crate::canvas::menu_body::pcolor(n, "main_color", crate::canvas::menu_body::MENU_MAIN_DEFAULT)));
    let plate = main_col
        .map(crate::canvas::menu_body::plate_fill)
        .unwrap_or(visuals.extreme_bg_color);
    let frame_col = main_col.unwrap_or(visuals.widgets.noninteractive.bg_stroke.color);

    // Whole-pad plate — but in "touched zones only" mode the plate is painted
    // PER ZONE inside the loop (active full, inactive dimmed) so the game shows
    // through the faded zones.
    if dim.is_none() {
        painter.rect_filled(rect, 4.0, plate);
    }

    // In mapping mode each zone shows its mapping OUTPUT (icon, or a live mini
    // vectorscope for analog) instead of the bare index. Ports mode keeps the
    // numbers (the ports ARE the zones' identity there).
    let mapping = snarl.get_node(node_id)
        .and_then(|n| n.params.get("zone_mode").and_then(|v| v.as_str())) == Some("mapping");
    let zone_maps: Vec<Value> = if mapping {
        snarl.get_node(node_id)
            .and_then(|n| n.params.get("zone_maps").and_then(|v| v.as_array()).cloned())
            .unwrap_or_default()
    } else { Vec::new() };
    // Virtual Menu per-zone icon/name overrides (empty for Touch Zones — the
    // icon override then shows on the module body + pinned grid too, matching
    // the overlay).
    let zone_meta = snarl.get_node(node_id)
        .filter(|n| n.module_id == "module.menu")
        .map(crate::canvas::menu_body::menu_zone_meta)
        .unwrap_or_default();
    let skin = remapper_resolve_skin(snarl, node_id, "auto", None);
    let ctx = ui.ctx().clone();

    // Zone rects: from the BSP tree in mapping mode (supports partial dividers),
    // else the legacy grid (ports mode). `idx` is the tree leaf id (== the old
    // grid index after migration), which is also the card `z`.
    let tree = if mapping { Some(tz_field_tree(snarl, node_id, field)) } else { None };
    let zones: Vec<(usize, [f32; 4])> = match &tree {
        Some(t) => t.zones().into_iter().map(|(id, r)| (id as usize, r)).collect(),
        None => {
            let zn = tz::zone_count(col_edges, row_edges);
            (0..zn).map(|idx| {
                let (x0, y0, x1, y1) = tz::zone_rect(idx, col_edges, row_edges);
                (idx, [x0, y0, x1, y1])
            }).collect()
        }
    };
    for &(idx, [x0, y0, x1, y1]) in &zones {
        let zr = egui::Rect::from_min_max(egui::pos2(to_x(x0), to_y(y0)), egui::pos2(to_x(x1), to_y(y1)));
        let live = zone_live.get(&(field, idx)).copied();
        let active = live.map(|z| z.2).unwrap_or(false);
        // "Touched zones only": non-active zones (plate + content) render through
        // the dimmed painter; the touched zone stays full.
        let zp: &egui::Painter = match (&dim, active) { (Some(d), false) => d, _ => painter };
        // Per-zone plate (only in dim mode — the normal path painted it whole).
        if dim.is_some() {
            zp.rect_filled(zr.shrink(0.5), 3.0, plate);
        }
        if active {
            painter.rect_filled(zr.shrink(1.0), 0.0, accent.gamma_multiply(0.35));
        }
        if mapping {
            // Adaptive-centre deflection published by tz_live_hits (relative or
            // absolute per the zone's setting) → the analog vectorscope, so it
            // matches the engine's stick value. +Y up (flip the unit-space +Y-down
            // value). Keyed by the START zone, so the scope stays on the zone the
            // finger began in even if it drifts to a neighbour.
            let deflect = ctx.data(|d| d.get_temp::<(u64, std::collections::HashMap<(usize, usize), (f32, f32)>)>(
                    egui::Id::new(("tz_live_defl", node_id.0))))
                .and_then(|(_, mp)| mp.get(&(field, idx)).copied())
                .map(|(dx, dy)| (dx, -dy));
            tz_paint_zone_mapping(zp, &ctx, node_id, zr, field, idx, &zone_maps, skin, deflect, accent, visuals, &zone_meta);
        } else {
            zp.text(zr.center(), egui::Align2::CENTER_CENTER, format!("{idx}"),
                egui::FontId::proportional(12.0), visuals.weak_text_color());
        }
    }
    for (&(f, idx), &(lx, ly, act)) in zone_live {
        if f != field || !act { continue; }
        if let Some(&(_, [x0, y0, x1, y1])) = zones.iter().find(|(zid, _)| *zid == idx) {
            painter.circle_filled(
                egui::pos2(to_x(x0 + lx * (x1 - x0)), to_y(y0 + ly * (y1 - y0))),
                5.0, egui::Color32::from_rgb(90, 200, 255));
        }
    }
    painter.rect_stroke(rect, 4.0,
        egui::Stroke::new(1.0, frame_col), egui::StrokeKind::Inside);

    // Gamepad-nav focus: the driver publishes (pass, field, axis, line, grabbed)
    // keyed by this node id when line-editing this pad in Easy mode. Highlight
    // the focused divider (accent, or green while grabbed). Keyed by node id, so
    // the in-canvas body (different node-id space) never false-matches.
    let nav_tz: Option<(u64, u64, u64, u64, bool)> =
        ui.ctx().data(|d| d.get_temp(egui::Id::new(("gp_nav_tz", node_id.0))));
    let cur_pass = ui.ctx().cumulative_pass_nr();
    let nav_focus = move |axis: u64, line: usize| -> Option<bool> {
        match nav_tz {
            Some((pass, f, a, l, grabbed))
                if cur_pass.saturating_sub(pass) <= 2
                    && f == field as u64 && a == axis && l == line as u64 => Some(grabbed),
            _ => None,
        }
    };
    let nav_stroke = |grabbed: bool| -> (f32, egui::Color32) {
        if grabbed { (3.0, egui::Color32::from_rgb(90, 220, 120)) } else { (3.0, accent) }
    };
    // Per-divider global-space hit-rects, published for the gamepad RS-cursor
    // hover-select in `nav_drive_touch_zones`. (axis: 0=col/1=row, index, rect).
    let to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
        .unwrap_or(egui::emath::TSTransform::IDENTITY);
    let mut nav_line_rects: Vec<(u8, u32, egui::Rect)> = Vec::new();

    // ── Mapping mode: partial dividers from the tree (drag to move, right-click
    // to remove/merge). Ports mode falls through to the full-cut grid editing. ──
    if let Some(tree) = &tree {
        let mut edited: Option<tz::ZoneNode> = None;
        let mut want_remove: Option<Vec<u8>> = None;
        // Per-axis divider index (V's and H's counted separately) so the gamepad
        // focus highlight + hit-rects line up with the nav driver's (axis, line)
        // model, which walks `dividers()` per axis.
        let (mut vcount, mut hcount) = (0u32, 0u32);
        for (di, div) in tree.dividers().iter().enumerate() {
            // The "−" removal button (from tz_tree_line_overlay) sits at the
            // divider midpoint; carve that band out of the drag handle so the
            // button always wins the pointer there and the drag is grabbable
            // anywhere else along the line.
            let mid = (div.span_lo + div.span_hi) * 0.5;
            let btn = 11.0; // half-band excluded around the midpoint button (px)
            let (p0, p1, hitr, segs, axis_v) = match div.axis {
                tz::Axis::V => {
                    let x = to_x(div.pos);
                    let (lo, hi, cy) = (to_y(div.span_lo), to_y(div.span_hi), to_y(mid));
                    let full = egui::Rect::from_min_max(egui::pos2(x - 4.0, lo), egui::pos2(x + 4.0, hi));
                    let mut segs = Vec::new();
                    if cy - btn > lo + 2.0 { segs.push(egui::Rect::from_min_max(egui::pos2(x - 4.0, lo), egui::pos2(x + 4.0, cy - btn))); }
                    if hi > cy + btn + 2.0 { segs.push(egui::Rect::from_min_max(egui::pos2(x - 4.0, cy + btn), egui::pos2(x + 4.0, hi))); }
                    (egui::pos2(x, lo), egui::pos2(x, hi), full, segs, true)
                }
                tz::Axis::H => {
                    let y = to_y(div.pos);
                    let (lo, hi, cx) = (to_x(div.span_lo), to_x(div.span_hi), to_x(mid));
                    let full = egui::Rect::from_min_max(egui::pos2(lo, y - 4.0), egui::pos2(hi, y + 4.0));
                    let mut segs = Vec::new();
                    if cx - btn > lo + 2.0 { segs.push(egui::Rect::from_min_max(egui::pos2(lo, y - 4.0), egui::pos2(cx - btn, y + 4.0))); }
                    if hi > cx + btn + 2.0 { segs.push(egui::Rect::from_min_max(egui::pos2(cx + btn, y - 4.0), egui::pos2(hi, y + 4.0))); }
                    (egui::pos2(lo, y), egui::pos2(hi, y), full, segs, false)
                }
            };
            let axis_idx = if axis_v { let i = vcount; vcount += 1; i }
                           else       { let i = hcount; hcount += 1; i };
            nav_line_rects.push((if axis_v { 0 } else { 1 }, axis_idx, to_global * hitr));
            // Union the (up to two) segments flanking the button into one response.
            let r = segs.iter().enumerate().fold(None::<egui::Response>, |acc, (si, seg)| {
                let resp = ui.interact(*seg, ui.id().with((node_id, id_salt, "tzdiv", field, di, si)),
                    egui::Sense::click_and_drag());
                Some(match acc { Some(a) => a | resp, None => resp })
            });
            let Some(r) = r else {
                painter.line_segment([p0, p1], egui::Stroke::new(1.0, visuals.weak_text_color()));
                continue;
            };
            let hot = r.hovered() || r.dragged();
            if hot {
                r.clone().on_hover_cursor(if axis_v { egui::CursorIcon::ResizeHorizontal }
                    else { egui::CursorIcon::ResizeVertical })
                    .on_hover_text("Drag to move · right-click to remove (merge)");
            }
            if r.dragged() {
                if let Some(p) = r.interact_pointer_pos() {
                    let want = if axis_v { (p.x - rect.left()) / rect.width() }
                               else { (p.y - rect.top()) / rect.height() };
                    let (lo, hi) = (div.lo + 0.03, div.hi - 0.03);
                    let t = if lo <= hi { want.clamp(lo, hi) } else { (div.lo + div.hi) * 0.5 };
                    let mut nt = tree.clone();
                    if nt.set_divider_t(&div.path, t) { edited = Some(nt); }
                }
            }
            if r.double_clicked() {
                // Recentre between the divider's IMMEDIATE neighbours (not the
                // midpoint of its parent span, which overshoots and squashes the
                // next zone in a 3+-zone tree).
                let target = tree.centered_divider_pos(&div.path)
                    .unwrap_or((div.lo + div.hi) * 0.5);
                let mut nt = tree.clone();
                if nt.set_divider_t(&div.path, target) { edited = Some(nt); }
            }
            if r.secondary_clicked() { want_remove = Some(div.path.clone()); }
            let (w, c) = if let Some(grabbed) = nav_focus(if axis_v { 0 } else { 1 }, axis_idx as usize) {
                nav_stroke(grabbed)
            } else if hot { (2.0, accent) } else { (1.0, visuals.weak_text_color()) };
            painter.line_segment([p0, p1], egui::Stroke::new(w, c));
        }
        if let Some(t) = edited { tz_set_field_tree(snarl, node_id, field, &t); }
        if let Some(path) = want_remove { tz_request_or_apply_merge(snarl, node_id, field, tree, &path); }

        let pass_nr = ui.ctx().cumulative_pass_nr();
        ui.ctx().data_mut(|d| d.insert_temp(
            egui::Id::new(("gp_nav_tz_lines", node_id.0, field)),
            (pass_nr, nav_line_rects)));
        return;
    }

    let mut new_cols = col_edges.to_vec();
    let mut cols_changed = false;
    for i in 0..col_edges.len() {
        let x = to_x(col_edges[i]);
        let hit = egui::Rect::from_min_max(egui::pos2(x - 4.0, rect.top()), egui::pos2(x + 4.0, rect.bottom()));
        nav_line_rects.push((0, i as u32, to_global * hit));
        let r = ui.interact(hit, ui.id().with((node_id, id_salt, "col", field, i)), egui::Sense::click_and_drag());
        let hot = r.hovered() || r.dragged();
        if hot { r.clone().on_hover_cursor(egui::CursorIcon::ResizeHorizontal); }
        if r.dragged() {
            if let Some(p) = r.interact_pointer_pos() {
                let lo = if i == 0 { 0.05 } else { new_cols[i - 1] + 0.04 };
                let hi = if i + 1 == col_edges.len() { 0.95 } else { col_edges[i + 1] - 0.04 };
                let want = (p.x - rect.left()) / rect.width();
                // Crowded neighbours can invert lo/hi — clamp would panic; pin to
                // the midpoint instead.
                new_cols[i] = if lo <= hi { want.clamp(lo, hi) } else { (lo + hi) * 0.5 };
                cols_changed = true;
            }
        }
        // Double-click the line (away from the "−") → recenter between its two
        // adjacent borders (neighbouring dividers or the field edge). Mirrors the
        // gamepad "recenter" (North) action.
        if r.double_clicked() {
            let lo = if i == 0 { 0.0 } else { col_edges[i - 1] };
            let hi = if i + 1 == col_edges.len() { 1.0 } else { col_edges[i + 1] };
            new_cols[i] = (lo + hi) * 0.5;
            cols_changed = true;
        }
        let (w, c) = if let Some(grabbed) = nav_focus(0, i) {
            nav_stroke(grabbed)
        } else if hot { (2.0, accent) } else { (1.0, visuals.weak_text_color()) };
        painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(w, c));
    }
    let mut new_rows = row_edges.to_vec();
    let mut rows_changed = false;
    for i in 0..row_edges.len() {
        let y = to_y(row_edges[i]);
        let hit = egui::Rect::from_min_max(egui::pos2(rect.left(), y - 4.0), egui::pos2(rect.right(), y + 4.0));
        nav_line_rects.push((1, i as u32, to_global * hit));
        let r = ui.interact(hit, ui.id().with((node_id, id_salt, "row", field, i)), egui::Sense::click_and_drag());
        let hot = r.hovered() || r.dragged();
        if hot { r.clone().on_hover_cursor(egui::CursorIcon::ResizeVertical); }
        if r.dragged() {
            if let Some(p) = r.interact_pointer_pos() {
                let lo = if i == 0 { 0.05 } else { new_rows[i - 1] + 0.04 };
                let hi = if i + 1 == row_edges.len() { 0.95 } else { row_edges[i + 1] - 0.04 };
                let want = (p.y - rect.top()) / rect.height();
                new_rows[i] = if lo <= hi { want.clamp(lo, hi) } else { (lo + hi) * 0.5 };
                rows_changed = true;
            }
        }
        if r.double_clicked() {
            let lo = if i == 0 { 0.0 } else { row_edges[i - 1] };
            let hi = if i + 1 == row_edges.len() { 1.0 } else { row_edges[i + 1] };
            new_rows[i] = (lo + hi) * 0.5;
            rows_changed = true;
        }
        let (w, c) = if let Some(grabbed) = nav_focus(1, i) {
            nav_stroke(grabbed)
        } else if hot { (2.0, accent) } else { (1.0, visuals.weak_text_color()) };
        painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(w, c));
    }
    if cols_changed || rows_changed {
        if let Some(node) = snarl.get_node_mut(node_id) {
            if cols_changed { tz_write_field_edges(node, field, "col_edges", &new_cols); }
            if rows_changed { tz_write_field_edges(node, field, "row_edges", &new_rows); }
        }
    }
    // Publish this pad's divider hit-rects (global space) for gamepad RS-cursor
    // hover-select. Keyed per (node, field) so split pads don't clobber each other.
    // NOTE: read the pass number BEFORE `data_mut` — calling a ctx accessor
    // inside the data lock re-enters it and deadlocks epaint's RwLock.
    let pass_nr = ui.ctx().cumulative_pass_nr();
    ui.ctx().data_mut(|d| d.insert_temp(
        egui::Id::new(("gp_nav_tz_lines", node_id.0, field)),
        (pass_nr, nav_line_rects)));
}

/// Pinned-widget renderer (Easy-mode sub-patch layout). Ports mode shows the
/// pad(s) with live dots and MOVE-only dividers — no add/remove (that would
/// require rewiring / could break bindings), no resize grip (the pin frame
/// resizes it), no mode toggle. Scales the single/split layout into `container`.
pub(crate) fn render_touch_zones_pinned(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    inner_snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    automap_parent: Option<&AutomapGlowParent<'_>>,
    style: Option<&crate::canvas::node::MenuStyleOverride>,
    edit_mode: bool,
) {
    use crate::canvas::node::ZoneVisibility;
    let visuals = ui.visuals().clone();
    // Colour resolution: per-pin override falls back FIELD-BY-FIELD to the
    // module's own colour params; both absent = the plain themed look.
    let node_main = inner_snarl.get_node(inner_id)
        .filter(|n| n.params.contains_key("main_color"))
        .map(|n| crate::canvas::menu_body::pcolor_bytes(
            n, "main_color", crate::canvas::menu_body::MENU_MAIN_DEFAULT));
    let node_hi = inner_snarl.get_node(inner_id)
        .filter(|n| n.params.contains_key("highlight_color"))
        .map(|n| crate::canvas::menu_body::pcolor_bytes(
            n, "highlight_color", crate::canvas::menu_body::MENU_HIGHLIGHT_DEFAULT));
    let main_b = style.and_then(|s| s.main).or(node_main);
    let hi_b = style.and_then(|s| s.hi).or(node_hi);
    // Highlight colour (opaque, for editor affordances) when themed, else
    // theme selection.
    let accent = hi_b
        .map(|h| crate::canvas::menu_body::ZoneColors::build(
            main_b.unwrap_or(crate::canvas::menu_body::MENU_MAIN_DEFAULT), h).accent)
        .unwrap_or(visuals.selection.bg_fill);
    // Plate/frame colour override for the grid painter (`None` = the field
    // painter's own param read — which matches when no pin override is set).
    let main_c32 = main_b
        .map(|c| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]));
    // Visibility gating applies to LIVE views only — the layout editor always
    // paints the pad so it can be seen, selected, and styled.
    let vis = if edit_mode { ZoneVisibility::Always }
              else { style.map(|s| s.visibility).unwrap_or_default() };
    let split = inner_snarl.get_node(inner_id)
        .and_then(|n| n.params.get("field_mode").and_then(|v| v.as_str())) == Some("split");
    let mapping = inner_snarl.get_node(inner_id)
        .and_then(|n| n.params.get("zone_mode").and_then(|v| v.as_str())) == Some("mapping");

    // Radial Virtual Menu: its zone tree is a synthetic 1×N strip, so the grid
    // painter below would show a flat row of columns. Paint the sector ring
    // instead — same shared geometry as the node body and the menu overlay,
    // with hover from the node's own eval mirror.
    let radial_menu = inner_snarl.get_node(inner_id)
        .map(|n| n.module_id == "module.menu"
            && n.params.get("menu_radial").and_then(|v| v.as_bool()).unwrap_or(false))
        .unwrap_or(false);
    if radial_menu {
        let (rect, resp) = ui.allocate_exact_size(container, egui::Sense::click());
        let zones = tz_field_tree(inner_snarl, inner_id, 0).zones();
        let (deadzone, origin, sel_zone, zone_maps) = inner_snarl.get_node(inner_id)
            .map(|n| (
                n.params.get("pointer_deadzone").and_then(|v| v.as_f64()).unwrap_or(0.25) as f32,
                n.params.get("menu_radial_origin").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                n.params.get("sel_zone").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
                n.params.get("zone_maps").and_then(|v| v.as_array()).cloned().unwrap_or_default(),
            ))
            .unwrap_or((0.25, 0.0, 0, Vec::new()));
        let (menu_live, zone_meta, ptr) = inner_snarl.get_node(inner_id)
            .map(|n| (
                crate::canvas::menu_body::menu_zone_live(n),
                crate::canvas::menu_body::menu_zone_meta(n),
                crate::canvas::menu_body::menu_pointer(n),
            ))
            .unwrap_or_else(|| (Default::default(), Default::default(), None));
        // Per-pin override colours over the module's own (both default-filled —
        // a menu is always themed).
        let colors = crate::canvas::menu_body::ZoneColors::build(
            main_b.unwrap_or(crate::canvas::menu_body::MENU_MAIN_DEFAULT),
            hi_b.unwrap_or(crate::canvas::menu_body::MENU_HIGHLIGHT_DEFAULT));
        let hover = menu_live.iter()
            .find(|(_, (_, _, act))| *act)
            .map(|((_, z), _)| *z as i32)
            .unwrap_or(-1);
        // Visibility gating: for a radial menu "touch active" = open + hovering
        // a zone (the eval mirror), so both non-Always modes hide the ring
        // until the menu is actually in use. The rect stays allocated so the
        // pin's geometry is stable.
        if !matches!(vis, ZoneVisibility::Always) && hover < 0 {
            return;
        }
        let picked = crate::canvas::menu_body::paint_radial_ring(
            ui, rect, &zones, deadzone, origin, hover,
            if mapping { Some(sel_zone) } else { None },
            &zone_maps, &zone_meta, colors, resp.interact_pointer_pos(), ptr,
        );
        if mapping && resp.clicked() {
            if let Some(z) = picked {
                if tz_pick_kind(inner_snarl, inner_id).is_some() {
                    tz_apply_pick(inner_snarl, inner_id, z);
                } else if let Some(node) = inner_snarl.get_node_mut(inner_id) {
                    node.params.insert("sel_field".to_string(), Value::from(0u64));
                    node.params.insert("sel_zone".to_string(), Value::from(z as u64));
                }
            }
        }
        return;
    }

    // Mapping mode has no zone output ports, so dots come from the resolved
    // device's live touch (same as the module body).
    let zone_live = if mapping {
        tz_live_hits(inner_snarl, inner_id, live_signals, automap_parent, ui.ctx())
    } else {
        inner_snarl.get_node(inner_id).map(tz_zone_live).unwrap_or_default()
    };

    // OnTouch / TouchedZones with nothing active: keep the pin's footprint
    // (stable layout, resizable frame) but paint nothing at all.
    if !matches!(vis, ZoneVisibility::Always) && !zone_live.values().any(|v| v.2) {
        ui.allocate_exact_size(container, egui::Sense::hover());
        return;
    }

    // Live-touch tab-follow (mapping mode, no capture in flight): touching a
    // zone selects it, mirroring the module body, so the pinned cards widget
    // filters to the zone under the finger.
    if mapping {
        // Suppress the follow ONLY while a gesture is being demonstrated
        // ("learning") — that swipe can cross zones and must not hijack the tab.
        // Once "captured", output is picked via buttons, so browsing/re-selecting
        // is safe (the trigger is zone-independent, so it just re-targets the
        // pending mapping to the touched zone).
        let follow_ok = inner_snarl.get_node(inner_id)
            .and_then(|n| n.params.get("_tz_phase").and_then(|v| v.as_str()))
            .unwrap_or("idle") != "learning";
        if follow_ok {
            let last: Option<(u64, usize, usize)> = ui.ctx()
                .data(|d| d.get_temp(egui::Id::new(("tz_last_origin", inner_id.0))));
            let cur_pass = ui.ctx().cumulative_pass_nr();
            if tz_pick_kind(inner_snarl, inner_id).is_some() {
                // Pick mode: a FRESH finger tap applies the pick to the touched zone.
                if let Some((p, _, z)) = last {
                    if cur_pass.saturating_sub(p) <= 1 { tz_apply_pick(inner_snarl, inner_id, z); }
                }
            } else {
                // Select the LAST touched-down zone. A FRESH touchdown wins outright
                // (a quick tap whose finger already lifted still selects — the
                // pass-stamp says it just happened), else fall back to its zone while
                // active → keep the current selection while active → the LOWEST active
                // zone (never `HashMap::iter` unordered, which flickers between two
                // fingers' zones).
                let sel = tz_read_selection(inner_snarl, inner_id);
                let fresh = last.filter(|(p, _, _)| cur_pass.saturating_sub(*p) <= 2)
                    .map(|(_, f, z)| (f, z));
                let follow = fresh
                    .or_else(|| last.map(|(_, f, z)| (f, z))
                        .filter(|fz| zone_live.get(fz).map(|v| v.2).unwrap_or(false)))
                    .or_else(|| zone_live.get(&sel).filter(|v| v.2).map(|_| sel))
                    .or_else(|| zone_live.iter().filter(|(_, v)| v.2).map(|(k, _)| *k).min());
                if let Some((f, z)) = follow {
                    if sel != (f, z) {
                        if let Some(node) = inner_snarl.get_node_mut(inner_id) {
                            node.params.insert("sel_field".to_string(), Value::from(f as u64));
                            node.params.insert("sel_zone".to_string(), Value::from(z as u64));
                        }
                        ui.ctx().request_repaint();
                    }
                }
            }
        }
    }

    let (rect, _) = ui.allocate_exact_size(container, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let (sel_field, sel_zone) = tz_read_selection(inner_snarl, inner_id);

    // "Touched zones only" renders the full pad EXACTLY like "show on touch",
    // but fades every non-active zone (plate + icon/label) to 20% so the
    // touched zone stands out — all visuals stay in place, structure (frame,
    // dividers) is untouched. (Radial menus were handled above.)
    let inactive_alpha = matches!(vis, ZoneVisibility::TouchedZones).then_some(0.2_f32);

    let draw = |field: usize, r: egui::Rect, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>| {
        let col = tz_read_field_edges(snarl, inner_id, field, "col_edges");
        let row = tz_read_field_edges(snarl, inner_id, field, "row_edges");
        let to_x = |u: f32| r.left() + u * r.width();
        let to_y = |u: f32| r.top() + u * r.height();
        // Mapping mode: click a zone → select it (registered BEFORE the
        // dividers so those thin drag handles stay on top and win clicks).
        let mtree = if mapping { Some(tz_field_tree(snarl, inner_id, field)) } else { None };
        if let Some(tree) = &mtree {
            let mut clicked: Option<usize> = None;
            for (id, [x0, y0, x1, y1]) in tree.zones() {
                let zr = egui::Rect::from_min_max(egui::pos2(to_x(x0), to_y(y0)), egui::pos2(to_x(x1), to_y(y1)));
                let zresp = ui.interact(zr, ui.id().with((inner_id, "pin_tzselect", field, id)), egui::Sense::click());
                if zresp.hovered() { zresp.clone().on_hover_cursor(egui::CursorIcon::PointingHand); }
                if zresp.clicked() { clicked = Some(id as usize); }
            }
            if let Some(idx) = clicked {
                if tz_pick_kind(snarl, inner_id).is_some() {
                    tz_apply_pick(snarl, inner_id, idx);
                } else if let Some(node) = snarl.get_node_mut(inner_id) {
                    node.params.insert("sel_field".to_string(), Value::from(field as u64));
                    node.params.insert("sel_zone".to_string(), Value::from(idx as u64));
                }
            }
        }
        tz_draw_field(inner_id, field, ui, snarl, &painter, r, &col, &row, &zone_live, &visuals, accent, main_c32, inactive_alpha, "pin");
        if mapping { tz_pick_banner(inner_id, ui, snarl, &painter, r, accent); }
        // Selected-zone outline on top of the pad fill.
        if let Some(tree) = &mtree {
            if sel_field == field {
                if let Some([x0, y0, x1, y1]) = tree.zone_rect(sel_zone as u32) {
                    let zr = egui::Rect::from_min_max(egui::pos2(to_x(x0), to_y(y0)), egui::pos2(to_x(x1), to_y(y1)));
                    painter.rect_stroke(zr.shrink(1.5), 2.0, egui::Stroke::new(2.0, accent), egui::StrokeKind::Inside);
                }
            }
        }
        // Hover-revealed +/- handles: tree ops in mapping mode, full-cut grid in
        // ports mode.
        if mapping {
            tz_tree_line_overlay(inner_id, field, ui, snarl, &painter, r, accent, &visuals);
        } else {
            tz_line_edit_overlay(inner_id, field, ui, snarl, &painter, r, &col, &row, accent, &visuals);
        }
    };

    if split {
        let gap = 12.0;
        let fw = ((rect.width() - gap) * 0.5).max(20.0);
        let a = egui::Rect::from_min_size(rect.min, egui::vec2(fw, rect.height()));
        let b = egui::Rect::from_min_size(egui::pos2(rect.min.x + fw + gap, rect.min.y), egui::vec2(fw, rect.height()));
        draw(0, a, ui, inner_snarl);
        draw(1, b, ui, inner_snarl);
    } else {
        draw(0, rect, ui, inner_snarl);
    }

    // Virtual Menu pinned in grid mode: zone-name labels over the field (the
    // TZ field painter predates `zone_meta`; the radial shape returned above
    // and paints labels inside the ring painter). Menus are always single
    // field.
    if inner_snarl.get_node(inner_id).map(|n| n.module_id == "module.menu").unwrap_or(false) {
        let metas = inner_snarl.get_node(inner_id)
            .map(crate::canvas::menu_body::menu_zone_meta).unwrap_or_default();
        if !metas.is_empty() {
            // Dim non-active labels in "touched zones only" mode, matching the
            // field painter's per-zone fade.
            let dim = inactive_alpha.map(|a| { let mut p = painter.clone(); p.set_opacity(a); p });
            for (zid, [x0, _y0, x1, y1]) in tz_field_tree(inner_snarl, inner_id, 0).zones() {
                let Some(m) = metas.get(&zid) else { continue };
                if m.label.is_empty() { continue; }
                let active = zone_live.get(&(0usize, zid as usize)).map(|z| z.2).unwrap_or(false);
                let lp: &egui::Painter = match (&dim, active) { (Some(d), false) => d, _ => &painter };
                let cx = rect.left() + (x0 + x1) * 0.5 * rect.width();
                let by = rect.top() + y1 * rect.height();
                let (f, txt) = crate::canvas::menu_body::fit_zone_label(
                    lp, &m.label, 11.0, (x1 - x0) * rect.width() - 6.0);
                lp.text(egui::pos2(cx, by - 3.0), egui::Align2::CENTER_BOTTOM, txt,
                    f, egui::Color32::from_gray(210));
            }
        }
    }

    // Mapping mode: the merge-confirm popup (raised by a "−" removal that would
    // drop mapped zones).
    if mapping {
        tz_render_merge_popup(ui, inner_snarl, inner_id);
    }
}

/// Rebuild the Touch Zones node's dynamic output ports so they match the current
/// per-field grids (rows × cols) plus a click port per field. Idempotent — bails
/// when already in sync, so it's safe to call every frame. Slot 0 (the AutoMap
/// passthrough) is always kept; changing a grid drops existing zone-port wiring
/// for now (zone indices reshuffle row-major).
pub(crate) fn regenerate_touch_zone_ports(node_id: NodeId, snarl: &mut Snarl<NodeData>) {
    use flexinput_core::touchzones as tz;

    // Build desired (id, label, type) triples from field_mode + per-field grids.
    let Some(node) = snarl.get_node(node_id) else { return };
    let split = node.params.get("field_mode").and_then(|v| v.as_str()) == Some("split");
    let single = !split;
    let n_fields = if split { 2 } else { 1 };
    let mut want_ids: Vec<String> = vec![tz::PASS_PIN_ID.to_string()];
    let mut want: Vec<(String, SignalType)> = Vec::new(); // for outputs[1..]
    // Mapping mode injects per-zone behaviours straight onto the AutoMap bus
    // (Remapper-style), so the node exposes ONLY the AutoMap passthrough — no
    // typed zone ports. Ports mode builds the per-zone X/Y/Active + Click ports.
    let mapping = node.params.get("zone_mode").and_then(|v| v.as_str()) == Some("mapping");
    if !mapping {
    for field in 0..n_fields {
        let col = tz_node_edges(node, field, "col_edges");
        let row = tz_node_edges(node, field, "row_edges");
        for idx in 0..tz::zone_count(&col, &row) {
            for comp in [tz::ZoneComp::X, tz::ZoneComp::Y, tz::ZoneComp::Active] {
                want_ids.push(tz::zone_pin_id(field, idx, comp));
                let ty = if matches!(comp, tz::ZoneComp::Active) { SignalType::Bool } else { SignalType::Float };
                want.push((tz::zone_pin_label(field, idx, comp, single), ty));
            }
        }
        want_ids.push(tz::click_pin_id(field));
        want.push((tz::click_pin_label(field, single), SignalType::Bool));
    }
    }

    let cur_ids: Vec<String> = node.params.get("output_pin_ids").and_then(|v| v.as_array())
        .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
        .unwrap_or_default();
    if cur_ids == want_ids {
        return;
    }

    let old_len = snarl.get_node(node_id).map_or(0, |n| n.outputs.len());
    for i in 1..old_len {
        snarl.drop_outputs(OutPinId { node: node_id, output: i });
    }
    if let Some(node) = snarl.get_node_mut(node_id) {
        node.outputs.truncate(1); // keep AutoMap passthrough at slot 0
        for (label, ty) in &want {
            node.outputs.push(PinDescriptor::new(label.clone(), *ty));
        }
        node.params.insert(
            "output_pin_ids".to_string(),
            Value::Array(want_ids.into_iter().map(Value::String).collect()),
        );
    }
}

/// Perform a relative grid edit on `field`, PRESERVING existing port wiring: a
/// wire to a surviving zone follows that zone to its new index. Snapshots all
/// output connections by stable pin id, rewrites the grid, rebuilds the ports,
/// then reconnects — remapping only the mutated field's zones (other fields and
/// the click ports keep their ids). Index remap math lives in
/// `flexinput_core::touchzones::apply_grid_op` (unit-tested there).
pub(crate) fn tz_restructure(node_id: NodeId, field: usize, op: flexinput_core::touchzones::GridOp, snarl: &mut Snarl<NodeData>) {
    use flexinput_core::touchzones as tz;
    let col = tz_read_field_edges(snarl, node_id, field, "col_edges");
    let row = tz_read_field_edges(snarl, node_id, field, "row_edges");
    let Some((new_col, new_row, remap)) = tz::apply_grid_op(op, &col, &row) else { return };

    // Snapshot current connections keyed by stable pin id (before regen drops them).
    let ids_before: Vec<String> = snarl.get_node(node_id)
        .and_then(|n| n.params.get("output_pin_ids").and_then(|v| v.as_array()))
        .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
        .unwrap_or_default();
    let mut snapshot: std::collections::HashMap<String, Vec<egui_snarl::InPinId>> = std::collections::HashMap::new();
    for (i, id) in ids_before.iter().enumerate() {
        let remotes = snarl.out_pin(OutPinId { node: node_id, output: i }).remotes.clone();
        if !remotes.is_empty() {
            snapshot.insert(id.clone(), remotes);
        }
    }

    if let Some(node) = snarl.get_node_mut(node_id) {
        tz_write_field_edges(node, field, "col_edges", &new_col);
        tz_write_field_edges(node, field, "row_edges", &new_row);
    }
    regenerate_touch_zone_ports(node_id, snarl);

    // Reconnect: for the mutated field, map new zone idx → old id via the inverse
    // remap; everything else keeps its id.
    let inv: std::collections::HashMap<usize, usize> = remap.iter().map(|(&o, &n)| (n, o)).collect();
    let ids_after: Vec<String> = snarl.get_node(node_id)
        .and_then(|n| n.params.get("output_pin_ids").and_then(|v| v.as_array()))
        .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
        .unwrap_or_default();
    for (i, id) in ids_after.iter().enumerate() {
        let src_id: Option<String> = match tz::parse_pin(id) {
            Some(tz::Pin::Zone { field: f, idx: nidx, comp }) if f == field => {
                inv.get(&nidx).map(|&oidx| tz::zone_pin_id(field, oidx, comp))
            }
            Some(_) => Some(id.clone()),
            None => None,
        };
        if let Some(remotes) = src_id.and_then(|s| snapshot.get(&s)) {
            let out = OutPinId { node: node_id, output: i };
            for &rem in remotes {
                snarl.connect(out, rem);
            }
        }
    }
}

/// Small painted +/- button overlaid on the field. Returns true when clicked.
pub(crate) fn tz_mini_button(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    id: egui::Id,
    center: egui::Pos2,
    glyph: &str,
    accent: egui::Color32,
    visuals: &egui::Visuals,
) -> bool {
    let hit = egui::Rect::from_center_size(center, egui::vec2(16.0, 16.0));
    let resp = ui.interact(hit, id, egui::Sense::click());
    let hot = resp.hovered();
    painter.circle_filled(center, 7.5, if hot { accent } else { visuals.widgets.inactive.bg_fill });
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::proportional(12.0),
        if hot { egui::Color32::WHITE } else { visuals.text_color() },
    );
    resp.clicked()
}

pub(crate) fn show_touch_zones_body(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) {
    // Lazy default init: single-field 2×2 grid, ports mode.
    if let Some(node) = snarl.get_node_mut(node_id) {
        if !node.params.contains_key("col_edges") {
            node.params.insert("zone_mode".to_string(), Value::String("ports".to_string()));
            node.params.insert("field_mode".to_string(), Value::String("single".to_string()));
            node.params.insert("col_edges".to_string(), Value::Array(vec![Value::from(0.5)]));
            node.params.insert("row_edges".to_string(), Value::Array(vec![Value::from(0.5)]));
        }
    }

    // Keep dynamic ports in sync with the grids (no-op when unchanged).
    regenerate_touch_zone_ports(node_id, snarl);

    let visuals = ui.visuals().clone();
    // Highlight colour: the `highlight_color` swatch (opaque, for editor
    // affordances) when set, else the theme selection colour (preserves
    // existing patches — real Touch Zones nodes carry no colour params).
    let accent = snarl.get_node(node_id)
        .filter(|n| n.params.contains_key("highlight_color"))
        .map(|n| crate::canvas::menu_body::ZoneColors::read(n).accent)
        .unwrap_or(visuals.selection.bg_fill);
    let split = snarl.get_node(node_id)
        .and_then(|n| n.params.get("field_mode").and_then(|v| v.as_str())) == Some("split");
    let mapping = snarl.get_node(node_id)
        .and_then(|n| n.params.get("zone_mode").and_then(|v| v.as_str())) == Some("mapping");

    // Live per-(field,zone) finger state for dots + active highlight. Ports mode
    // reconstructs from the node's OWN zone outputs (works for network/collector
    // touch). Mapping mode has no zone ports, so it reads the resolved upstream
    // device's touch pins directly (local device; network-forwarded touch in
    // mapping mode shows no dot — a known gap).
    let zone_live = if mapping {
        tz_live_hits(snarl, node_id, live_signals, automap_parent, ui.ctx())
    } else {
        snarl.get_node(node_id).map(tz_zone_live).unwrap_or_default()
    };

    // Live-touch tab selection: touching a zone selects it so its cards show.
    // Suppressed while a Learn capture is in flight — the tab must stay on the
    // zone the Learn started on, and the gesture may be demonstrated ANYWHERE on
    // the pad without hijacking the selection.
    // Suppress the follow ONLY during a gesture demo ("learning") — see the pinned
    // widget's copy for the rationale. "captured" browses freely (trigger is
    // zone-independent, so re-selecting just re-targets the pending mapping).
    let follow_ok = snarl.get_node(node_id)
        .and_then(|n| n.params.get("_tz_phase").and_then(|v| v.as_str())).unwrap_or("idle") != "learning";
    if mapping && follow_ok {
        let last: Option<(u64, usize, usize)> = ui.ctx()
            .data(|d| d.get_temp(egui::Id::new(("tz_last_origin", node_id.0))));
        let cur_pass = ui.ctx().cumulative_pass_nr();
        if tz_pick_kind(snarl, node_id).is_some() {
            if let Some((p, _, z)) = last {
                if cur_pass.saturating_sub(p) <= 1 { tz_apply_pick(snarl, node_id, z); }
            }
        } else {
            // Select the LAST touched-down zone. A FRESH touchdown wins outright —
            // even a quick tap whose finger has already lifted by the time we read
            // (the pass-stamp says it just happened), which the "still-active" check
            // below would otherwise miss. For a held/sliding finger the origin goes
            // stale, so we fall back to: its zone while still active → keep the
            // current selection while active → the LOWEST active zone (never
            // `HashMap::iter` unordered, which flickers between two fingers' zones).
            let sel = tz_read_selection(snarl, node_id);
            let fresh = last.filter(|(p, _, _)| cur_pass.saturating_sub(*p) <= 2)
                .map(|(_, f, z)| (f, z));
            let follow = fresh
                .or_else(|| last.map(|(_, f, z)| (f, z))
                    .filter(|fz| zone_live.get(fz).map(|v| v.2).unwrap_or(false)))
                .or_else(|| zone_live.get(&sel).filter(|v| v.2).map(|_| sel))
                .or_else(|| zone_live.iter().filter(|(_, v)| v.2).map(|(k, _)| *k).min());
            if let Some((f, z)) = follow {
                if sel != (f, z) {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert("sel_field".to_string(), Value::from(f as u64));
                        node.params.insert("sel_zone".to_string(), Value::from(z as u64));
                    }
                    ui.ctx().request_repaint();
                }
            }
        }
    }

    let field_w = snarl.get_node(node_id)
        .and_then(|n| n.params.get("field_w").and_then(|v| v.as_f64()))
        .unwrap_or(420.0) as f32;
    let field_h = snarl.get_node(node_id)
        .and_then(|n| n.params.get("field_h").and_then(|v| v.as_f64()))
        .unwrap_or(260.0) as f32;

    ui.vertical(|ui| {
        // ── Mode toggles ─────────────────────────────────────────
        ui.horizontal(|ui| {
            // Ports ⇄ Mapping. Ports = typed per-zone outputs. Mapping = inject
            // per-zone behaviours onto the AutoMap bus (Remapper-style).
            let mut want_mapping = mapping;
            ui.label("Mode:");
            if ui.selectable_label(!want_mapping, "Ports")
                .on_hover_text("Expose typed X / Y / Active outputs per zone.").clicked() { want_mapping = false; }
            if ui.selectable_label(want_mapping, "Mapping")
                .on_hover_text("Map each zone to gamepad/key/stick inputs on the AutoMap bus.").clicked() { want_mapping = true; }
            if want_mapping != mapping {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert("zone_mode".to_string(),
                        Value::String(if want_mapping { "mapping" } else { "ports" }.to_string()));
                }
                regenerate_touch_zone_ports(node_id, snarl);
            }

            ui.separator();
            let mut split_v = split;
            if ui.checkbox(&mut split_v, "Split pads")
                .on_hover_text("Track touch 1 and touch 2 on separate fields, each with its own click (e.g. Steam Controller).")
                .changed()
            {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert("field_mode".to_string(),
                        Value::String(if split_v { "split" } else { "single" }.to_string()));
                    // Initialise the second field's grid on first enable.
                    if split_v && !node.params.contains_key("col_edges1") {
                        tz_write_field_edges(node, 1, "col_edges", &[0.5]);
                        tz_write_field_edges(node, 1, "row_edges", &[0.5]);
                    }
                }
                regenerate_touch_zone_ports(node_id, snarl);
            }
            ui.separator();
            crate::canvas::menu_body::show_zone_color_row(node_id, ui, snarl);
        });

        // Re-read mode flags AFTER the toggles (regen already ran) so the rest of
        // this frame renders consistently in the new mode — never a mixed frame
        // where the field/cards run with stale `mapping` while the ports/params
        // already flipped.
        let split = snarl.get_node(node_id)
            .and_then(|n| n.params.get("field_mode").and_then(|v| v.as_str())) == Some("split");
        let single = !split;
        let mapping = snarl.get_node(node_id)
            .and_then(|n| n.params.get("zone_mode").and_then(|v| v.as_str())) == Some("mapping");

        // Field area(s); the union is registered as the pinnable "field" element.
        let field_area = if tz_n_fields(snarl, node_id) == 2 {
            // Split: two pads side by side with a gap. Only the RIGHT pad shows the
            // resize grip; it writes the shared size so both resize symmetrically.
            ui.horizontal_top(|ui| {
                let a = ui.vertical(|ui| {
                    render_touch_field(node_id, 0, single, false, mapping, ui, snarl, &zone_live, &visuals, accent, field_w, field_h)
                }).inner;
                ui.add_space(16.0);
                let b = ui.vertical(|ui| {
                    render_touch_field(node_id, 1, single, true, mapping, ui, snarl, &zone_live, &visuals, accent, field_w, field_h)
                }).inner;
                a.union(b)
            }).inner
        } else {
            render_touch_field(node_id, 0, single, true, mapping, ui, snarl, &zone_live, &visuals, accent, field_w, field_h)
        };

        // Pinnable to a sub-patch/Easy-mode layout (ports mode = move-only field).
        register_exposable_element(ui, node_id, "field", field_area);

        // Confirm popup for a divider removal that would drop mapped zones.
        if mapping { tz_render_merge_popup(ui, snarl, node_id); }

        // ── Mapping mode: zone-tab card list (separately pinnable) ──────────
        if mapping {
            ui.add_space(6.0);
            let cards_area = ui.vertical(|ui| {
                render_touch_zone_cards(node_id, ui, snarl, &visuals, accent, live_signals, automap_parent);
            }).response.rect;
            register_exposable_element(ui, node_id, "cards", cards_area);
        }
    });
}

/// Render one touch field (in-canvas / advanced editing): the pad with draggable
/// dividers + live dots, the relative line +/- overlay, and (when `show_resize`)
/// the corner resize grip. Returns the field's rect (so the caller can register
/// the pinnable "field" element over the union of pads).
#[allow(clippy::too_many_arguments)]
/// Read the currently-selected (field, zone) for mapping mode (defaults 0,0).
pub(crate) fn tz_read_selection(snarl: &Snarl<NodeData>, node_id: NodeId) -> (usize, usize) {
    snarl.get_node(node_id).map(|n| (
        n.params.get("sel_field").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
        n.params.get("sel_zone").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
    )).unwrap_or((0, 0))
}

/// Mapping-mode card list for the selected zone (the zone is the filter "tab").
/// Cards live in `params["zone_maps"]` as Remapper-style objects tagged with
/// `f`/`z`; each renders through the SHARED [`remapper_mapping_card_pixel`] so the
/// look, press modes (tap/double/short/long/hold/analog), turbo, and delete match
/// the Remapper / Map Action / Lean cards. Trigger + target editing is supplied
/// here (the card's chords are paint-only), since the trigger is the zone gesture
/// rather than a captured device input.
/// Commit a captured Touch Zones mapping into `zone_maps` and reset the Learn
/// state. Shared by the mouse "＋ Add" button and the gamepad `_tz_commit_add`
/// path. Analog outputs (mouse / stick) default to "analog" press mode.
pub(crate) fn tz_commit_card(snarl: &mut Snarl<NodeData>, node_id: NodeId,
    f: usize, z: usize, trigger: &str, draft_out: &[String])
{
    let is_analog = tz_out_pin_is_analog;
    let mode = if draft_out.iter().any(|p| is_analog(p)) { "analog" } else { "down" };
    if let Some(node) = snarl.get_node_mut(node_id) {
        let mut m = serde_json::Map::new();
        m.insert("f".into(), Value::from(f as u64));
        m.insert("z".into(), Value::from(z as u64));
        m.insert("in".into(), Value::Array(vec![Value::from(trigger)]));
        m.insert("out".into(), Value::Array(draft_out.iter().map(|s| Value::from(s.as_str())).collect()));
        m.insert("mode".into(), Value::from(mode));
        let mut cards = node.params.get("zone_maps").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        cards.push(Value::Object(m));
        node.params.insert("zone_maps".into(), Value::Array(cards));
        node.params.insert("_tz_phase".into(), Value::from("idle"));
        for k in ["_tz_trig", "_tz_draft_out", "_tz_gp_arm", "_tz_gp_base", "_tz_gp_seen"] { node.params.remove(k); }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_touch_zone_cards(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    _visuals: &egui::Visuals,
    accent: egui::Color32,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) {
    use flexinput_core::touchzones as tz;
    // Fixed mapping-card width — the header rows pin their right-aligned controls
    // to this, so the layout stays put when the touchpad widget is scaled up.
    const TZ_CARD_W: f32 = 358.0;
    // Virtual Menu variant: the zone IS the trigger (token "menu_sel" — fires
    // on menu selection), so there is no touch-gesture Learn phase. "Learn"
    // arms the gamepad DESTINATION capture directly and "Assign…" opens the
    // picker with this menu's own target pins disabled (no self-targeting).
    let menu_mode = snarl.get_node(node_id).map(|n| n.module_id == "module.menu").unwrap_or(false);
    let menu_excl: Option<String> = if menu_mode {
        snarl.get_node(node_id)
            .and_then(|n| n.params.get("menu_id").and_then(|v| v.as_str()))
            .map(|id| format!("menu:{id}"))
    } else {
        None
    };
    let (sel_f, sel_z) = tz_read_selection(snarl, node_id);
    let single = snarl.get_node(node_id)
        .and_then(|n| n.params.get("field_mode").and_then(|v| v.as_str())) != Some("split");
    let skin = remapper_resolve_skin(snarl, node_id, "auto", None);
    let dev = remapper_upstream_device_id(snarl, node_id, 0, automap_parent);

    let getp = |snarl: &Snarl<NodeData>, k: &str| -> Option<Value> {
        snarl.get_node(node_id).and_then(|n| n.params.get(k).cloned())
    };
    let phase = getp(snarl, "_tz_phase").and_then(|v| v.as_str().map(String::from)).unwrap_or_else(|| "idle".into());

    // ── Learn state machine ───────────────────────────────────────────────
    // idle → Learn → (demonstrate on pad) → captured → Assign / gamepad → commit.
    // (Menu nodes never enter "learning" — their trigger is fixed.)
    if !menu_mode && phase == "learning" {
        if let Some(trig) = tz_learn_capture(snarl, node_id, live_signals, dev.as_deref()) {
            // The zone the gesture STARTED on becomes the mapping's target — matching
            // the Learn hint "demonstrate … on a zone". Located from the captured
            // touchdown point in the current field's tree. (The tab-follow was
            // suppressed during "learning", so sel_field still holds the active field.)
            let sx = getp(snarl, "_tz_cap_sx").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
            let sy = getp(snarl, "_tz_cap_sy").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
            let start_zone = tz_field_tree(snarl, node_id, sel_f).locate(sx, sy).0 as usize;
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("_tz_trig".into(), Value::from(trig.as_str()));
                node.params.insert("_tz_phase".into(), Value::from("captured"));
                node.params.insert("sel_zone".into(), Value::from(start_zone as u64));
            }
        }
    }
    // Gamepad-learn output: while armed, the pressed gamepad CHORD becomes the
    // draft output — accumulated while held and finalised on release, reusing the
    // Remapper's combo-capture shape so multi-button outputs work here too.
    if phase == "captured"
        && getp(snarl, "_tz_gp_arm").and_then(|v| v.as_bool()).unwrap_or(false)
    {
        // Suppress gamepad UI navigation this + next frame so the button the user
        // presses reaches THIS capture instead of driving the cursor/menus. Read
        // by `run_gamepad_nav` (goes inert while the flag is fresh).
        let pass = ui.ctx().cumulative_pass_nr();
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("fxi_tz_gp_learn"), pass));
        ui.ctx().request_repaint();
        let pressed_now: Vec<String> = dev.as_deref()
            .map(|d| remapper_pressed_now(live_signals, d)).unwrap_or_default();
        // Baseline: the pins already held at the instant we armed (typically the
        // button the user pressed to arm — South/🎮). We latch it once, then only
        // accept a pin that is NOT in the baseline, i.e. a FRESH press. Without
        // this the still-held arming button gets captured as the output the same
        // frame ("it just binds North immediately").
        let base: Option<Vec<String>> = getp(snarl, "_tz_gp_base")
            .and_then(|v| v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()));
        let base = match base {
            Some(b) => b,
            None => {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert("_tz_gp_base".into(),
                        Value::Array(pressed_now.iter().map(|p| Value::from(p.as_str())).collect()));
                }
                pressed_now.clone()
            }
        };
        // Fresh presses = held now but not part of the arming baseline. Accumulate
        // the peak chord (sticky) into the draft, then finalise when the whole
        // combo releases. `_tz_gp_seen` records that at least one fresh press has
        // landed this session, so a draft lingering from a prior pick can't latch
        // on the very first frame.
        let seen = getp(snarl, "_tz_gp_seen").and_then(|v| v.as_bool()).unwrap_or(false);
        let fresh: Vec<String> = pressed_now.iter().filter(|p| !base.contains(*p)).cloned().collect();
        if !fresh.is_empty() {
            // The first fresh press of the session replaces any prior draft;
            // further simultaneous presses extend the chord.
            let mut draft: Vec<String> = if seen {
                getp(snarl, "_tz_draft_out").and_then(|v| v.as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()))
                    .unwrap_or_default()
            } else { Vec::new() };
            for p in &fresh { if !draft.contains(p) { draft.push(p.clone()); } }
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("_tz_draft_out".into(),
                    Value::Array(draft.iter().map(|p| Value::from(p.as_str())).collect()));
                node.params.insert("_tz_gp_seen".into(), Value::from(true));
            }
        } else if seen {
            // Whole combo released → finalise the chord and disarm.
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("_tz_gp_arm".into(), Value::from(false));
                node.params.remove("_tz_gp_base");
                node.params.remove("_tz_gp_seen");
            }
        }
    }
    // Draft output chord accumulated by the picker / gamepad-learn. Unlike the
    // Remapper, we do NOT commit on the first pick — the user builds a chord in
    // the picker and presses "Add" (below) to commit, so multi-key outputs work
    // and the picker doesn't vanish mid-selection.
    let draft_out: Vec<String> = getp(snarl, "_tz_draft_out")
        .and_then(|v| v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()))
        .unwrap_or_default();
    let trigger = getp(snarl, "_tz_trig").and_then(|v| v.as_str().map(String::from)).unwrap_or_default();

    // Gamepad "Add": the nav driver sets `_tz_commit_add` to commit the captured
    // mapping (same path as the ＋ Add button).
    if getp(snarl, "_tz_commit_add").and_then(|v| v.as_bool()).unwrap_or(false) {
        if let Some(node) = snarl.get_node_mut(node_id) { node.params.remove("_tz_commit_add"); }
        if phase == "captured" && !draft_out.is_empty() && !trigger.is_empty() {
            tz_commit_card(snarl, node_id, sel_f, sel_z, &trigger, &draft_out);
        }
    }

    // Whether ANY card (any zone) drives a relative-mouse output — gates the
    // node-global mouse-speed control so pure keyboard/stick maps stay uncluttered.
    let has_mouse_card = snarl.get_node(node_id)
        .and_then(|n| n.params.get("zone_maps").and_then(|v| v.as_array()))
        .map(|cards| cards.iter().any(|c| c.get("out").and_then(|o| o.as_array())
            .map(|a| a.iter().any(|p| matches!(p.as_str(), Some("mouse") | Some("mouse_x") | Some("mouse_y"))))
            .unwrap_or(false)))
        .unwrap_or(false);
    // Whether ANY card drives an analog output (stick/mouse/scroll) — gates the
    // "Touchpad mode" dropdown (relative/absolute + touchpad apply to analog).
    let has_analog_card = snarl.get_node(node_id)
        .and_then(|n| n.params.get("zone_maps").and_then(|v| v.as_array()))
        .map(|cards| cards.iter().any(|c| c.get("out").and_then(|o| o.as_array())
            .map(|a| a.iter().any(|p| p.as_str().map(tz_out_pin_is_analog).unwrap_or(false)))
            .unwrap_or(false)))
        .unwrap_or(false);
    // Current SELECTED-ZONE "Touchpad mode" (synced / percard / touchpad; default
    // synced). Stored per-zone on that zone's cards (like `adaptive`).
    let tp_mode: String = snarl.get_node(node_id)
        .and_then(|n| n.params.get("zone_maps").and_then(|v| v.as_array()))
        .and_then(|cards| cards.iter()
            .filter(|c| c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) == sel_f as u64
                     && c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) == sel_z as u64)
            .find_map(|c| c.get("tp_mode").and_then(|v| v.as_str()).map(String::from)))
        .unwrap_or_else(|| "synced".into());

    // ── Header ────────────────────────────────────────────────────────────
    // Row 1: which zone + capture STATUS (listening / registered trigger →
    // picked output). Row 2 (below): the action BUTTONS + mouse multiplier.
    // Split so they stop competing for the pinned widget's limited width.
    let label = if single { format!("Zone {sel_z}") }
                else { format!("{}{}", tz::field_letter(sel_f), sel_z) };
    // Rect of the Hold checkbox — published LAST in the action-rect list so
    // gamepad nav can focus + toggle it (see `nav_tz_action_items`).
    let mut hold_rect: Option<egui::Rect> = None;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!("{label} mappings")).strong().color(accent));
        // Hold-zone toggle: keep a gesture that STARTS in this zone bound to it
        // even if the finger slides into a neighbour (so the neighbour's mapping
        // doesn't also fire). Affects touch/click triggers — a touch-gesture
        // concept, so hidden on menu nodes (a menu pointer just highlights).
        if !menu_mode {
            let mut hold = tz_zone_held(snarl, node_id, sel_f, sel_z);
            let cb = ui.checkbox(&mut hold, "Hold")
                .on_hover_text("Hold zone: a touch that starts in this zone stays bound to it for the whole gesture, even if the finger slides into another zone — so the other zone won't trigger. Gamepad: focus it and press South to toggle.");
            hold_rect = Some(cb.rect);
            if cb.changed() {
                tz_set_zone_held(snarl, node_id, sel_f, sel_z, hold);
            }
        }
        // Migrate: move THIS zone's mappings onto another zone you tap/click next.
        if tz_pick_kind(snarl, node_id).as_deref() == Some("migrate") {
            if ui.button("✖ Cancel move").clicked() { tz_cancel_pick(snarl, node_id); }
        } else if ui.button("⇄ Move…")
            .on_hover_text("Move this zone's mappings to another zone — click/tap the destination next.")
            .clicked()
        {
            tz_start_migrate(snarl, node_id);
        }
        match phase.as_str() {
            "learning" => {
                ui.label(egui::RichText::new("· listening — touch / click / swipe a zone…")
                    .italics().color(accent));
            }
            "captured" => {
                ui.label(egui::RichText::new("·").weak());
                if menu_mode {
                    // The trigger is implicit (this zone's selection) — show
                    // where the output goes instead of a trigger chip.
                    ui.label(egui::RichText::new("on select →").weak());
                } else {
                    remapper_render_chip(ui, &trigger, skin);
                    ui.label(egui::RichText::new("→").weak());
                }
                if draft_out.is_empty() {
                    let hint = if menu_mode
                        && getp(snarl, "_tz_gp_arm").and_then(|v| v.as_bool()).unwrap_or(false)
                    {
                        "press a gamepad button…"
                    } else {
                        "(pick output)"
                    };
                    ui.label(egui::RichText::new(hint).italics().weak());
                } else {
                    remapper_render_chord(ui, &draft_out, skin);
                }
            }
            _ => {}
        }
    });
    // Row 2: actions + mouse multiplier. Constrained to the mapping-card width
    // (TZ_CARD_W) so the right-aligned mouse multiplier pins to the CARD's right
    // edge — not the widget's available width, which grows when the touchpad is
    // scaled up (that made the control drift ever further right and jam against
    // the scrollbar).
    // Action-button rects, captured in the SAME order as `nav_tz_action_items`
    // (app.rs) so the gamepad-nav glow rings the focused button and scroll-into-
    // view lands on it. Order per phase: idle=[learn]; learning=[cancel];
    // captured=[assign, gamepad, add, cancel].
    let mut act_rects: Vec<egui::Rect> = Vec::new();
    let mut mouse_rect: Option<egui::Rect> = None;
    let mut mode_rect: Option<egui::Rect> = None;
    ui.allocate_ui_with_layout(
        egui::vec2(TZ_CARD_W, ui.spacing().interact_size.y.max(20.0)),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
        match phase.as_str() {
            "idle" if menu_mode => {
                // Menu: the trigger is this zone's selection — go straight to
                // picking the DESTINATION (gamepad learn or the picker).
                let b = ui.button("Learn")
                    .on_hover_text("Learn a gamepad button as this zone's output — press one on the pad.");
                act_rects.push(b.rect);
                if b.clicked() {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert("_tz_phase".into(), Value::from("captured"));
                        node.params.insert("_tz_trig".into(), Value::from("menu_sel"));
                        node.params.insert("_tz_gp_arm".into(), Value::from(true));
                        for k in ["_tz_draft_out", "_tz_gp_base", "_tz_gp_seen"] { node.params.remove(k); }
                    }
                }
                let b = ui.button("Assign…")
                    .on_hover_text("Pick keyboard / mouse / stick / macro outputs for this zone. Pick several for a chord, then Add.");
                act_rects.push(b.rect);
                if b.clicked() {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert("_tz_phase".into(), Value::from("captured"));
                        node.params.insert("_tz_trig".into(), Value::from("menu_sel"));
                        node.params.remove("_tz_draft_out");
                    }
                    request_special_picker(ui.ctx(), SpecialPickerRequest {
                        inner: node_id,
                        path: subpatch_path(automap_parent),
                        draft_key: "_tz_draft_out".to_string(),
                        phase_key: None,
                        touch_zones: true,
                        exclude_pin_prefix: menu_excl.clone(),
                    });
                }
            }
            "idle" => {
                let b = ui.button("Learn")
                    .on_hover_text("Demonstrate a touch, click, or swipe on a zone, then assign an output.");
                act_rects.push(b.rect);
                if b.clicked() {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert("_tz_phase".into(), Value::from("learning"));
                        for k in ["_tz_trig", "_tz_cap_active", "_tz_cap_sx", "_tz_cap_sy",
                                  "_tz_cap_click", "_tz_cap_moved", "_tz_cap_dir"] { node.params.remove(k); }
                    }
                }
            }
            "learning" => {
                let b = ui.button("Cancel");
                act_rects.push(b.rect);
                if b.clicked() {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert("_tz_phase".into(), Value::from("idle"));
                    }
                }
            }
            _ => { // captured
                let b = ui.button("Assign…")
                    .on_hover_text("Pick keyboard / mouse / mouse-delta / stick outputs. Pick several for a chord, then Add.");
                act_rects.push(b.rect);
                if b.clicked() {
                    request_special_picker(ui.ctx(), SpecialPickerRequest {
                        inner: node_id,
                        path: subpatch_path(automap_parent),
                        draft_key: "_tz_draft_out".to_string(),
                        phase_key: None,
                        touch_zones: true,
                        exclude_pin_prefix: menu_excl.clone(),
                    });
                }
                let armed = getp(snarl, "_tz_gp_arm").and_then(|v| v.as_bool()).unwrap_or(false);
                let b = ui.add(egui::Button::new(if armed { "🎮…" } else { "🎮" }))
                    .on_hover_text("Learn a gamepad button as the output — press one now.");
                act_rects.push(b.rect);
                if b.clicked() {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert("_tz_gp_arm".into(), Value::from(!armed));
                        node.params.remove("_tz_gp_base");
                        node.params.remove("_tz_gp_seen");
                    }
                }
                let add = ui.add_enabled(!draft_out.is_empty(),
                    egui::Button::new(egui::RichText::new("Add").strong()));
                act_rects.push(add.rect);
                if add.on_hover_text("Add this zone mapping").clicked() && !draft_out.is_empty() {
                    tz_commit_card(snarl, node_id, sel_f, sel_z, &trigger, &draft_out);
                }
                let b = ui.button("Cancel");
                act_rects.push(b.rect);
                if b.clicked() {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        node.params.insert("_tz_phase".into(), Value::from("idle"));
                        for k in ["_tz_draft_out", "_tz_gp_arm", "_tz_gp_base", "_tz_gp_seen"] { node.params.remove(k); }
                    }
                }
            }
        }
        // Node-global analog controls, right-aligned: the "Touchpad mode" dropdown
        // (relative/absolute + touchpad, shown for any analog card) and the mouse-
        // speed multiplier (only when a card drives the mouse). Both are gamepad
        // targets; rects publish mode → mouse → hold, matching nav_tz_action_items.
        if has_analog_card || has_mouse_card {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Mouse-speed sits rightmost (added first in a right-to-left row).
                if has_mouse_card {
                    // Per-zone speed (stored on the selected zone's cards like
                    // tp_mode/adaptive), migrating from the old node-global value.
                    let mut spd = snarl.get_node(node_id)
                        .and_then(|n| n.params.get("zone_maps").and_then(|v| v.as_array()))
                        .and_then(|cards| cards.iter()
                            .filter(|c| c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) == sel_f as u64
                                     && c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) == sel_z as u64)
                            .find_map(|c| c.get("mouse_speed").and_then(|v| v.as_f64())))
                        .or_else(|| getp(snarl, "mouse_speed").and_then(|v| v.as_f64()))
                        .unwrap_or(1.0) as f32;
                    let dv = ui.add(egui::DragValue::new(&mut spd).speed(0.02).range(0.1..=10.0).prefix("🖱 "))
                        .on_hover_text("This zone's relative-mouse speed multiplier (1.0 ≈ a firm gyro/right-stick flick at full zone deflection, or roughly a 1:1 touchpad sweep). The sink's own mouse sensitivity still applies on top. Gamepad: focus it and change with the d-pad / left stick.");
                    mouse_rect = Some(dv.rect);
                    if dv.changed() {
                        if let Some(node) = snarl.get_node_mut(node_id) {
                            if let Some(cards) = node.params.get_mut("zone_maps").and_then(|v| v.as_array_mut()) {
                                for c in cards.iter_mut() {
                                    let f = c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                                    let z = c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                                    if f == sel_f && z == sel_z {
                                        if let Some(o) = c.as_object_mut() {
                                            o.insert("mouse_speed".into(), Value::from(spd as f64));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // "Touchpad mode": how analog deflection is derived. A ComboBox for
                // mouse; gamepad cycles it in place with LT/RT (see nav). The
                // relative/absolute VALUE lives per-card (below); this picks whether
                // the top card drives all (Synced), each card is independent
                // (Per-card), or the pointer tracks finger motion (Touchpad).
                if has_analog_card {
                    let label = match tp_mode.as_str() {
                        "percard" => "⌖ Per-card",
                        "touchpad" => "⌖ Touchpad",
                        _ => "⌖ Synced",
                    };
                    let mut chosen: Option<&str> = None;
                    let cb = egui::ComboBox::from_id_salt(("tz_tp_mode", node_id.0))
                        .selected_text(egui::RichText::new(label).size(11.0))
                        .width(96.0)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(tp_mode == "synced", "Synced (top card drives all)").clicked() { chosen = Some("synced"); }
                            if ui.selectable_label(tp_mode == "percard", "Per-card (each independent)").clicked() { chosen = Some("percard"); }
                            if ui.selectable_label(tp_mode == "touchpad", "Touchpad (finger motion)").clicked() { chosen = Some("touchpad"); }
                        });
                    let mode_resp = cb.response.on_hover_text("How a zone's analog deflection is derived. Synced: every card in a zone uses the top card's relative/absolute setting. Per-card: each card uses its own. Touchpad: the mouse pointer follows the finger's motion like a laptop touchpad (stick/scroll still use deflection). Gamepad: focus it and cycle with LT/RT.");
                    mode_rect = Some(mode_resp.rect);
                    if let Some(m) = chosen {
                        // Write the mode onto every card of the SELECTED zone.
                        if let Some(node) = snarl.get_node_mut(node_id) {
                            if let Some(cards) = node.params.get_mut("zone_maps").and_then(|v| v.as_array_mut()) {
                                for c in cards.iter_mut() {
                                    let f = c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                                    let z = c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                                    if f == sel_f && z == sel_z {
                                        if let Some(o) = c.as_object_mut() {
                                            o.insert("tp_mode".into(), Value::from(m));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            });
        }
    });
    // Order MUST match nav_tz_action_items: mode, mouse_speed, hold.
    if let Some(r) = mode_rect { act_rects.push(r); }
    if let Some(r) = mouse_rect { act_rects.push(r); }
    if let Some(r) = hold_rect { act_rects.push(r); }
    // Publish the action-button rects (scope "zone_maps") so the gamepad-nav
    // overlay rings the focused button + can scroll it into view — same channel
    // the Remapper's action row uses.
    publish_nav_action_rects_scoped(ui, node_id, "zone_maps", &act_rects);
    // Keep polling live input while a capture is in flight.
    if phase != "idle" { ui.ctx().request_repaint(); }

    // ── Existing cards for the selected zone (display + press-mode + delete +
    // drag-to-reorder). The list is a FILTERED subset of `zone_maps` (this zone
    // only), so reorder runs in DISPLAY-index space and the reordered subset is
    // written back into the same array slots — other zones' cards stay put. ──
    let mut cards: Vec<Value> = snarl.get_node(node_id)
        .and_then(|n| n.params.get("zone_maps").and_then(|v| v.as_array()).cloned())
        .unwrap_or_default();
    let display: Vec<usize> = cards.iter().enumerate().filter(|(_, c)|
        c.get("f").and_then(|v| v.as_u64()).unwrap_or(0) == sel_f as u64 &&
        c.get("z").and_then(|v| v.as_u64()).unwrap_or(0) == sel_z as u64)
        .map(|(i, _)| i).collect();
    let mut dirty = false;
    let mut remove: Option<usize> = None; // full-array index
    if display.is_empty() && phase == "idle" {
        let hint = if menu_mode {
            "No mappings — Learn a gamepad output or Assign… one for this zone."
        } else {
            "No mappings — press Learn, then demonstrate on a zone."
        };
        ui.label(egui::RichText::new(hint).weak());
    }
    let reorder_enabled = display.len() > 1;
    // Live deflection magnitude of the selected zone — the preview dot on any
    // open card curve editor. Computed once for the whole card list.
    // Raw adaptive-centre deflection (dfx, dfy) of the selected zone, in y-down unit
    // space (published by tz_live_hits). Kept as the 2D vector so each card can
    // derive the RIGHT preview input: an analog card wants the magnitude, but a
    // swipe-direction card is a 1-D gesture — it must show only the component along
    // its own axis (matching the engine's `dir`), so the preview dot and the
    // threshold line agree with what actually fires.
    let tz_live_defl: Option<(f32, f32)> = if menu_mode {
        // Menu zones have no touch deflection; the curve preview dot stays put.
        None
    } else {
        let _ = tz_live_hits(snarl, node_id, live_signals, automap_parent, ui.ctx());
        ui.ctx().data(|d| d.get_temp::<(u64, std::collections::HashMap<(usize, usize), (f32, f32)>)>(
                egui::Id::new(("tz_live_defl", node_id.0))))
            .and_then(|(_, mp)| mp.get(&(sel_f, sel_z)).copied())
    };
    // Preview input for a card: swipe cards → 1-D directional value along the swipe
    // axis (engine ax=dfx, ay=−dfy; up→−dfy, down→dfy, left→−dfx, right→dfx),
    // clamped ≥0; analog cards → 2-D deflection magnitude.
    let card_live_mag = |in_pins: &[String]| -> Option<f32> {
        tz_live_defl.map(|(dfx, dfy)| {
            match in_pins.first().map(String::as_str) {
                Some("tz_swipe_up")    => (-dfy).clamp(0.0, 1.0),
                Some("tz_swipe_down")  => (dfy).clamp(0.0, 1.0),
                Some("tz_swipe_left")  => (-dfx).clamp(0.0, 1.0),
                Some("tz_swipe_right") => (dfx).clamp(0.0, 1.0),
                _ => (dfx * dfx + dfy * dfy).sqrt().min(1.0),
            }
        })
    };
    // Gamepad nav still edits ONE curve per zone (the shared driver has a
    // single geometry channel per node) — attach it to the FIRST analog card,
    // matching what `tz_zone_curve`/`tz_set_zone_curve` in the nav path edit.
    let mut nav_curve_given = false;
    let mut rv = ReorderView::begin(
        ui, egui::Id::new(("fxi_tz_reorder", node_id.0, sel_f, sel_z)), reorder_enabled);
    for (slot, &i) in display.iter().enumerate() {
        if let Some(h) = rv.gap_before(slot) { draw_insertion_gap(ui, h); }
        let mut working = cards[i].as_object().cloned().unwrap_or_default();
        let before = working.clone();
        let in_pins: Vec<String> = working.get("in").and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
        let out_pins: Vec<String> = working.get("out").and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
        let drag_off = rv.offset_for(slot);
        let card_analog = out_pins.iter().any(|p| tz_out_pin_is_analog(p));
        let nav_uid = if card_analog && !nav_curve_given {
            nav_curve_given = true;
            Some(node_id.0)
        } else {
            None
        };
        // The first analog card of the zone is the "top" card that drives the
        // others in Synced mode (matches the engine's adaptive_for).
        let is_top_analog = nav_uid.is_some();
        ui.allocate_ui_with_layout(
            egui::vec2(TZ_CARD_W, 1.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                let result = remapper_mapping_card_pixel(
                    ui, node_id, i, &mut working,
                    &in_pins, Some(&out_pins), skin,
                    true, reorder_enabled, drag_off, "zone_maps", card_analog,
                );
                if result.delete_clicked { remove = Some(i); }
                rv.observe(slot, &result);
                // Response curve + threshold. Analog cards shape the zone's
                // deflection (no threshold — the gate is touch presence). Swipe
                // cards ALSO get the curve, WITH the activation threshold: it gates
                // the swipe's directional magnitude into a hold (see the engine).
                let is_swipe = in_pins.first().map(|t| t.starts_with("tz_swipe")).unwrap_or(false);
                if card_analog || is_swipe {
                    mapping_card_curve_section(
                        ui, node_id, "zone_maps", i, &mut working,
                        is_swipe, card_live_mag(&in_pins), if card_analog { nav_uid } else { None },
                    );
                    // Per-card relative/absolute (adaptive centre / off-centre
                    // tolerance). Hidden in Touchpad mode. Analog cards follow the
                    // mode (Synced → only the zone's top card shows it, driving the
                    // rest); swipe cards always show their own.
                    let show_adaptive = if is_swipe {
                        tp_mode != "touchpad"
                    } else {
                        match tp_mode.as_str() {
                            "touchpad" => false,
                            "synced"   => is_top_analog,
                            _          => true, // percard
                        }
                    };
                    if show_adaptive {
                        let mut ap = working.get("adaptive").and_then(|v| v.as_f64())
                            .unwrap_or(0.30) as f32 * 100.0;
                        let label = if tp_mode == "synced" { "Rel. center (drives zone)" } else { "Rel. center" };
                        // Gamepad focus: this is card field 7 (see nav_drive_remap_card).
                        // Ring + scroll-to it when the entered card focuses that field.
                        let pass = ui.ctx().cumulative_pass_nr();
                        let adaptive_focused = ui.ctx()
                            .data(|d| d.get_temp::<(u64, usize, bool)>(
                                egui::Id::new(("gp_nav_remap_card", node_id.0, "zone_maps"))))
                            .filter(|(p, sel, ent)| *ent && *sel == i && pass.saturating_sub(*p) <= 1)
                            .and_then(|_| ui.ctx().data(|d| d.get_temp::<(u64, u64)>(
                                egui::Id::new(("gp_nav_remap_card_field", node_id.0, "zone_maps")))))
                            .map(|(p, f)| f == 7 && pass.saturating_sub(p) <= 1)
                            .unwrap_or(false);
                        let sl = ui.add(egui::Slider::new(&mut ap, 0.0..=100.0)
                                .text(label).suffix("%").fixed_decimals(0))
                            .on_hover_text("How much of the zone acts as a relative centre for THIS card's analog deflection. 0% = the fixed zone centre (absolute); 100% = wherever your finger first lands becomes the centre (fully relative). In Synced mode the top card's value drives every card in the zone. Gamepad: enter the card and cycle to this field, then change with up/down.");
                        if adaptive_focused {
                            let accent = ui.visuals().selection.stroke.color;
                            ui.painter().rect_stroke(sl.rect.expand(2.0), 3.0,
                                egui::Stroke::new(1.5, accent), egui::StrokeKind::Outside);
                            sl.scroll_to_me(None);
                        }
                        if sl.changed() {
                            working.insert("adaptive".into(), Value::from((ap / 100.0) as f64));
                        }
                    }
                }
            },
        );
        if working != before {
            cards[i] = Value::Object(working);
            dirty = true;
        }
    }
    if let Some(h) = rv.gap_after_last(display.len()) { draw_insertion_gap(ui, h); }
    if let Some((from, to)) = rv.finish(ui) {
        // from/to are DISPLAY slots — reorder the subset, write back into slots.
        let mut sub: Vec<Value> = display.iter().map(|&fi| cards[fi].clone()).collect();
        reorder_array(&mut sub, from, to);
        for (k, &fi) in display.iter().enumerate() { cards[fi] = sub[k].clone(); }
        dirty = true;
    }
    if let Some(i) = remove { cards.remove(i); dirty = true; }
    if dirty {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("zone_maps".to_string(), Value::Array(cards));
        }
    }

    // The zone-level "Relative center" slider that used to sit here is gone: the
    // relative/absolute setting is now PER-CARD (a control on each analog card in
    // the loop above), governed by the node "Touchpad mode" dropdown in the action
    // row (Synced = top card drives the zone; Per-card = each independent; Touchpad
    // = finger-motion pointer).
}

/// UI-side trigger capture during Learn: track the primary finger (touch1) from
/// touch-down to release and classify the gesture — swipe (moved past threshold),
/// click (pad pressed), else plain touch. Returns the trigger token on release.
/// Scratch persists in `_tz_cap_*` node params.
pub(crate) fn tz_learn_capture(
    snarl: &mut Snarl<NodeData>,
    node_id: NodeId,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    dev: Option<&str>,
) -> Option<String> {
    use flexinput_core::touchzones as tz;
    let dev = dev?;
    let readf = |pin: &str| live_signals.get(&(dev.to_string(), pin.to_string())).map(|s| s.as_float()).unwrap_or(0.0);
    let readb = |pin: &str| live_signals.get(&(dev.to_string(), pin.to_string())).map(|s| s.as_bool()).unwrap_or(false);
    const SWIPE_THRESH: f32 = 0.18;
    let active = readb("touch1_active");
    let click = readb("btn_touchpad");
    let (ux, uy) = tz::pad_point_to_unit(readf("touch1_x"), readf("touch1_y"));
    let node = snarl.get_node_mut(node_id)?;
    let prev_active = node.params.get("_tz_cap_active").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut result = None;
    if active {
        if !prev_active {
            node.params.insert("_tz_cap_sx".into(), Value::from(ux as f64));
            node.params.insert("_tz_cap_sy".into(), Value::from(uy as f64));
            node.params.insert("_tz_cap_click".into(), Value::from(false));
            node.params.insert("_tz_cap_moved".into(), Value::from(false));
            node.params.insert("_tz_cap_dir".into(), Value::from(0u64));
        } else {
            if click { node.params.insert("_tz_cap_click".into(), Value::from(true)); }
            if !node.params.get("_tz_cap_moved").and_then(|v| v.as_bool()).unwrap_or(false) {
                let sx = node.params.get("_tz_cap_sx").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                let sy = node.params.get("_tz_cap_sy").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                let dx = ux - sx;
                let dy = uy - sy;
                if dx.abs().max(dy.abs()) > SWIPE_THRESH {
                    let dir: u64 = if dx.abs() >= dy.abs() { if dx > 0.0 { 4 } else { 3 } }
                                   else if dy < 0.0 { 1 } else { 2 };
                    node.params.insert("_tz_cap_dir".into(), Value::from(dir));
                    node.params.insert("_tz_cap_moved".into(), Value::from(true));
                }
            }
        }
        node.params.insert("_tz_cap_active".into(), Value::from(true));
    } else {
        if prev_active {
            let moved = node.params.get("_tz_cap_moved").and_then(|v| v.as_bool()).unwrap_or(false);
            let clicked = node.params.get("_tz_cap_click").and_then(|v| v.as_bool()).unwrap_or(false);
            let dir = node.params.get("_tz_cap_dir").and_then(|v| v.as_u64()).unwrap_or(0);
            result = Some(if moved {
                match dir { 1 => "tz_swipe_up", 2 => "tz_swipe_down", 3 => "tz_swipe_left", _ => "tz_swipe_right" }.to_string()
            } else if clicked { "tz_click".to_string() } else { "tz_touch".to_string() });
        }
        node.params.insert("_tz_cap_active".into(), Value::from(false));
    }
    result
}

/// Hover-revealed +/- line-editing overlay over one pad `rect` (local layer
/// space). Only "−" marks show at rest (one per interior divider per crossing
/// band); hovering reveals flanking "+"; border "+" appears near a field edge.
/// Applies the resulting grid op wire-preservingly via `tz_restructure`. Shared
/// by the in-canvas body and the pinned widget (mapping mode). See
/// `render_touch_field` for the original prose.
#[allow(clippy::too_many_arguments)]
pub(crate) fn tz_line_edit_overlay(
    node_id: NodeId,
    field: usize,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    painter: &egui::Painter,
    rect: egui::Rect,
    col_edges: &[f32],
    row_edges: &[f32],
    accent: egui::Color32,
    visuals: &egui::Visuals,
) {
    use flexinput_core::touchzones as tz;
    let to_x = |u: f32| rect.left() + u * rect.width();
    let to_y = |u: f32| rect.top() + u * rect.height();
    let cols = tz::cols(col_edges);
    let rows = tz::rows(row_edges);
    let mut op: Option<tz::GridOp> = None;

    let off = 18.0;      // "+" flanking distance from the "−"
    let edge = 30.0;     // border-proximity threshold (px)
    let inset = 12.0;    // border "+" inset so it sits fully inside the field
    let from_global = ui.ctx().layer_transform_to_global(ui.layer_id())
        .unwrap_or(egui::emath::TSTransform::IDENTITY)
        .inverse();
    let ptr = ui.input(|i| i.pointer.hover_pos()).map(|p| from_global * p);
    let band_mid = |b: usize, n: usize, edges: &[f32]| -> f32 {
        let lo = if b == 0 { 0.0 } else { edges[b - 1] };
        let hi = if b == n - 1 { 1.0 } else { edges[b] };
        (lo + hi) * 0.5
    };
    let band_of = |u: f32, edges: &[f32]| edges.iter().filter(|e| u >= **e).count();

    for line in 1..cols {
        let x = to_x(col_edges[line - 1]);
        for band in 0..rows {
            let y = to_y(band_mid(band, rows, row_edges));
            let c = egui::pos2(x, y);
            let pill = egui::Rect::from_center_size(c, egui::vec2(2.0 * off + 20.0, 22.0));
            let expanded = ptr.is_some_and(|p| pill.contains(p));
            if expanded {
                if tz_mini_button(ui, painter, ui.id().with((node_id, "tzcL", field, line, band)),
                    egui::pos2(x - off, y), "+", accent, visuals) {
                    op = Some(tz::GridOp::InsertCol(line - 1));
                }
                if tz_mini_button(ui, painter, ui.id().with((node_id, "tzcR", field, line, band)),
                    egui::pos2(x + off, y), "+", accent, visuals) {
                    op = Some(tz::GridOp::InsertCol(line));
                }
                // "−" shows with the flanking "+", only on hover (dynamic).
                if tz_mini_button(ui, painter, ui.id().with((node_id, "tzc-", field, line, band)),
                    c, "−", accent, visuals) {
                    op = Some(tz::GridOp::RemoveCol(line - 1));
                }
            }
        }
    }

    for line in 1..rows {
        let y = to_y(row_edges[line - 1]);
        for band in 0..cols {
            let x = to_x(band_mid(band, cols, col_edges));
            let c = egui::pos2(x, y);
            let pill = egui::Rect::from_center_size(c, egui::vec2(22.0, 2.0 * off + 20.0));
            let expanded = ptr.is_some_and(|p| pill.contains(p));
            if expanded {
                if tz_mini_button(ui, painter, ui.id().with((node_id, "tzrU", field, line, band)),
                    egui::pos2(x, y - off), "+", accent, visuals) {
                    op = Some(tz::GridOp::InsertRow(line - 1));
                }
                if tz_mini_button(ui, painter, ui.id().with((node_id, "tzrD", field, line, band)),
                    egui::pos2(x, y + off), "+", accent, visuals) {
                    op = Some(tz::GridOp::InsertRow(line));
                }
                // "−" shows with the flanking "+", only on hover (dynamic).
                if tz_mini_button(ui, painter, ui.id().with((node_id, "tzr-", field, line, band)),
                    c, "−", accent, visuals) {
                    op = Some(tz::GridOp::RemoveRow(line - 1));
                }
            }
        }
    }

    if let Some(p) = ptr.filter(|p| rect.contains(*p)) {
        let ux = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        let uy = ((p.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
        if p.x - rect.left() < edge {
            let y = to_y(band_mid(band_of(uy, row_edges), rows, row_edges));
            if tz_mini_button(ui, painter, ui.id().with((node_id, "tzbL", field)),
                egui::pos2(rect.left() + inset, y), "+", accent, visuals) {
                op = Some(tz::GridOp::InsertCol(0));
            }
        }
        if rect.right() - p.x < edge {
            let y = to_y(band_mid(band_of(uy, row_edges), rows, row_edges));
            if tz_mini_button(ui, painter, ui.id().with((node_id, "tzbR", field)),
                egui::pos2(rect.right() - inset, y), "+", accent, visuals) {
                op = Some(tz::GridOp::InsertCol(cols - 1));
            }
        }
        if p.y - rect.top() < edge {
            let x = to_x(band_mid(band_of(ux, col_edges), cols, col_edges));
            if tz_mini_button(ui, painter, ui.id().with((node_id, "tzbT", field)),
                egui::pos2(x, rect.top() + inset), "+", accent, visuals) {
                op = Some(tz::GridOp::InsertRow(0));
            }
        }
        if rect.bottom() - p.y < edge {
            let x = to_x(band_mid(band_of(ux, col_edges), cols, col_edges));
            if tz_mini_button(ui, painter, ui.id().with((node_id, "tzbB", field)),
                egui::pos2(x, rect.bottom() - inset), "+", accent, visuals) {
                op = Some(tz::GridOp::InsertRow(rows - 1));
            }
        }
    }

    if let Some(op) = op {
        tz_restructure(node_id, field, op, snarl);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_touch_field(
    node_id: NodeId,
    field: usize,
    single: bool,
    show_resize: bool,
    mapping: bool,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    zone_live: &std::collections::HashMap<(usize, usize), (f32, f32, bool)>,
    visuals: &egui::Visuals,
    accent: egui::Color32,
    field_w: f32,
    field_h: f32,
) -> egui::Rect {
    use flexinput_core::touchzones as tz;

    let col_edges = tz_read_field_edges(snarl, node_id, field, "col_edges");
    let row_edges = tz_read_field_edges(snarl, node_id, field, "row_edges");

    if !single {
        ui.label(egui::RichText::new(format!("Pad {} — touch {}", tz::field_letter(field), field + 1))
            .small().strong().color(accent));
    }

    let (rect, _resp) = ui.allocate_exact_size(egui::vec2(field_w, field_h), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let to_x = |u: f32| rect.left() + u * rect.width();
    let to_y = |u: f32| rect.top() + u * rect.height();

    // Mapping mode: clicking a zone selects it (the card list below filters to
    // the selected zone — the "tab per zone" model). Registered BEFORE the
    // dividers / +/- overlay so those thin controls stay on top and win clicks.
    let (sel_field, sel_zone) = tz_read_selection(snarl, node_id);
    let mtree = if mapping { Some(tz_field_tree(snarl, node_id, field)) } else { None };
    if let Some(tree) = &mtree {
        let mut clicked: Option<usize> = None;
        for (id, [x0, y0, x1, y1]) in tree.zones() {
            let zr = egui::Rect::from_min_max(egui::pos2(to_x(x0), to_y(y0)), egui::pos2(to_x(x1), to_y(y1)));
            let zresp = ui.interact(zr, ui.id().with((node_id, "tzselect", field, id)), egui::Sense::click());
            if zresp.hovered() { zresp.clone().on_hover_cursor(egui::CursorIcon::PointingHand); }
            if zresp.clicked() { clicked = Some(id as usize); }
        }
        if let Some(idx) = clicked {
            if tz_pick_kind(snarl, node_id).is_some() {
                tz_apply_pick(snarl, node_id, idx);
            } else if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("sel_field".to_string(), Value::from(field as u64));
                node.params.insert("sel_zone".to_string(), Value::from(idx as u64));
            }
        }
    }

    // Pad visuals + dividers (tree-aware in mapping mode; grid in ports mode).
    tz_draw_field(node_id, field, ui, snarl, &painter, rect, &col_edges, &row_edges, zone_live, visuals, accent, None, None, "canvas");
    if mapping { tz_pick_banner(node_id, ui, snarl, &painter, rect, accent); }

    // Selected-zone outline (mapping mode) — drawn on top of the pad fill.
    if let Some(tree) = &mtree {
        if sel_field == field {
            if let Some([x0, y0, x1, y1]) = tree.zone_rect(sel_zone as u32) {
                let zr = egui::Rect::from_min_max(egui::pos2(to_x(x0), to_y(y0)), egui::pos2(to_x(x1), to_y(y1)));
                painter.rect_stroke(zr.shrink(1.5), 2.0, egui::Stroke::new(2.0, accent), egui::StrokeKind::Inside);
            }
        }
    }

    // Line editing: same hover-revealed +/- handles in both modes. Mapping mode
    // drives the tree (+ subdivides that zone, − removes/merges); ports mode drives
    // the full-cut grid.
    if mapping {
        tz_tree_line_overlay(node_id, field, ui, snarl, &painter, rect, accent, visuals);
    } else {
        tz_line_edit_overlay(node_id, field, ui, snarl, &painter, rect, &col_edges, &row_edges, accent, visuals);
    }

    // ── Resize grip (bottom-right corner). In split mode only the right pad
    // shows it; it writes the SHARED field size so both pads resize together. ─
    if show_resize {
        let hs = 14.0;
        let handle = egui::Rect::from_min_max(
            egui::pos2(rect.right() - hs, rect.bottom() - hs),
            egui::pos2(rect.right(), rect.bottom()),
        );
        let hr = ui.interact(handle, ui.id().with((node_id, "tzresize")), egui::Sense::drag());
        if hr.hovered() || hr.dragged() {
            hr.clone().on_hover_cursor(egui::CursorIcon::ResizeNwSe);
        }
        let grip = if hr.hovered() || hr.dragged() { accent } else { visuals.weak_text_color() };
        for k in 1..=3 {
            let o = k as f32 * 3.5;
            painter.line_segment(
                [egui::pos2(rect.right() - o, rect.bottom()), egui::pos2(rect.right(), rect.bottom() - o)],
                egui::Stroke::new(1.0, grip),
            );
        }
        if hr.dragged() {
            let d = hr.drag_delta();
            let nw = (field_w + d.x).clamp(200.0, 900.0);
            let nh = (field_h + d.y).clamp(120.0, 600.0);
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("field_w".to_string(), Value::from(nw as f64));
                node.params.insert("field_h".to_string(), Value::from(nh as f64));
            }
        }
    }

    rect
}








