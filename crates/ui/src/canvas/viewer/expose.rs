//! Layout-mode / overlay-pick ctx-key API: element exposure registration,
//! pick request/result handshakes, special-picker requests.

use super::*;

// ── Layout mode (Pin element) overlay framework ───────────────────────────────
//
// When a sub-patch's editor window is in Layout mode, body renderers register
// hit-rects for each exposable UI element via `register_exposable_element`.
// The helper draws a tinted overlay and converts a click into a pin request,
// stashed in egui memory and read by `show_subpatch_editors` after render.
//
// Module-side renderers that haven't been instrumented yet simply don't
// register anything, so they're invisible to the layout selector.

pub(crate) const LAYOUT_ACTIVE_KEY:  &str = "fxi_layout_active";
pub(crate) const LAYOUT_PENDING_KEY: &str = "fxi_layout_pending";

// ── Overlay pick mode ─────────────────────────────────────────────────────────
//
// A second "armed" state alongside layout mode, entered from the overlay edit
// toolbar's "Add element" button. Exposable elements light up amber (vs. the
// layout mode's blue) in the MAIN window and first-level sub-patch editors;
// a click stashes a PENDING request (no path context — the clicked viewport
// doesn't know where its snarl lives). `app.rs` drains PENDING at the call
// sites that DO know the context and re-stashes it as a RESULT carrying the
// full node path, which the overlay consumes to create the pin.

pub(crate) const OVERLAY_PICK_ACTIVE_KEY:  &str = "fxi_overlay_pick_active";
pub(crate) const OVERLAY_PICK_PENDING_KEY: &str = "fxi_overlay_pick_pending";
pub(crate) const OVERLAY_PICK_RESULT_KEY:  &str = "fxi_overlay_pick_result";
// All viewports share one egui Context (and thus these temp slots), so pick
// mode would also arm elements inside NESTED sub-patch editors — which MVP
// can't address (paths are one level deep). Those editors set this around
// their canvas render so their elements stay cold instead of amber-but-dead.
pub(crate) const OVERLAY_PICK_SUPPRESSED_KEY: &str = "fxi_overlay_pick_suppressed";
// Pick DESTINATION discriminator. The info overlay and the config overlay (M3)
// both drive `register_exposable_element` through the SAME armed-pick state (so
// the amber highlights + the app.rs/subpatch.rs PENDING→RESULT path resolution
// are shared verbatim). This flag says WHICH overlay armed the current pick, so
// only that overlay consumes the resolved result. `false`/absent = info overlay
// (the pre-M3 default); `true` = config overlay.
pub(crate) const OVERLAY_PICK_DEST_CONFIG_KEY: &str = "fxi_overlay_pick_dest_config";

pub fn set_overlay_pick_suppressed(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(OVERLAY_PICK_SUPPRESSED_KEY), on));
}

/// Mark the currently-armed pick as destined for the config overlay (`true`) or
/// the info overlay (`false`). Set right after `set_overlay_pick_active(ctx, true)`.
pub fn set_overlay_pick_dest_config(ctx: &egui::Context, is_config: bool) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(OVERLAY_PICK_DEST_CONFIG_KEY), is_config));
}

/// Is the armed pick destined for the config overlay? Absent = info overlay.
pub fn overlay_pick_dest_config(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(egui::Id::new(OVERLAY_PICK_DEST_CONFIG_KEY))).unwrap_or(false)
}

pub fn set_overlay_pick_active(ctx: &egui::Context, active: bool) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(OVERLAY_PICK_ACTIVE_KEY), active));
    if !active {
        // Drop any half-finished request so a cancelled pick can't pin later.
        ctx.data_mut(|d| {
            d.remove_temp::<(usize, String, [f32; 2])>(egui::Id::new(OVERLAY_PICK_PENDING_KEY));
            d.remove_temp::<(Vec<usize>, usize, String, [f32; 2])>(egui::Id::new(OVERLAY_PICK_RESULT_KEY));
            // Reset the destination back to the info-overlay default so a
            // cancelled config pick can't leak into a later info pick.
            d.remove_temp::<bool>(egui::Id::new(OVERLAY_PICK_DEST_CONFIG_KEY));
        });
    }
}

pub fn overlay_pick_active(ctx: &egui::Context) -> bool {
    ctx.data(|d| {
        d.get_temp::<bool>(egui::Id::new(OVERLAY_PICK_ACTIVE_KEY)).unwrap_or(false)
            && !d.get_temp::<bool>(egui::Id::new(OVERLAY_PICK_SUPPRESSED_KEY)).unwrap_or(false)
    })
}

/// Raw click from `register_exposable_element` while overlay pick is armed:
/// `(inner_node_id, element_id, source_size)`. Drained by app.rs at the
/// canvas / sub-patch-editor call sites (which know the path context).
pub fn take_overlay_pick_pending(ctx: &egui::Context) -> Option<(usize, String, [f32; 2])> {
    ctx.data_mut(|d| d.remove_temp::<(usize, String, [f32; 2])>(egui::Id::new(OVERLAY_PICK_PENDING_KEY)))
}

/// Path-resolved pick, ready for the overlay to turn into an ExposedModule:
/// `(source_path, inner_node_id, element_id, source_size)`.
pub fn put_overlay_pick_result(
    ctx: &egui::Context,
    path: Vec<usize>,
    inner_node_id: usize,
    element_id: String,
    size: [f32; 2],
) {
    ctx.data_mut(|d| {
        d.insert_temp(
            egui::Id::new(OVERLAY_PICK_RESULT_KEY),
            (path, inner_node_id, element_id, size),
        );
    });
}

pub fn take_overlay_pick_result(
    ctx: &egui::Context,
) -> Option<(Vec<usize>, usize, String, [f32; 2])> {
    ctx.data_mut(|d| {
        d.remove_temp::<(Vec<usize>, usize, String, [f32; 2])>(egui::Id::new(OVERLAY_PICK_RESULT_KEY))
    })
}

pub fn set_layout_mode_active(ctx: &egui::Context, active: bool) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(LAYOUT_ACTIVE_KEY), active));
}

/// A request from a Remapper/Lean "Special…" button (mouse click) to open the
/// shared KB/M + touchpad picker. The viewer supplies the inner node + its draft
/// and phase param keys; `app.rs` resolves the containing sub-patch (or top
/// level) at the `Canvas::show` call site and opens the picker.
#[derive(Clone)]
pub struct SpecialPickerRequest {
    /// The Remapper/Lean node id (within its own snarl).
    pub inner: NodeId,
    // (Default impl below — egui's `remove_temp` requires `Default`.)
    /// Sub-patch path from the tab canvas to the snarl holding `inner`
    /// (node ids, outermost first; empty = the node sits directly on the tab
    /// canvas). Derived from the AutoMap parent chain via [`subpatch_path`].
    pub path: Vec<usize>,
    pub draft_key: String,
    /// Phase param key for Lean sections (`_lean_<side>_phase`); `None` for the
    /// Remapper (which uses `ui_phase`).
    pub phase_key: Option<String>,
    /// Touch Zones variant: hide the touchpad cluster (a touchpad can't remap to
    /// itself) and offer mouse-movement delta as an output. Default false.
    pub touch_zones: bool,
    /// Pin-id prefix whose cells render disabled for this session (e.g.
    /// `menu:{id}` so a Virtual Menu's own cards can't target that menu).
    pub exclude_pin_prefix: Option<String>,
}

/// Outermost (tab-canvas-level) sub-patch node id for an AutoMap parent chain,
/// or `None` at the top level. Used to address a Special-picker target.
pub fn root_subpatch_id(parent: Option<&AutomapGlowParent<'_>>) -> Option<NodeId> {
    let mut cur = parent?;
    while let Some(prev) = cur.prev { cur = prev; }
    Some(cur.subpatch_node_id)
}

/// Full sub-patch path for an AutoMap parent chain: the subpatch node ids from
/// the OUTERMOST level down to the node's immediate container (empty at the
/// top level). Unlike [`root_subpatch_id`] this keeps every level, so the
/// Special picker can address nodes nested arbitrarily deep.
pub fn subpatch_path(parent: Option<&AutomapGlowParent<'_>>) -> Vec<usize> {
    let mut rev = Vec::new();
    let mut cur = parent;
    while let Some(p) = cur {
        rev.push(p.subpatch_node_id.0);
        cur = p.prev;
    }
    rev.reverse();
    rev
}

impl Default for SpecialPickerRequest {
    fn default() -> Self {
        Self { inner: NodeId(0), path: Vec::new(), draft_key: String::new(), phase_key: None, touch_zones: false, exclude_pin_prefix: None }
    }
}

pub(crate) const SPECIAL_PICKER_REQ_KEY: &str = "fxi_special_picker_request";

/// Set from a Special button click; consumed by `app.rs` after the canvas/editor
/// `show()` returns (which is where the sub-patch `outer` context is known).
pub fn request_special_picker(ctx: &egui::Context, req: SpecialPickerRequest) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(SPECIAL_PICKER_REQ_KEY), req));
}

pub fn take_special_picker_request(ctx: &egui::Context) -> Option<SpecialPickerRequest> {
    ctx.data_mut(|d| d.remove_temp::<SpecialPickerRequest>(egui::Id::new(SPECIAL_PICKER_REQ_KEY)))
}

pub fn layout_mode_active(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(egui::Id::new(LAYOUT_ACTIVE_KEY))).unwrap_or(false)
}

/// Pin request returned to the editor. Includes the source-rect size so the
/// new ExposedModule starts at the same dimensions the element had on the
/// original module body, instead of an arbitrary default.
pub fn take_layout_pending(ctx: &egui::Context) -> Option<(usize, String, [f32; 2])> {
    ctx.data_mut(|d| d.remove_temp::<(usize, String, [f32; 2])>(egui::Id::new(LAYOUT_PENDING_KEY)))
}

/// Draws a tinted highlight + click target on `rect`. No-op when layout mode
/// is off, so callers can sprinkle it around without conditionals. The rect's
/// size is captured into the pin request so the new pinned widget inherits
/// the source element's dimensions.
pub(crate) fn register_exposable_element(
    ui: &mut egui::Ui,
    node_id: NodeId,
    element_id: &str,
    rect: egui::Rect,
) {
    let layout = layout_mode_active(ui.ctx());
    let picking = !layout && overlay_pick_active(ui.ctx());
    if !layout && !picking { return; }
    if rect.area() < 4.0 { return; }
    let id = egui::Id::new(("fxi_layout_elem", node_id.0, element_id));
    let resp = ui.interact(rect, id, egui::Sense::click());
    let painter = ui.painter();
    // Layout mode is blue; overlay pick is amber so the two armed states
    // can't be confused.
    let (fill, outline) = match (picking, resp.hovered()) {
        (false, true)  => (egui::Color32::from_rgba_unmultiplied(120, 200, 255, 90),
                           egui::Color32::from_rgb(180, 230, 255)),
        (false, false) => (egui::Color32::from_rgba_unmultiplied(80, 160, 220, 35),
                           egui::Color32::from_rgb(80, 160, 220)),
        (true,  true)  => (egui::Color32::from_rgba_unmultiplied(255, 200, 90, 90),
                           egui::Color32::from_rgb(255, 220, 140)),
        (true,  false) => (egui::Color32::from_rgba_unmultiplied(230, 160, 40, 35),
                           egui::Color32::from_rgb(230, 160, 40)),
    };
    painter.rect_filled(rect, 4.0, fill);
    let s = egui::Stroke::new(1.5, outline);
    painter.line_segment([rect.left_top(),     rect.right_top()],    s);
    painter.line_segment([rect.right_top(),    rect.right_bottom()], s);
    painter.line_segment([rect.right_bottom(), rect.left_bottom()],  s);
    painter.line_segment([rect.left_bottom(),  rect.left_top()],     s);
    if resp.clicked() {
        let size = [rect.width().max(40.0), rect.height().max(20.0)];
        let key = if picking { OVERLAY_PICK_PENDING_KEY } else { LAYOUT_PENDING_KEY };
        ui.ctx().data_mut(|d| {
            d.insert_temp(
                egui::Id::new(key),
                (node_id.0, element_id.to_string(), size),
            );
        });
    }
}
