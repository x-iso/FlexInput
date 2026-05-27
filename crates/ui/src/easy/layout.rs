//! Auto-positioning of `device.source` / `device.sink` nodes around
//! the sub-patch in Easy mode.
//!
//! Whenever Easy mode adds or replaces a device node, this helper
//! arranges everything around the sub-patch so the Advanced-mode view
//! is readable: source on the left of the sub-patch, gamepad sink to
//! the right, keymouse sink below the gamepad sink (or in its place
//! if no gamepad output is active).

use eframe::egui;
use egui_snarl::NodeId;
use flexinput_virtual::kind_prefix;

use crate::canvas::Canvas;

// Horizontal breathing room between a node's right edge and the
// next node's left edge, in canvas-space pixels (snarl coords).
// 200px keeps the AutoMap chevron + a comfortable wire arc clear
// even when sub-patches embed wide body items.
const GAP: f32 = 200.0;
// Fallback widths used when we can't read the live measured size.
// Device.source headers include the Calibrate/Hz/Deadzone/Gyro row,
// which routinely runs ~420px. Device.sink headers are narrower
// (~260px) but we use the source value for both to keep symmetry
// and ensure clearance when sources sit beside sinks visually.
const FALLBACK_DEVICE_W: f32 = 420.0;
const FALLBACK_SINK_W: f32 = 280.0;
const FALLBACK_SUBPATCH_W: f32 = 360.0;
const FALLBACK_SUBPATCH_H: f32 = 200.0;
// Padding around the sub-patch's measured body box to land on a
// reasonable outer-node width.
const SUBPATCH_BODY_PAD_W: f32 = 48.0;
const SUBPATCH_BODY_PAD_H: f32 = 120.0; // header + footer chrome
// Live-measured node widths are stashed in egui temp data each
// frame by the snarl viewer. We read those (if present) to place
// nodes precisely; otherwise we fall back to the constants above.
// See `try_measured_width` for the key shape.

/// Read snarl's per-node measured size from egui temp data. egui-snarl
/// keys node state by `parent_ui_id.with("flexinput_canvas").with(("snarl-node", NodeId))`
/// inside `NodeState::load`. We don't know the parent_ui_id here
/// (the snarl call site is deep inside the canvas show flow), so we
/// can't reproduce the exact key; instead we fall back to the
/// caller-supplied constants. A future enhancement could stash a
/// (NodeId → Vec2) snapshot at the end of each canvas show.
fn measured_width_or(_id: NodeId, _ctx: Option<&egui::Context>, fallback: f32) -> f32 {
    fallback
}

pub fn reposition_io_nodes(canvas: &mut Canvas) {
    reposition_io_nodes_with_ctx(canvas, None);
}

pub fn reposition_io_nodes_with_ctx(canvas: &mut Canvas, ctx: Option<&egui::Context>) {
    let snarl = &mut canvas.snarl;

    // Locate the sub-patch + estimate its bounding box. Width = max
    // of FALLBACK and (max body item right-edge + padding); height
    // similar. Real snarl-rendered size isn't available here (no UI
    // frame yet for newly-spawned nodes), so we estimate from the
    // body item bboxes which ARE persisted.
    let mut subpatch_info: Option<(NodeId, egui::Pos2, f32, f32)> = None;
    for (id, n) in snarl.nodes_ids_data() {
        if n.value.module_id == "subpatch" {
            let pos = snarl.get_node_info(id).map(|i| i.pos).unwrap_or_default();
            let (w, h) = match n.value.subpatch.as_ref() {
                Some(sp) => {
                    let (mut max_x, mut max_y) = (0.0f32, 0.0f32);
                    for it in &sp.items {
                        let (lp, ls) = it.bbox();
                        max_x = max_x.max(lp[0] + ls[0]);
                        max_y = max_y.max(lp[1] + ls[1]);
                    }
                    (
                        (max_x + SUBPATCH_BODY_PAD_W).max(FALLBACK_SUBPATCH_W),
                        (max_y + SUBPATCH_BODY_PAD_H).max(FALLBACK_SUBPATCH_H),
                    )
                }
                None => (FALLBACK_SUBPATCH_W, FALLBACK_SUBPATCH_H),
            };
            subpatch_info = Some((id, pos, w, h));
            break;
        }
    }
    let (subpatch_pos, sp_w, sp_h) = match subpatch_info {
        Some((_, pos, w, h)) => (pos, w, h),
        None => (egui::pos2(0.0, 0.0), FALLBACK_SUBPATCH_W, FALLBACK_SUBPATCH_H),
    };

    let mut source_id: Option<NodeId> = None;
    let mut gamepad_sink_id: Option<NodeId> = None;
    let mut keymouse_sink_id: Option<NodeId> = None;
    for (id, n) in snarl.nodes_ids_data() {
        match n.value.module_id.as_str() {
            "device.source" => source_id = Some(id),
            "device.sink" => {
                let did = n.value.params.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
                let prefix = kind_prefix(did);
                if prefix == "virtual.keymouse" {
                    keymouse_sink_id = Some(id);
                } else if prefix == "virtual.xinput" || prefix == "virtual.ds4" {
                    gamepad_sink_id = Some(id);
                }
            }
            _ => {}
        }
    }

    // Source: place GAP px to the left of the sub-patch. Subtract
    // the source node's measured width so its right edge — where
    // the AutoMap output pin lives — sits GAP px from the sub-patch's
    // left edge.
    if let Some(id) = source_id {
        let w = measured_width_or(id, ctx, FALLBACK_DEVICE_W);
        if let Some(info) = snarl.get_node_info_mut(id) {
            info.pos = egui::pos2(
                subpatch_pos.x - w - GAP,
                subpatch_pos.y,
            );
        }
    }
    // Gamepad sink: GAP px past the sub-patch's right edge.
    let sink_x = subpatch_pos.x + sp_w + GAP;
    if let Some(id) = gamepad_sink_id {
        if let Some(info) = snarl.get_node_info_mut(id) {
            info.pos = egui::pos2(sink_x, subpatch_pos.y);
        }
    }
    // Keymouse sink: below the gamepad sink (sub-patch height + GAP)
    // or co-located if no gamepad sink exists.
    if let Some(id) = keymouse_sink_id {
        if let Some(info) = snarl.get_node_info_mut(id) {
            let y = if gamepad_sink_id.is_some() {
                subpatch_pos.y + sp_h + GAP
            } else {
                subpatch_pos.y
            };
            info.pos = egui::pos2(sink_x, y);
        }
    }
    let _ = FALLBACK_SINK_W; // reserved for future per-sink-width refinement
}
