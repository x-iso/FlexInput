//! The second look a pinned row can wear: the module's own, as redesigned - value pills,
//! joined segments, switches drawn as switches - laid out across the pin's frame.
//!
//! A pinned element is drawn in the look it was made with unless it says otherwise
//! (`ExposedModule::pin_style`): a layout built before the redesign was built round the old
//! rows' proportions, and an overlay styled to sit over one game may suit either. So each row
//! that was redesigned has both, and the layout editor's inspector switches between them for
//! the pins that have a choice ([`supported`]). Elements that look the same either way - the
//! graphs, the whole-module pins - have no switch.

use super::*;

/// Whether `element` of `module_id` has a Style 2 look.
pub(crate) fn supported(module_id: &str, element: &str) -> bool {
    matches!(
        (module_id, element),
        (
            "module.response_curve" | "module.vec_response_curve" | "module.twoway_response_curve",
            "scale_row" | "range_row" | "grid_row"
        ) | ("display.oscilloscope" | "display.trigscope", "controls" | "grid_row")
            | ("processing.rws", "style" | "flick" | "flick_opts" | "flick_speed" | "suppress")
            | ("module.area_mapper", "options" | "layers" | "press" | "border")
            | ("module.menu", "options")
            | ("module.vec_reshape", "target_row" | "options_row" | "range_row" | "grid_row" | "preset_row")
    ) || from_fields(module_id, element)
}

/// The rows whose Style 2 look is drawn from their field table ([`render_fields`]): the
/// settings the gamepad walks, each as the control the module's body draws for it - a value
/// pill, a joined segment of choices, a word switch - in the gamepad's order. Their bodies
/// are pills already; pinned they were bare drag fields and checkboxes.
pub(crate) const FROM_FIELDS: &[(&str, &str)] = &[
    ("generator.envelope", "time_row"),
    ("generator.envelope", "mode_row"),
    ("generator.envelope", "sustain_row"),
    ("generator.envelope", "grid_row"),
    ("generator.envelope", "grid_options_row"),
    ("generator.oscillator", "freq"),
    ("generator.oscillator", "phase"),
    ("generator.oscillator", "shape"),
    ("logic.counter", "mode"),
    ("logic.counter", "range_mode"),
    ("logic.counter", "step"),
    ("logic.counter", "min_max"),
    ("logic.delay", "mode"),
    ("logic.delay", "time"),
    ("logic.has_changed", "hysteresis"),
    ("math.negate", "unipolar"),
    ("math.quantize", "factor"),
    ("math.quantize", "mode"),
    ("module.sample_hold", "mode"),
    ("module.vec_to_deflection", "angle_unit"),
    ("module.average", "samples"),
    ("module.average", "spike_mad"),
    ("module.dc_filter", "window_ms"),
    ("module.dc_filter", "decay_ms"),
    ("module.delay", "ms"),
    ("module.response_curve", "grid_options_row"),
    ("module.vec_response_curve", "grid_options_row"),
    ("module.twoway_response_curve", "grid_options_row"),
    ("module.twoway_response_curve", "hyst_row"),
    ("module.twoway_response_curve", "interp_row"),
    ("module.twoway_response_curve", "lane_toggle"),
    ("module.stick_rotation", "options"),
    ("module.stick_rotation", "takeover"),
    ("module.stick_rotation", "hold"),
    ("module.stick_rotation", "sens"),
    ("module.stick_rotation", "deadzone"),
    ("module.stick_rotation", "stabilise"),
    ("processing.gyro_3dof", "pointer_mode"),
    ("processing.gyro_3dof", "steering_mode"),
    ("processing.gyro_3dof", "steering_opts"),
    ("processing.gyro_3dof", "lean_threshold"),
    ("processing.gyro_3dof", "roll_mix"),
    ("processing.gyro_3dof", "hold_offset"),
    ("processing.gyro_3dof", "edge_turn"),
    ("processing.gyro_3dof", "orient_source"),
    ("processing.gyro_3dof", "gyro_invert"),
    ("processing.gyro_3dof", "accel_invert"),
    ("processing.rws", "scale"),
    ("processing.rws", "rws"),
    ("processing.rws", "stick_dps"),
    ("processing.rws", "gyro_scale"),
    ("processing.rws", "vh"),
];

fn from_fields(module_id: &str, element: &str) -> bool {
    FROM_FIELDS.iter().any(|&(m, e)| m == module_id && e == element)
}

/// The most controls a band of a field-table row holds: past it the row takes another band
/// (3DOF's Edge turn has nine), so the pin doesn't shrink to one long thin line.
const FIELDS_PER_BAND: usize = 4;

/// A word for a pill `cell` tall: as it is written (`Pitch+Yaw`) where the pill has the
/// height for its capitals, lower case where it is a thin strip and they would crowd it.
fn cased(word: &str, cell: egui::Rect) -> String {
    if cell.height() >= 20.0 {
        let mut c = word.chars();
        c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
    } else {
        word.to_lowercase()
    }
}

/// A choice's word: its value with the underscores out (`delay_true` → `delay true`).
fn choice_word(value: &str) -> String {
    value.replace('_', " ")
}

/// How many decimals a value stepped by `step` reads at, at `v` (display units).
fn decimals(step: crate::app::NavStep, v: f32, span: f32) -> usize {
    use crate::app::NavStep;
    let of = |s: f32| if s >= 1.0 { 0 } else { ((-s.log10()).ceil().max(0.0) as usize).min(4) };
    match step {
        NavStep::Fixed(s) | NavStep::FixedScaled { coarse: s, .. } => of(s),
        NavStep::Decade => {
            if v.abs() >= 100.0 { 0 } else if v.abs() >= 10.0 { 1 } else { 2 }
        }
        NavStep::Linear => {
            if span >= 100.0 { 0 } else if span >= 10.0 { 1 } else { 2 }
        }
    }
}

/// A Style 2 row drawn from its field table (`app::element_fields`): one control per field,
/// [`FIELDS_PER_BAND`] to a band, writing the params as the gamepad does - a display-scaled
/// value back in param units, the whole-unit ones as integers.
fn render_fields(inner_id: NodeId, module_id: &str, element: &str, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    use super::curve_rows::{pill_bands, pill_combo, real_pill, segmented, word_toggle, word_w};
    use crate::app::{NavField, NavStep};
    let Some(node) = snarl.get_node(inner_id) else { return };
    let fields = crate::app::element_fields(module_id, element, Some(node));
    if fields.is_empty() {
        return;
    }
    let param = |k: &str| node.params.get(k).cloned();
    let num = |k: &str, d: f32| param(k).and_then(|v| v.as_f64()).map_or(d, |v| v as f32);
    let text = |k: &str| param(k).and_then(|v| v.as_str().map(str::to_string));
    let flag = |k: &str| param(k).and_then(|v| v.as_bool()).unwrap_or(false);
    // What each field holds now, read before anything is drawn (drawing writes nothing; the
    // writes are gathered and made at the end).
    enum Now {
        Value(f32),
        Choice(String),
        Pair(usize),
        Flag(bool),
        Nothing,
    }
    let now: Vec<Now> = fields
        .iter()
        .map(|f| match &f.field {
            NavField::Value { key, default, .. } => Now::Value(num(key, *default)),
            NavField::Enum { key, opts } => Now::Choice(text(key).unwrap_or_else(|| opts.first().unwrap_or(&"").to_string())),
            NavField::EnumPair { key_a, key_b, opts } => {
                let (a, b) = (text(key_a), text(key_b));
                Now::Pair(opts.iter().position(|o| a.as_deref() == Some(o.1) && b.as_deref() == Some(o.2)).unwrap_or(usize::MAX))
            }
            NavField::Toggle { key } => Now::Flag(flag(key)),
            _ => Now::Nothing,
        })
        .collect();
    let w = ui.available_width();
    let mut rects: Vec<egui::Rect> = Vec::new();
    let mut set: Vec<(&'static str, serde_json::Value)> = Vec::new();
    for (band, chunk) in fields.chunks(FIELDS_PER_BAND).enumerate() {
        let first = band * FIELDS_PER_BAND;
        let mins: Vec<f32> = chunk
            .iter()
            .map(|f| match &f.field {
                NavField::Value { .. } => (word_w(ui, &f.label.to_lowercase()) + 56.0).max(100.0),
                NavField::Toggle { .. } => word_w(ui, &f.label.to_lowercase()),
                NavField::Enum { opts, .. } if opts.len() <= 4 => opts.iter().map(|o| word_w(ui, &choice_word(o))).sum(),
                NavField::Enum { .. } => 160.0,
                NavField::EnumPair { opts, .. } => opts.iter().map(|o| word_w(ui, &o.0.to_lowercase())).sum(),
                _ => 60.0,
            })
            .collect();
        let cells = pill_bands(ui, w, &mins);
        for (k, (f, cell)) in chunk.iter().zip(cells.iter().copied()).enumerate() {
            let i = first + k;
            let label = f.label.to_lowercase();
            let salt = ("ps2_field", inner_id.0, i);
            // What the field does, as its tooltip: the module's own words for it, where the
            // field table has them (`field_hint`). It was the label again - "interp ms".
            let key = match &f.field {
                NavField::Value { key, .. } | NavField::Enum { key, .. } | NavField::Toggle { key } => Some(*key),
                NavField::EnumPair { key_a, .. } => Some(*key_a),
                _ => None,
            };
            let hint: &str = key.and_then(|k| crate::app::field_hint(module_id, k)).unwrap_or(&f.label);
            match (&f.field, &now[i]) {
                (NavField::Value { key, lo, hi, step, .. }, Now::Value(v)) => {
                    let factor = match step {
                        NavStep::FixedScaled { factor, .. } => factor.max(f32::EPSILON),
                        _ => 1.0,
                    };
                    let (lo, hi) = (lo * factor, hi * factor);
                    let mut disp = v * factor;
                    let places = decimals(*step, disp, hi - lo);
                    let shown = format!("{disp:.places$}");
                    let per_px = match step {
                        NavStep::Fixed(s) | NavStep::FixedScaled { coarse: s, .. } => s * 0.2,
                        NavStep::Decade => 10f32.powf(disp.abs().max(1e-3).log10().floor()) * 0.02,
                        NavStep::Linear => (hi - lo).abs() / 300.0,
                    };
                    if real_pill(ui, inner_id, &format!("ps2_{element}_{i}"), cell, &label, &mut disp, &shown, lo..=hi, per_px, hint) {
                        let v = disp / factor;
                        let whole = matches!(*key, "grid_x" | "grid_y" | "trail_ms");
                        set.push((key, if whole { serde_json::json!(v.round() as i64) } else { serde_json::json!(v as f64) }));
                    }
                }
                (NavField::Enum { key, opts }, Now::Choice(cur)) if opts.len() <= 4 => {
                    let words: Vec<String> = opts.iter().map(|o| cased(&choice_word(o), cell)).collect();
                    let options: Vec<(&'static str, &str, &str)> = opts.iter().zip(&words).map(|(o, w)| (*o, w.as_str(), hint)).collect();
                    let cur: &str = cur;
                    if let Some(pick) = segmented(ui, cell, &cur, &options, salt) {
                        set.push((key, serde_json::json!(pick)));
                    }
                }
                (NavField::Enum { key, opts }, Now::Choice(cur)) => {
                    let words: Vec<String> = opts.iter().map(|o| choice_word(o)).collect();
                    let options: Vec<(String, &str)> = opts.iter().zip(&words).map(|(o, w)| (o.to_string(), w.as_str())).collect();
                    if let Some(pick) = pill_combo(ui, cell, &label, cur, &options, hint, salt) {
                        set.push((key, serde_json::json!(pick)));
                    }
                }
                (NavField::EnumPair { key_a, key_b, opts }, Now::Pair(cur)) => {
                    let words: Vec<String> = opts.iter().map(|o| cased(o.0, cell)).collect();
                    let options: Vec<(usize, &str, &str)> = words.iter().enumerate().map(|(j, w)| (j, w.as_str(), hint)).collect();
                    // A mode's drawing, where it has one (3DOF's Pointer and Steering rows).
                    let icons: Vec<Option<&'static [u8]>> = opts
                        .iter()
                        .map(|o| if module_id == "processing.gyro_3dof" { super::gyro_body::gyro_axis_icon(o.2) } else { None })
                        .collect();
                    if let Some(j) = super::curve_rows::segmented_icons(ui, cell, cur, &options, &icons, salt) {
                        set.push((key_a, serde_json::json!(opts[j].1)));
                        set.push((key_b, serde_json::json!(opts[j].2)));
                    }
                }
                (NavField::Toggle { key }, Now::Flag(on)) => {
                    let mut on = *on;
                    if word_toggle(ui, cell, &mut on, &label, hint, salt) {
                        set.push((key, serde_json::json!(on)));
                    }
                }
                _ => {}
            }
            rects.push(cell);
        }
    }
    publish_nav_field_rects(ui, inner_id, &rects);
    if !set.is_empty() {
        if let Some(node) = snarl.get_node_mut(inner_id) {
            for (k, v) in set {
                node.params.insert(k.to_string(), v);
            }
        }
        mark_overlay_param_write(ui.ctx());
    }
}

/// How wide one band of `element` is at 1×, before it has been drawn once and said so
/// itself (see [`fit`]).
fn first_guess(element: &str) -> f32 {
    match element {
        "controls" => 650.0,
        "grid_row" => 380.0,
        "range_row" => 320.0,
        "style" => 400.0,
        "flick" | "flick_opts" => 430.0,
        _ => 220.0,
    }
}

/// How many bands `element` has, before it has been drawn once and said so itself.
fn first_bands(element: &str) -> u32 {
    match element {
        "options" | "analog" => 2,
        "cell" => 3,
        _ => 1,
    }
}

/// Draw a Style 2 row inside `container` - exactly inside it, whatever its shape.
///
/// The row is one band at the largest scale that fits both ways: the frame's height over a
/// band's, and its width over the width one band of the row needs at 1×. That width comes
/// from the row's own constants (`curve_rows::with_zoom` collects it as the row lays itself
/// out), so it is the same at every scale and the scale is a function of the frame alone:
/// nothing drawn is measured and fed back. The band sits in the middle of a frame taller
/// than it, and the frame is allocated whole, so the pin never reports a size of its own.
pub(crate) fn fit(
    ui: &mut egui::Ui,
    inner_id: NodeId,
    element: &str,
    container: egui::Vec2,
    draw: impl FnOnce(&mut egui::Ui),
) {
    let container = egui::vec2(container.x.max(1.0), container.y.max(1.0));
    let key = egui::Id::new(("pin_style2_one_band", ui.ctx().viewport_id(), inner_id.0, element));
    let measured: Option<(f32, u32)> = ui.ctx().data(|d| d.get_temp(key));
    let (one, bands): (f32, u32) = measured.unwrap_or_else(|| (first_guess(element), first_bands(element)));
    // Only a row that is one band of `pill_bands` stretches (`bands` is 0 for a row that
    // lays itself out otherwise): a row of several, or one that mixes its own rows in,
    // came out taller than its frame.
    let stretchy = measured.is_some() && bands == 1;
    let (one, bands) = (one.max(1.0), bands.max(1));
    // An element of several bands (the Area Mapper's options, a menu's pointer and selection)
    // shares the frame's height among them.
    let base = super::curve_rows::band_h(ui).max(1.0) * bands as f32;
    let scale = (container.y / base).min(container.x / one).max(0.2).min(4.0);
    // Held to its scale by the frame's width, a row of bands takes the height left over too
    // (up to three times its own): taller pills, a mode's drawing over its word. A row that
    // lays itself out stays its own height, in the middle of the frame.
    let stretch = if stretchy { (container.y / (base * scale)).clamp(1.0, 3.0) } else { 1.0 };
    let (rect, _) = ui.allocate_exact_size(container, egui::Sense::hover());
    let h = (base * scale * stretch).min(rect.height());
    let row = egui::Rect::from_min_size(egui::pos2(rect.left(), rect.center().y - h * 0.5), egui::vec2(rect.width(), h));
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(row)
            .layout(egui::Layout::top_down(egui::Align::Min))
            .id_salt(("pin_style2", inner_id.0, element)),
    );
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    super::scale::scale_ui_style(&mut child, scale);
    child.spacing_mut().item_spacing.y = 0.0;
    let ((), now, now_bands) = super::curve_rows::with_stretch(stretch, || super::curve_rows::with_zoom(scale, || draw(&mut child)));
    if now > 0.0 && measured.is_none_or(|(o, b)| (now - o).abs() > 0.5 || now_bands != b) {
        ui.ctx().data_mut(|d| d.insert_temp(key, (now, now_bands)));
    }
}

/// Draw `element` in its Style 2 look into `container`, with no live input to hand (the
/// tests). Only called for [`supported`] ones.
#[cfg(test)]
pub(crate) fn render(
    inner_id: NodeId,
    module_id: &str,
    element: &str,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    render_live(inner_id, module_id, element, ui, snarl, container, &Default::default(), None);
}

/// Draw `element` in its Style 2 look into `container`. Only called for [`supported`] ones.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_live(
    inner_id: NodeId,
    module_id: &str,
    element: &str,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) {
    fit(ui, inner_id, element, container, |ui| draw(inner_id, module_id, element, ui, snarl, container, live_signals, automap_parent));
}

#[allow(clippy::too_many_arguments)]
fn draw(
    inner_id: NodeId,
    module_id: &str,
    element: &str,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) {
    if from_fields(module_id, element) {
        let _ = container;
        render_fields(inner_id, module_id, element, ui, snarl);
        return;
    }
    match module_id {
        "module.area_mapper" => {
            crate::canvas::area_body::render_area_row_style2(inner_id, element, ui, snarl, live_signals, automap_parent)
        }
        "module.menu" => crate::canvas::menu_body::render_menu_options_style2(inner_id, ui, snarl),
        "module.vec_reshape" => {
            let w = ui.available_width();
            super::reshape::vrs_pill_row(inner_id, ui, snarl, element, w);
        }
        "display.oscilloscope" | "display.trigscope" => super::waveform::render_scope_rows_style2(
            module_id == "display.trigscope",
            inner_id,
            ui,
            snarl,
            container,
            element == "controls",
        ),
        "processing.rws" => super::rws::render_rws_row_style2(inner_id, element, ui, snarl, container),
        _ => super::curve_rows::render_curve_row_style2(inner_id, module_id, element, ui, snarl, container),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A layout saved before the field existed has no `pin_style`, and reads as Style 1 - the
    // look it was laid out for. A new pin is made as Style 2 and says so in the file.
    #[test]
    fn an_old_pin_keeps_its_look_and_a_new_one_says_which_it_has() {
        let old = r#"{"inner_node_id":3,"element_id":"scale_row","pos":[0,0],"size":[200,24]}"#;
        let m: crate::canvas::node::ExposedModule = serde_json::from_str(old).expect("an old pin loads");
        assert_eq!(m.pin_style, None);
        assert_eq!(m.pin_style.unwrap_or(1), 1);
        let mut new = m.clone();
        new.pin_style = Some(2);
        let text = serde_json::to_string(&new).expect("it saves");
        assert!(text.contains("\"pin_style\":2"), "{text}");
        assert!(!serde_json::to_string(&m).unwrap().contains("pin_style"), "an unset style is left out of the file");
    }

    // Every Style 2 row draws, at a roomy frame and a cramped one, without panicking, and
    // says where its fields are - as many as the gamepad walks for that element - so the
    // config overlay's focus glow has something to land on.
    #[test]
    fn every_second_look_draws_and_says_where_its_fields_are() {
        let pairs = [
            ("module.response_curve", "scale_row", 3),
            ("module.response_curve", "range_row", 4),
            ("module.response_curve", "grid_row", 3),
            ("module.vec_response_curve", "scale_row", 2),
            ("module.vec_response_curve", "range_row", 2),
            ("module.twoway_response_curve", "range_row", 4),
            ("display.oscilloscope", "controls", 6),
            ("display.oscilloscope", "grid_row", 6),
            ("display.trigscope", "controls", 5),
            ("processing.rws", "style", 4),
            ("processing.rws", "flick", 4),
            ("processing.rws", "flick_opts", 3),
            ("processing.rws", "flick_speed", 2),
            ("processing.rws", "suppress", 1),
            ("module.area_mapper", "options", 5),
            ("module.area_mapper", "layers", 1),
            ("module.area_mapper", "press", 1),
            ("module.menu", "options", 9),
            ("module.vec_reshape", "target_row", 2),
            ("module.vec_reshape", "options_row", 2),
            ("module.vec_reshape", "range_row", 2),
            ("module.vec_reshape", "grid_row", 4),
        ];
        for (module, element, count) in pairs {
            assert!(supported(module, element), "{module}/{element}");
            let desc = flexinput_modules::all_modules()
                .into_iter()
                .map(|r| r.descriptor)
                .find(|d| d.id == module)
                .expect("registered");
            for container in [egui::vec2(420.0, 30.0), egui::vec2(90.0, 18.0)] {
                let mut snarl: Snarl<NodeData> = Snarl::new();
                let id = snarl.insert_node(egui::Pos2::ZERO, NodeData::from(&desc));
                let ctx = egui::Context::default();
                for _ in 0..3 {
                    let _ = ctx.run(egui::RawInput::default(), |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(("gp_nav_cur_element", id.0)), element.to_string()));
                            render(id, module, element, ui, &mut snarl, container);
                        });
                    });
                }
                let rects: Option<(u64, Vec<egui::Rect>)> =
                    ctx.data(|d| d.get_temp(egui::Id::new(("gp_nav_field_rects", id.0, element.to_string()))));
                let rects = rects.map(|(_, r)| r).unwrap_or_default();
                assert_eq!(rects.len(), count, "{module}/{element} at {container:?}: {rects:?}");
                assert!(rects.iter().any(|r| r.is_positive()), "{module}/{element}: no field drawn");
            }
        }
    }

    // Every row drawn from its field table publishes one rect per field the gamepad walks, all
    // inside the frame, at a roomy frame and a cramped one.
    #[test]
    fn every_field_table_row_draws_its_fields_inside_its_frame() {
        for &(module, element) in FROM_FIELDS {
            assert!(supported(module, element), "{module}/{element}");
            let Some(desc) = flexinput_modules::all_modules().into_iter().map(|r| r.descriptor).find(|d| d.id == module) else {
                panic!("{module} isn't a module");
            };
            for container in [egui::vec2(480.0, 40.0), egui::vec2(120.0, 30.0), egui::vec2(300.0, 120.0)] {
                let mut snarl: Snarl<NodeData> = Snarl::new();
                let id = snarl.insert_node(egui::Pos2::ZERO, NodeData::from(&desc));
                let want = crate::app::element_fields(module, element, snarl.get_node(id)).len();
                assert!(want > 0, "{module}/{element} has no fields");
                let ctx = egui::Context::default();
                let frame = egui::Rect::from_min_size(egui::pos2(20.0, 20.0), container);
                for _ in 0..4 {
                    let _ = ctx.run(egui::RawInput::default(), |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            let mut pin = ui.new_child(egui::UiBuilder::new().max_rect(frame));
                            pin.ctx().data_mut(|d| d.insert_temp(egui::Id::new(("gp_nav_cur_element", id.0)), element.to_string()));
                            render(id, module, element, &mut pin, &mut snarl, container);
                        });
                    });
                }
                let rects: Option<(u64, Vec<egui::Rect>)> =
                    ctx.data(|d| d.get_temp(egui::Id::new(("gp_nav_field_rects", id.0, element.to_string()))));
                let rects = rects.map(|(_, r)| r).unwrap_or_default();
                assert_eq!(rects.len(), want, "{module}/{element} at {container:?}");
                for r in &rects {
                    assert!(frame.expand(0.6).contains_rect(*r), "{module}/{element} in {container:?}: {r:?} outside {frame:?}");
                }
            }
        }
    }

    // The bug that blew a sub-patch layout up: a Style 2 row's size has to come from its
    // frame and nothing else. At every shape of frame - tiny, a sliver, tall, huge - and
    // through a resize there and back, everything the row draws and every field it
    // publishes stays inside the frame, the frame is all it allocates, and a frame that
    // holds still gets the same drawing every pass.
    #[test]
    fn a_second_look_stays_inside_its_frame_at_any_size() {
        let pairs = [
            ("module.response_curve", "scale_row"),
            ("module.response_curve", "range_row"),
            ("module.response_curve", "grid_row"),
            ("module.vec_response_curve", "range_row"),
            ("module.twoway_response_curve", "scale_row"),
            ("display.oscilloscope", "controls"),
            ("display.oscilloscope", "grid_row"),
            ("display.trigscope", "controls"),
            ("processing.rws", "style"),
            ("processing.rws", "flick"),
            ("processing.rws", "flick_opts"),
            ("processing.rws", "flick_speed"),
            ("processing.rws", "suppress"),
            ("module.area_mapper", "options"),
            ("module.area_mapper", "layers"),
            ("module.area_mapper", "press"),
            ("module.area_mapper", "border"),
            ("module.menu", "options"),
            ("module.vec_reshape", "target_row"),
            ("module.vec_reshape", "grid_row"),
            ("module.vec_reshape", "preset_row"),
        ];
        let sizes = [
            egui::vec2(420.0, 30.0),
            egui::vec2(90.0, 18.0),
            egui::vec2(40.0, 200.0),
            egui::vec2(1600.0, 24.0),
            egui::vec2(900.0, 260.0),
            egui::vec2(260.0, 90.0),
            egui::vec2(12.0, 6.0),
            egui::vec2(420.0, 30.0),
        ];
        for (module, element) in pairs {
            let desc = flexinput_modules::all_modules()
                .into_iter()
                .map(|r| r.descriptor)
                .find(|d| d.id == module)
                .expect("registered");
            let mut snarl: Snarl<NodeData> = Snarl::new();
            let id = snarl.insert_node(egui::Pos2::ZERO, NodeData::from(&desc));
            let ctx = egui::Context::default();
            let origin = egui::pos2(30.0, 40.0);
            for container in sizes {
                let frame = egui::Rect::from_min_size(origin, container);
                let mut last: Option<(egui::Rect, Vec<egui::Rect>)> = None;
                for pass in 0..6 {
                    let mut drawn = egui::Rect::NOTHING;
                    let _ = ctx.run(egui::RawInput::default(), |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            let mut pin = ui.new_child(egui::UiBuilder::new().max_rect(frame));
                            pin.ctx().data_mut(|d| d.insert_temp(egui::Id::new(("gp_nav_cur_element", id.0)), element.to_string()));
                            render(id, module, element, &mut pin, &mut snarl, container);
                            drawn = pin.min_rect();
                        });
                    });
                    let rects: Option<(u64, Vec<egui::Rect>)> =
                        ctx.data(|d| d.get_temp(egui::Id::new(("gp_nav_field_rects", id.0, element.to_string()))));
                    let rects = rects.map(|(_, r)| r).unwrap_or_default();
                    let tol = frame.expand(0.6);
                    assert!(
                        (drawn.width() - container.x).abs() < 0.6 && (drawn.height() - container.y).abs() < 0.6,
                        "{module}/{element} in {container:?}: the pin took {drawn:?}"
                    );
                    if container.x >= 60.0 && container.y >= 14.0 {
                        for r in rects.iter().filter(|r| r.is_positive()) {
                            assert!(tol.contains_rect(*r), "{module}/{element} in {container:?}: a field at {r:?} is outside {frame:?}");
                        }
                    }
                    if pass >= 3 {
                        if let Some(prev) = &last {
                            assert_eq!(prev, &(drawn, rects.clone()), "{module}/{element} in {container:?}: still moving on pass {pass}");
                        }
                    }
                    last = Some((drawn, rects));
                }
            }
        }
    }

    // Through the real pinned path: a Style 2 row leaves no measured size behind, so the
    // layout editor's resize clamp (`clamp_pin_frame_to_content`) has nothing to hold the
    // frame to - the measurement it used to find was the runaway one - and a measurement
    // left from when the pin wore Style 1 is dropped.
    #[test]
    fn a_second_look_leaves_no_measurement_for_the_resize_to_chase() {
        let desc = flexinput_modules::all_modules()
            .into_iter()
            .map(|r| r.descriptor)
            .find(|d| d.id == "module.response_curve")
            .expect("registered");
        let mut snarl: Snarl<NodeData> = Snarl::new();
        let id = snarl.insert_node(egui::Pos2::ZERO, NodeData::from(&desc));
        let outer = NodeId(77);
        let ctx = egui::Context::default();
        let live = std::collections::HashMap::new();
        let panic = crate::app::PanicShortcut::default();
        let mut key = None;
        for style in [1u8, 1, 1, 2, 2, 2] {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let frame = egui::Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(296.0, 38.0));
                    let mut pin = ui.new_child(egui::UiBuilder::new().max_rect(frame));
                    key = Some(super::super::scale::pin_ws_nat_key(pin.ctx(), outer.0, id.0, "scale_row"));
                    super::super::pinned::render_pinned_element(
                        id, "module.response_curve", "scale_row", &mut pin, &mut snarl, frame.size(),
                        &live, &panic, None, None, outer, true, None, None, None, style,
                    );
                    if style == 2 {
                        assert!(
                            (pin.min_rect().width() - 296.0).abs() < 0.6,
                            "the Style 2 pin took {:?}", pin.min_rect()
                        );
                    }
                });
            });
            let nat: Option<egui::Vec2> = ctx.data(|d| d.get_temp(key.expect("rendered")));
            if style == 1 {
                assert!(nat.is_some(), "Style 1 still measures itself");
            } else {
                assert!(nat.is_none(), "a Style 2 pin left a measurement: {nat:?}");
            }
        }
    }

    // The frame the feedback loop saved, from a real workspace: it comes back a usable size
    // on load, and frames that never ran away are left exactly as they were.
    #[test]
    fn a_runaway_frame_comes_back_and_a_sane_one_is_left_alone() {
        use crate::canvas::node::{repair_runaway_items, ExposedModule, LayoutItem};
        let pin = |size: [f32; 2], pos: [f32; 2]| {
            let mut m: ExposedModule = serde_json::from_str(
                r#"{"inner_node_id":3,"element_id":"scale_row","pos":[0,0],"size":[1,1]}"#,
            )
            .expect("a pin");
            m.size = size;
            m.pos = pos;
            LayoutItem::Module(m)
        };
        let mut items = vec![
            pin([9_699_328.0, 37.84375], [8.0, 280.0]),
            pin([296.0, 18.0], [328.0, 632.0]),
            pin([f32::NAN, f32::INFINITY], [-5.0, 1.0e9]),
        ];
        assert!(repair_runaway_items(&mut items));
        let rect = |i: usize| match &items[i] {
            LayoutItem::Module(m) => (m.size, m.pos),
            _ => unreachable!(),
        };
        let (size, pos) = rect(0);
        assert!((size[0] - 378.4375).abs() < 0.01 && size[1] == 37.84375 && pos == [8.0, 280.0], "{size:?} {pos:?}");
        assert_eq!(rect(1), ([296.0, 18.0], [328.0, 632.0]), "a sane frame is not touched");
        let (size, pos) = rect(2);
        assert_eq!((size, pos), ([400.0, 40.0], [0.0, 0.0]));
        assert!(!repair_runaway_items(&mut items), "and a repaired layout stays repaired");
    }

    // Pictures of the Style 2 rows in frames of several shapes, each frame outlined, for
    // looking at what the numbers above can't say. One sheet per row.
    #[test]
    #[ignore = "writes pictures, asserts nothing"]
    fn shoot_second_looks() {
        let pairs = [
            ("module.response_curve", "scale_row"),
            ("module.response_curve", "range_row"),
            ("module.response_curve", "grid_row"),
            ("display.oscilloscope", "controls"),
            ("processing.rws", "flick"),
            ("module.area_mapper", "options"),
            ("module.menu", "options"),
            ("module.vec_reshape", "grid_row"),
            ("processing.gyro_3dof", "edge_turn"),
            ("processing.gyro_3dof", "hold_offset"),
            ("module.stick_rotation", "takeover"),
            ("generator.envelope", "time_row"),
            ("processing.rws", "vh"),
        ];
        let sizes = [
            egui::vec2(296.0, 18.0),
            egui::vec2(296.0, 38.0),
            egui::vec2(600.0, 30.0),
            egui::vec2(160.0, 60.0),
            egui::vec2(900.0, 90.0),
        ];
        for (module, element) in pairs {
            let desc = flexinput_modules::all_modules()
                .into_iter()
                .map(|r| r.descriptor)
                .find(|d| d.id == module)
                .expect("registered");
            let mut snarl: Snarl<NodeData> = Snarl::new();
            let id = snarl.insert_node(egui::Pos2::ZERO, NodeData::from(&desc));
            let ctx = egui::Context::default();
            crate::app::fonts::setup_fonts(&ctx);
            let mut shot = crate::canvas::shot::Shot::default();
            let mut shapes = Vec::new();
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(960.0, 360.0));
            for _ in 0..4 {
                let input = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
                let out = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let mut y = 16.0;
                        for (k, size) in sizes.iter().enumerate() {
                            let frame = egui::Rect::from_min_size(egui::pos2(16.0, y), *size);
                            ui.painter().rect_stroke(frame.expand(1.0), 0.0, egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(255, 200, 80)), egui::StrokeKind::Outside);
                            let mut pin = ui.new_child(egui::UiBuilder::new().max_rect(frame).id_salt(k));
                            render(id, module, element, &mut pin, &mut snarl, *size);
                            y += size.y + 16.0;
                        }
                    });
                });
                shot.absorb(&out.textures_delta);
                shapes = out.shapes;
            }
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/module_shots")
                .join(format!("style2_{}_{element}.png", module.replace('.', "_")));
            shot.save(&ctx, &shapes, screen, 1.0, &path).expect("the picture saves");
            println!("{}", path.display());
        }
    }

    // Every control-font face at the ends of its size range, as a body row of each kind of
    // control - for looking at how the faces sit in the pills and whether a large size
    // spills out of them. Sets the process-wide control font, so it runs alone (ignored).
    #[test]
    #[ignore = "writes a picture, asserts nothing; sets the process-wide control font"]
    fn shoot_control_fonts() {
        use super::super::curve_rows::{pill_bands, pill_combo, real_pill, segmented, word_toggle, word_w};
        use super::super::simple_bodies::{calibrate_control_fonts, control_text, set_control_font};
        let ctx = egui::Context::default();
        crate::app::fonts::setup_fonts(&ctx);
        // `SHOT_PPP=3` for a close look at how the text sits in the pills; `SHOT_SIZES=15`
        // for some sizes only.
        let ppp: f32 = std::env::var("SHOT_PPP").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
        ctx.set_pixels_per_point(ppp);
        let mut shot = crate::canvas::shot::Shot::default();
        let mut shapes = Vec::new();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(760.0, 1150.0));
        let faces = [(false, false, "normal"), (false, true, "normal bold"), (true, false, "console"), (true, true, "console bold")];
        let sizes: Vec<f32> = std::env::var("SHOT_SIZES")
            .ok()
            .map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect())
            .unwrap_or_else(|| vec![11.0, 13.0, 15.0, 18.0]);
        let node = NodeId(1);
        for pass in 0..4 {
            let input = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
            let out = ctx.run(input, |ctx| {
                if pass > 0 {
                    calibrate_control_fonts(ctx);
                }
                egui::CentralPanel::default().show(ctx, |ui| {
                    for (mono, bold, name) in faces {
                        for &size in &sizes {
                            set_control_font(mono, bold, size);
                            ui.label(egui::RichText::new(format!("{name} {size}")).small().weak());
                            let mut on = true;
                            let mut off = false;
                            let mut v = 0.25_f32;
                            let cells = pill_bands(ui, 700.0, &[word_w(ui, "sticky"), 150.0, 120.0, 150.0, 80.0, word_w(ui, "smooth")]);
                            word_toggle(ui, cells[0], &mut on, "sticky", "", ("fs_t", mono, bold, size as i32));
                            segmented(ui, cells[1], &1, &[(0, "ports", ""), (1, "mapping", "")], ("fs_s", mono, bold, size as i32));
                            real_pill(ui, node, &format!("fs_p{mono}{bold}{size}"), cells[2], "dz|deadzone", &mut v, "0.25", 0.0..=1.0, 0.01, "");
                            pill_combo(ui, cells[3], "show", &"hold".to_string(), &[("hold".to_string(), "hold")], "", ("fs_c", mono, bold, size as i32));
                            ui.put(cells[4], egui::Button::new(control_text("Learn")));
                            word_toggle(ui, cells[5], &mut off, "smooth", "", ("fs_t2", mono, bold, size as i32));
                        }
                    }
                });
            });
            shot.absorb(&out.textures_delta);
            shapes = out.shapes;
        }
        set_control_font(false, false, 15.0);
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/module_shots/control_fonts.png");
        shot.save(&ctx, &shapes, screen, ppp, &path).expect("the picture saves");
        println!("{}", path.display());
    }

    #[test]
    fn the_redesigned_rows_have_a_second_look_and_the_graphs_dont() {
        assert!(supported("module.response_curve", "scale_row"));
        assert!(supported("display.trigscope", "grid_row"));
        assert!(supported("processing.rws", "flick"));
        assert!(!supported("module.response_curve", "curve"), "a graph looks the same either way");
        assert!(!supported("module.remapper", "whole_module"));
    }
}
