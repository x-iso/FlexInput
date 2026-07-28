//! Audio Stream Haptics node body: params, slider rows, pinned renderers,
//! EF scope / spectrum EQ draws, capture status.

use super::*;

/// Body for the Audio Stream Haptics node: capture-target mode (process / focused
/// / system), a process picker (process mode), a status row, and the standard-
/// Audio Stream Haptics calibration parameters, snapshotted from node params each
/// frame. Shared with the scope/spectrum draw fns so they can preview the same
/// shaping the engine applies (Volume → Curve → amp range).
pub(crate) struct AsthParams {
    mode: String,
    target_name: String,
    include_tree: bool,
    modulator: f32,
    volume: f32,
    freq_bias: f32,
    curve: f32,
    amp_min: f32,
    amp_max: f32,
    release: f32,
    crossover: f32,
    /// Swap which band is the carrier vs the modulator (default: LF carrier, HF mod).
    swap: bool,
}

impl AsthParams {
    /// Apply the engine's amplitude shaping (Curve exponent → range remap) to a
    /// loudness value, so the EF scope shows the *actual* haptic output. Volume is
    /// NOT applied here — it's input gain in the capture thread, so the loudness this
    /// receives is already post-Volume. Mirrors `audio_stream_haptics_publish`.
    fn shape_amp(&self, loudness: f32) -> f32 {
        let curve = self.curve.clamp(0.3, 3.0);
        let shaped = loudness.clamp(0.0, 1.0).powf(curve);
        if shaped <= 0.0 { return 0.0; }
        let lo = self.amp_min.min(self.amp_max);
        let hi = self.amp_min.max(self.amp_max);
        (lo + shaped * (hi - lo)).clamp(0.0, 1.0)
    }

    /// Re-weight a raw `(lf_energy, hf_energy)` split by the band-balance control,
    /// returning balanced fractions that sum to ≤1 — mirrors the engine. `freq_bias`
    /// is the balance: -1 = all LF, +1 = all HF, 0 = natural.
    fn balance_fracs(&self, lf_e: f32, hf_e: f32) -> (f32, f32) {
        let b = self.freq_bias.clamp(-1.0, 1.0);
        let lf = lf_e * (1.0 - b).clamp(0.0, 2.0);
        let hf = hf_e * (1.0 + b).clamp(0.0, 2.0);
        let t = lf + hf;
        if t > 1.0e-6 { (lf / t, hf / t) } else { (0.0, 0.0) }
    }
}

/// One calibration row in the ASTH node: a fixed-width label cell followed by a
/// control that fills the remaining row width (so sliders scale with the module
/// width and stay left-aligned regardless of label length). `add` receives the
/// width to fill and returns the control's `Response`; OR its `.changed()` into
/// `changed`.
pub(crate) fn asth_slider_row(
    ui: &mut egui::Ui,
    label: &str,
    label_w: f32,
    body_w: f32,
    scale: f32,
    changed: &mut bool,
    add: impl FnOnce(&mut egui::Ui, f32) -> egui::Response,
) -> egui::Rect {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        slider_label(ui, label, label_w);
        // Rail extends to the SHARED right edge so every row's control lines up.
        let w = (asth_row_right_edge(body_w, scale) - label_w).max(40.0 * scale);
        ui.spacing_mut().slider_width = w;
        *changed |= add(ui, w).changed();
    }).response.rect
}

/// The uniform width of the editable value box on the Volume/Release/Crossover rows.
pub(crate) const ASTH_VAL_BOX_W: f32 = 50.0;
/// Width of the Amplitude row's response-curve box (matches `header_controls::curve_box`).
pub(crate) const ASTH_CURVE_BOX_W: f32 = 34.0;
/// Right slack inside `body_w` — the SHARED right edge of every row sits here.
pub(crate) const ASTH_RIGHT_INSET: f32 = 6.0;
/// Gap between the Amplitude range slider and the curve box.
pub(crate) const ASTH_BOX_GAP: f32 = 4.0;

/// The single shared RIGHT EDGE that every row's right-most element aligns to: the
/// value boxes (Volume/Release/Crossover), the curve box (Amplitude), and the plain
/// sliders (Balance/Rumble mix) all end here. `scale` keeps the inset proportional on
/// an enlarged pinned row. (The Amplitude *range slider* stops short of this to leave
/// room for the curve box, which then reaches the edge — see asth_amp_slider_right.)
pub(crate) fn asth_row_right_edge(body_w: f32, scale: f32) -> f32 {
    body_w - ASTH_RIGHT_INSET * scale
}

/// Right edge of the Amplitude row's RANGE SLIDER: stops `curve_w + gap` short of the
/// shared edge so the curve box occupies that trailing slot and reaches the edge.
pub(crate) fn asth_amp_slider_right(body_w: f32, scale: f32) -> f32 {
    asth_row_right_edge(body_w, scale) - (ASTH_CURVE_BOX_W + ASTH_BOX_GAP) * scale
}

/// A calibration row: left label, a flexible slider rail, and a fixed-width EDITABLE
/// value box whose right edge aligns to `asth_value_right_edge(body_w)`. Pixel-precise
/// (item spacing zeroed, explicit `add_space`) so all rows line up exactly. `edit`
/// renders the DragValue (the editable readout) into the fixed box and reports change.
/// `scale` scales the fixed cells so they grow with the text on a pinned, enlarged row.
pub(crate) fn asth_value_row(
    ui: &mut egui::Ui,
    label: &str,
    label_w: f32,
    body_w: f32,
    scale: f32,
    changed: &mut bool,
    // Draws the slider rail (given its width) then the editable value box (given its
    // width), returning whether either changed. A single closure so it can hold one
    // `&mut` to the row's value without the borrow checker seeing two simultaneously.
    draw: impl FnOnce(&mut egui::Ui, f32, f32) -> bool,
) -> egui::Rect {
    let val_box_w = ASTH_VAL_BOX_W * scale;
    let gap = ASTH_BOX_GAP * scale;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        slider_label(ui, label, label_w);
        // Value box right edge = the shared row right edge; rail fills the space
        // between the label and the box.
        let right_edge = asth_row_right_edge(body_w, scale);
        let rail_w = (right_edge - val_box_w - gap - label_w).max(40.0 * scale);
        ui.spacing_mut().slider_width = rail_w;
        *changed |= draw(ui, rail_w, val_box_w);
    }).response.rect
}

/// Inside an `asth_value_row` draw closure: render the slider then the fixed-width
/// right-aligned editable value box for a single `value`, both editing it. Returns
/// whether `value` changed. `gap` is the (scaled) space before the box.
pub(crate) fn asth_slider_and_box(
    ui: &mut egui::Ui,
    value: &mut f32,
    box_w: f32,
    gap: f32,
    slider: impl FnOnce(&mut egui::Ui, &mut f32) -> egui::Response,
    drag: impl FnOnce(&mut f32) -> egui::DragValue<'_>,
) -> bool {
    let mut changed = slider(ui, value).changed();
    ui.add_space(gap);
    let row_h = ui.spacing().interact_size.y.max(14.0);
    ui.allocate_ui_with_layout(
        egui::vec2(box_w, row_h),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| { changed |= ui.add(drag(value)).changed(); },
    );
    changed
}

/// One pinnable/calibration row of the Audio Stream Haptics body. Each variant maps
/// to a stable `element_id` (so it can be pinned to a sub-patch body individually)
/// and is rendered by the SAME code inline (in the node body) and when pinned.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum AsthRow { Mode, Volume, Release, Crossover, Amplitude, Balance, Swap, RumbleMix }

impl AsthRow {
    /// Stable element id used for pinning + the pinned-element dispatch.
    pub(crate) fn element_id(self) -> &'static str {
        match self {
            AsthRow::Mode      => "asth_mode_row",
            AsthRow::Volume    => "asth_volume",
            AsthRow::Release   => "asth_release",
            AsthRow::Crossover => "asth_crossover",
            AsthRow::Amplitude => "asth_amplitude",
            AsthRow::Balance   => "asth_balance",
            AsthRow::Swap      => "asth_swap_row",
            AsthRow::RumbleMix => "asth_rumble_mix",
        }
    }
    /// Calibration rows drawn by the shared `asth_draw_row` slider renderer. The
    /// capture-mode block (`Mode`) is drawn separately by `asth_draw_mode_block` and
    /// is therefore NOT in this list — but it is still a recognized pinnable element.
    const ALL: [AsthRow; 7] = [
        AsthRow::Volume, AsthRow::Release, AsthRow::Crossover, AsthRow::Amplitude,
        AsthRow::Balance, AsthRow::Swap, AsthRow::RumbleMix,
    ];
    /// Recognize any pinnable ASTH element id (the calibration rows plus the mode block).
    pub(crate) fn from_element_id(id: &str) -> Option<AsthRow> {
        if id == AsthRow::Mode.element_id() { return Some(AsthRow::Mode); }
        AsthRow::ALL.into_iter().find(|r| r.element_id() == id)
    }
}

/// Draw ONE calibration row into the current `ui`, mutating `a`. Returns
/// `(changed, row_rect)`. `body_w` is the row's content width (slider scales with it).
/// Shared by the node body (inline) and the pinned-element renderer.
pub(crate) fn asth_draw_row(
    ui: &mut egui::Ui,
    row: AsthRow,
    a: &mut AsthParams,
    body_w: f32,
    scale: f32,
) -> (bool, egui::Rect) {
    let label_w = 64.0 * scale;
    let mut changed = false;
    let rect = match row {
        // The mode block is drawn by `asth_draw_mode_block`, not as a slider row;
        // delegate here so this match stays exhaustive and any stray dispatch is safe.
        AsthRow::Mode => {
            let (ch, rect) = asth_draw_mode_block(ui, a, body_w, ui.id().with("asth_mode_fallback"));
            changed |= ch;
            rect
        }
        AsthRow::Volume => {
            let mut v = a.volume;
            let rect = asth_value_row(ui, "Volume", label_w, body_w, scale, &mut changed,
                |ui, _rail, box_w| asth_slider_and_box(ui, &mut v, box_w, ASTH_BOX_GAP * scale,
                    |ui, val| ui.add(egui::Slider::new(val, 0.0..=2.0).show_value(false))
                        .on_hover_text("Input gain applied before detection — lower it to tame a hot/clipping source."),
                    |val| egui::DragValue::new(val).speed(0.01).range(0.0..=2.0).max_decimals(2)),
            );
            if (v - a.volume).abs() > f32::EPSILON { a.volume = v; changed = true; }
            rect
        }
        AsthRow::Release => {
            let mut ms = a.release;
            let rect = asth_value_row(ui, "Release", label_w, body_w, scale, &mut changed,
                |ui, _rail, box_w| asth_slider_and_box(ui, &mut ms, box_w, ASTH_BOX_GAP * scale,
                    |ui, val| ui.add(egui::Slider::new(val, 1.0..=500.0).smart_aim(false).show_value(false))
                        .on_hover_text("How quickly rumble fades after the audio stops."),
                    |val| egui::DragValue::new(val).speed(1.0).range(1.0..=500.0).max_decimals(0).suffix(" ms")),
            );
            if (ms - a.release).abs() > f32::EPSILON { a.release = ms.round(); changed = true; }
            rect
        }
        AsthRow::Crossover => {
            let mut hz = a.crossover;
            let rect = asth_value_row(ui, "Crossover", label_w, body_w, scale, &mut changed,
                |ui, _rail, box_w| asth_slider_and_box(ui, &mut hz, box_w, ASTH_BOX_GAP * scale,
                    |ui, val| ui.add(egui::Slider::new(val, 60.0..=800.0).logarithmic(true).show_value(false))
                        .on_hover_text("Split between the low (LF) and high (HF) carriers; the LRA plays both at once."),
                    |val| egui::DragValue::new(val).speed(1.0).range(60.0..=800.0).max_decimals(0).suffix(" Hz")),
            );
            if (hz - a.crossover).abs() > f32::EPSILON { a.crossover = hz; changed = true; }
            rect
        }
        AsthRow::Amplitude => ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            slider_label(ui, "Amplitude", label_w);
            // Range slider stops short of the shared edge to leave room for the curve
            // box, which fills the trailing slot and reaches the shared edge — so the
            // curve box's right edge lines up with the value boxes above.
            let sw = (asth_amp_slider_right(body_w, scale) - label_w).clamp(40.0 * scale, 800.0 * scale);
            let mut lo = a.amp_min; let mut hi = a.amp_max;
            let r = crate::canvas::header_controls::range_slider(ui, &mut lo, &mut hi, 0.0, 1.0, sw)
                .on_hover_text("Floor (lift weak audio off the dead zone) … ceiling (cap). Curve box reshapes the response.");
            if r.changed() || (lo - a.amp_min).abs() > f32::EPSILON || (hi - a.amp_max).abs() > f32::EPSILON {
                a.amp_min = lo; a.amp_max = hi; changed = true;
            }
            ui.add_space(ASTH_BOX_GAP * scale);
            let mut exp = a.curve;
            crate::canvas::header_controls::curve_box(ui, &mut exp, 1.0);
            if (exp - a.curve).abs() > f32::EPSILON { a.curve = exp; changed = true; }
        }).response.rect,
        AsthRow::Balance => asth_slider_row(ui, "Balance", label_w, body_w, scale, &mut changed, |ui, _w| {
            ui.add(egui::Slider::new(&mut a.freq_bias, -1.0..=1.0).show_value(false))
                .on_hover_text("◄ smooth low rumble · high-frequency texture ►. \
                                The modulator band is rendered as a rapid amplitude \
                                flutter on the felt carrier (the Switch LRA can't play a separate tone).")
        }),
        AsthRow::Swap => ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(4.0 * scale);
            ui.allocate_ui_with_layout(
                egui::vec2(label_w, 18.0 * scale),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| { ui.label(egui::RichText::new("Swap").size(11.0 * scale)); },
            );
            let r = ui.checkbox(&mut a.swap, "HF carrier / LF texture")
                .on_hover_text("Default: LF is the felt carrier, HF flutters it. \
                                Swap to make HF the carrier and LF the modulator.");
            if r.changed() { changed = true; }
        }).response.rect,
        AsthRow::RumbleMix => asth_slider_row(ui, "Rumble mix", label_w, body_w, scale, &mut changed, |ui, _w| {
            ui.add(egui::Slider::new(&mut a.modulator, 0.0..=1.0).show_value(false))
                .on_hover_text("◄ gate (audio only when the game rumbles) · replace (pure audio) ►")
        }),
    };
    (changed, rect)
}

/// Scale the current Ui's text/interact metrics by the pinned container HEIGHT (so
/// dragging a pinned row taller grows the text + control), independent of width
/// (which the row's own `body_w` uses to scale the slider). `natural_h` is the row's
/// authored height. Returns the applied scale so the caller can scale the row's fixed
/// cell widths (label / value box) by the SAME factor — otherwise the grown text
/// overflows those fixed cells (the overlap/crop the user saw).
/// Analytic minimum row width at scale 1.0: fixed cells (label / value box /
/// curve box / inset) plus the flexible rail collapsed to its 40px minimum.
/// Used to cap the height-driven scale by the available WIDTH — without it a
/// short-and-narrow pin grows its text until nothing is left for the slider.
pub(crate) fn asth_row_min_w(row: AsthRow) -> f32 {
    match row {
        // Capture-mode block: App/Focused/System selector + caption.
        AsthRow::Mode => 240.0,
        // label 64 + rail 40 + gap 4 + value box 50 + inset 6.
        AsthRow::Volume | AsthRow::Release | AsthRow::Crossover => 164.0,
        // label 64 + rail 40 + gap 4 + curve box 34 + inset 6.
        AsthRow::Amplitude => 148.0,
        // label 64 + rail 40 + inset 6.
        AsthRow::Balance | AsthRow::RumbleMix => 110.0,
        // label cell + "HF carrier / LF texture" checkbox.
        AsthRow::Swap => 230.0,
    }
}

/// Publish an ASTH pin's analytic natural size into the shared per-pin cache
/// (same one the measured row widgets use), so the layout resize handle can
/// constrain the frame to the no-crop envelope via `clamp_pin_frame_to_content`.
pub(crate) fn asth_seed_pin_natural(ui: &egui::Ui, natural: egui::Vec2) {
    if let Some(k) = ui.ctx().data(|d| d.get_temp::<egui::Id>(pin_ws_key_scratch())) {
        ui.ctx().data_mut(|d| d.insert_temp(k, natural));
    }
}

pub(crate) fn apply_asth_row_height_scale(ui: &mut egui::Ui, container_h: f32, natural_h: f32, max_scale: f32) -> f32 {
    let scale = (container_h / natural_h.max(1.0)).min(max_scale).clamp(0.5, 4.0);
    if (scale - 1.0).abs() < 0.02 { return 1.0; }
    let style = ui.style_mut();
    for (_, font_id) in style.text_styles.iter_mut() {
        font_id.size = (font_id.size * scale).max(7.0);
    }
    let sp = &mut style.spacing;
    sp.button_padding *= scale;
    sp.item_spacing *= scale;
    sp.interact_size.y = (sp.interact_size.y * scale).max(14.0);
    sp.slider_rail_height = (sp.slider_rail_height * scale).max(4.0);
    sp.icon_width = (sp.icon_width * scale).max(10.0);
    sp.icon_width_inner = (sp.icon_width_inner * scale).max(7.0);
    scale
}

/// Load the calibration params for an ASTH node (shared by the body + pinned rows).
pub(crate) fn asth_params_from_node(snarl: &Snarl<NodeData>, node_id: NodeId) -> AsthParams {
    snarl.get_node(node_id).map(|n| {
        let p = &n.params;
        AsthParams {
            mode: p.get("asth_mode").and_then(|v| v.as_str()).unwrap_or("system").to_string(),
            target_name: p.get("asth_target_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            include_tree: p.get("asth_include_tree").and_then(|v| v.as_bool()).unwrap_or(true),
            modulator: p.get("asth_modulator").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
            volume: p.get("asth_volume").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
            freq_bias: p.get("asth_freq_bias").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
            curve: p.get("asth_curve").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
            amp_min: p.get("asth_amp_min").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
            amp_max: p.get("asth_amp_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
            release: p.get("asth_release").and_then(|v| v.as_f64()).unwrap_or(30.0) as f32,
            crossover: p.get("asth_crossover").and_then(|v| v.as_f64()).unwrap_or(250.0) as f32,
            swap: p.get("asth_swap").and_then(|v| v.as_bool()).unwrap_or(false),
        }
    }).unwrap_or(AsthParams {
        mode: "system".into(), target_name: String::new(), include_tree: true,
        modulator: 1.0, volume: 1.0, freq_bias: 0.0, curve: 1.0,
        amp_min: 0.0, amp_max: 1.0, release: 30.0, crossover: 250.0, swap: false,
    })
}

/// Persist the subset of AsthParams that the calibration ROWS edit (not mode/target,
/// which only the body's top section sets). Used by the pinned-row renderer.
pub(crate) fn asth_write_params(snarl: &mut Snarl<NodeData>, node_id: NodeId, a: &AsthParams) {
    if let Some(n) = snarl.get_node_mut(node_id) {
        n.params.insert("asth_modulator".into(), serde_json::Value::from(a.modulator as f64));
        n.params.insert("asth_volume".into(), serde_json::Value::from(a.volume as f64));
        n.params.insert("asth_freq_bias".into(), serde_json::Value::from(a.freq_bias as f64));
        n.params.insert("asth_curve".into(), serde_json::Value::from(a.curve as f64));
        n.params.insert("asth_amp_min".into(), serde_json::Value::from(a.amp_min as f64));
        n.params.insert("asth_amp_max".into(), serde_json::Value::from(a.amp_max as f64));
        n.params.insert("asth_release".into(), serde_json::Value::from(a.release as f64));
        n.params.insert("asth_crossover".into(), serde_json::Value::from(a.crossover as f64));
        n.params.insert("asth_swap".into(), serde_json::Value::Bool(a.swap));
    }
}

/// Draw the capture-mode block (App/Focused/System selector + process picker +
/// include-tree checkbox + live status line), mutating `a`. Returns `(changed, rect)`.
/// `salt` namespaces the process ComboBox so the body and a pinned copy don't collide.
/// Shared by the node body (inline) and the pinned-element renderer so the mode block
/// is pinnable like the calibration rows.
pub(crate) fn asth_draw_mode_block(
    ui: &mut egui::Ui,
    a: &mut AsthParams,
    body_w: f32,
    salt: egui::Id,
) -> (bool, egui::Rect) {
    let mut changed = false;
    let resp = ui.vertical(|ui| {
        ui.set_max_width(body_w);
        ui.label("Capture from:");
        ui.horizontal(|ui| {
            changed |= ui.selectable_value(&mut a.mode, "process".to_string(), "App").changed();
            changed |= ui.selectable_value(&mut a.mode, "focused".to_string(), "Focused").changed();
            changed |= ui.selectable_value(&mut a.mode, "system".to_string(), "System").changed();
        });
        if a.mode == "process" {
            let label = if a.target_name.is_empty() { "Pick app…".to_string() } else { a.target_name.clone() };
            egui::ComboBox::from_id_salt(("asth_proc", salt))
                .selected_text(label)
                .width((body_w - 4.0).clamp(120.0, 400.0))
                .show_ui(ui, |ui| {
                    for (exe, title) in crate::process_list::enumerate_windows() {
                        let item = if title.is_empty() {
                            exe.clone()
                        } else {
                            format!("{title}\n{exe}")
                        };
                        if ui.selectable_label(a.target_name.eq_ignore_ascii_case(&exe), item).clicked() {
                            a.target_name = exe.clone();
                            changed = true;
                        }
                    }
                });
            changed |= ui.checkbox(&mut a.include_tree, "Include child processes").changed();
        }
        if a.mode == "focused" {
            changed |= ui.checkbox(&mut a.include_tree, "Include child processes").changed();
        }
        let status = current_capture_status(&a.mode, &a.target_name);
        ui.add_space(2.0);
        ui.label(egui::RichText::new(status).small().weak());
    });
    (changed, resp.response.rect)
}

/// Render the ASTH capture-mode block pinned to a sub-patch body, sized to `container`.
pub(crate) fn render_asth_pinned_mode(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let mut a = asth_params_from_node(snarl, inner_id);
    ui.set_max_width(container.x);
    let min_w = asth_row_min_w(AsthRow::Mode);
    asth_seed_pin_natural(ui, egui::vec2(min_w, 96.0));
    let scale = apply_asth_row_height_scale(ui, container.y.max(20.0), 96.0,
        container.x / min_w);
    let _ = scale; // height-scale applies to text/spacing via the style mutation above
    let body_w = container.x.clamp(120.0, 1200.0);
    let (changed, _rect) = asth_draw_mode_block(ui, &mut a, body_w, ui.id().with(inner_id));
    if changed {
        if let Some(n) = snarl.get_node_mut(inner_id) {
            n.params.insert("asth_mode".into(), serde_json::Value::String(a.mode));
            n.params.insert("asth_target_name".into(), serde_json::Value::String(a.target_name));
            n.params.insert("asth_include_tree".into(), serde_json::Value::Bool(a.include_tree));
        }
    }
}

/// Render a single ASTH row pinned to a sub-patch body, sized to `container`:
/// WIDTH scales the slider rail, HEIGHT scales the text + control size.
pub(crate) fn render_asth_pinned_row(
    inner_id: NodeId,
    element_id: &str,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let Some(row) = AsthRow::from_element_id(element_id) else { return; };
    let mut a = asth_params_from_node(snarl, inner_id);
    ui.set_max_width(container.x);
    // Height drives text/control scale; width drives the slider (via body_w
    // below). The returned scale also grows the row's fixed cells (label /
    // value box) so the enlarged text doesn't overflow them, and is capped by
    // the width so those cells never squeeze the rail below its minimum.
    let min_w = asth_row_min_w(row);
    asth_seed_pin_natural(ui, egui::vec2(min_w, 22.0));
    let scale = apply_asth_row_height_scale(ui, container.y.max(20.0), 22.0,
        container.x / min_w);
    let body_w = container.x.clamp(40.0, 1200.0);
    let (changed, _rect) = asth_draw_row(ui, row, &mut a, body_w, scale);
    if changed { asth_write_params(snarl, inner_id, &a); }
}

/// Render the ASTH scope (EF oscilloscope + spectrum/EQ) pinned to a sub-patch body,
/// filling `container`. The EQ remains editable; edits persist back to the node.
/// `bridged_parent` is the sub-patch chain to this inner node, used to resolve the
/// namespaced capture uid so the live data actually shows (not "no signal").
pub(crate) fn render_asth_pinned_scope(
    inner_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    bridged_parent: Option<&AutomapGlowParent<'_>>,
) {
    let uid = effective_publish_uid(inner_id, bridged_parent);
    let a = asth_params_from_node(snarl, inner_id);
    let mut eq = snarl.get_node(inner_id)
        .and_then(|n| n.params.get("asth_eq_points").and_then(|v| v.as_array()).map(|arr| {
            arr.iter().filter_map(|pt| {
                let p = pt.as_array()?;
                Some([p.get(0)?.as_f64()? as f32, p.get(1)?.as_f64()? as f32])
            }).collect::<Vec<_>>()
        }))
        .filter(|v| v.len() >= 2)
        .unwrap_or_else(|| vec![[0.0, 0.5], [1.0, 0.5]]);

    ui.set_max_width(container.x);
    let total_h = container.y.max(80.0);
    // EXPLICIT sizes for both halves — in a pinned region the Ui is height-unconstrained
    // so `available_height()` is unreliable (the spectrum vanished). Pass the sizes in
    // directly: EF ~40% on top, spectrum/EQ the remainder. `ui.vertical` stacks them.
    let ef_h = (total_h * 0.4).clamp(28.0, total_h - 50.0);
    let spec_h = (total_h - ef_h - 3.0).max(40.0);
    let mut changed = false;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        draw_asth_ef_scope_sized(uid, ui, &a, egui::vec2(container.x, ef_h));
        changed = draw_asth_spectrum_eq_sized(inner_id, uid, ui, &mut eq, &a, egui::vec2(container.x, spec_h));
    });
    if changed {
        if let Some(n) = snarl.get_node_mut(inner_id) {
            let arr = serde_json::Value::Array(eq.iter()
                .map(|p| serde_json::json!([p[0], p[1]])).collect());
            n.params.insert("asth_eq_points".into(), arr);
        }
    }
}

/// rumble modulator slider. All persisted into `params` (`asth_*`) which the
/// engine reads (see `eval::loopback_request_from_params` + the eval block).
pub(crate) fn show_audio_stream_haptics_body(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) {
    // Effective uid for live capture data (raw at top level, namespaced in a
    // sub-patch) — must match what the capture manager + engine use.
    let uid = effective_publish_uid(node_id, automap_parent);
    // Snapshot params.
    let mut a = asth_params_from_node(snarl, node_id);

    // Band EQ control points (x = band position 0..1, y = gain 0..1). Default flat
    // at unity → single-carrier behavior until the user sculpts it.
    let mut asth_eq_points: Vec<[f32; 2]> = snarl.get_node(node_id)
        .and_then(|n| n.params.get("asth_eq_points").and_then(|v| v.as_array()).map(|arr| {
            arr.iter().filter_map(|pt| {
                let a = pt.as_array()?;
                Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
            }).collect::<Vec<_>>()
        }))
        .filter(|v| v.len() >= 2)
        .unwrap_or_else(|| vec![[0.0, 0.5], [1.0, 0.5]]);

    let mut changed = false;
    // Cell so the Resize closure (which borrows `ui`) can flag EQ edits without
    // also mutably borrowing `changed` across the closure boundary.
    let changed_inner = std::cell::Cell::new(false);

    // The scope's persisted width is the single source of truth for the whole
    // node's content width: pin the column to it (min == max) so every slider row
    // fills exactly that width and all of them grow/shrink together when the scope
    // is resized. (The scope's own resize handle is the only width control.)
    let scope_size = read_widget_size(snarl, node_id, "asth_scope_size", egui::vec2(240.0, 150.0));
    let body_w = scope_size.x.clamp(180.0, 900.0);

    ui.vertical(|ui| {
        ui.set_min_width(body_w);
        ui.set_max_width(body_w);

        // ── Capture mode ──────────────────────────────────────────────────────
        // Drawn by the SHARED `asth_draw_mode_block` (so it renders identically
        // inline and when pinned to a sub-patch body). Registered as a pinnable
        // element in Layout mode. Includes the App/Focused/System selector, the
        // process picker (App mode), the include-tree checkbox, and the live status.
        let (mode_ch, mode_rect) = asth_draw_mode_block(ui, &mut a, body_w, egui::Id::new(("asth_mode", node_id)));
        changed |= mode_ch;
        register_exposable_element(ui, node_id, AsthRow::Mode.element_id(), mode_rect);

        // ── Calibration rows. Each row is drawn by the SHARED `asth_draw_row` (so it
        //    renders identically inline and when pinned to a sub-patch body) and is
        //    registered as a pinnable element in Layout mode. Value boxes align to a
        //    shared right edge; the Amplitude curve box anchors the right. ──────────
        ui.add_space(4.0);
        for row in AsthRow::ALL {
            let (ch, rect) = asth_draw_row(ui, row, &mut a, body_w, 1.0);
            changed |= ch;
            register_exposable_element(ui, node_id, row.element_id(), rect);
        }

        // ── Combined 2-part scope: EF oscilloscope (top) + spectrum/EQ (bottom),
        //    one resizable handle for both. ─────────────────────────────────────
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Scope — drag EQ pts · dbl-click add · right-click remove").small().weak());
        let scope_top = ui.cursor().top();
        let scope_left = ui.min_rect().left();
        let new_sc = egui::Resize::default()
            .id_salt(("asth_scope_resize", node_id))
            .default_size(scope_size)
            .min_size(egui::vec2(180.0, 90.0))
            .max_size(egui::vec2(900.0, 600.0))
            .show(ui, |ui| {
                // Top ~40% = EF oscilloscope, bottom ~60% = spectrum/EQ.
                let total_h = ui.available_height().max(80.0);
                let ef_h = (total_h * 0.4).clamp(28.0, total_h - 50.0);
                ui.allocate_ui(egui::vec2(ui.available_width(), ef_h), |ui| {
                    draw_asth_ef_scope(uid, ui, &a);
                });
                ui.add_space(3.0);
                let ch = draw_asth_spectrum_eq(node_id, uid, ui, &mut asth_eq_points, &a);
                changed_inner.set(changed_inner.get() || ch);
                ui.min_rect().size()
            });
        if (new_sc - scope_size).length() > 0.5 { write_widget_size(snarl, node_id, "asth_scope_size", new_sc); }
        // The scope (EF + spectrum/EQ) is pinnable as one element too.
        let scope_rect = egui::Rect::from_min_size(egui::pos2(scope_left, scope_top), new_sc);
        register_exposable_element(ui, node_id, "asth_scope", scope_rect);
    });
    changed |= changed_inner.get();

    if changed {
        if let Some(n) = snarl.get_node_mut(node_id) {
            n.params.insert("asth_mode".into(), serde_json::Value::String(a.mode));
            n.params.insert("asth_target_name".into(), serde_json::Value::String(a.target_name));
            n.params.insert("asth_include_tree".into(), serde_json::Value::Bool(a.include_tree));
            n.params.insert("asth_modulator".into(), serde_json::Value::from(a.modulator as f64));
            n.params.insert("asth_volume".into(), serde_json::Value::from(a.volume as f64));
            n.params.insert("asth_freq_bias".into(), serde_json::Value::from(a.freq_bias as f64));
            n.params.insert("asth_curve".into(), serde_json::Value::from(a.curve as f64));
            n.params.insert("asth_amp_min".into(), serde_json::Value::from(a.amp_min as f64));
            n.params.insert("asth_amp_max".into(), serde_json::Value::from(a.amp_max as f64));
            n.params.insert("asth_release".into(), serde_json::Value::from(a.release as f64));
            n.params.insert("asth_crossover".into(), serde_json::Value::from(a.crossover as f64));
            n.params.insert("asth_swap".into(), serde_json::Value::Bool(a.swap));
            let eq = serde_json::Value::Array(asth_eq_points.iter()
                .map(|p| serde_json::json!([p[0], p[1]])).collect());
            n.params.insert("asth_eq_points".into(), eq);
        }
    }
}

pub(crate) fn draw_asth_ef_scope(uid: usize, ui: &mut egui::Ui, params: &AsthParams) {
    draw_asth_ef_scope_sized(uid, ui, params,
        egui::vec2(ui.available_width().max(120.0), ui.available_height().max(32.0)));
}

pub(crate) fn draw_asth_ef_scope_sized(uid: usize, ui: &mut egui::Ui, params: &AsthParams, size: egui::Vec2) {
    let size = egui::vec2(size.x.max(60.0), size.y.max(20.0));
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    painter.rect_filled(rect, 2.0, visuals.extreme_bg_color);

    // Baseline (0) at the bottom — these are unipolar 0..1 magnitudes.
    let base_y = rect.bottom() - 2.0;
    painter.line_segment(
        [egui::pos2(rect.left(), base_y), egui::pos2(rect.right(), base_y)],
        egui::Stroke::new(0.5, visuals.weak_text_color()),
    );

    #[cfg(windows)]
    let points = flexinput_devices::loopback_manager::latest_scope(uid);
    #[cfg(not(windows))]
    let points: Vec<()> = { let _ = (uid, params); Vec::new() };

    #[cfg(windows)]
    {
        let n = points.len();
        if n >= 2 {
            let h = rect.height() - 4.0;
            let x_at = |i: usize| rect.left() + (i as f32 / (n - 1) as f32) * rect.width();
            let y_at = |v: f32| base_y - v.clamp(0.0, 1.0) * h;

            // Faint raw audio peak trace.
            let audio: Vec<egui::Pos2> = points.iter().enumerate()
                .map(|(i, p)| egui::pos2(x_at(i), y_at(p.audio_l.max(p.audio_r)))).collect();
            for w in audio.windows(2) {
                painter.line_segment([w[0], w[1]], egui::Stroke::new(1.0, visuals.weak_text_color()));
            }
            // Two SHAPED haptic-carrier traces, colored to match the spectrum bars
            // (warm = LF carrier, cool = HF carrier). Each is the shaped envelope
            // scaled by that carrier's energy share, so you see how the audio splits
            // across the two carriers the LRA plays. Volume/Curve/range reshape both.
            let lf_col = egui::Color32::from_rgb(255, 180, 60);
            let hf_col = egui::Color32::from_rgb(80, 200, 230);
            // Apply the Balance control to the recorded raw LF/HF energy split, so
            // dragging Balance visibly redistributes amplitude between the traces.
            let lf: Vec<egui::Pos2> = points.iter().enumerate()
                .map(|(i, p)| {
                    let (frac, _) = params.balance_fracs(p.env_lf, p.env_hf);
                    egui::pos2(x_at(i), y_at(params.shape_amp(p.env_l.max(p.env_r)) * frac))
                }).collect();
            let hf: Vec<egui::Pos2> = points.iter().enumerate()
                .map(|(i, p)| {
                    let (_, frac) = params.balance_fracs(p.env_lf, p.env_hf);
                    egui::pos2(x_at(i), y_at(params.shape_amp(p.env_l.max(p.env_r)) * frac))
                }).collect();
            for w in lf.windows(2) { painter.line_segment([w[0], w[1]], egui::Stroke::new(1.5, lf_col)); }
            for w in hf.windows(2) { painter.line_segment([w[0], w[1]], egui::Stroke::new(1.5, hf_col)); }
        } else {
            painter.text(rect.center(), egui::Align2::CENTER_CENTER,
                "no signal", egui::FontId::proportional(11.0), visuals.weak_text_color());
        }
    }
    #[cfg(not(windows))]
    {
        let _ = points;
        painter.text(rect.center(), egui::Align2::CENTER_CENTER,
            "Windows only", egui::FontId::proportional(11.0), visuals.weak_text_color());
    }

    // Keep the scope live while the node body is on screen.
    ui.ctx().request_repaint();
}

/// Live spectrum + interactive band-EQ editor for the Audio Stream Haptics node.
///
/// Draws the log-band audio spectrum (~40 Hz – 1.25 kHz) as bars, overlays an
/// editable per-band gain curve (the "band EQ" — x = band position, y = gain),
/// and highlights the band the multi-band engine collapses to as the Switch Pro
/// carrier (the pad plays one carrier per side). The EQ gain is applied to the
/// drawn bars too, so you see exactly which part of the audio drives rumble.
///
/// Interaction (mirrors the response-curve editor): drag a point to reshape,
/// double-click to add a point, right-click a point to remove it. Mutates
/// `eq_points` in place and returns whether it changed.
pub(crate) fn draw_asth_spectrum_eq(node_id: NodeId, uid: usize, ui: &mut egui::Ui, eq_points: &mut Vec<[f32; 2]>, params: &AsthParams) -> bool {
    draw_asth_spectrum_eq_sized(node_id, uid, ui, eq_points, params,
        egui::vec2(ui.available_width().max(140.0), ui.available_height().max(50.0)))
}

pub(crate) fn draw_asth_spectrum_eq_sized(node_id: NodeId, uid: usize, ui: &mut egui::Ui, eq_points: &mut Vec<[f32; 2]>, params: &AsthParams, size: egui::Vec2) -> bool {
    let size = egui::vec2(size.x.max(80.0), size.y.max(40.0));
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    painter.rect_filled(rect, 2.0, visuals.extreme_bg_color);

    let x_of = |t: f32| rect.left() + t.clamp(0.0, 1.0) * rect.width();
    let y_of = |g: f32| rect.bottom() - g.clamp(0.0, 1.0) * rect.height();
    let t_of = |x: f32| ((x - rect.left()) / rect.width()).clamp(0.0, 1.0);
    let g_of = |y: f32| ((rect.bottom() - y) / rect.height()).clamp(0.0, 1.0);

    let mut changed = false;

    #[cfg(windows)]
    let bands = flexinput_devices::loopback_manager::latest_spectrum(uid);
    #[cfg(not(windows))]
    let bands: [f32; 0] = { let _ = uid; [] };

    // Crossover position (live preview of the Crossover slider): splits LF | HF.
    let xpos = crate::canvas::curve::crossover_hz_to_pos(params.crossover);

    // ── Spectrum bars (gained by the EQ so you see the post-EQ drive). LF bars
    //    tinted warm, HF bars cool. Brightness peak-holds then fades over the
    //    Release time, so a transient flashes bright then decays. ───────────────
    let n = bands.len();
    if n > 0 {
        let scaled: Vec<f32> = bands.iter().enumerate().map(|(i, &m)| {
            let pos = (i as f32 + 0.5) / n as f32;
            let gain = crate::canvas::curve::sample_curve(&eq_points, pos, &[]).clamp(0.0, 1.0);
            m.max(0.0).sqrt() * gain
        }).collect();
        let max = scaled.iter().cloned().fold(0.0f32, f32::max).max(0.02);
        let bar_w = rect.width() / n as f32;

        // Per-bar peak-hold brightness state, kept in egui memory keyed by node, and
        // decayed toward the live level by a factor derived from the Release time.
        let mem_id = egui::Id::new(("asth_bar_hold", uid));
        let mut holds: Vec<f32> = ui.ctx().memory_mut(|m| m.data.get_temp(mem_id).unwrap_or_default());
        if holds.len() != n { holds = vec![0.0; n]; }
        let dt = ui.input(|i| i.stable_dt).clamp(0.0, 0.1);
        // Release seconds → per-frame decay (one e-fold over the release time).
        let rel_s = (params.release / 1000.0).max(0.01);
        let decay = (-dt / rel_s).exp();

        let lf_base = egui::Color32::from_rgb(255, 180, 60);
        let hf_base = egui::Color32::from_rgb(80, 200, 230);
        for (i, &v) in scaled.iter().enumerate() {
            let pos = (i as f32 + 0.5) / n as f32;
            let level = (v / max).clamp(0.0, 1.0);
            // Peak-hold: jump up instantly, fall by the release decay.
            holds[i] = level.max(holds[i] * decay);
            let norm = level;
            let x0 = rect.left() + i as f32 * bar_w;
            let bar = egui::Rect::from_min_max(
                egui::pos2(x0 + 0.5, rect.bottom() - norm * rect.height()),
                egui::pos2(x0 + bar_w - 0.5, rect.bottom()),
            );
            // Brightness tracks the held peak: active bars glow, then fade out.
            let base = if pos < xpos { lf_base } else { hf_base };
            let bright = 0.30 + 0.70 * holds[i];
            painter.rect_filled(bar, 0.0, base.gamma_multiply(bright));
        }
        ui.ctx().memory_mut(|m| m.data.insert_temp(mem_id, holds));

        // ── Two carrier markers at the engine's per-band collapse positions.
        //    Each marker's WEIGHT reflects its balanced energy share, so the
        //    Balance control visibly emphasizes one carrier over the other. ─────
        let lf = crate::canvas::curve::multiband_collapse_band(&bands, &eq_points, 0.0, xpos);
        let hf = crate::canvas::curve::multiband_collapse_band(&bands, &eq_points, xpos, 1.0);
        let (lf_frac, hf_frac) = params.balance_fracs(
            lf.map(|(_, e)| e).unwrap_or(0.0), hf.map(|(_, e)| e).unwrap_or(0.0));
        if let Some((lf_c, _)) = lf {
            let cx = x_of(lf_c);
            painter.line_segment([egui::pos2(cx, rect.top()), egui::pos2(cx, rect.bottom())],
                egui::Stroke::new(1.0 + 2.5 * lf_frac, lf_base));
        }
        if let Some((hf_c, _)) = hf {
            let cx = x_of(hf_c);
            painter.line_segment([egui::pos2(cx, rect.top()), egui::pos2(cx, rect.bottom())],
                egui::Stroke::new(1.0 + 2.5 * hf_frac, hf_base));
        }
    }

    // ── Crossover divider line. ───────────────────────────────────────────────
    let xc = x_of(xpos);
    painter.line_segment([egui::pos2(xc, rect.top()), egui::pos2(xc, rect.bottom())],
        egui::Stroke::new(1.0, visuals.weak_text_color()));

    // ── EQ curve polyline. ────────────────────────────────────────────────────
    let curve_col = visuals.text_color();
    let poly: Vec<egui::Pos2> = (0..=40).map(|k| {
        let t = k as f32 / 40.0;
        egui::pos2(x_of(t), y_of(crate::canvas::curve::sample_curve(&eq_points, t, &[])))
    }).collect();
    for w in poly.windows(2) {
        painter.line_segment([w[0], w[1]], egui::Stroke::new(1.2, curve_col));
    }

    // ── Editable control points. ──────────────────────────────────────────────
    let ptr = resp.interact_pointer_pos().or_else(|| ui.input(|i| i.pointer.hover_pos()));
    let mut drag_idx: Option<usize> = None;
    if let Some(p) = ptr {
        if rect.contains(p) {
            // Nearest point within grab radius.
            let mut best = (f32::MAX, 0usize);
            for (i, pt) in eq_points.iter().enumerate() {
                let d = (egui::pos2(x_of(pt[0]), y_of(pt[1])) - p).length();
                if d < best.0 { best = (d, i); }
            }
            if best.0 < 10.0 { drag_idx = Some(best.1); }
        }
    }
    // Gamepad-nav: which dot the driver highlighted (and whether it's in dot-move
    // mode). Published under ("gp_nav_curve_sel", node) — the SAME channel the
    // response-curve bodies use, so the EQ reuses the whole curve-dot nav path.
    let nav_sel: Option<(u64, usize, bool)> = ui.ctx().data(|d|
        d.get_temp(egui::Id::new(("gp_nav_curve_sel", node_id.0))));
    let nav_sel = nav_sel.filter(|(pass, _, _)| crate::widgets::nav_pass_matches(ui.ctx(), *pass));
    for (i, pt) in eq_points.iter().enumerate() {
        let c = egui::pos2(x_of(pt[0]), y_of(pt[1]));
        let hot = drag_idx == Some(i);
        painter.circle_filled(c, if hot { 4.5 } else { 3.0 },
            if hot { visuals.selection.stroke.color } else { curve_col });
        if let Some((_, sel_i, editing_dot)) = nav_sel {
            if sel_i == i {
                let accent = visuals.selection.stroke.color;
                let [r8, g8, b8, _] = accent.to_array();
                for k in 0..5 {
                    let t = (k as f32 + 1.0) / 5.0;
                    let rr = (if editing_dot { 16.0 } else { 12.0 }) * t;
                    let a = ((if editing_dot { 170.0 } else { 120.0 }) * (1.0 - t)) as u8;
                    if a == 0 { continue; }
                    painter.circle_stroke(c, rr,
                        egui::Stroke::new(2.0, egui::Color32::from_rgba_unmultiplied(r8, g8, b8, a)));
                }
                painter.circle_filled(c, if editing_dot { 6.0 } else { 5.0 }, accent);
                painter.circle_stroke(c, if editing_dot { 6.0 } else { 5.0 },
                    egui::Stroke::new(1.5, egui::Color32::WHITE));
            }
        }
    }

    // Gamepad-nav: publish EQ graph geometry (rect + axis bounds, in GLOBAL screen
    // space) so the driver maps graph↔screen for dot stepping / cursor / moves.
    // Bounds are X 0..1 (band position) and Y 0..1 (EQ gain).
    {
        let pass = ui.ctx().cumulative_pass_nr();
        let to_global = ui.ctx().layer_transform_to_global(ui.layer_id())
            .unwrap_or(egui::emath::TSTransform::IDENTITY);
        let screen_rect = to_global * rect;
        ui.ctx().data_mut(|d| d.insert_temp(
            egui::Id::new(("gp_nav_curve_geom", node_id.0)),
            (pass, screen_rect, 0.0f32, 1.0f32, 0.0f32, 1.0f32)));
    }

    // Drag a point (endpoints keep their x fixed; middle points move freely).
    if resp.dragged() {
        if let (Some(i), Some(p)) = (drag_idx, resp.interact_pointer_pos()) {
            let is_end = i == 0 || i == eq_points.len() - 1;
            if !is_end { eq_points[i][0] = t_of(p.x); }
            eq_points[i][1] = g_of(p.y);
            // Keep points sorted by x (middle ones can cross).
            eq_points.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap_or(std::cmp::Ordering::Equal));
            changed = true;
        }
    }
    // Double-click to add a point at the pointer.
    if resp.double_clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            eq_points.push([t_of(p.x), g_of(p.y)]);
            eq_points.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap_or(std::cmp::Ordering::Equal));
            changed = true;
        }
    }
    // Right-click (secondary) on a point removes it (keep at least 2).
    if resp.secondary_clicked() {
        if let (Some(i), true) = (drag_idx, eq_points.len() > 2) {
            if i != 0 && i != eq_points.len() - 1 {
                eq_points.remove(i);
                changed = true;
            }
        }
    }

    ui.ctx().request_repaint();
    changed
}

/// Human-readable description of what the node is capturing right now, for the
/// node-body status row. Resolves focused/process names live (Windows only).
pub(crate) fn current_capture_status(mode: &str, target_name: &str) -> String {
    match mode {
        "system" => "Capturing: system audio mix".to_string(),
        "process" => {
            if target_name.is_empty() {
                "No app selected".to_string()
            } else {
                #[cfg(windows)]
                {
                    match flexinput_devices::loopback_haptic::process::pid_for_name(target_name) {
                        Some(pid) => format!("Capturing: {target_name} (pid {pid})"),
                        None => format!("Waiting for {target_name} to run…"),
                    }
                }
                #[cfg(not(windows))]
                { format!("Capturing: {target_name}") }
            }
        }
        "focused" => {
            #[cfg(windows)]
            {
                match flexinput_devices::loopback_haptic::process::foreground_pid()
                    .and_then(flexinput_devices::loopback_haptic::process::name_for_pid)
                {
                    Some(name) => format!("Capturing: focused app ({name})"),
                    None => "Capturing: focused app".to_string(),
                }
            }
            #[cfg(not(windows))]
            { "Capturing: focused app".to_string() }
        }
        _ => String::new(),
    }
}


