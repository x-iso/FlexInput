//! Pinned-widget scaling + gamepad-nav rect publishing (see repo
//! memory/pinned_widget_scaling.md for the canonical rules).

use super::*;

/// Scales the current Ui's font and interact metrics so the inner widgets
/// visually grow/shrink with the container, instead of staying tiny while only
/// the surrounding "crop area" gets bigger. Returns the chosen scale so the
/// caller can apply it to fixed-width helpers (e.g. spacing, separators).
///
/// `natural` is the size the row was authored for (typically the size captured
/// at pin time / the rect this element occupies on the original module body).
/// Publish the per-field interactive-control rects of a pinned multi-control row
/// so the gamepad-nav driver can draw a glow on the focused field. Rects are the
/// `Response` rects in the SAME order as `nav_element_fields` for this element,
/// converted to global screen space. Keyed by inner node id; pass-stamped so the
/// driver can ignore stale data. Cheap no-op cost when nav is inactive (just a
/// temp insert), so renderers always publish.
pub(crate) fn publish_nav_field_rects(ui: &egui::Ui, inner_id: NodeId, local_rects: &[egui::Rect]) {
    if local_rects.is_empty() { return; }
    let to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
        .unwrap_or(egui::emath::TSTransform::IDENTITY);
    let rects: Vec<egui::Rect> = local_rects.iter().map(|r| to_global * *r).collect();
    let pass = ui.ctx().cumulative_pass_nr();
    // Key by (inner, element): a single inner node exposes several element rows,
    // each rendered separately, so the inner id alone is ambiguous.
    let element: String = ui.ctx().data(|d|
        d.get_temp(egui::Id::new(("gp_nav_cur_element", inner_id.0)))).unwrap_or_default();
    ui.ctx().data_mut(|d| d.insert_temp(
        egui::Id::new(("gp_nav_field_rects", inner_id.0, element)), (pass, rects)));
}

/// Publish the remapper-family action buttons' GLOBAL rects (Learn, Special,
/// Add) so the nav driver can glow the focused one. NOTHING rects are omitted.
/// Order matches `nav_remap_action_items`: Learn, Special, Add — entries with a
/// NOTHING rect are skipped so the published list lines up with the driver's
/// (which already drops Special/Add when they don't apply).
pub(crate) fn publish_nav_action_rects(ui: &egui::Ui, node_id: NodeId, action_rects: &[egui::Rect]) {
    publish_nav_action_rects_scoped(ui, node_id, "mappings", action_rects);
}

/// As `publish_nav_action_rects`, but scoped by a string discriminator so two
/// mapping lists sharing one node (gyro Lean left/right) publish independently.
pub(crate) fn publish_nav_action_rects_scoped(ui: &egui::Ui, node_id: NodeId, scope: &str, action_rects: &[egui::Rect]) {
    let to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
        .unwrap_or(egui::emath::TSTransform::IDENTITY);
    // Keep the LOCAL rects too, in publish order, for the scroll-into-view check.
    let mut local: Vec<egui::Rect> = Vec::with_capacity(action_rects.len());
    let mut rects: Vec<egui::Rect> = Vec::with_capacity(action_rects.len());
    for &r in action_rects {
        if r.is_finite() && r.width() > 0.5 { local.push(r); rects.push(to_global * r); }
    }
    // IMPORTANT: never nest ctx lock acquisitions. `cumulative_pass_nr()`,
    // `data()`, and `data_mut()` each take the egui ctx lock; calling one inside
    // another's closure deadlocks epaint (the freeze the user hit). So read every
    // ctx value into a plain local FIRST, then operate on locals.
    let pass = ui.ctx().cumulative_pass_nr();
    let action_sel: Option<(u64, usize)> = ui.ctx()
        .data(|d| d.get_temp(egui::Id::new(("gp_nav_remap_action", node_id.0, scope))));
    let clip = ui.clip_rect();

    ui.ctx().data_mut(|d| d.insert_temp(
        egui::Id::new(("gp_nav_action_rects", node_id.0, scope)), (pass, rects)));

    // If an action button is the current nav selection and it's (partly) above/
    // below the visible band, request a scroll so it comes into view. The action
    // row sits at the top of the body, so this brings the buttons back on-screen
    // when the user navigates up from the cards.
    let act_sel = action_sel
        .filter(|(p, _)| crate::widgets::nav_pass_matches(ui.ctx(), *p))
        .map(|(_, i)| i)
        .filter(|i| *i != usize::MAX);
    if let Some(ai) = act_sel {
        if let Some(r) = local.get(ai) {
            let mut need = 0.0f32;
            if r.top() < clip.top() + 4.0 { need = r.top() - (clip.top() + 4.0); }
            else if r.bottom() > clip.bottom() - 4.0 { need = r.bottom() - (clip.bottom() - 4.0); }
            if need.abs() > 1.0 {
                // Scroll-into-view temp stays UNscoped (keyed by node id): the
                // two consumers read it unscoped, and lean left/right action
                // rows live in separate scroll bodies so cross-talk is harmless.
                ui.ctx().data_mut(|d| d.insert_temp(
                    egui::Id::new(("gp_nav_remap_scroll", node_id.0)),
                    (pass, need)));
                request_repaint_throttled(ui.ctx());
            }
        }
    }
}

/// Ctx-data scratch: the natural-size cache key of the pin currently being
/// rendered. Set by `render_pinned_element` before dispatch, read here so the
/// measured content size (cached under that key) replaces the caller's guess.
pub(crate) fn pin_ws_key_scratch() -> egui::Id { egui::Id::new("pin_ws_key_scratch") }
/// Ctx-data scratch: the scale this pass actually applied, so
/// `render_pinned_element` can normalize its post-render measurement.
pub(crate) fn pin_ws_applied_scratch() -> egui::Id { egui::Id::new("pin_ws_applied_scratch") }
/// Ctx-data scratch: the natural size `apply_widget_scale` resolved for the
/// current pin (measured cache or fallback), read by `pin_flex_width`.
pub(crate) fn pin_ws_resolved_scratch() -> egui::Id { egui::Id::new("pin_ws_resolved_scratch") }
/// Ctx-data scratch: how much width the row's flexible element stretched
/// beyond its minimum this pass. `render_pinned_element` subtracts it from the
/// measured width so the cached natural always describes the row at MINIMUM
/// flexible width (otherwise the fill would feed back into the measurement).
pub(crate) fn pin_ws_flex_scratch() -> egui::Id { egui::Id::new("pin_ws_flex_scratch") }

/// Width for the ONE width-flexible element of a pinned row (a slider rail):
/// its minimum scaled width plus all of the container's surplus width. This is
/// the ASTH row model — text scales with the frame HEIGHT, the slider absorbs
/// extra WIDTH. Call after `apply_widget_scale`, at most once per row.
pub(crate) fn pin_flex_width(ui: &egui::Ui, container: egui::Vec2, min_w: f32) -> f32 {
    let scale: f32 = ui.ctx().data(|d| d.get_temp(pin_ws_applied_scratch())).unwrap_or(1.0);
    let nat: Option<egui::Vec2> = ui.ctx().data(|d| d.get_temp(pin_ws_resolved_scratch()));
    let surplus = match nat {
        // `nat.x` is the row width with the flexible element at `min_w`.
        Some(n) => (container.x - n.x * scale - 2.0).max(0.0),
        None => 0.0,
    };
    ui.ctx().data_mut(|d| d.insert_temp(pin_ws_flex_scratch(), surplus));
    min_w * scale + surplus
}

pub(crate) fn apply_widget_scale(ui: &mut egui::Ui, container: egui::Vec2, natural: egui::Vec2) -> f32 {
    // Text/controls scale with the container HEIGHT, capped by what the WIDTH
    // can hold, so a pinned row grows with its frame but never crops out of
    // it: when the frame is too narrow for the height-scaled text (plus the
    // minimum width of any flexible element), the width cap wins and the text
    // shrinks to keep the whole row inside the frame.
    // `natural` is only a first-frame estimate: once the row has rendered
    // once, `render_pinned_element` caches the measured content size (at
    // minimum flexible width) and that replaces the estimate — so a
    // snugly-framed row sits at ~1.0 and grows in lockstep with the frame.
    let key: Option<egui::Id> = ui.ctx().data(|d| d.get_temp(pin_ws_key_scratch()));
    let natural = key
        .and_then(|k| ui.ctx().data(|d| d.get_temp::<egui::Vec2>(k)))
        .unwrap_or(natural);
    let mut scale = (container.y / natural.y.max(1.0))
        .min(container.x / natural.x.max(1.0))
        .clamp(0.5, 4.0);
    if (scale - 1.0).abs() < 0.02 { scale = 1.0; }
    if key.is_some() {
        ui.ctx().data_mut(|d| {
            d.insert_temp(pin_ws_applied_scratch(), scale);
            d.insert_temp(pin_ws_resolved_scratch(), natural);
        });
    }
    if scale == 1.0 { return 1.0; }

    // Scale all named text styles uniformly so labels, buttons, and DragValues
    // all grow together. Egui clones the style on edit, so this only affects
    // the current sub-Ui (the allocate_ui_at_rect closure), not the parent.
    let style = ui.style_mut();
    for (_, font_id) in style.text_styles.iter_mut() {
        font_id.size = (font_id.size * scale).max(6.0);
    }
    let sp = &mut style.spacing;
    sp.button_padding *= scale;
    sp.item_spacing   *= scale;
    sp.interact_size.y = (sp.interact_size.y * scale).max(12.0);
    sp.icon_width      = (sp.icon_width * scale).max(8.0);
    sp.icon_width_inner = (sp.icon_width_inner * scale).max(6.0);
    sp.slider_width    *= scale;
    sp.combo_width     *= scale;
    scale
}
