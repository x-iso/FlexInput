//! Auto-positioning of `device.source` / `device.sink` nodes around
//! the sub-patch in Easy mode.
//!
//! Whenever Easy mode adds or replaces a device node, this helper
//! arranges everything around the sub-patch so the Advanced-mode view
//! is readable: sources stack in a left column, sinks in a right
//! column. Each column hugs the sub-patch edge (small `EDGE_GAP`) and
//! its first node is top-aligned with the sub-patch; additional nodes
//! stack downward with `STACK_GAP` between them.

use eframe::egui;
use egui_snarl::NodeId;
use flexinput_virtual::kind_prefix;

use crate::canvas::Canvas;

// Horizontal gap between a side column and the nearest sub-patch edge,
// and the vertical gap between stacked nodes in a column (canvas-space
// px). Small so the I/O nodes read as belonging to the sub-patch.
const EDGE_GAP: f32 = 15.0;
const STACK_GAP: f32 = 15.0;
// Fallback widths used when we can't read the live measured size.
// Device.source headers include the Calibrate/Hz/Deadzone/Gyro row,
// which routinely runs ~420px. Device.sink headers are narrower
// (~260px) but we use the source value for both to keep symmetry
// and ensure clearance when sources sit beside sinks visually.
const FALLBACK_DEVICE_W: f32 = 420.0;
const FALLBACK_SINK_W: f32 = 280.0;
const FALLBACK_SUBPATCH_W: f32 = 360.0;
// Fallback node HEIGHTS for vertical stacking (the layout runs right
// after a node is added, before it renders, so live heights aren't
// available — mirrors the width fallbacks). These are the COLLAPSED-ish
// rendered heights of the compact Easy I/O nodes: a source header
// (title + Calibrate/Hz + Deadzone/Gyro rows) ≈ 80px; a sink header
// (title + one control row: model+Rumble, or mouse-speed) ≈ 56px.
// Tuned so the second stacked node sits ~15px below the first, not far
// below it.
const FALLBACK_DEVICE_H: f32 = 80.0;
const FALLBACK_SINK_H: f32 = 56.0;
// First-frame-only fallback: padding added to the sub-patch's BODY bbox to
// approximate the outer-NODE width, used ONLY until the node has rendered once
// and `final_node_rect` supplies the real measured width (which is what we use
// thereafter, and which reflects Layout-editor resizes). Covers both pin columns
// + frame insets + the AutoMap outlet chevron.
const SUBPATCH_BODY_PAD_W: f32 = 130.0;

pub fn reposition_io_nodes(canvas: &mut Canvas) {
    reposition_io_nodes_with_ctx(canvas, None);
}

pub fn reposition_io_nodes_with_ctx(canvas: &mut Canvas, ctx: Option<&egui::Context>) {
    // Pull this canvas's measured node rects (canvas-space w/h), stashed by
    // `viewer::final_node_rect` each frame. With real sizes the I/O nodes hug
    // the sub-patch correctly AND reflow after the Layout editor resizes it. On
    // the very first frame a node exists (or with no ctx) the map lacks it, so
    // fall back to the constants below.
    let salt = canvas.view_salt;
    let measured: crate::canvas::viewer::NodeRectMap = ctx
        .and_then(|c| c.data(|d| d.get_temp::<crate::canvas::viewer::NodeRectMap>(
            crate::canvas::viewer::node_rects_id(salt))))
        .unwrap_or_default();
    let meas_w = |id: NodeId, fb: f32| measured.get(&id.0).map(|r| r[2]).filter(|w| *w > 1.0).unwrap_or(fb);
    let meas_h = |id: NodeId, fb: f32| measured.get(&id.0).map(|r| r[3]).filter(|h| *h > 1.0).unwrap_or(fb);

    let snarl = &mut canvas.snarl;

    // Locate the sub-patch. Prefer its REAL measured width (covers all chrome +
    // reflects Layout-editor resizes); fall back to a body-bbox estimate only
    // until it's been rendered once.
    let mut subpatch: Option<(NodeId, egui::Pos2)> = None;
    let mut sp_bbox_w = FALLBACK_SUBPATCH_W;
    for (id, n) in snarl.nodes_ids_data() {
        if n.value.module_id == "subpatch" {
            let pos = snarl.get_node_info(id).map(|i| i.pos).unwrap_or_default();
            if let Some(sp) = n.value.subpatch.as_ref() {
                let mut max_x = 0.0f32;
                for it in &sp.items {
                    let (lp, ls) = it.bbox();
                    max_x = max_x.max(lp[0] + ls[0]);
                }
                sp_bbox_w = (max_x + SUBPATCH_BODY_PAD_W).max(FALLBACK_SUBPATCH_W);
            }
            subpatch = Some((id, pos));
            break;
        }
    }
    let (subpatch_pos, sp_w) = match subpatch {
        Some((id, pos)) => (pos, meas_w(id, sp_bbox_w)),
        None => (egui::pos2(0.0, 0.0), FALLBACK_SUBPATCH_W),
    };

    // Collect the two columns. SOURCES go left; SINKS go right with the gamepad
    // pad first (primary output) then keyboard/mouse. Deterministic order
    // (insertion order) so the stack doesn't reshuffle frame to frame.
    let mut sources: Vec<NodeId> = Vec::new();
    let mut gamepad_sinks: Vec<NodeId> = Vec::new();
    let mut keymouse_sinks: Vec<NodeId> = Vec::new();
    for (id, n) in snarl.nodes_ids_data() {
        match n.value.module_id.as_str() {
            "device.source" => sources.push(id),
            "device.sink" => {
                let did = n.value.params.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
                if kind_prefix(did) == "virtual.keymouse" {
                    keymouse_sinks.push(id);
                } else {
                    // Every other sink is a gamepad output (HIDMaestro kinds,
                    // plus legacy ViGEm ids kept for un-migrated patches).
                    gamepad_sinks.push(id);
                }
            }
            _ => {}
        }
    }
    let sinks: Vec<NodeId> = gamepad_sinks.into_iter().chain(keymouse_sinks).collect();

    // Sinks column: left edge EDGE_GAP px right of the sub-patch's right edge.
    let sink_x = subpatch_pos.x + sp_w + EDGE_GAP;

    // Stack a column downward from the sub-patch top: first node top-aligned with
    // the sub-patch, each subsequent node STACK_GAP below the PREVIOUS node's
    // bottom (using that node's own measured height). `right_aligned` places a
    // node by its right edge EDGE_GAP px left of the sub-patch (sources) vs by
    // its left edge at `sink_x` (sinks). Per-node width is measured too, so a
    // wide source still ends EDGE_GAP from the sub-patch.
    let mut place_column = |snarl: &mut egui_snarl::Snarl<crate::canvas::NodeData>,
                            ids: &[NodeId], right_aligned: bool, fb_w: f32, fb_h: f32| {
        let mut y = subpatch_pos.y;
        for &id in ids {
            let w = meas_w(id, fb_w);
            let h = meas_h(id, fb_h);
            let x = if right_aligned { subpatch_pos.x - EDGE_GAP - w } else { sink_x };
            let target = egui::pos2(x, y);
            if let Some(info) = snarl.get_node_info_mut(id) {
                // Only write when it actually moves — the per-frame Easy reflow
                // would otherwise mark the snarl dirty every frame and pin a
                // repaint loop. Sub-pixel epsilon tolerates measurement jitter.
                if info.pos.distance(target) > 0.5 {
                    info.pos = target;
                }
            }
            y += h + STACK_GAP;
        }
    };
    place_column(snarl, &sources, true, FALLBACK_DEVICE_W, FALLBACK_DEVICE_H);
    place_column(snarl, &sinks, false, FALLBACK_SINK_W, FALLBACK_SINK_H);
}
