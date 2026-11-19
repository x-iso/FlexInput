//! RWS Aim module body (Real-World Sensitivity).
//!
//! Layout: the input-mode dropdown and the Calibrate Start/Stop button live in
//! the node HEADER (see `show_header`); the body holds the numeric knobs (two to
//! a row: Mouse Scale | Stick °/s, RWS | V/H), the measure auto-cal, the
//! calibration ruler viewport (`field`), and its style row. Every
//! element is registered as a pinnable + gamepad-editable element so it can be
//! dropped onto the config overlay for live calibration — the ruler `field`
//! itself carries the full calibration control set (Scale / Calibrate / Speed /
//! RWS) so it can be run AND stopped from the gamepad with nothing else pinned,
//! which matters because the mouse output is busy driving the game meanwhile.
//! The Flick stick category is the flick circle (`flick_view`, see
//! [`render_rws_flick_view`]) with the settings beside it, grouped by the part of
//! the circle each sets ([`FLICK_ROWS`]).
//! Real evaluation is `compute_rws` in the engine.

use super::*;

/// Signal-graph gyro normalization: ±1.0 == ±this many deg/s. Mirrors the engine
/// `compute_rws`'s constant (kept local to avoid a cross-crate path for the live
/// preview's rate → deg/s conversion; keep the two in sync).
const GYRO_REF_DPS: f32 = 2000.0;

/// Read the ruler style params (transparent-BG by default).
fn rws_field_style(snarl: &Snarl<NodeData>, node_id: NodeId) -> (f32, f32, bool) {
    snarl
        .get_node(node_id)
        .map(|n| {
            let a = n.params.get("field_bg_alpha").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            let t = n.params.get("field_tick_deg").and_then(|v| v.as_f64()).unwrap_or(15.0) as f32;
            // The degree labels are always drawn: the preview is little use without them,
            // so the switch that hid them is gone (a saved `field_labels` is ignored).
            (a.clamp(0.0, 1.0), t.clamp(5.0, 90.0), true)
        })
        .unwrap_or((0.0, 15.0, true))
}

/// How Mouse Scale is shown and edited. `scale` is always STORED as mouse dots
/// (counts) per degree; the `scale_unit` param ("deg" | "360", toggled in the
/// header) is a view preference only — not in presets — that can show it per
/// full 360° turn instead, as Steam Input and sensitivity databases list it.
#[derive(Clone, Copy)]
pub(crate) struct RwsScaleUnit {
    per_360: bool,
}

impl RwsScaleUnit {
    pub(crate) fn of(snarl: &Snarl<NodeData>, node_id: NodeId) -> Self {
        let per_360 = snarl.get_node(node_id)
            .and_then(|n| n.params.get("scale_unit").and_then(|v| v.as_str()))
            == Some("360");
        Self { per_360 }
    }

    pub(crate) fn per_360(self) -> bool { self.per_360 }

    fn factor(self) -> f32 { if self.per_360 { 360.0 } else { 1.0 } }

    /// A DragValue over the displayed value (`shown`, from [`Self::shown`]).
    fn drag_value(self, shown: &mut f32) -> egui::DragValue<'_> {
        let dv = egui::DragValue::new(shown).range(0.0..=100_000.0 * self.factor());
        if self.per_360 { dv.speed(10.0).max_decimals(0) } else { dv.speed(0.05).max_decimals(3) }
    }

    fn shown(self, scale: f32) -> f32 { scale * self.factor() }

    /// The displayed value converted back to the stored per-degree `scale`.
    fn to_param(self, shown: f32) -> Option<Value> {
        Number::from_f64(shown as f64 / self.factor() as f64).map(Value::Number)
    }

    fn suffix(self) -> &'static str { if self.per_360 { " /360°" } else { " /°" } }

    /// Readout text with the unit, e.g. "151.51 /°" or "54545 /360°".
    fn text(self, scale: f32) -> String {
        if self.per_360 { format!("{:.0} /360°", self.shown(scale)) } else { format!("{scale:.2} /°") }
    }

    fn per_text(self) -> &'static str { if self.per_360 { "per 360° turn" } else { "per degree" } }
}

/// Mouse-sensitivity hint for calibrating without a known value.
const RWS_LOW_SENS_TIP: &str = "No known value? Calibrate at a low in-game sensitivity — usually finer aim steps";

/// The narrowest each constant wants on a band: the label cell and a number that can run to
/// five digits (`100000 /360°`). `curve_rows::bands` shares the rest of a band between the
/// groups in it, and moves a group to the next band when even this won't fit beside it.
const RWS_SCALE_W: f32 = 140.0;
const RWS_SDPS_W: f32 = 112.0;
const RWS_RWS_W: f32 = 160.0;
const RWS_GSC_W: f32 = 112.0;
const RWS_VH_W: f32 = 200.0;
/// The narrowest the body is drawn: the widest pill group in it.
const RWS_MIN_W: f32 = 236.0;
pub(crate) fn show_rws_body(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    body_w: f32,
    body_h: f32,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) {
    let (scale, rws, stick_max, gyro_scale) = snarl
        .get_node(node_id)
        .map(|n| {
            let scale = n.params.get("scale").and_then(|v| v.as_f64()).unwrap_or(100.0) as f32;
            let rws = n.params.get("rws").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let stick_max = n.params.get("stick_out_dps").and_then(|v| v.as_f64()).unwrap_or(360.0) as f32;
            let gyro_scale = n.params.get("gyro_scale").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            (scale, rws, stick_max, gyro_scale)
        })
        .unwrap_or((100.0, 1.0, 360.0, 1.0));
    let (bg_alpha, tick_deg, labels) = rws_field_style(snarl, node_id);
    let unit = RwsScaleUnit::of(snarl, node_id);

    let mut set: Vec<(&str, Value)> = Vec::new();

    // The snarl body ui is left-to-right by default; wrap so the rows stack.
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
        // The calibration view is the module's surface: the corner grip sizes it, and the
        // rows above and below it are as wide as it is. It was as wide as whatever row
        // happened to be widest and a fixed height per view.
        let mut surface = super::kit::Surface::begin(ui, node_id, body_w, body_h, egui::vec2(RWS_MIN_W, 80.0));
        ui.set_max_width(surface.size.x);
        ui.set_min_width(surface.size.x);
        surface.need(RWS_MIN_W);

    // ── Real world calibration and scale ──
    // The module's settings are in categories, each folded and unfolded by its heading
    // (`curve_rows::section`); the preview is folded until it is wanted. This one holds two
    // kinds of number, said apart by a caption each: the calibration values, which say
    // what one degree of turn is to each output and which a calibration measures, and
    // the output sensitivity on top of them - Real World Scale and the V/H biases.
    let sens_open = super::curve_rows::section(ui, snarl, node_id, "sens", "Real world calibration and scale", true);
    if sens_open {
    // The four calibrated constants as value pills: groups that share a band while the
    // width holds them, and a band each when it doesn't. They used to be two rows of caption
    // plus DragValue plus a separator, which is a lot of height for four numbers and a right
    // edge none of them lined up with. Still four elements, so each pins on its own.
    let band = super::curve_rows::band_h(ui);
    let ctrl = band - 4.0;
    let (gyro_vh, stick_vh) = snarl.get_node(node_id).map(|n| (
        n.params.get("gyro_vh_ratio").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
        n.params.get("stick_vh_ratio").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
    )).unwrap_or((1.0, 1.0));
    {
        let w = ui.available_width();
        let caption = |ui: &mut egui::Ui, text: &str| {
            ui.label(egui::RichText::new(text).small().weak());
        };
        let lay = |ui: &mut egui::Ui, mins: &[f32]| -> Vec<egui::Rect> {
            let (cells, h) = super::curve_rows::bands(w, mins, band, 3.0 * super::kit::GAP);
            let (area, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
            cells
                .into_iter()
                .map(|c| {
                    let c = c.translate(area.min.to_vec2());
                    egui::Rect::from_min_size(egui::pos2(c.left(), c.center().y - ctrl * 0.5), egui::vec2(c.width(), ctrl))
                })
                .collect()
        };
        caption(ui, "calibration values");
        let cal = lay(ui, &[RWS_SCALE_W, RWS_SDPS_W, RWS_GSC_W]);

        // Mouse Scale — mouse dots per degree (or per 360°); the calibrated ground truth (the
        // value box the calibration viewport edits in the config overlay).
        let mut shown = unit.shown(scale);
        let text = unit.text(scale);
        if super::curve_rows::real_pill(ui, node_id, "rws_scale", cal[0], "scale|mouse scale", &mut shown, &text, 0.0..=100_000.0 * unit.factor(), if unit.per_360() { 10.0 } else { 0.05 },
            "Mouse Move (XY) output: mouse dots per degree of turn, or per 360°\n(the unit switch is in the header) — the calibrated 1:1 ground truth.\nCalibrate this and RWS 1 feels like a 1:1 physical rotation.")
        {
            if let Some(p) = unit.to_param(shown) { set.push(("scale", p)); }
        }
        register_exposable_element(ui, node_id, "scale", cal[0]);

        // Stick output scaling (the game's camera turn rate at full stick; the Stick-output
        // equivalent of Mouse Scale).
        let mut sm = stick_max;
        let text = format!("{sm:.0} °/s");
        if super::curve_rows::real_pill(ui, node_id, "rws_sdps", cal[1], "stick|stick speed", &mut sm, &text, 1.0..=100_000.0, 5.0,
            "Right-Stick output: the game's camera turn rate at full deflection.\nWire the Stick output to a virtual Right Stick for stick-aim games.")
        {
            if let Some(n) = Number::from_f64(sm as f64) { set.push(("stick_out_dps", Value::Number(n))); }
        }
        register_exposable_element(ui, node_id, "stick_dps", cal[1]);

        // Gyro output scaling (the inverse of the game's own gyro sensitivity; the
        // Gyro-output equivalent of Mouse Scale).
        let mut gs = gyro_scale;
        let text = format!("{gs:.3}");
        if super::curve_rows::real_pill(ui, node_id, "rws_gsc", cal[2], "gyro|gyro scale", &mut gs, &text, 0.0..=1000.0, 0.005,
            "Map output: the pad's own gyro, for a game that aims with it. Wire the pad into\nthe Device input and the Map output on to the virtual pad; its gyro axes are\nmultiplied by this (and by Real World Scale). Calibrate it and the game turns\nas far as the pad does.")
        {
            if let Some(n) = Number::from_f64(gs as f64) { set.push(("gyro_scale", Value::Number(n))); }
        }
        register_exposable_element(ui, node_id, "gyro_scale", cal[2]);

        caption(ui, "output sensitivity");
        // Real World Scale first, then the two V/H biases: one band where the width holds
        // them, Real World Scale on a band of its own where it doesn't - most of the width,
        // its name in full, as the one number here that is a feel rather than a measurement.
        let out = lay(ui, &[RWS_RWS_W, RWS_VH_W]);
        // Real World Scale: the sensitivity, as a multiple of the calibrated 1:1 turn.
        let mut v = rws;
        let text = format!("{v:.2}");
        if super::curve_rows::real_pill(ui, node_id, "rws_rws", out[0], "rws|real world scale", &mut v, &text, 0.01..=50.0, 0.01,
            "Real World Scale: the sensitivity, as a multiple of the calibrated 1:1 turn.\n1.0 = the camera turns as far as your hands do; 2.0 = twice as far.\nIt applies to every output, and isn't touched by calibrating.")
        {
            if let Some(n) = Number::from_f64(v as f64) { set.push(("rws", Value::Number(n))); }
        }
        register_exposable_element(ui, node_id, "rws", out[0]);

        // V/H bias — vertical sensitivity relative to horizontal (the calibrated reference),
        // separately for the gyro source and stick sources. 1.0 = equal.
        let mut g = gyro_vh;
        let mut s = stick_vh;
        let (half, gap) = ((out[1].width() - super::kit::GAP) / 2.0, super::kit::GAP);
        let gy = egui::Rect::from_min_size(out[1].min, egui::vec2(half, ctrl));
        let st = egui::Rect::from_min_size(egui::pos2(out[1].left() + half + gap, out[1].top()), egui::vec2(half, ctrl));
        // Named for which they are in the label - "gy v/h" short, "gyro v/h" long - where the
        // value cell said it again ("gy 1.00").
        let gt = format!("{g:.2}");
        if super::curve_rows::real_pill(ui, node_id, "rws_gy", gy, "gy v/h|gyro v/h", &mut g, &gt, 0.0..=8.0, 0.01,
            "Vertical sensitivity relative to horizontal (1.0 = equal;\n>1 = look up/down faster). Horizontal is the calibrated\nreference — the gyro source.")
        {
            if let Some(n) = Number::from_f64(g as f64) { set.push(("gyro_vh_ratio", Value::Number(n))); }
        }
        let st2 = format!("{s:.2}");
        if super::curve_rows::real_pill(ui, node_id, "rws_st", st, "st v/h|stick v/h", &mut s, &st2, 0.0..=8.0, 0.01,
            "Vertical sensitivity relative to horizontal (1.0 = equal;\n>1 = look up/down faster). Horizontal is the calibrated\nreference — the stick sources.")
        {
            if let Some(n) = Number::from_f64(s as f64) { set.push(("stick_vh_ratio", Value::Number(n))); }
        }
        register_exposable_element(ui, node_id, "vh", out[1]);
    }
    rws_calibrate_button(node_id, ui, snarl, automap_parent);
    }

    let mode = snarl.get_node(node_id)
        .and_then(|n| n.params.get("field_mode").and_then(|v| v.as_str()))
        .unwrap_or("ruler").to_string();
    let fov = snarl.get_node(node_id)
        .and_then(|n| n.params.get("field_fov").and_then(|v| v.as_f64()))
        .unwrap_or(90.0) as f32;

    // ── Heading rotation preview: the ruler / room the turn is drawn in. Folded until
    // wanted: it is the module's surface (the corner grip sizes it) and little use day to day.
    let view_open = super::curve_rows::section(ui, snarl, node_id, "view", "Heading rotation preview", false);
    if view_open {
        let frect = render_rws_field(node_id, ui, snarl, surface.size, false);
        register_exposable_element(ui, node_id, "field", frect);
        set.extend(rws_body_settings(node_id, ui, snarl, mode.as_str(), fov, bg_alpha, tick_deg, labels, Some(&["style"]), true).0);
    }

    // ── Flick stick: a circle showing the flick live, beside the settings that shape it,
    // each group captioned in the colour of the part of the circle it sets ──
    if super::curve_rows::section(ui, snarl, node_id, "flick", "Flick stick", true) {
        set.extend(rws_flick_section(node_id, ui, snarl, mode.as_str(), fov, bg_alpha, tick_deg, labels));
    }

    ui.label(egui::RichText::new("→ wire Mouse Move (XY) to KB/M “Mouse XY (move)”").small().weak())
        .on_hover_text("RWS drives the mouse via the displacement pin, which ignores the\nKB/M card's mouse sensitivity — so the calibration is portable.");
    if view_open {
        surface.end(ui);
    } else {
        surface.end_folded(ui);
    }
    }); // ui.vertical

    if !set.is_empty() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            for (k, v) in set {
                node.params.insert(k.to_string(), v);
            }
        }
    }
}

/// The body's way to calibrate: a button that pins the Real World Calibration widget to the
/// config overlay (`crate::rws_cal`), or, once it is there, a line saying so. Labelled with
/// the pad the module reads, since a patch can have one RWS Aim per pad and each has its own
/// widget. The calibration itself runs over the game, from the pad.
pub(crate) fn rws_calibrate_button(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &Snarl<NodeData>,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) -> egui::Rect {
    let path = super::subpatch_path(automap_parent);
    let jsm = snarl.get_node(node_id).is_some_and(|n| n.module_id == "module.jsm");
    let sets = if jsm {
        "REAL_WORLD_CALIBRATION or VIRTUAL_STICK_CALIBRATION"
    } else {
        "Mouse Scale, Stick °/s or the gyro scale"
    };
    let pinned = crate::rws_cal::is_pinned(ui.ctx(), &path, node_id.0);
    let pad = crate::rws_cal::pad_name(snarl, node_id);
    let ctrl = super::curve_rows::band_h(ui) - 4.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ctrl), egui::Sense::hover());
    let pad_part = if pad.is_empty() { String::new() } else { format!(" · {pad}") };
    if pinned {
        let mut on = true;
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt(("rws_cal_pinned", node_id)));
        c.disable();
        super::curve_rows::word_toggle(
            &mut c,
            rect,
            &mut on,
            &format!("✓ calibration on the overlay{pad_part}"),
            "Open the config overlay and press Real World Calibration with the pad.",
            ("rws_cal_pinned", node_id),
        );
        // The hover text of a disabled control is not shown; say it on the row instead.
        ui.interact(rect, egui::Id::new(("rws_cal_pinned_hover", node_id.0)), egui::Sense::hover())
            .on_hover_text("Open the config overlay and press Real World Calibration with the pad.\nRemove it there to get this button back.");
    } else {
        let mut off = false;
        if super::curve_rows::word_toggle(
            ui,
            rect,
            &mut off,
            &format!("calibrate on the overlay{pad_part}"),
            &format!(
                "Pin a Real World Calibration button to the config overlay. Pressed with the pad over \
                 the game, it walks you through turning one full 360° and sets {sets} from it."
            ),
            ("rws_cal_pin", node_id),
        ) {
            crate::rws_cal::request_pin(ui.ctx(), path, node_id.0);
        }
    }
    rect
}

/// The last calibration's result message and when it was made, if any.
pub(crate) fn rws_last_result(ctx: &egui::Context, node_id: NodeId) -> Option<(String, f64)> {
    ctx.data(|d| d.get_temp::<(String, f64)>(rws_meas_msg_key(node_id)))
}

/// The Real World Calibration widget, pinned to the config overlay: one big button that
/// starts the guided calibration (`crate::rws_cal`). It needs gamepad navigation, which runs
/// the procedure; without it the button says so and does nothing. It is also where a sweep
/// the pad finished is back-solved, as the old widget did.
pub(crate) fn render_rws_cal_widget(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    // A finish the pad asked for: back-solve and stop, here as the measure widget did. A
    // JSM node's answer goes into its config text.
    let jsm = snarl.get_node(node_id).is_some_and(|n| n.module_id == "module.jsm");
    let (axis, theta, peak) = rws_measure_state(node_id, snarl);
    let finish = snarl.get_node(node_id).and_then(|n| n.params.get("cal_finish").and_then(|v| v.as_bool())).unwrap_or(false);
    if finish && jsm {
        if super::jsm::jsm_finish_pending(node_id, ui, snarl) {
            mark_overlay_param_write(ui.ctx());
        }
    } else if finish {
        if axis == "pitch" || axis == "yaw" {
            rws_finish_measure(node_id, ui, snarl, &axis, theta, peak);
        } else if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("cal_finish".into(), Value::Bool(false));
        }
        mark_overlay_param_write(ui.ctx());
    }
    let session = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("cal_session").and_then(|v| v.as_str()))
        .unwrap_or("off")
        .to_string();
    let pad_on = crate::config_overlay::config_pad_active(ui.ctx());
    let pad = crate::rws_cal::pad_name(snarl, node_id);
    let (rect, _) = ui.allocate_exact_size(container, egui::Sense::hover());
    let resp = ui
        .interact(rect, egui::Id::new(("rws_cal_widget", node_id.0, ui.layer_id())), egui::Sense::click())
        .on_hover_text(if pad_on {
            "Calibrate with the pad: choose what to calibrate, turn one full 360°, finish."
        } else {
            "Turn on gamepad navigation first: the calibration is done from the pad, over the game."
        });
    let v = ui.visuals();
    let running = session == "choose" || session == "guided";
    let fill = if running {
        v.selection.bg_fill
    } else if resp.hovered() && pad_on {
        v.widgets.hovered.bg_fill
    } else {
        v.widgets.inactive.bg_fill
    };
    let text_col = if pad_on || running { v.strong_text_color() } else { v.weak_text_color() };
    ui.painter().rect_filled(rect, 8.0, fill);
    let h = rect.height();
    let title = if running { "Calibrating…" } else { "Real World Calibration" };
    let sub = if !pad_on && !running {
        "needs gamepad navigation".to_string()
    } else if pad.is_empty() {
        if jsm { "no pad on the input" } else { "no pad on the Rotation input" }.to_string()
    } else {
        pad
    };
    ui.painter().text(
        rect.center() - egui::vec2(0.0, h * 0.14),
        egui::Align2::CENTER_CENTER,
        title,
        crate::settings::ui_font((h * 0.34).clamp(10.0, 28.0)),
        text_col,
    );
    ui.painter().text(
        rect.center() + egui::vec2(0.0, h * 0.24),
        egui::Align2::CENTER_CENTER,
        sub,
        crate::settings::ui_font((h * 0.2).clamp(8.0, 16.0)),
        v.weak_text_color(),
    );
    publish_nav_field_rects(ui, node_id, &[rect]);
    if resp.clicked() && pad_on && !running {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("cal_session".into(), Value::String("choose".into()));
        }
        mark_overlay_param_write(ui.ctx());
    }
}

/// One control among the flick settings: see [`FLICK_ROWS`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FlickItem {
    Zone,
    Fwd,
    Aim,
    AimX,
    Within,
    Slow,
    Snap,
    Stab,
    Out,
    Block,
}

/// The flick settings as rows: the element each pins as, its controls in the gamepad's order
/// (`element_fields`), the share of the row each takes, and the narrowest the row reads at.
/// The body draws the first six, grouped by what they do to the flick circle; the rest are the
/// rows as they were before the circle, which no body draws any more but a layout that pinned
/// one still does. An element of two rows lays them side by side where they fit.
const FLICK_ROWS: &[(&str, &[FlickItem], &[f32], f32)] = {
    use FlickItem::*;
    &[
        // The four beside the circle are as wide as each other, so they end together.
        ("flick_zone", &[Zone, Fwd], &[1.0, 1.0], 216.0),
        ("flick_aim", &[Aim, AimX], &[1.0, 1.0], 216.0),
        ("flick_speed", &[Slow, Within], &[1.15, 1.0], 216.0),
        ("flick_feel", &[Snap, Stab], &[1.0, 1.0], 216.0),
        ("flick_route", &[Out], &[1.0], 170.0),
        ("flick_route", &[Block], &[1.0], 180.0),
        // Before the circle.
        ("flick", &[Zone, Snap], &[1.0, 1.15], 236.0),
        ("flick", &[Aim, AimX], &[1.0, 1.0], 180.0),
        ("flick_opts", &[Stab, Fwd], &[1.0, 1.0], 196.0),
        ("flick_opts", &[Out], &[1.0], 170.0),
        ("suppress", &[Block], &[1.0], 180.0),
    ]
};

/// The flick rows the body draws, in its order, each under a caption, with the colour that
/// caption shares with the flick circle (see [`paint_rws_flick_view`]).
const FLICK_BODY_ROWS: [(&str, &str, Option<egui::Color32>); 5] = [
    ("flick_zone", "flick zone · forward", Some(FLICK_COL)),
    ("flick_feel", "snap and tracking", None),
    ("flick_aim", "stick aim inside it", Some(AIM_COL)),
    ("flick_speed", "speed gate", Some(GATE_COL)),
    ("flick_route", "where it goes", None),
];

/// The flick circle's colours, which the captions over the rows that set each part repeat.
const FLICK_COL: egui::Color32 = egui::Color32::from_rgb(236, 138, 48);
const FWD_COL: egui::Color32 = egui::Color32::from_rgb(76, 178, 110);
const AIM_COL: egui::Color32 = egui::Color32::from_rgb(78, 146, 230);
const GATE_COL: egui::Color32 = egui::Color32::from_rgb(222, 188, 44);

/// The body's view-style and flick settings: see [`show_rws_body`]. `only` names the
/// elements drawn (the view `style`, [`FLICK_ROWS`]); `body` registers them as pinnable.
#[allow(clippy::too_many_arguments)]
fn rws_body_settings(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &Snarl<NodeData>,
    mode: &str,
    fov: f32,
    bg_alpha: f32,
    tick_deg: f32,
    _labels: bool,
    only: Option<&[&str]>,
    body: bool,
) -> (Vec<(&'static str, Value)>, Vec<egui::Rect>) {
    use super::curve_rows::{real_pill, segmented, word_toggle, pill_combo, Groups};
    // The field rects in the order the gamepad walks the element `only` names.
    let mut fields: Vec<egui::Rect> = Vec::new();
    let mut set: Vec<(&'static str, Value)> = Vec::new();
    let num = |v: f32| Number::from_f64(f64::from(v)).map(Value::Number);
    let p = |k: &str, d: f64| snarl.get_node(node_id).and_then(|n| n.params.get(k).and_then(|v| v.as_f64())).unwrap_or(d) as f32;
    let b = |k: &str| snarl.get_node(node_id).and_then(|n| n.params.get(k).and_then(|v| v.as_bool())).unwrap_or(false);
    let (dz, smooth, aim_on, aim_x) = (p("flick_deadzone", 0.85), p("flick_smooth_ms", 100.0), b("stick_aim_enabled"), p("stick_aim_rws", 1.0));
    let (stab, fwd) = (p("flick_stabilise_ms", 0.0), p("flick_fwd_dz_deg", 0.0));
    let (speed, slow) = (p("flick_speed_ms", 0.0), b("flick_slow_lock"));
    let out = snarl.get_node(node_id).map_or("mouse", |n| flexinput_engine::eval::rws_flick_mode(&n.params));
    let suppress = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("suppress_source").and_then(|v| v.as_str()))
        .unwrap_or("off")
        .to_string();

    let gap = super::curve_rows::zoomed(super::kit::GAP);
    let w = ui.available_width();
    // The view and its BG / FOV / tick pills, then the flick rows. Sized for their words: a
    // pill needs its label cell (monospace) and its value. One pill each: the background,
    // and the field of view (room) and tick spacing (ruler) when the mode draws them -
    // "both" has all three.
    let view_pills = 1 + usize::from(mode != "ruler") + usize::from(mode != "room");
    let view_min = view_pills as f32 * 80.0 + (view_pills - 1) as f32 * super::kit::GAP;
    let mins: Vec<f32> = [150.0, view_min].into_iter().chain(FLICK_ROWS.iter().map(|r| r.3)).collect();
    let owner = |k: usize| if k < 2 { "style" } else { FLICK_ROWS[k - 2].0 };
    let g = Groups::layout(ui, w, &mins, |k| only.is_none_or(|o| o.contains(&owner(k))));
    let none = egui::Rect::NOTHING;
    // A group not drawn is a rect nothing is put in; the drawing below runs either way
    // only for the groups that are there.
    let cell = |i: usize| g.get(i).unwrap_or(none);
    let drawn = |i: usize| g.get(i).is_some();
    // `n` slots across a cell, in proportion to `weights`.
    let split = |c: egui::Rect, weights: &[f32]| -> Vec<egui::Rect> {
        let total: f32 = weights.iter().sum();
        let room = c.width() - gap * (weights.len() as f32 - 1.0);
        let mut x = c.left();
        weights
            .iter()
            .map(|wt| {
                let r = egui::Rect::from_min_size(egui::pos2(x, c.top()), egui::vec2(room * wt / total, c.height()));
                x = r.right() + gap;
                r
            })
            .collect()
    };

    // The view.
    if drawn(0) { if let Some(m) = segmented(
        ui,
        cell(0),
        &mode.to_string(),
        &[
            ("ruler".to_string(), "ruler", "A ruler of degrees across the turn"),
            ("room".to_string(), "room", "A room drawn round you, at the in-game field of view"),
            ("both".to_string(), "both", "The ruler over the room"),
        ],
        ("rws_view", node_id),
    ) {
        set.push(("field_mode", Value::String(m)));
    }
    fields.push(cell(0)); }
    if body && drawn(0) {
        register_exposable_element(ui, node_id, "style", cell(0).union(cell(1)));
    }
    if drawn(1) {
        let room = mode != "ruler";
        let ruler = mode != "room";
        let mut weights = vec![1.0];
        if room {
            weights.push(1.0);
        }
        if ruler {
            weights.push(1.0);
        }
        let slots = split(cell(1), &weights);
        let mut k = 0;
        let mut a = bg_alpha;
        let bg_slot = slots[k];
        if real_pill(ui, node_id, "rws_bg", slots[k], "bg|background", &mut a, &format!("{bg_alpha:.2}"), 0.0..=1.0, 0.01, "Viewport background opacity (0 = transparent).") {
            if let Some(n) = num(a) { set.push(("field_bg_alpha", n)); }
        }
        k += 1;
        // Gamepad order: View, FOV, BG, Ticks, Labels.
        fields.push(if room { slots[k] } else { none });
        fields.push(bg_slot);
        if room {
            let mut fv = fov;
            if real_pill(ui, node_id, "rws_fov", slots[k], "fov|field of view", &mut fv, &format!("{fov:.0}°"), 30.0..=140.0, 0.5,
                "Horizontal field of view — set this to match your in-game FOV so the turn rate reads 1:1.")
            {
                if let Some(n) = num(fv.round()) { set.push(("field_fov", n)); }
            }
            k += 1;
        }
        if ruler {
            let mut td = tick_deg;
            if real_pill(ui, node_id, "rws_tick", slots[k], "tick|tick spacing", &mut td, &format!("{tick_deg:.0}°"), 5.0..=90.0, 0.5, "Minor tick spacing.") {
                if let Some(n) = num(td.round()) { set.push(("field_tick_deg", n)); }
            }
            fields.push(slots[k]);
        }
    }

    // The flick rows. Each control's id names the row it is drawn in: one setting can be in
    // two pinned rows of a layout (an old row and a new one), and the ids are global.
    let outs: Vec<(String, &str)> = RWS_FLICK_OUT_OPTS.iter().map(|o| (o.to_string(), rws_flick_out_label(o))).collect();
    let mut element_rects: Vec<(&str, egui::Rect)> = Vec::new();
    for (r, &(element, items, weights, _)) in FLICK_ROWS.iter().enumerate() {
        if !drawn(r + 2) {
            continue;
        }
        let c = cell(r + 2);
        match element_rects.iter_mut().find(|(e, _)| *e == element) {
            Some((_, u)) => *u = u.union(c),
            None => element_rects.push((element, c)),
        }
        let key = |base: &str| format!("{base}:{element}");
        for (&item, slot) in items.iter().zip(split(c, weights)) {
            fields.push(slot);
            match item {
                FlickItem::Zone => {
                    let mut d = dz;
                    if real_pill(ui, node_id, &key("rws_fdz"), slot, "zone|flick zone", &mut d, &format!("{dz:.2}"), 0.1..=0.99, 0.005,
                        "Where the flick zone starts, as a fraction of the stick's travel: the orange ring of the \
                         flick circle. Push the stick out into it and the camera snaps to the stick's direction; \
                         turn the stick round and the camera turns with it. Once out, the flick follows the stick \
                         down to the dashed ring before it lets go. A flick is 1:1: Real World Scale doesn't \
                         apply. The Flick On input turns it off for a mode shift.")
                    {
                        if let Some(n) = num(d) { set.push(("flick_deadzone", n)); }
                    }
                }
                FlickItem::Fwd => {
                    let mut f = fwd;
                    if real_pill(ui, node_id, &key("rws_fwd"), slot, "fwd±|forward, no snap", &mut f, &format!("{fwd:.0}°"), 0.0..=90.0, 0.5,
                        "The green wedge at the top of the flick circle: a flick that starts within this many \
                         degrees of straight up doesn't snap. It only takes hold of the camera, so turning the \
                         stick from there turns it - a 360° with the stick, say. 0 = every flick snaps.")
                    {
                        if let Some(n) = num(f.round()) { set.push(("flick_fwd_dz_deg", n)); }
                    }
                }
                FlickItem::Aim => {
                    let mut on = aim_on;
                    if word_toggle(ui, slot, &mut on, "stick aim",
                        "Aim with the stick inside the flick zone, as a rate: the blue disc of the flick circle, \
                         turning faster the further out the stick is. With the flick off the whole stick aims. \
                         It feeds both outputs, at its own × and the stick V/H bias. The Flick On input turns \
                         it off too.", ("rws_aim", node_id, element))
                    {
                        set.push(("stick_aim_enabled", Value::Bool(on)));
                    }
                }
                FlickItem::AimX => {
                    let mut x = aim_x;
                    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(slot).id_salt(("rws_aim_x", node_id, element)));
                    if !aim_on {
                        c.disable();
                    }
                    if real_pill(&mut c, node_id, &key("rws_aimx"), slot, "×|aim ×", &mut x, &format!("{aim_x:.2}"), 0.01..=50.0, 0.01,
                        "Stick aim's own sensitivity, a multiple of the calibrated turn rate (independent of \
                         Real World Scale).")
                    {
                        if let Some(n) = num(x) { set.push(("stick_aim_rws", n)); }
                    }
                }
                FlickItem::Within => {
                    let mut sp = speed;
                    if real_pill(ui, node_id, &key("rws_within"), slot, "within|flick within", &mut sp, &format!("{speed:.0} ms"), 0.0..=300.0, 1.0,
                        "How long a push out of the centre has to reach the flick zone to count as a flick - the \
                         yellow ring round the stick's dot counts it down. With the flick speed gate on, a slower \
                         push doesn't flick. Either way a push that flicks throws away the Stick aim it made on its \
                         way out, and a slower one has its aim caught up. 0 = off: the zone alone decides.")
                    {
                        if let Some(n) = num(sp.round()) { set.push(("flick_speed_ms", n)); }
                    }
                }
                FlickItem::Slow => {
                    let mut sl = slow;
                    if word_toggle(ui, slot, &mut sl, "speed gate|flick speed gate",
                        "Flick only on a fast push: one that reaches the flick zone within the time beside this. \
                         A slower push doesn't flick until the stick is back at the centre (the flick zone goes \
                         grey on the flick circle): with Stick aim on it aims, at full rate out past the zone, and \
                         with it off the stick is left to whatever else it does - a block set to In flick zone \
                         lets it through. Off, any push that reaches the zone flicks.", ("rws_slowlock", node_id, element))
                    {
                        set.push(("flick_slow_lock", Value::Bool(sl)));
                        // A gate with no time does nothing: give it one to start from.
                        if sl && speed <= 0.0 {
                            if let Some(n) = num(120.0) { set.push(("flick_speed_ms", n)); }
                        }
                    }
                }
                FlickItem::Snap => {
                    let mut sm = smooth;
                    if real_pill(ui, node_id, &key("rws_fsm"), slot, "snap|snap time", &mut sm, &format!("{smooth:.0} ms"), 0.0..=500.0, 1.0,
                        "How long the flick's snap takes: the turn to the stick's direction is spread over this \
                         time rather than made at once (0 = instant). The arc by the flick circle's arrow fills \
                         as the outputs turn it.")
                    {
                        if let Some(n) = num(sm.round()) { set.push(("flick_smooth_ms", n)); }
                    }
                }
                FlickItem::Stab => {
                    let mut st = stab;
                    if real_pill(ui, node_id, &key("rws_stab"), slot, "stab|stabilise", &mut st, &format!("{stab:.0} ms"), 0.0..=300.0, 1.0,
                        "Steadies the stick's direction while a flick is held out and turned: a thumb's tremor is \
                         held back, a deliberate sweep goes through. Whatever it still holds back when you let go \
                         is turned then. 0 = off.")
                    {
                        if let Some(n) = num(st.round()) { set.push(("flick_stabilise_ms", n)); }
                    }
                }
                FlickItem::Out => {
                    if let Some(o) = pill_combo(ui, slot, "out|flick output", &out.to_string(), &outs,
                        "Where the flick goes.\n• Flick pin · mouse — ONLY the Flick output, in mouse counts (Mouse Scale).\n\
                         • Flick pin · stick — ONLY the Flick output, as stick deflection (Stick °/s).\n\
                         • Mouse + Stick — added into BOTH the Mouse Move and Stick outputs.\n\
                         With a Flick pin option and the pin unwired, the flick is off.", ("rws_out", node_id, element))
                    {
                        set.push(("flick_output", Value::String(o)));
                    }
                }
                FlickItem::Block => {
                    if let Some(v) = pill_combo(ui, slot, "block|block stick", &suppress,
                        &[("off".to_string(), "Off"), ("full".to_string(), "Full"), ("deadzone".to_string(), "In flick zone")],
                        "Block the stick wired into Flick from leaking to its default mapping (e.g. the virtual Right Stick). \
                         This module still reads it.\n• Off — no block.\n• Full — always while Flick or Stick aim is on.\n\
                         • In flick zone — only while the stick is out in the flick zone.\nA calibration sweep always blocks it.",
                        ("rws_block", node_id, element))
                    {
                        set.push(("suppress_source", Value::String(v)));
                    }
                }
            }
        }
    }
    if body {
        for (element, r) in element_rects {
            register_exposable_element(ui, node_id, element, r);
        }
    }
    (set, fields)
}

/// A caption over a group of rows, with a dot in the colour its part of the flick circle is
/// drawn in.
fn flick_caption(ui: &mut egui::Ui, text: &str, col: Option<egui::Color32>) {
    let font = egui::TextStyle::Small.resolve(ui.style());
    let h = font.size + 2.0;
    let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::hover());
    let mut x = r.left();
    if let Some(c) = col {
        ui.painter().circle_filled(egui::pos2(x + 4.0, r.center().y), 3.5, c);
        x += 11.0;
    }
    ui.painter().text(egui::pos2(x, r.center().y), egui::Align2::LEFT_CENTER, text, font, ui.visuals().weak_text_color());
}

/// The body's Flick stick category: the flick circle, and beside it (or under it, where the
/// body is narrow) the settings that shape it, a captioned group for each part; then where
/// the flick goes, the whole width, and why it is off when it is.
#[allow(clippy::too_many_arguments)]
fn rws_flick_section(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &Snarl<NodeData>,
    mode: &str,
    fov: f32,
    bg_alpha: f32,
    tick_deg: f32,
    labels: bool,
) -> Vec<(&'static str, Value)> {
    let mut set = Vec::new();
    let rows = |ui: &mut egui::Ui, which: &[(&str, &str, Option<egui::Color32>)], set: &mut Vec<(&'static str, Value)>| {
        for &(element, caption, col) in which {
            flick_caption(ui, caption, col);
            set.extend(rws_body_settings(node_id, ui, snarl, mode, fov, bg_alpha, tick_deg, labels, Some(&[element]), true).0);
        }
    };
    let w = ui.available_width();
    let gap = 3.0 * super::kit::GAP;
    // The column beside the circle is as wide as its widest row needs; the circle has the rest,
    // up to a size past which it only takes room.
    let col_need = FLICK_ROWS[..4].iter().map(|r| r.3).fold(0.0, f32::max) * super::simple_bodies::control_width_factor();
    let side_room = w - col_need - gap;
    if side_room >= 132.0 {
        let side = side_room.min(220.0);
        let top = ui.cursor().min;
        let col_rect = egui::Rect::from_min_max(egui::pos2(top.x + side + gap, top.y), egui::pos2(top.x + w, top.y + 4000.0));
        let mut col = ui.new_child(
            egui::UiBuilder::new().max_rect(col_rect).layout(egui::Layout::top_down(egui::Align::Min)).id_salt(("rws_flick_col", node_id)),
        );
        col.spacing_mut().item_spacing = ui.spacing().item_spacing;
        rows(&mut col, &FLICK_BODY_ROWS[..4], &mut set);
        let h = col.min_rect().height().max(side);
        let circle = egui::Rect::from_min_size(egui::pos2(top.x, top.y + (h - side) * 0.5), egui::vec2(side, side));
        let mut cc = ui.new_child(egui::UiBuilder::new().max_rect(circle).id_salt(("rws_flick_circle", node_id)));
        let r = render_rws_flick_view(node_id, &mut cc, snarl, circle.size());
        ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
        register_exposable_element(ui, node_id, "flick_view", r);
    } else {
        let side = w.min(200.0);
        let (area, _) = ui.allocate_exact_size(egui::vec2(w, side), egui::Sense::hover());
        let circle = egui::Rect::from_center_size(area.center(), egui::vec2(side, side));
        let mut cc = ui.new_child(egui::UiBuilder::new().max_rect(circle).id_salt(("rws_flick_circle", node_id)));
        let r = render_rws_flick_view(node_id, &mut cc, snarl, circle.size());
        register_exposable_element(ui, node_id, "flick_view", r);
        rows(ui, &FLICK_BODY_ROWS[..4], &mut set);
    }
    rows(ui, &FLICK_BODY_ROWS[4..], &mut set);
    if let Some(hint) = rws_flick_hint(snarl, node_id) {
        ui.label(egui::RichText::new(hint).small().weak());
    }
    set
}

/// The flick circle of an RWS Aim node, `container` big, the circle in the middle of it.
///
/// The stick's travel as a disc, with what each part of it does: the flick zone (an orange
/// ring from the flick deadzone out), the forward wedge in it where a flick doesn't snap
/// (green), Stick aim's disc inside the zone (blue, deeper where it turns faster), the
/// release floor a held flick follows the stick down to (dashed), and the speed gate's
/// start (a yellow ring round the centre). On it, live: the stick as a dot coloured by what
/// it is doing, with a short trail; the speed gate counting down round the dot; and while a
/// flick is held, an arrow head outside the ring where it points, with the arc it has turned
/// - solid as far as the outputs have turned it, faint where the snap is still smoothing in.
/// A line under the circle says it in words. Reads the engine's live state
/// (`rws_flick_view`); before the engine has run the node, the settings alone.
pub(crate) fn render_rws_flick_view(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &Snarl<NodeData>,
    container: egui::Vec2,
) -> egui::Rect {
    use flexinput_engine::eval::RwsGesture;
    let size = egui::vec2(container.x.max(48.0), container.y.max(48.0));
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let Some(node) = snarl.get_node(node_id) else { return rect };
    let pf = |k: &str, d: f32| node.params.get(k).and_then(|v| v.as_f64()).map_or(d, |v| v as f32);
    let pb = |k: &str| node.params.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let dz = pf("flick_deadzone", 0.85).clamp(0.05, 0.99);
    let fwd = pf("flick_fwd_dz_deg", 0.0).clamp(0.0, 180.0);
    let speed_s = pf("flick_speed_ms", 0.0).max(0.0) / 1000.0;
    let live = flexinput_engine::eval::rws_flick_view(&node.extra.last_out, node.outputs.len());
    let reaches = rws_flick_reaches(snarl, node_id);
    let flick_on = live.map_or(reaches, |v| v.flick_on);
    let aim_on = live.map_or(pb("stick_aim_enabled"), |v| v.aim_on);
    let v = live.unwrap_or_default();
    let gated = flick_on && speed_s > 0.0;
    let locked = gated && pb("flick_slow_lock") && v.gesture == RwsGesture::Steering;
    let deciding = gated && v.gesture == RwsGesture::Deciding;
    let left = if deciding { (1.0 - v.gate_s / speed_s).clamp(0.0, 1.0) } else { 0.0 };

    let visuals = ui.visuals().clone();
    let weak = visuals.weak_text_color();
    let line = visuals.widgets.noninteractive.fg_stroke.color.gamma_multiply(0.6);
    let painter = ui.painter_at(rect);
    let font_px = (rect.width().min(rect.height()) * 0.065).clamp(9.0, 13.0);
    let label_h = font_px + 6.0;
    let side = rect.width().min(rect.height() - label_h).max(24.0);
    // Room outside the ring for the arrow head.
    let margin = (side * 0.075).clamp(7.0, 15.0);
    let r = (side * 0.5 - margin).max(8.0);
    let c = egui::pos2(rect.center().x, rect.top() + (rect.height() - label_h) * 0.5);
    // The bus counts y up; the screen counts it down.
    let at = |p: glam::Vec2| c + egui::vec2(p.x, -p.y) * r;
    let toward = |deg: f32| {
        let a = deg.to_radians();
        egui::vec2(a.sin(), -a.cos())
    };
    let arc = |rad: f32, from: f32, to: f32| -> Vec<egui::Pos2> {
        let n = ((to - from).abs() / 3.0).ceil().clamp(1.0, 240.0) as usize;
        (0..=n).map(|i| c + toward(from + (to - from) * i as f32 / n as f32) * rad).collect()
    };
    // A band from `r0` to `r1` across the degrees `from`..`to`, its colour going from `c0` on
    // the inside to `c1` on the outside: one mesh, so bands that meet leave no seam.
    let band = |r0: f32, r1: f32, from: f32, to: f32, c0: egui::Color32, c1: egui::Color32| {
        if r1 <= r0 {
            return;
        }
        let n = ((to - from).abs() / 4.0).ceil().clamp(2.0, 96.0) as u32;
        let mut mesh = egui::Mesh::default();
        for i in 0..=n {
            let d = toward(from + (to - from) * i as f32 / n as f32);
            mesh.colored_vertex(c + d * r0, c0);
            mesh.colored_vertex(c + d * r1, c1);
        }
        for i in 0..n {
            let k = 2 * i;
            mesh.add_triangle(k, k + 1, k + 2);
            mesh.add_triangle(k + 1, k + 3, k + 2);
        }
        painter.add(mesh);
    };
    let ring = |r0: f32, r1: f32, col: egui::Color32| band(r0, r1, 0.0, 360.0, col, col);

    // The stick's travel.
    painter.circle_filled(c, r, weak.gamma_multiply(0.06));
    // Stick aim: deeper the further out, as it turns faster; past the zone too while a slow
    // push has the zone shut (at full rate), and all of the stick with the flick off.
    if aim_on {
        let ramp_to = if flick_on { dz } else { 1.0 };
        band(0.0, ramp_to * r, 0.0, 360.0, AIM_COL.gamma_multiply(0.03), AIM_COL.gamma_multiply(0.3));
        if locked {
            ring(dz * r, r, AIM_COL.gamma_multiply(0.3));
        }
    }
    // The flick zone: brighter while the speed gate counts down, as the place to reach in time.
    if flick_on && !locked {
        ring(dz * r, r, FLICK_COL.gamma_multiply(0.24 + 0.3 * left));
        if fwd > 0.0 {
            band(dz * r, r, -fwd, fwd, FWD_COL.gamma_multiply(0.6), FWD_COL.gamma_multiply(0.6));
        }
        painter.circle_stroke(c, dz * r, egui::Stroke::new(1.0_f32, FLICK_COL.gamma_multiply(0.85)));
        // The release floor a held flick follows the stick down to.
        let floor = flexinput_engine::eval::rws_flick_release_floor(dz) * r;
        let mut pts = arc(floor, 0.0, 360.0);
        pts.push(pts[0]);
        painter.extend(egui::Shape::dashed_line(&pts, egui::Stroke::new(1.0_f32, weak.gamma_multiply(0.8)), 3.0, 3.0));
    } else if flick_on {
        // Shut for this push: hatched grey.
        ring(dz * r, r, weak.gamma_multiply(0.14));
        let mut a = 0.0;
        while a < 360.0 {
            painter.line_segment([c + toward(a) * dz * r, c + toward(a + 25.0) * r], egui::Stroke::new(1.0_f32, weak.gamma_multiply(0.35)));
            a += 20.0;
        }
    }
    if gated {
        painter.circle_stroke(c, flexinput_engine::eval::RWS_GESTURE_OUT * r, egui::Stroke::new(1.0_f32, GATE_COL.gamma_multiply(0.7)));
    }
    painter.circle_stroke(c, r, egui::Stroke::new(1.0_f32, line));
    // Straight up, the direction a flick is measured from: a notch on the ring.
    painter.line_segment([c + toward(0.0) * (r - 4.0), c + toward(0.0) * (r + 3.0)], egui::Stroke::new(1.5_f32, line));

    // Where the parts are, in words, where there is room for them.
    let small = crate::settings::ui_font((font_px - 1.0).max(8.0));
    if flick_on && (1.0 - dz) * r >= font_px + 1.0 {
        painter.text(c + egui::vec2(0.0, (1.0 + dz) * 0.5 * r), egui::Align2::CENTER_CENTER, if locked { "shut" } else { "flick" }, small.clone(), weak);
    }
    if aim_on && r >= 48.0 {
        let y = if flick_on { dz * r * 0.62 } else { r * 0.62 };
        painter.text(c + egui::vec2(0.0, y), egui::Align2::CENTER_CENTER, "aim", small.clone(), weak);
    }

    let now = ui.input(|i| i.time);
    let mut animating = deciding;
    // The last flick, fading after it lets go.
    // Kept per circle: the same node can be drawn in the body and in pins at once.
    let ghost_key = ui.id().with(("rws_flick_ghost", node_id.0));
    if v.engaged {
        ui.ctx().data_mut(|d| d.insert_temp(ghost_key, (now, v.heading_deg, v.turned_deg, v.no_snap)));
    }
    let flick = |heading: f32, turned: f32, owed: f32, alpha: f32, no_snap: bool| {
        let col = if no_snap { FWD_COL } else { FLICK_COL }.gamma_multiply(alpha);
        let turned = turned.clamp(-360.0, 360.0);
        let owed = owed.clamp(-turned.abs(), turned.abs());
        let (start, done) = (heading - turned, heading - owed);
        let rad = r + 2.5;
        if turned.abs() > 0.5 {
            painter.add(egui::Shape::line(arc(rad, start, done), egui::Stroke::new(3.0_f32, col)));
            // Still to turn: dashed, on to the arrow.
            if owed.abs() > 0.5 {
                painter.extend(egui::Shape::dashed_line(&arc(rad, done, heading), egui::Stroke::new(2.0_f32, col.gamma_multiply(0.8)), 4.0, 3.0));
            }
        }
        let d = toward(heading);
        let across = egui::vec2(-d.y, d.x);
        painter.line_segment([c, c + d * r], egui::Stroke::new(1.0_f32, col.gamma_multiply(0.5)));
        let base = c + d * (r + 3.5);
        let tip = c + d * (r + margin);
        let half = margin * 0.6;
        painter.add(egui::Shape::convex_polygon(vec![tip, base + across * half, base - across * half], col, egui::Stroke::NONE));
    };
    if v.engaged {
        flick(v.heading_deg, v.turned_deg, v.owed_deg, if v.dipping { 0.6 } else { 1.0 }, v.no_snap);
    } else if let Some((t, heading, turned, no_snap)) = ui.ctx().data(|d| d.get_temp::<(f64, f32, f32, bool)>(ghost_key)) {
        let age = (now - t) as f32;
        if age < 0.7 {
            flick(heading, turned, 0.0, 0.5 * (1.0 - age / 0.7), no_snap);
            animating = true;
        }
    }

    // The stick, coloured by what it is doing.
    let m = v.stick.length();
    let in_aim = aim_on && m > 0.02 && (!flick_on || m < dz || locked);
    let (dot, filled) = if live.is_none() || v.measuring {
        (weak, false)
    } else if v.engaged {
        (if v.no_snap { FWD_COL } else { FLICK_COL }, !v.dipping)
    } else if deciding {
        (GATE_COL, true)
    } else if v.gesture == RwsGesture::Flicked {
        (FLICK_COL, false)
    } else if in_aim || (v.gesture == RwsGesture::Steering && aim_on) {
        (AIM_COL, true)
    } else if flick_on && m >= dz {
        (FLICK_COL, false)
    } else {
        (weak, false)
    };
    // A short trail behind it, so a push's speed shows.
    let trail_key = ui.id().with(("rws_flick_trail", node_id.0));
    let mut trail: Vec<(f64, glam::Vec2)> = ui.ctx().data(|d| d.get_temp(trail_key)).unwrap_or_default();
    trail.retain(|(t, _)| now - t < 0.25);
    if live.is_some() && trail.last().is_none_or(|(_, p)| (*p - v.stick).length() > 1e-3) {
        trail.push((now, v.stick));
    }
    let clamp = |p: glam::Vec2| if p.length() > 1.05 { p / p.length() * 1.05 } else { p };
    for pair in trail.windows(2) {
        let age = (now - pair[1].0) as f32 / 0.25;
        painter.line_segment([at(clamp(pair[0].1)), at(clamp(pair[1].1))], egui::Stroke::new(2.0_f32, dot.gamma_multiply(0.5 * (1.0 - age).max(0.0))));
    }
    animating |= trail.len() > 1;
    ui.ctx().data_mut(|d| d.insert_temp(trail_key, trail));
    let p = at(clamp(v.stick));
    if m > 0.02 {
        painter.line_segment([c, p], egui::Stroke::new(1.0_f32, dot.gamma_multiply(0.45)));
    }
    let dot_r = (r * 0.06).clamp(3.5, 6.0);
    if filled {
        painter.circle_filled(p, dot_r, dot);
    } else {
        painter.circle_stroke(p, dot_r, egui::Stroke::new(1.5_f32, dot));
    }
    // The speed gate counting down: the ring round the dot empties as the time runs out.
    if deciding {
        let rr = dot_r + 3.5;
        painter.circle_stroke(p, rr, egui::Stroke::new(1.0_f32, GATE_COL.gamma_multiply(0.3)));
        let n = 24;
        let pts: Vec<egui::Pos2> = (0..=n)
            .map(|i| {
                let a = (360.0 * left * i as f32 / n as f32).to_radians();
                p + egui::vec2(a.sin(), -a.cos()) * rr
            })
            .collect();
        painter.add(egui::Shape::line(pts, egui::Stroke::new(2.0_f32, GATE_COL)));
    }

    // In words.
    let status = if live.is_none() {
        "—".to_string()
    } else if v.measuring {
        "calibration sweep".to_string()
    } else if !flick_on && !aim_on {
        if reaches { "off: Flick On input" } else { "flick off" }.to_string()
    } else if v.engaged {
        if v.dipping {
            "letting go…".to_string()
        } else if v.no_snap {
            format!("turning {:+.0}° · no snap", v.turned_deg)
        } else {
            format!("flick {:+.0}°", v.turned_deg)
        }
    } else if deciding {
        format!("flick? {:.0} of {:.0} ms", v.gate_s * 1000.0, speed_s * 1000.0)
    } else if v.gesture == RwsGesture::Steering {
        if locked { "steering · zone shut" } else { "steering" }.to_string()
    } else if v.gesture == RwsGesture::Flicked {
        "flicked · to centre".to_string()
    } else if in_aim {
        let rate = if flick_on { m / dz } else { m };
        format!("aim {:.0}%{}", rate.min(1.0) * 100.0, if flick_on { "" } else { " · flick off" })
    } else if !flick_on {
        "stick aim only".to_string()
    } else {
        "centre".to_string()
    };
    painter.text(
        egui::pos2(rect.center().x, rect.bottom() - 2.0),
        egui::Align2::CENTER_BOTTOM,
        status,
        crate::settings::ui_font(font_px),
        if live.is_some() && (v.engaged || deciding || m > 0.02) { visuals.text_color() } else { weak },
    );
    if animating {
        ui.ctx().request_repaint();
    }
    rect
}

/// A pinned RWS row in the new style - `style` or one of [`FLICK_ROWS`] - laid out across
/// the pin's frame. See `pin_style2`.
pub(crate) fn render_rws_row_style2(
    node_id: NodeId,
    element: &str,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (bg_alpha, tick_deg, labels) = rws_field_style(snarl, node_id);
    let mode = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get("field_mode").and_then(|v| v.as_str()))
        .unwrap_or("ruler")
        .to_string();
    let fov = snarl.get_node(node_id).and_then(|n| n.params.get("field_fov").and_then(|v| v.as_f64())).unwrap_or(90.0) as f32;
    // Drawn inside `pin_style2::fit`, at the zoom it set, the frame's width wide.
    let _ = container;
    let (set, fields) = rws_body_settings(node_id, ui, snarl, &mode, fov, bg_alpha, tick_deg, labels, Some(&[element]), false);
    publish_nav_field_rects(ui, node_id, &fields);
    if !set.is_empty() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            for (k, v) in set {
                node.params.insert(k.to_string(), v);
            }
        }
        mark_overlay_param_write(ui.ctx());
    }
}

/// Whether the flick reaches anything, from the module's own settings and
/// wiring: merged into the outputs ("Mouse + Stick"), or through a wired Flick
/// pin. The engine decides the same from the same two facts
/// (`rws_stick_uses`), so the body can say when the flick is off and why.
fn rws_flick_reaches(snarl: &Snarl<NodeData>, node_id: NodeId) -> bool {
    let Some(n) = snarl.get_node(node_id) else { return false };
    flexinput_engine::eval::rws_flick_mode(&n.params) == "both"
        || !snarl.out_pin(OutPinId { node: node_id, output: 2 }).remotes.is_empty()
}

/// A line under the flick rows when the flick is off because its output goes
/// nowhere — there is no checkbox for it any more, so say what turns it on.
fn rws_flick_hint(snarl: &Snarl<NodeData>, node_id: NodeId) -> Option<&'static str> {
    (!rws_flick_reaches(snarl, node_id))
        .then_some("Flick off: wire the Flick output, or set out to Mouse + Stick")
}

/// The flick row, laid out into an existing horizontal row: the flick deadzone
/// and snap smoothing, then Stick aim and its multiplier. Returns the params to
/// write and the rects of its gamepad fields, in their order (`nav/fields.rs`):
/// Deadzone, Smooth, Stick aim, Aim ×. The multiplier is always drawn (greyed
/// while Stick aim is off) so every field has a place to glow.
fn rws_flick_row(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &Snarl<NodeData>,
) -> (Vec<(&'static str, Value)>, [egui::Rect; 4]) {
    let (flick_dz, flick_sm, aim_on, aim_rws) = snarl.get_node(node_id).map(|n| {
        (
            n.params.get("flick_deadzone").and_then(|v| v.as_f64()).unwrap_or(0.85) as f32,
            n.params.get("flick_smooth_ms").and_then(|v| v.as_f64()).unwrap_or(100.0) as f32,
            n.params.get("stick_aim_enabled").and_then(|v| v.as_bool()).unwrap_or(false),
            n.params.get("stick_aim_rws").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
        )
    }).unwrap_or((0.85, 100.0, false, 1.0));
    let mut set: Vec<(&'static str, Value)> = Vec::new();
    let mut fr = [egui::Rect::NOTHING; 4];
    ui.label(egui::RichText::new("Flick dz").small().weak())
        .on_hover_text("Flick stick, on the stick wired to the Flick input: push it past the\ndeadzone to snap the camera to that direction; hold it out and rotate\nto track. Flicks are 1:1 (RWS does not apply). On wherever its output\ngoes (out, below); the Flick On input turns it off for a mode shift.");
    let mut dz = flick_dz;
    let r = ui.add(egui::DragValue::new(&mut dz).speed(0.01).range(0.1..=0.99))
        .on_hover_text("Deadzone — stick magnitude needed to engage a flick.");
    if r.changed() {
        if let Some(n) = Number::from_f64(dz as f64) { set.push(("flick_deadzone", Value::Number(n))); }
    }
    fr[0] = r.rect;
    ui.label(egui::RichText::new("smooth").small().weak());
    let mut sm = flick_sm;
    let r = ui.add(egui::DragValue::new(&mut sm).speed(1.0).range(0.0..=500.0).suffix(" ms"))
        .on_hover_text("Smoothing window for the initial flick snap (0 = instant).");
    if r.changed() {
        if let Some(n) = Number::from_f64(sm as f64) { set.push(("flick_smooth_ms", Value::Number(n))); }
    }
    fr[1] = r.rect;
    // Stick-aim — with the flick on it lives inside the deadzone; with it off,
    // it uses the full stick range.
    let mut ae = aim_on;
    let r = ui.checkbox(&mut ae, egui::RichText::new("Stick aim").small())
        .on_hover_text("Use the stick wired to the Flick input as a rate aim feeding BOTH\noutputs (its own RWS + the stick V/H bias). With the flick on it acts\nINSIDE the deadzone (past → flick); with it off it uses the full stick\nrange. The stick still reaches its default mapping unless\n“Suppress flick stick” blocks it. The Flick On input turns it off too.");
    if r.changed() {
        set.push(("stick_aim_enabled", Value::Bool(ae)));
    }
    fr[2] = r.rect;
    let mut ar = aim_rws;
    let r = ui.add_enabled(aim_on, egui::DragValue::new(&mut ar).speed(0.01).range(0.01..=50.0).prefix("×"))
        .on_hover_text("Stick-aim RWS multiplier (independent of the main RWS).");
    if r.changed() {
        if let Some(n) = Number::from_f64(ar as f64) { set.push(("stick_aim_rws", Value::Number(n))); }
    }
    fr[3] = r.rect;
    (set, fr)
}

/// Flick row (deadzone, smoothing, Stick aim), as a standalone pinnable element.
pub(crate) fn render_rws_flick(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(260.0, 22.0));
    let (set, fr) = ui.horizontal(|ui| rws_flick_row(node_id, ui, snarl)).inner;
    publish_nav_field_rects(ui, node_id, &fr);
    if !set.is_empty() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            for (k, v) in set { node.params.insert(k.to_string(), v); }
        }
    }
}

/// The flick's options, laid out into an existing horizontal row: the tracking
/// stabiliser (holds back a thumb's tremor while the stick is held out), the
/// forward deadzone (a flick that engages within ±this many degrees of straight up
/// doesn't snap — it only primes the stick for rotating) and where the flick goes
/// (the Flick pin alone as mouse counts or stick deflection, or merged into the
/// Mouse and Stick outputs). Returns the params to write and the rects of its
/// gamepad fields, in their order: Stabilise, Fwd, Flick out.
fn rws_flick_opts_row(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &Snarl<NodeData>,
) -> (Vec<(&'static str, Value)>, [egui::Rect; 3]) {
    let (stab, fwd, out) = snarl.get_node(node_id).map(|n| (
        n.params.get("flick_stabilise_ms").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
        n.params.get("flick_fwd_dz_deg").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
        flexinput_engine::eval::rws_flick_mode(&n.params),
    )).unwrap_or((0.0, 0.0, "mouse"));
    let mut set: Vec<(&'static str, Value)> = Vec::new();
    let mut fr = [egui::Rect::NOTHING; 3];
    ui.label(egui::RichText::new("stab").small().weak());
    let mut s = stab;
    let r = ui.add(egui::DragValue::new(&mut s).speed(1.0).range(0.0..=300.0).suffix(" ms"))
        .on_hover_text("Stabilise: hold back a thumb's tremor while the stick is held out\nand rotated. A deliberate sweep opens it up; whatever it is still\nholding back when you let go is turned then, so the camera still\nends where the stick pointed. 0 = off.");
    if r.changed() {
        if let Some(n) = Number::from_f64(s.round() as f64) { set.push(("flick_stabilise_ms", Value::Number(n))); }
    }
    fr[0] = r.rect;
    ui.label(egui::RichText::new("fwd ±").small().weak());
    let mut f = fwd;
    let r = ui.add(egui::DragValue::new(&mut f).speed(1.0).range(0.0..=90.0).suffix("°"))
        .on_hover_text("Forward deadzone: a flick that engages within this many degrees of\nstraight up doesn't snap the camera — the stick only engages, so\nrotating it from there turns the camera (e.g. a 360° with the stick).\n0 = every flick snaps.");
    if r.changed() {
        if let Some(n) = Number::from_f64(f.round() as f64) { set.push(("flick_fwd_dz_deg", Value::Number(n))); }
    }
    fr[1] = r.rect;
    ui.label(egui::RichText::new("out").small().weak());
    let r = egui::ComboBox::from_id_salt((node_id, "rws_flick_out"))
        .selected_text(rws_flick_out_label(out))
        .width(116.0)
        .show_ui(ui, |ui| {
            for val in RWS_FLICK_OUT_OPTS {
                if ui.selectable_label(out == *val, rws_flick_out_label(val)).clicked() {
                    set.push(("flick_output", Value::String(val.to_string())));
                }
            }
        })
        .response
        .on_hover_text("Where the flick goes.\n• Flick pin · mouse — ONLY the Flick output, in mouse counts (Mouse Scale).\n• Flick pin · stick — ONLY the Flick output, as stick deflection (Stick °/s).\n  Past full tilt the rest of the turn carries over to the next ticks,\n  so a flick still lands where you pointed, just over a longer time.\n• Mouse + Stick — added into BOTH the Mouse Move and Stick outputs.\nWith a Flick pin option and the pin unwired, the flick is off.");
    fr[2] = r.rect;
    (set, fr)
}

/// The speed gate, laid out into an existing horizontal row: how fast a push
/// out of the centre must reach the flick zone to flick (0 = off), and whether
/// a slower push keeps the flick zone off until the stick is back at the centre.
/// Returns the params to write and the rects of its gamepad fields, in their
/// order: Speed, Slow lock.
fn rws_flick_speed_row(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &Snarl<NodeData>,
) -> (Vec<(&'static str, Value)>, [egui::Rect; 2]) {
    let (speed, lock) = snarl.get_node(node_id).map(|n| (
        n.params.get("flick_speed_ms").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
        n.params.get("flick_slow_lock").and_then(|v| v.as_bool()).unwrap_or(false),
    )).unwrap_or((0.0, false));
    let mut set: Vec<(&'static str, Value)> = Vec::new();
    let mut fr = [egui::Rect::NOTHING; 2];
    ui.label(egui::RichText::new("flick within").small().weak());
    let mut s = speed;
    let r = ui.add(egui::DragValue::new(&mut s).speed(1.0).range(0.0..=300.0).suffix(" ms"))
        .on_hover_text("Speed gate between Stick aim and the flick. Every push out of the\ncentre is judged by how fast it reaches the flick deadzone:\n• within this time — a flick. The Stick aim it made on the way out is\n  thrown away, so the flick lands clean, and Stick aim stays off until\n  the stick is back at the centre.\n• slower — steering. The aim held back while deciding catches up\n  over the flick's smoothing time, so none of it is lost.\n0 = off (the deadzone alone decides).");
    if r.changed() {
        if let Some(n) = Number::from_f64(s.round() as f64) { set.push(("flick_speed_ms", Value::Number(n))); }
    }
    fr[0] = r.rect;
    let mut l = lock;
    let r = ui.add_enabled(speed > 0.0, egui::Checkbox::new(&mut l, egui::RichText::new("slow = aim only").small()))
        .on_hover_text("A push too slow to flick keeps the flick zone off until the stick is\nback at the centre: out past the deadzone it just aims, at full rate.\nSo the stick steers like a normal aim stick, and a fast push still\nflicks.");
    if r.changed() {
        set.push(("flick_slow_lock", Value::Bool(l)));
    }
    fr[1] = r.rect;
    (set, fr)
}

/// Speed-gate row (flick within / slow = aim only), as a standalone pinnable
/// element.
pub(crate) fn render_rws_flick_speed(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(220.0, 22.0));
    let (set, fr) = ui.horizontal(|ui| rws_flick_speed_row(node_id, ui, snarl)).inner;
    publish_nav_field_rects(ui, node_id, &fr);
    if !set.is_empty() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            for (k, v) in set { node.params.insert(k.to_string(), v); }
        }
    }
}

/// RWS Aim header toggle: this module turns the flicks mapping cards send.
pub(crate) fn rws_map_flicks_header_toggle(
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    node: NodeId,
) {
    use flexinput_core::flick::TARGET_PARAM;
    let mut on = snarl.get_node(node)
        .and_then(|n| n.params.get(TARGET_PARAM)).and_then(|v| v.as_bool()).unwrap_or(false);
    let resp = ui.checkbox(&mut on, egui::RichText::new("Map flicks here").small());
    if resp.changed() {
        if let Some(n) = snarl.get_node_mut(node) {
            n.params.insert(TARGET_PARAM.into(), Value::Bool(on));
        }
    }
    resp.on_hover_text("Turn the flicks mapping cards send. While one RWS Aim on this tab has\nthis on, Remapper, Touch Zones, Lean, Virtual Menu and Area Mapper cards\ncan output a flick by a set angle, one per press: Choose → Keyboard & Mouse\nfor a mouse flick, Choose → Gamepad for a stick flick.\nIt goes out of the Flick pin if that's wired for the same kind, else the\nMouse or Stick output, through Scale but not RWS, like the flick stick.\nTick only one module per tab: each ticked one turns every flick.");
}

/// The `flick_output` values, in the order the dropdown and the gamepad cycle
/// them. The first is the default.
pub(crate) const RWS_FLICK_OUT_OPTS: &[&str] = &["mouse", "stick", "both"];

fn rws_flick_out_label(o: &str) -> &'static str {
    match o {
        "stick" => "Flick pin · stick",
        "both" => "Mouse + Stick",
        _ => "Flick pin · mouse",
    }
}

/// The flick used to be switched on by a `flick_enabled` checkbox; it is now on
/// wherever its output goes. Ticked with the old merged output ("main", or the
/// key absent) → "both", which merges it into the outputs as before; unticked →
/// the pin-only default, inert until the Flick pin is wired — so Stick aim keeps
/// the stick's whole range, as it did. A pin mode picked since is kept.
/// Idempotent: only acts while the old key is there.
pub(crate) fn migrate_rws_flick_params(params: &mut std::collections::HashMap<String, Value>) {
    let Some(enabled) = params.remove("flick_enabled") else { return };
    let enabled = enabled.as_bool().unwrap_or(false);
    let picked = params.get("flick_output").and_then(|v| v.as_str())
        .filter(|o| *o == "mouse" || *o == "stick")
        .map(str::to_string);
    let new = match (picked, enabled) {
        (Some(o), _) => o,
        (None, true) => "both".to_string(),
        (None, false) => "mouse".to_string(),
    };
    params.insert("flick_output".into(), Value::String(new));
}

/// Flick options row (stabilise + forward deadzone + output), as a standalone pinnable
/// element.
pub(crate) fn render_rws_flick_opts(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(300.0, 22.0));
    let (set, fr) = ui.horizontal(|ui| rws_flick_opts_row(node_id, ui, snarl)).inner;
    publish_nav_field_rects(ui, node_id, &fr);
    if !set.is_empty() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            for (k, v) in set { node.params.insert(k.to_string(), v); }
        }
    }
}

/// Source-suppression dropdown (None / Left / Right / Both stick), as a
/// standalone pinnable element.
pub(crate) fn render_rws_suppress(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let suppress = snarl.get_node(node_id)
        .and_then(|n| n.params.get("suppress_source").and_then(|v| v.as_str()))
        .unwrap_or("off").to_string();
    let mut set: Vec<(&str, Value)> = Vec::new();
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(160.0, 22.0));
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Suppress").small().weak());
        egui::ComboBox::from_id_salt((node_id, "rws_pin_suppress"))
            .selected_text(match suppress.as_str() {
                "full" => "Full", "deadzone" => "In deadzone", _ => "Off",
            })
            .width(96.0)
            .show_ui(ui, |ui| {
                for (val, lbl) in [("off", "Off"), ("full", "Full"), ("deadzone", "In deadzone")] {
                    if ui.selectable_label(suppress == val, lbl).clicked() {
                        set.push(("suppress_source", Value::String(val.to_string())));
                    }
                }
            });
    });
    if !set.is_empty() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            for (k, v) in set { node.params.insert(k.to_string(), v); }
        }
    }
}

/// Mouse Scale value box as a standalone pinnable element, in the header's
/// display unit (see [`RwsScaleUnit`]).
pub(crate) fn render_rws_scale(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let scale = snarl.get_node(node_id)
        .and_then(|n| n.params.get("scale").and_then(|v| v.as_f64()))
        .unwrap_or(100.0) as f32;
    let unit = RwsScaleUnit::of(snarl, node_id);
    let mut new_scale = None;
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(150.0, 22.0));
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Mouse Scale").weak());
        let mut v = unit.shown(scale);
        // The value box is the row's flexible element (see `render_dragvalue_param`);
        // its minimum leaves room for the longer per-360 number + suffix.
        let w = pin_flex_width(ui, container, if unit.per_360() { 96.0 } else { 72.0 });
        let h = ui.spacing().interact_size.y;
        if ui.add_sized([w, h], unit.drag_value(&mut v).suffix(unit.suffix())).changed() {
            new_scale = unit.to_param(v);
        }
    });
    if let (Some(p), Some(node)) = (new_scale, snarl.get_node_mut(node_id)) {
        node.params.insert("scale".into(), p);
    }
}

/// The portable RWS "feel set" saved to / loaded from a `.fxrws` preset: the
/// calibrated constants plus the sensitivity/flick/aim knobs. Excludes transient
/// calibration state and the ruler/room view style (which is cosmetic).
pub(crate) const RWS_PRESET_KEYS: &[&str] = &[
    "scale", "rws", "stick_out_dps", "gyro_scale", "max_rate_dps",
    "gyro_vh_ratio", "stick_vh_ratio",
    "flick_deadzone", "flick_smooth_ms", "flick_fwd_dz_deg", "flick_output",
    "flick_stabilise_ms", "flick_speed_ms", "flick_slow_lock",
    "stick_aim_enabled", "stick_aim_rws", "suppress_source",
];

/// Write the node's RWS feel set to a `.fxrws` JSON preset. Includes a derived
/// `counts_per_360` (= `scale` × 360) for cross-reference with community per-game
/// values — there is no standard interchange format, so this is FlexInput-native.
pub(crate) fn rws_save_preset(
    snarl: &Snarl<NodeData>,
    node_id: NodeId,
    path: &std::path::Path,
) -> std::io::Result<()> {
    let node = snarl.get_node(node_id);
    let mut params = serde_json::Map::new();
    if let Some(node) = node {
        for k in RWS_PRESET_KEYS {
            if let Some(v) = node.params.get(*k) {
                params.insert((*k).to_string(), v.clone());
            }
        }
    }
    let scale = node
        .and_then(|n| n.params.get("scale").and_then(|v| v.as_f64()))
        .unwrap_or(100.0);
    let doc = serde_json::json!({
        "format": "flexinput-rws",
        "version": 1,
        "game": "",
        "notes": "",
        "counts_per_360": scale * 360.0,
        "params": params,
    });
    let json = serde_json::to_vec_pretty(&doc)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(path, json)
}

/// Apply a `.fxrws` preset's feel set onto the node. Only recognised keys are
/// copied, so a preset from a newer version never injects unknown params.
pub(crate) fn rws_load_preset(
    snarl: &mut Snarl<NodeData>,
    node_id: NodeId,
    path: &std::path::Path,
) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let doc: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if doc.get("format").and_then(|v| v.as_str()) != Some("flexinput-rws") {
        return Err("Not a FlexInput RWS preset".into());
    }
    let params = doc.get("params").and_then(|v| v.as_object())
        .ok_or_else(|| "preset has no params".to_string())?;
    if let Some(node) = snarl.get_node_mut(node_id) {
        for k in RWS_PRESET_KEYS {
            if let Some(v) = params.get(*k) {
                node.params.insert((*k).to_string(), v.clone());
            }
        }
        // A preset saved while the flick had its checkbox: read it the way a
        // saved node's is (see `migrate_rws_flick_params`).
        if let Some(v) = params.get("flick_enabled") {
            node.params.insert("flick_enabled".into(), v.clone());
            if !params.contains_key("flick_output") {
                node.params.remove("flick_output");
            }
            migrate_rws_flick_params(&mut node.params);
        }
    }
    Ok(())
}

// ── Measure calibration (user turns a known amount; we back-solve a constant) ──
//
// `cal_measure` drives the engine to emit only the measured axis at the BASE scale
// (rws 1, no V/H) through only the chosen `cal_output`, and to integrate the
// physical rotation at tick rate. The engine publishes that angle (and the peak
// unclamped stick deflection) as display-only trailing outputs, read here from
// `extra.last_out` — so every rendered copy of this widget (the config-overlay pin,
// an open sub-patch editor's body) sees the SAME measurement, however often each
// window repaints. On Finish:
//   Mouse → scale         = old · |angle| / target
//   Stick → stick_out_dps = old · target / |angle|
// with target 180° (pitch) or 360° (yaw).

/// Past this peak deflection the stick was pinned at full tilt for part of the
/// sweep, so the game turned slower than the gyro and a Stick back-solve would be
/// wrong. Small headroom for noise.
const RWS_STICK_SAT_LIMIT: f32 = 1.02;

fn rws_meas_msg_key(node_id: NodeId) -> egui::Id { egui::Id::new(("rws_cal_msg", node_id.0)) }

/// Ctx flag: a pinned calibration widget wrote node params outside the canvas's
/// own edit tracking (the config overlay renders in its own viewport). The config
/// overlay consumes it and bumps the tab canvas `mutation_gen`, so an open
/// sub-patch editor re-pulls the real values instead of showing — or writing back
/// — its stale copy.
fn overlay_write_id() -> egui::Id { egui::Id::new("fxi_rws_overlay_param_write") }

/// A pinned widget wrote node params from the overlay's viewport, which the
/// canvas's own edit tracking can't see. The overlay bumps the tab canvas
/// generation on this, so an open sub-patch editor re-pulls rather than writing
/// its stale copy back over the change. Set by the RWS calibration widget and by
/// the JSM editor.
pub(crate) fn mark_overlay_param_write(ctx: &egui::Context) {
    ctx.data_mut(|d| d.insert_temp(overlay_write_id(), true));
}

/// Read-and-clear the overlay write flag (see [`mark_overlay_param_write`]).
pub(crate) fn take_overlay_param_write(ctx: &egui::Context) -> bool {
    let set = ctx.data(|d| d.get_temp::<bool>(overlay_write_id())).unwrap_or(false);
    if set {
        ctx.data_mut(|d| d.insert_temp(overlay_write_id(), false));
    }
    set
}

/// The active sweep ("off"/"pitch"/"yaw"), the engine-measured angle (signed
/// degrees), and the sweep's peak unclamped stick deflection.
fn rws_measure_state(node_id: NodeId, snarl: &Snarl<NodeData>) -> (String, f32, f32) {
    let Some(node) = snarl.get_node(node_id) else { return ("off".into(), 0.0, 0.0) };
    let axis = node.params.get("cal_measure").and_then(|v| v.as_str())
        .filter(|a| *a == "pitch" || *a == "yaw")
        .unwrap_or("off")
        .to_string();
    let float_out = |i: usize| match node.extra.last_out.get(i).copied().flatten() {
        Some(Signal::Float(f)) if f.is_finite() => f,
        _ => 0.0,
    };
    let n = node.outputs.len();
    (
        axis,
        float_out(flexinput_engine::eval::rws_cal_deg_out(n)),
        float_out(flexinput_engine::eval::rws_cal_peak_out(n)),
    )
}

/// Which output the measure calibrates — chosen EXPLICITLY by the user via the
/// `cal_output` param ("mouse" | "stick"), not guessed from wiring (a patch can
/// wire both and switch between them with selectors). Mouse → `scale`
/// (counts/degree); Stick → `stick_out_dps` (turn rate at full deflection).
pub(crate) fn rws_cal_output_is_stick(snarl: &Snarl<NodeData>, node_id: NodeId) -> bool {
    snarl.get_node(node_id)
        .and_then(|n| n.params.get("cal_output").and_then(|v| v.as_str()))
        == Some("stick")
}

/// Complete the active sweep: back-solve the constant for the chosen output from
/// the engine's measured angle, stop measuring, and stash a result message
/// (old → new, or why nothing changed) the widget shows for a few seconds.
fn rws_finish_measure(
    node_id: NodeId,
    ui: &egui::Ui,
    snarl: &mut Snarl<NodeData>,
    axis: &str,
    theta: f32,
    peak_defl: f32,
) {
    let target = if axis == "yaw" { 360.0_f32 } else { 180.0 };
    let is_stick = rws_cal_output_is_stick(snarl, node_id);
    let is_gyro = snarl.get_node(node_id).and_then(|n| n.params.get("cal_output").and_then(|v| v.as_str())) == Some("gyro");
    let unit = RwsScaleUnit::of(snarl, node_id);
    let mut msg = String::new();
    if let Some(node) = snarl.get_node_mut(node_id) {
        let param = |node: &NodeData, k: &str, d: f32| {
            node.params.get(k).and_then(|v| v.as_f64()).map(|v| v as f32).unwrap_or(d)
        };
        if theta.abs() < 5.0 {
            msg = "⚠ No rotation measured — nothing changed".into();
        } else if is_stick && peak_defl > RWS_STICK_SAT_LIMIT {
            msg = format!("⚠ Turned faster than full stick ({peak_defl:.1}×) — nothing changed; turn slower and re-run");
        } else if is_stick {
            let old = param(node, "stick_out_dps", 360.0).max(1.0);
            let new = (old * target / theta.abs()).max(1.0);
            if let Some(n) = Number::from_f64(new as f64) {
                node.params.insert("stick_out_dps".into(), Value::Number(n));
            }
            msg = format!("✓ Stick °/s {old:.0} → {new:.0}");
        } else if is_gyro {
            // The game turned `target` while the Gyro output said `theta` at the old
            // scale: its gyro sensitivity is target / (theta · old), and the scale
            // that cancels it - one pad degree, one game degree - is the inverse.
            let old = param(node, "gyro_scale", 1.0);
            if old > 0.0 {
                let new = old * theta.abs() / target;
                if let Some(n) = Number::from_f64(new as f64) {
                    node.params.insert("gyro_scale".into(), Value::Number(n));
                }
                msg = format!("✓ Gyro scale {old:.3} → {new:.3}");
            } else {
                msg = "⚠ Gyro scale is 0 — set a nonzero Gyro scale first".into();
            }
        } else {
            let old = param(node, "scale", 100.0);
            if old > 0.0 {
                let new = old * theta.abs() / target;
                if let Some(n) = Number::from_f64(new as f64) {
                    node.params.insert("scale".into(), Value::Number(n));
                }
                msg = format!("✓ Mouse Scale {} → {}", unit.text(old), unit.text(new));
            } else {
                msg = "⚠ Mouse Scale is 0 — set a nonzero Mouse Scale first".into();
            }
        }
        node.params.insert("cal_measure".into(), Value::String("off".into()));
        node.params.insert("cal_finish".into(), Value::Bool(false));
    }
    let now = ui.input(|i| i.time);
    ui.ctx().data_mut(|d| d.insert_temp(rws_meas_msg_key(node_id), (msg, now)));
}

/// The measure-calibration controls. Gamepad flow (see `nav_drive_rws_measure`):
/// enter the widget with South, then ◄► pick the method, ▲▼ the output, Ⓐ start,
/// Ⓐ finish, Ⓑ cancel/back, Ⓨ toggles the 360°-only snapshot reference. Mouse
/// users click the buttons directly. A guide line spells out where to point the
/// camera; a short result message reports the outcome. Shared by the module body
/// and the pinnable element. `split_hints` puts the guide's gamepad hint on a
/// line of its own (the body, which should stay narrow) instead of appending it
/// to the instruction (the pin, whose sized frames expect one line). Returns
/// whether it wrote node params this frame.
pub(crate) fn rws_measure_controls(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    split_hints: bool,
) -> bool {
    // The instruction is prose; the hint names buttons, so it is drawn with the
    // connected pad's own glyphs (`{pin}` placeholders) rather than letters that
    // are only right on an Xbox pad. Both halves are laid out as rows either way
    // — a glyph can't be interpolated into a string — so `split_hints` now only
    // decides whether they share a row.
    let guide = |ui: &mut egui::Ui, instruction: &str, hint: &str| {
        if split_hints {
            ui.label(egui::RichText::new(instruction).small().weak());
            super::pad_hints::pad_hint(ui, hint);
        } else {
            // One row, not wrapped: the pin's sized frames were laid out for a
            // single line of guidance.
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("{instruction}  ·  ")).small().weak());
                super::pad_hints::pad_hint_inline(ui, hint);
            });
        }
    };
    let (axis, theta, peak_defl) = rws_measure_state(node_id, snarl);
    let (ref_on, pending, finish_flag, cal_output, scale, stick_dps) = snarl.get_node(node_id).map(|n| (
        n.params.get("cal_ref_shot").and_then(|v| v.as_bool()).unwrap_or(false),
        n.params.get("cal_pending").and_then(|v| v.as_str()).filter(|p| *p == "pitch" || *p == "yaw").unwrap_or("yaw").to_string(),
        n.params.get("cal_finish").and_then(|v| v.as_bool()).unwrap_or(false),
        n.params.get("cal_output").and_then(|v| v.as_str()).filter(|o| *o == "mouse" || *o == "stick").unwrap_or("mouse").to_string(),
        n.params.get("scale").and_then(|v| v.as_f64()).unwrap_or(100.0) as f32,
        n.params.get("stick_out_dps").and_then(|v| v.as_f64()).unwrap_or(360.0) as f32,
    )).unwrap_or((false, "yaw".into(), false, "mouse".into(), 100.0, 360.0));

    let measuring = axis == "pitch" || axis == "yaw";
    let is_stick = cal_output == "stick";
    let unit = RwsScaleUnit::of(snarl, node_id);
    let mut set: Vec<(&str, Value)> = Vec::new();
    let mut do_finish = false;
    let mut cancel = false;
    // The method row's rect → the config-overlay focus glow while entered.
    let mut method_rect = egui::Rect::NOTHING;

    if measuring {
        let target = if axis == "yaw" { 360.0_f32 } else { 180.0 };
        let out_lbl = if is_stick { "Stick °/s" } else { "Mouse Scale" };
        let r = ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("Measuring {axis} → {out_lbl}: {:.0}° / {:.0}°", theta.abs(), target))
                .strong().color(egui::Color32::from_rgb(255, 200, 80)));
            if ui.button(egui::RichText::new("✓ Finish").small()).clicked() { do_finish = true; }
            if ui.button(egui::RichText::new("✗ Cancel").small()).clicked() { cancel = true; }
        });
        method_rect = r.response.rect;
        if is_stick && peak_defl > RWS_STICK_SAT_LIMIT {
            ui.label(egui::RichText::new(format!("⚠ Too fast — the stick maxed out ({peak_defl:.1}×). Cancel and turn slower."))
                .small().color(egui::Color32::from_rgb(230, 150, 110)));
        }
        // The 360° also counts the stick on the Flick input circled round its edge
        // (the engine adds it to the gyro), so say so when one is wired.
        let flick_wired = !snarl.in_pin(InPinId { node: node_id, input: 1 }).remotes.is_empty();
        let instruction = match (axis.as_str(), is_stick, flick_wired) {
            ("pitch", false, _) => "Turn the camera fully UP, then Finish",
            ("pitch", true, _) => "Turn the camera fully UP (steadily, not too fast), then Finish",
            (_, false, false) => "Turn one full 360°, then Finish",
            (_, true, false) => "Turn one full 360° (steadily, not too fast), then Finish",
            (_, false, true) => "Turn one full 360° — circle the flick stick for the bulk, the pad to line up — then Finish",
            (_, true, true) => "Turn one full 360°, steadily — circle the flick stick for the bulk, the pad to line up — then Finish",
        };
        guide(ui, instruction, "{btn_south} Finish · {btn_east} Cancel");
    } else if split_hints {
        // In the body: the method and the output as two joined pills, the snapshot as a
        // switch - the value they set is the pill above, so it isn't repeated here.
        use super::curve_rows::{bands, segmented, word_toggle, band_h};
        let band = band_h(ui);
        let ctrl = band - 4.0;
        let w = ui.available_width();
        let mins = [150.0, 130.0, if pending == "yaw" { 90.0 } else { 0.0 }];
        let mins: Vec<f32> = mins.iter().copied().filter(|m| *m > 0.0).collect();
        let (cells, h) = bands(w, &mins, band, 3.0 * super::kit::GAP);
        let (area, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
        let cell = |i: usize| {
            let c = cells[i].translate(area.min.to_vec2());
            egui::Rect::from_min_size(egui::pos2(c.left(), c.center().y - ctrl * 0.5), egui::vec2(c.width(), ctrl))
        };
        // Either method starts its sweep; the one the pad would start is lit.
        if let Some(m) = segmented(
            ui,
            cell(0),
            &pending,
            &[
                ("pitch".to_string(), "cal ↕180°", "Vertical: aim straight DOWN, then turn straight UP (horizontal blocked)."),
                ("yaw".to_string(), "cal ↔360°", "Horizontal: turn one full 360° (vertical blocked). Circling the stick on the Flick input round its edge turns it too."),
            ],
            ("rws_cal_method", node_id),
        ) {
            set.push(("cal_measure", Value::String(m)));
        } else {
            // A click on the lit one starts it too.
            let lit = if pending == "pitch" { 0 } else { 1 };
            let half = cell(0).width() / 2.0;
            let r = egui::Rect::from_min_size(egui::pos2(cell(0).left() + lit as f32 * half, cell(0).top()), egui::vec2(half, ctrl));
            if ui.rect_contains_pointer(r) && ui.input(|i| i.pointer.primary_clicked()) {
                set.push(("cal_measure", Value::String(pending.clone())));
            }
        }
        method_rect = cell(0);
        if let Some(o) = segmented(
            ui,
            cell(1),
            &cal_output,
            &[
                ("mouse".to_string(), "mouse", "Calibrate the Mouse Move (XY) output (Mouse Scale, counts/degree)."),
                ("stick".to_string(), "stick", "Calibrate the Stick output (°/s at full deflection)."),
            ],
            ("rws_cal_out", node_id),
        ) {
            set.push(("cal_output", Value::String(o)));
        }
        if pending == "yaw" {
            let mut shot = ref_on;
            if word_toggle(ui, cell(2), &mut shot, "snapshot",
                "Freeze the game frame behind the overlay at sweep start and show its left half at 70% as an \
                 alignment reference for the full 360° turn.", ("rws_cal_shot", node_id))
            {
                set.push(("cal_ref_shot", Value::Bool(shot)));
            }
        }
        let _ = (scale, stick_dps, &unit);
        // Recent result message (a few seconds after a Finish).
        let msg = ui.ctx().data(|d| d.get_temp::<(String, f64)>(rws_meas_msg_key(node_id)));
        if let Some((m, t)) = msg {
            if ui.input(|i| i.time) - t < 5.0 {
                let col = if m.starts_with('✓') { egui::Color32::from_rgb(150, 220, 150) } else { egui::Color32::from_rgb(230, 180, 120) };
                ui.label(egui::RichText::new(m).small().color(col));
            }
        }
        if pending == "pitch" {
            guide(ui, "Aim the camera straight DOWN first",
                "{btn_south} Start · {dpad_left}{dpad_right} method · {dpad_up}{dpad_down} output");
        } else {
            guide(ui, "Aim straight ahead first",
                "{btn_south} Start · {dpad_left}{dpad_right} method · {dpad_up}{dpad_down} output · {btn_north} snapshot");
        }
        if !is_stick {
            ui.label(egui::RichText::new(RWS_LOW_SENS_TIP).small().weak());
        }
    } else {
        let r = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Auto-cal").small().weak())
                .on_hover_text("Turn the camera a known amount; the constant for the chosen output\nis solved from the measured rotation.\n\nNo reference value for this game? Set a LOW in-game mouse sensitivity\nfirst: in most games it sets the turn per mouse dot, so lower means\nfiner angular resolution (more dots per 360°).");
            // Method buttons; the gamepad-highlighted (pending) one is marked.
            let r_p = ui.selectable_label(pending == "pitch", egui::RichText::new("↕180°").small())
                .on_hover_text("Vertical: aim straight DOWN, then turn straight UP (horizontal blocked).");
            let r_y = ui.selectable_label(pending == "yaw", egui::RichText::new("↔360°").small())
                .on_hover_text("Horizontal: turn one full 360° (vertical blocked). Circling the\nstick on the Flick input round its edge turns it too, on top of the gyro.");
            if r_p.clicked() { set.push(("cal_measure", Value::String("pitch".into()))); }
            if r_y.clicked() { set.push(("cal_measure", Value::String("yaw".into()))); }
            method_rect = r_p.rect.union(r_y.rect);
            ui.separator();
            // Which OUTPUT to calibrate — explicit, because a patch may wire both
            // and switch between them with selectors (auto-detect can't know).
            ui.label(egui::RichText::new("out").small().weak());
            if ui.selectable_label(!is_stick, egui::RichText::new("Mouse").small())
                .on_hover_text("Calibrate the Mouse Move (XY) output (Mouse Scale, counts/degree).")
                .clicked()
            {
                set.push(("cal_output", Value::String("mouse".into())));
            }
            if ui.selectable_label(is_stick, egui::RichText::new("Stick").small())
                .on_hover_text("Calibrate the Stick output (°/s at full deflection).")
                .clicked()
            {
                set.push(("cal_output", Value::String("stick".into())));
            }
            // The constant this output uses right now, so a Finish visibly moves it.
            let cur = if is_stick { format!("= {stick_dps:.0} °/s") } else { format!("= {}", unit.text(scale)) };
            ui.label(egui::RichText::new(cur).small().weak());
            // Snapshot comparison — only meaningful for the 360° horizontal method.
            if pending == "yaw" {
                let mut shot = ref_on;
                if ui.checkbox(&mut shot, egui::RichText::new("snapshot").small())
                    .on_hover_text("Freeze the game frame behind the overlay at sweep start and show its\nleft half at 70% as an alignment reference for the full 360° turn.")
                    .changed()
                {
                    set.push(("cal_ref_shot", Value::Bool(shot)));
                }
            }
        });
        if method_rect == egui::Rect::NOTHING { method_rect = r.response.rect; }
        // Recent result message (a few seconds after a Finish).
        let msg = ui.ctx().data(|d| d.get_temp::<(String, f64)>(rws_meas_msg_key(node_id)));
        if let Some((m, t)) = msg {
            if ui.input(|i| i.time) - t < 5.0 {
                let col = if m.starts_with('✓') { egui::Color32::from_rgb(150, 220, 150) } else { egui::Color32::from_rgb(230, 180, 120) };
                ui.label(egui::RichText::new(m).small().color(col));
            }
        }
        if pending == "pitch" {
            guide(ui, "Aim the camera straight DOWN first",
                "{btn_south} Start · {dpad_left}{dpad_right} method · {dpad_up}{dpad_down} output");
        } else {
            guide(ui, "Aim straight ahead first",
                "{btn_south} Start · {dpad_left}{dpad_right} method ·                  {dpad_up}{dpad_down} output · {btn_north} snapshot");
        }
        // Mouse only: the in-game mouse sensitivity sets the angular resolution.
        if !is_stick {
            ui.label(egui::RichText::new(RWS_LOW_SENS_TIP).small().weak());
        }
    }

    publish_nav_field_rects(ui, node_id, &[method_rect]);

    if finish_flag || do_finish {
        if measuring {
            rws_finish_measure(node_id, ui, snarl, &axis, theta, peak_defl);
        } else if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("cal_finish".into(), Value::Bool(false));
        }
        true
    } else if cancel {
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert("cal_measure".into(), Value::String("off".into()));
        }
        true
    } else if !set.is_empty() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            for (k, v) in set { node.params.insert(k.to_string(), v); }
        }
        true
    } else {
        false
    }
}

/// Measure-calibration controls as a standalone pinnable element. Writes made
/// here (config overlay) are flagged so open sub-patch editors re-sync.
pub(crate) fn render_rws_measure(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(210.0, 24.0));
    if rws_measure_controls(node_id, ui, snarl, false) {
        mark_overlay_param_write(ui.ctx());
    }
}

/// V/H bias (gyro + stick ratios) as a standalone pinnable element. Publishes
/// both field rects so the config-overlay gamepad glow lands on each control.
pub(crate) fn render_rws_vh(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (gyro_vh, stick_vh) = snarl.get_node(node_id).map(|n| (
        n.params.get("gyro_vh_ratio").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
        n.params.get("stick_vh_ratio").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
    )).unwrap_or((1.0, 1.0));
    let mut set: Vec<(&str, Value)> = Vec::new();
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(200.0, 22.0));
    let mut fr = [egui::Rect::NOTHING; 2];
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("V/H").small().weak());
        ui.label(egui::RichText::new("gyro").small().weak());
        let mut g = gyro_vh;
        let r0 = ui.add(egui::DragValue::new(&mut g).speed(0.01).range(0.0..=8.0));
        if r0.changed() { if let Some(n) = Number::from_f64(g as f64) { set.push(("gyro_vh_ratio", Value::Number(n))); } }
        fr[0] = r0.rect;
        ui.label(egui::RichText::new("stick").small().weak());
        let mut s = stick_vh;
        let r1 = ui.add(egui::DragValue::new(&mut s).speed(0.01).range(0.0..=8.0));
        if r1.changed() { if let Some(n) = Number::from_f64(s as f64) { set.push(("stick_vh_ratio", Value::Number(n))); } }
        fr[1] = r1.rect;
    });
    publish_nav_field_rects(ui, node_id, &fr);
    if !set.is_empty() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            for (k, v) in set { node.params.insert(k.to_string(), v); }
        }
    }
}

/// Ruler style row (BG opacity / tick spacing / labels), as a standalone
/// pinnable element.
pub(crate) fn render_rws_style(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (bg_alpha, tick_deg, _) = rws_field_style(snarl, node_id);
    let (mode, fov) = snarl.get_node(node_id).map(|n| {
        (
            n.params.get("field_mode").and_then(|v| v.as_str()).unwrap_or("ruler").to_string(),
            n.params.get("field_fov").and_then(|v| v.as_f64()).unwrap_or(90.0) as f32,
        )
    }).unwrap_or(("ruler".to_string(), 90.0));
    let mut set: Vec<(&str, Value)> = Vec::new();
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(200.0, 22.0));
    ui.horizontal(|ui| {
        for (val, lbl) in [("ruler", "Ruler"), ("room", "Room"), ("both", "Both")] {
            if ui.selectable_label(mode == val, egui::RichText::new(lbl).small()).clicked() {
                set.push(("field_mode", Value::String(val.to_string())));
            }
        }
        let mut a = bg_alpha;
        if ui.add(egui::DragValue::new(&mut a).speed(0.01).range(0.0..=1.0).prefix("BG ")).changed() {
            if let Some(n) = Number::from_f64(a as f64) { set.push(("field_bg_alpha", Value::Number(n))); }
        }
        if mode != "ruler" {
            let mut fv = fov;
            if ui.add(egui::DragValue::new(&mut fv).speed(1.0).range(30.0..=140.0).prefix("FOV ").suffix("°")).changed() {
                if let Some(n) = Number::from_f64(fv as f64) { set.push(("field_fov", Value::Number(n))); }
            }
        }
        if mode != "room" {
            let mut td = tick_deg;
            if ui.add(egui::DragValue::new(&mut td).speed(1.0).range(5.0..=90.0).suffix("°")).changed() {
                if let Some(n) = Number::from_f64(td as f64) { set.push(("field_tick_deg", Value::Number(n))); }
            }
        }
    });
    if !set.is_empty() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            for (k, v) in set { node.params.insert(k.to_string(), v); }
        }
    }
}

/// Draw the ruler/room reference viewport: a degree ruler with a fixed centre
/// marker (and/or the 3D room) that follows the module's live yaw output
/// (recovered from the input rotation rate) so the user can eyeball that the
/// in-game turn tracks 1:1. Background is transparent by default
/// (`field_bg_alpha` = 0). Returns the allocated rect so the body can register it
/// as pinnable.
pub(crate) fn render_rws_field(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
    is_pinned: bool,
) -> egui::Rect {
    let (scale, rws) = snarl
        .get_node(node_id)
        .map(|n| {
            (
                n.params.get("scale").and_then(|v| v.as_f64()).unwrap_or(100.0) as f32,
                n.params.get("rws").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
            )
        })
        .unwrap_or((100.0, 1.0));
    let (bg_alpha, tick_deg, labels) = rws_field_style(snarl, node_id);
    let (mode, fov) = snarl
        .get_node(node_id)
        .map(|n| {
            (
                n.params.get("field_mode").and_then(|v| v.as_str()).unwrap_or("ruler").to_string(),
                n.params.get("field_fov").and_then(|v| v.as_f64()).unwrap_or(90.0) as f32,
            )
        })
        .unwrap_or(("ruler".to_string(), 90.0));

    let size = egui::vec2(container.x.max(80.0), container.y.max(48.0));
    let (rect, _resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    let vis_rect = rect.intersect(ui.clip_rect());
    if vis_rect.width() < 2.0 || vis_rect.height() < 2.0 {
        return rect;
    }
    let painter = ui.painter_at(vis_rect);

    // Background — transparent by default; opaque only if the user dials it up.
    if bg_alpha > 0.001 {
        let a = (bg_alpha * 255.0).round().clamp(0.0, 255.0) as u8;
        painter.rect_filled(rect, 4.0, egui::Color32::from_rgba_unmultiplied(16, 18, 24, a));
    }

    // Live phase (degrees), persisted per layer + node: rotate the reference at
    // the SAME rate the aim OUTPUT produces (RWS applied) so you can eyeball that
    // the room/ruler tracks the in-game turn. Read the input rotation RATE from
    // the wire source (a rate, so integrating by the UI dt is cadence-independent,
    // unlike the per-tick output displacement) and interpret it as compute_rws does.
    let dt = ui.input(|i| i.stable_dt).clamp(0.0, 0.1);
    let key = egui::Id::new(("rws_field_phase", ui.layer_id().id, node_id.0));
    let mut phase = ui.ctx().data(|d| d.get_temp::<f32>(key)).unwrap_or(0.0);
    let rate = snarl
        .in_pin(InPinId { node: node_id, input: 0 })
        .remotes
        .first()
        .copied()
        .and_then(|src| snarl.get_node(src.node).and_then(|n| n.extra.last_out.get(src.output).copied().flatten()))
        .map(|s| match s {
            Signal::Vec2(v) => v.x,
            Signal::Float(f) => f,
            _ => 0.0,
        })
        .unwrap_or(0.0);
    let k = GYRO_REF_DPS;
    phase += rate * k * rws * dt;
    if !phase.is_finite() {
        phase = 0.0;
    }
    phase = phase.rem_euclid(360.0);
    ui.ctx().data_mut(|d| d.insert_temp(key, phase));

    let cx = rect.center().x;
    let show_room = mode == "room" || mode == "both";
    let show_ruler = mode == "ruler" || mode == "both";
    if show_room {
        // 3D cube-room interior: a stronger rotation reference than the flat
        // ruler. Rotates at exactly `phase` (scale-independent), FOV-matched to
        // the game so the visual turn rate matches when Scale is calibrated.
        paint_rws_room(&painter, rect, phase, fov, bg_alpha);
    }
    if show_ruler {
        // In "both" mode the ruler is a centred band across the room (aligned
        // with the centre reference marker); otherwise it fills the field.
        let ruler_rect = if mode == "both" {
            let h = (rect.height() * 0.4).clamp(24.0, 60.0);
            egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), h))
        } else {
            rect
        };
        paint_rws_ruler(&painter, ruler_rect, phase, tick_deg, labels, mode == "both");
    }

    // Fixed centre reference marker (where the camera points "now").
    painter.line_segment(
        [egui::pos2(cx, rect.top() + 2.0), egui::pos2(cx, rect.bottom() - 2.0)],
        egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(120, 200, 255)),
    );

    // Status label (bottom-left).
    painter.text(
        egui::pos2(rect.left() + 4.0, rect.bottom() - 3.0),
        egui::Align2::LEFT_BOTTOM,
        "live",
        crate::settings::ui_font(9.0),
        crate::settings::tinted_gray(120),
    );

    // Editable Scale box, overlaid top-centre (the "value box in the middle").
    // Only on the pinned overlay reference: the module body already has its own
    // Scale row, and `ui.put`'s absolute placement would disrupt the body's
    // vertical layout (pushing the following rows over the viewport).
    if is_pinned {
        let box_w = (rect.width() * 0.5).clamp(56.0, 120.0);
        let box_h = (rect.height() * 0.3).clamp(18.0, 24.0);
        let box_rect = egui::Rect::from_center_size(
            egui::pos2(cx, rect.top() + box_h * 0.5 + 3.0),
            egui::vec2(box_w, box_h),
        );
        let unit = RwsScaleUnit::of(snarl, node_id);
        let mut sv = unit.shown(scale);
        if ui
            .put(box_rect, unit.drag_value(&mut sv))
            .on_hover_text(format!(
                "Calibrated Mouse Scale (mouse dots {}).\nDrag until the game matches this reference's spin.",
                unit.per_text()))
            .changed()
        {
            if let (Some(n), Some(p)) = (snarl.get_node_mut(node_id), unit.to_param(sv)) {
                n.params.insert("scale".into(), p);
            }
        }
    }

    ui.ctx().request_repaint();
    rect
}

/// Draw the flat degree ruler into `rect`: a horizontal scale scrolling under
/// the field centre. `compact` shrinks the ticks for the "both"-mode strip.
fn paint_rws_ruler(
    painter: &egui::Painter,
    rect: egui::Rect,
    phase: f32,
    tick_deg: f32,
    labels: bool,
    compact: bool,
) {
    let cx = rect.center().x;
    let cy = rect.center().y;
    let span_deg = 180.0_f32;
    let ppd = rect.width() / span_deg;
    let half = span_deg / 2.0;
    let (minor_h, major_h) = if compact { (4.0, 8.0) } else { (6.0, 12.0) };
    painter.line_segment(
        [egui::pos2(rect.left(), cy), egui::pos2(rect.right(), cy)],
        egui::Stroke::new(1.0_f32, crate::settings::tinted_gray(90)),
    );

    // Minor ticks at the chosen spacing.
    let minor_stroke = egui::Stroke::new(1.0_f32, crate::settings::tinted_gray(115));
    let mut d = ((phase - half) / tick_deg).ceil() * tick_deg;
    while d <= phase + half {
        let x = cx + (d - phase) * ppd;
        painter.line_segment([egui::pos2(x, cy - minor_h), egui::pos2(x, cy + minor_h)], minor_stroke);
        d += tick_deg;
    }

    // Major ticks + labels at each 90° (independent of the minor spacing).
    let major_stroke = egui::Stroke::new(1.0_f32, crate::settings::tinted_gray(170));
    let font = crate::settings::ui_font((rect.height() * 0.16).clamp(7.0, 11.0));
    let mut d = ((phase - half) / 90.0).ceil() * 90.0;
    while d <= phase + half {
        let x = cx + (d - phase) * ppd;
        painter.line_segment([egui::pos2(x, cy - major_h), egui::pos2(x, cy + major_h)], major_stroke);
        if labels {
            let deg = (d.rem_euclid(360.0)).round() as i32 % 360;
            // The first and last labels of the visible span sit on the ruler's own ends:
            // centred there they are half outside the rect, which is why a wide field used
            // to read "0°" as "'0°". Near an edge, anchor the number inside it instead.
            let at_edge = 14.0;
            let align = if x < rect.left() + at_edge {
                egui::Align2::LEFT_TOP
            } else if x > rect.right() - at_edge {
                egui::Align2::RIGHT_TOP
            } else {
                egui::Align2::CENTER_TOP
            };
            painter.text(
                egui::pos2(x.clamp(rect.left(), rect.right()), cy + major_h + 1.0),
                align,
                format!("{deg}°"),
                font.clone(),
                crate::settings::tinted_gray(200),
            );
        }
        d += 90.0;
    }
}

/// Sutherland–Hodgman clip of a convex polygon (camera space) against the near
/// plane (forward = −z ≥ `near`). Keeps the room's face fills finite when the
/// camera sits inside and a face wraps behind it.
fn clip_poly_near(verts: &[glam::Vec3], near: f32) -> Vec<glam::Vec3> {
    let mut out: Vec<glam::Vec3> = Vec::with_capacity(verts.len() + 2);
    let n = verts.len();
    for i in 0..n {
        let cur = verts[i];
        let nxt = verts[(i + 1) % n];
        let cf = -cur.z; // forward distance
        let nf = -nxt.z;
        let cin = cf >= near;
        let nin = nf >= near;
        if cin {
            out.push(cur);
        }
        if cin != nin {
            let t = (near - cf) / (nf - cf);
            out.push(cur + (nxt - cur) * t);
        }
    }
    out
}

/// Paint a cube-room interior seen from a camera at its centre, yawed by
/// `yaw_deg`, with a horizontal field-of-view of `fov_deg`. Each wall/floor/
/// ceiling is a distinctly-coloured grid so rotation direction is unmistakable;
/// `shade` (0..1, the BG-opacity slider) adds per-cell "headlight"-shaded solid
/// fills (bright head-on, dark at grazing corners) for real depth across each
/// wall instead of one flat tone. The camera sits inside, so every face straddles
/// the near plane — segments and face polygons are clipped to it before
/// projection. Pure painter (no wgpu): a rotation reference the user matches the
/// game camera to (FOV-matched → equal on-screen turn rate at 1:1).
fn paint_rws_room(painter: &egui::Painter, rect: egui::Rect, yaw_deg: f32, fov_deg: f32, shade: f32) {
    use glam::{Quat, Vec3};
    let r = 1.0_f32;
    let cx = rect.center().x;
    let cy = rect.center().y;
    let near = 0.02_f32;
    let fov = fov_deg.clamp(20.0, 160.0).to_radians();
    // Focal length in pixels: half-width / tan(halfFOV). At 90° the front wall
    // exactly fills the viewport width — matching a game at the same FOV.
    let f = (rect.width() * 0.5) / (fov * 0.5).tan();
    // World→camera. A positive yaw (mouse/aim to the RIGHT) must scroll the room
    // to the LEFT, so the world rotates by +yaw about Y here (a point dead ahead
    // moves to −x in camera space).
    let inv = Quat::from_rotation_y(yaw_deg.to_radians());
    let project = |p: Vec3| -> egui::Pos2 {
        let z = -p.z;
        egui::pos2(cx + f * p.x / z, cy - f * p.y / z)
    };

    // Clip world segment a→b to the near plane, then project both ends. Returns
    // `None` when the whole segment is behind the camera.
    let clip_project = |aw: Vec3, bw: Vec3| -> Option<(egui::Pos2, egui::Pos2)> {
        let mut a = inv * aw;
        let mut b = inv * bw;
        let za = -a.z; // forward distance (camera looks down −Z)
        let zb = -b.z;
        if za <= near && zb <= near {
            return None;
        }
        if za <= near || zb <= near {
            let t = (near - za) / (zb - za);
            let mid = a + (b - a) * t;
            if za <= near {
                a = mid;
            } else {
                b = mid;
            }
        }
        Some((project(a), project(b)))
    };
    let line = |aw: Vec3, bw: Vec3, stroke: egui::Stroke| {
        if let Some((pa, pb)) = clip_project(aw, bw) {
            painter.line_segment([pa, pb], stroke);
        }
    };

    // Distinct colours per face so orientation reads at a glance.
    let front = egui::Color32::from_rgb(210, 90, 80); // z = −r (ahead at yaw 0)
    let back = egui::Color32::from_rgb(80, 130, 210); // z = +r
    let left = egui::Color32::from_rgb(90, 190, 110); // x = −r
    let right = egui::Color32::from_rgb(220, 160, 70); // x = +r
    let floor = crate::settings::tinted_gray(70);
    let ceil = crate::settings::tinted_gray(150);

    // Shaded solid fills (opacity = the BG slider). Each face is a grid of small
    // cells, and every cell is shaded by a "headlight" term |view·normal| — the
    // camera sits at the room centre, so a wall's middle faces the camera head-on
    // (bright) while its corners are grazing (dark). That per-cell gradient gives
    // real depth instead of one flat tone per wall. Faces drawn far→near so the
    // nearer wall layers on top. Each face: origin `p0`, span axes `ax`,`ay`,
    // colour, world normal.
    if shade > 0.02 {
        let d = 2.0 * r;
        let faces: [(Vec3, Vec3, Vec3, egui::Color32, Vec3); 6] = [
            (Vec3::new(-r, -r, -r), Vec3::new(d, 0.0, 0.0), Vec3::new(0.0, 0.0, d), floor, Vec3::Y),
            (Vec3::new(-r, r, -r), Vec3::new(d, 0.0, 0.0), Vec3::new(0.0, 0.0, d), ceil, Vec3::Y),
            (Vec3::new(-r, -r, -r), Vec3::new(d, 0.0, 0.0), Vec3::new(0.0, d, 0.0), front, Vec3::Z),
            (Vec3::new(-r, -r, r), Vec3::new(d, 0.0, 0.0), Vec3::new(0.0, d, 0.0), back, Vec3::Z),
            (Vec3::new(-r, -r, -r), Vec3::new(0.0, 0.0, d), Vec3::new(0.0, d, 0.0), left, Vec3::X),
            (Vec3::new(r, -r, -r), Vec3::new(0.0, 0.0, d), Vec3::new(0.0, d, 0.0), right, Vec3::X),
        ];
        let a8 = (shade * 205.0).round().clamp(0.0, 255.0) as u8;
        let m = 4; // shading cells per axis
        let mut order: Vec<usize> = (0..6).collect();
        let fdepth = |i: usize| -> f32 {
            let (p0, ax, ay, ..) = faces[i];
            -(inv * (p0 + ax * 0.5 + ay * 0.5)).z
        };
        order.sort_by(|&a, &b| fdepth(b).partial_cmp(&fdepth(a)).unwrap_or(std::cmp::Ordering::Equal));
        for idx in order {
            let (p0, ax, ay, base, normal) = faces[idx];
            for gi in 0..m {
                for gj in 0..m {
                    let (u0, u1) = (gi as f32 / m as f32, (gi + 1) as f32 / m as f32);
                    let (v0, v1) = (gj as f32 / m as f32, (gj + 1) as f32 / m as f32);
                    let corners = [
                        p0 + ax * u0 + ay * v0,
                        p0 + ax * u1 + ay * v0,
                        p0 + ax * u1 + ay * v1,
                        p0 + ax * u0 + ay * v1,
                    ];
                    let center = p0 + ax * ((u0 + u1) * 0.5) + ay * ((v0 + v1) * 0.5);
                    // Camera at origin → view ray to the cell is just its direction.
                    let facing = center.normalize_or_zero().dot(normal).abs();
                    let b = 0.18 + 0.82 * facing;
                    let cam: Vec<Vec3> = corners.iter().map(|c| inv * *c).collect();
                    let poly = clip_poly_near(&cam, near);
                    if poly.len() < 3 {
                        continue;
                    }
                    let pts: Vec<egui::Pos2> = poly.iter().map(|p| project(*p)).collect();
                    let col = egui::Color32::from_rgba_unmultiplied(
                        (base.r() as f32 * b) as u8,
                        (base.g() as f32 * b) as u8,
                        (base.b() as f32 * b) as u8,
                        a8,
                    );
                    painter.add(egui::Shape::convex_polygon(pts, col, egui::Stroke::NONE));
                }
            }
        }
    }

    // Wireframe grid over every face.
    let n = 4; // grid divisions per face
    let step = 2.0 * r / n as f32;
    let s = |c: egui::Color32| egui::Stroke::new(1.0_f32, c);
    for i in 0..=n {
        let t = -r + i as f32 * step;
        // Floor (y = −r) and ceiling (y = +r): grid over x,z.
        for (yf, c) in [(-r, floor), (r, ceil)] {
            line(Vec3::new(t, yf, -r), Vec3::new(t, yf, r), s(c));
            line(Vec3::new(-r, yf, t), Vec3::new(r, yf, t), s(c));
        }
        // Front/back walls (z = ∓r): grid over x,y.
        for (zf, c) in [(-r, front), (r, back)] {
            line(Vec3::new(t, -r, zf), Vec3::new(t, r, zf), s(c));
            line(Vec3::new(-r, t, zf), Vec3::new(r, t, zf), s(c));
        }
        // Left/right walls (x = ∓r): grid over y,z.
        for (xf, c) in [(-r, left), (r, right)] {
            line(Vec3::new(xf, -r, t), Vec3::new(xf, r, t), s(c));
            line(Vec3::new(xf, t, -r), Vec3::new(xf, t, r), s(c));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dots-per-360 entry is stored per degree and reads back as the same
    /// whole number; per-degree mode stores the entry unchanged.
    #[test]
    fn scale_unit_round_trips_through_stored_per_degree_value() {
        let per_360 = RwsScaleUnit { per_360: true };
        let stored = per_360.to_param(54_545.0).and_then(|v| v.as_f64()).unwrap();
        assert!((stored - 54_545.0 / 360.0).abs() < 1e-3);
        assert_eq!(per_360.text(stored as f32), "54545 /360°");

        let per_deg = RwsScaleUnit { per_360: false };
        let stored = per_deg.to_param(151.5).and_then(|v| v.as_f64()).unwrap();
        assert_eq!(stored, 151.5);
        assert_eq!(per_deg.text(stored as f32), "151.50 /°");
    }

    fn rws_node() -> (Snarl<NodeData>, NodeId) {
        let desc = flexinput_modules::all_modules()
            .into_iter()
            .map(|r| r.descriptor)
            .find(|d| d.id == "processing.rws")
            .expect("registered");
        let mut snarl: Snarl<NodeData> = Snarl::new();
        let id = snarl.insert_node(egui::Pos2::ZERO, NodeData::from(&desc));
        (snarl, id)
    }

    /// `view` as the engine leaves it in the node's `last_out`.
    fn set_view(snarl: &mut Snarl<NodeData>, id: NodeId, view: flexinput_engine::eval::RwsFlickView) {
        let n = snarl.get_node_mut(id).expect("node");
        let at = flexinput_engine::eval::rws_flick_view_out(n.outputs.len());
        n.extra.last_out = vec![None; at + 2];
        let [a, b] = view.encode();
        n.extra.last_out[at] = Some(a);
        n.extra.last_out[at + 1] = Some(b);
    }

    fn texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
        fn walk(s: &egui::Shape, out: &mut Vec<String>) {
            match s {
                egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        shapes.iter().for_each(|c| walk(&c.shape, &mut out));
        out
    }

    // Every flick row - the ones round the circle and the ones from before it, which old
    // layouts still pin - draws as many fields as the gamepad walks for it, inside its frame.
    #[test]
    fn every_flick_row_draws_its_fields_inside_its_frame() {
        for (element, count) in [
            ("flick_zone", 2),
            ("flick_aim", 2),
            ("flick_speed", 2),
            ("flick_feel", 2),
            ("flick_route", 2),
            ("flick", 4),
            ("flick_opts", 3),
            ("suppress", 1),
        ] {
            for container in [egui::vec2(420.0, 30.0), egui::vec2(120.0, 30.0), egui::vec2(300.0, 120.0)] {
                let (mut snarl, id) = rws_node();
                assert_eq!(crate::app::element_fields("processing.rws", element, snarl.get_node(id)).len(), count, "{element}");
                let ctx = egui::Context::default();
                let frame = egui::Rect::from_min_size(egui::pos2(20.0, 20.0), container);
                for _ in 0..4 {
                    let _ = ctx.run(egui::RawInput::default(), |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            let mut pin = ui.new_child(egui::UiBuilder::new().max_rect(frame));
                            pin.ctx().data_mut(|d| d.insert_temp(egui::Id::new(("gp_nav_cur_element", id.0)), element.to_string()));
                            super::super::pin_style2::render(id, "processing.rws", element, &mut pin, &mut snarl, container);
                        });
                    });
                }
                let rects: Option<(u64, Vec<egui::Rect>)> =
                    ctx.data(|d| d.get_temp(egui::Id::new(("gp_nav_field_rects", id.0, element.to_string()))));
                let rects = rects.map(|(_, r)| r).unwrap_or_default();
                assert_eq!(rects.len(), count, "{element} at {container:?}: {rects:?}");
                for r in &rects {
                    assert!(frame.expand(0.6).contains_rect(*r), "{element} in {container:?}: {r:?} outside {frame:?}");
                }
            }
        }
    }

    // The words under the flick circle follow what the engine says the stick is doing.
    #[test]
    fn the_flick_circle_says_what_the_stick_is_doing() {
        use flexinput_engine::eval::{RwsFlickView, RwsGesture};
        let base = RwsFlickView { flick_on: true, aim_on: true, ..Default::default() };
        let cases = [
            (RwsFlickView { stick: glam::Vec2::new(0.4, 0.0), ..base }, "aim 47%"),
            (RwsFlickView { stick: glam::Vec2::new(0.6, 0.0), gesture: RwsGesture::Deciding, gate_s: 0.04, ..base }, "flick? 40 of 120 ms"),
            (RwsFlickView { stick: glam::Vec2::new(0.95, 0.0), gesture: RwsGesture::Steering, ..base }, "steering · zone shut"),
            (
                RwsFlickView { stick: glam::Vec2::new(0.9, -0.5), engaged: true, heading_deg: 120.0, turned_deg: 120.0, owed_deg: 40.0, gesture: RwsGesture::Flicked, ..base },
                "flick +120°",
            ),
            (RwsFlickView { stick: glam::Vec2::new(0.1, 0.95), engaged: true, heading_deg: 6.0, turned_deg: 0.0, no_snap: true, ..base }, "turning +0° · no snap"),
            (RwsFlickView { stick: glam::Vec2::new(0.2, 0.0), gesture: RwsGesture::Flicked, ..base }, "flicked · to centre"),
            (RwsFlickView { stick: glam::Vec2::new(0.9, 0.0), flick_on: false, aim_on: false, ..base }, "flick off"),
        ];
        for (view, want) in cases {
            let (mut snarl, id) = rws_node();
            if let Some(n) = snarl.get_node_mut(id) {
                n.params.insert("flick_speed_ms".into(), serde_json::json!(120.0));
                n.params.insert("flick_slow_lock".into(), serde_json::json!(true));
                n.params.insert("flick_deadzone".into(), serde_json::json!(0.85));
            }
            set_view(&mut snarl, id, view);
            let ctx = egui::Context::default();
            let mut shapes = Vec::new();
            for _ in 0..2 {
                let out = ctx.run(egui::RawInput::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        render_rws_flick_view(id, ui, &snarl, egui::vec2(180.0, 180.0));
                    });
                });
                shapes = out.shapes;
            }
            let said = texts(&shapes);
            assert!(said.iter().any(|t| t == want), "{view:?}: wanted {want:?}, said {said:?}");
        }
    }

    // The flick circle in each state it draws, side by side, for looking at.
    #[test]
    #[ignore = "writes a picture, asserts nothing"]
    fn shoot_flick_circle() {
        use flexinput_engine::eval::{RwsFlickView, RwsGesture};
        let base = RwsFlickView { flick_on: true, aim_on: true, ..Default::default() };
        let states = [
            ("centre", base, false),
            ("aim", RwsFlickView { stick: glam::Vec2::new(-0.35, 0.3), ..base }, false),
            ("deciding", RwsFlickView { stick: glam::Vec2::new(0.45, 0.4), gesture: RwsGesture::Deciding, gate_s: 0.05, ..base }, false),
            ("snapping", RwsFlickView { stick: glam::Vec2::new(0.87, -0.5), engaged: true, heading_deg: 120.0, turned_deg: 120.0, owed_deg: 50.0, gesture: RwsGesture::Flicked, ..base }, false),
            ("tracked", RwsFlickView { stick: glam::Vec2::new(-0.7, -0.7), engaged: true, heading_deg: -135.0, turned_deg: 225.0, gesture: RwsGesture::Flicked, ..base }, false),
            ("no snap", RwsFlickView { stick: glam::Vec2::new(0.5, 0.85), engaged: true, heading_deg: 30.0, turned_deg: 24.0, no_snap: true, ..base }, false),
            ("steering, shut", RwsFlickView { stick: glam::Vec2::new(0.95, 0.1), gesture: RwsGesture::Steering, ..base }, true),
            ("flick off", RwsFlickView { stick: glam::Vec2::new(0.5, 0.2), flick_on: false, ..base }, false),
        ];
        let ctx = egui::Context::default();
        crate::app::fonts::setup_fonts(&ctx);
        if std::env::var("SHOT_LIGHT").is_ok() {
            ctx.set_visuals(egui::Visuals::light());
        }
        let mut shot = crate::canvas::shot::Shot::default();
        let mut shapes = Vec::new();
        let side = 200.0;
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(4.0 * (side + 16.0) + 16.0, 2.0 * (side + 32.0) + 16.0));
        let nodes: Vec<(Snarl<NodeData>, NodeId)> = states
            .iter()
            .map(|(_, view, lock)| {
                let (mut snarl, id) = rws_node();
                if let Some(n) = snarl.get_node_mut(id) {
                    n.params.insert("flick_speed_ms".into(), serde_json::json!(120.0));
                    n.params.insert("flick_slow_lock".into(), serde_json::json!(*lock));
                    n.params.insert("flick_fwd_dz_deg".into(), serde_json::json!(15.0));
                    n.params.insert("flick_deadzone".into(), serde_json::json!(0.8));
                }
                set_view(&mut snarl, id, *view);
                (snarl, id)
            })
            .collect();
        for _ in 0..3 {
            let input = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    for (k, ((name, _, _), (snarl, id))) in states.iter().zip(&nodes).enumerate() {
                        let x = 16.0 + (k % 4) as f32 * (side + 16.0);
                        let y = 16.0 + (k / 4) as f32 * (side + 32.0);
                        ui.painter().text(egui::pos2(x, y), egui::Align2::LEFT_TOP, *name, egui::FontId::proportional(11.0), ui.visuals().weak_text_color());
                        let frame = egui::Rect::from_min_size(egui::pos2(x, y + 16.0), egui::vec2(side, side));
                        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(frame).id_salt(k));
                        render_rws_flick_view(*id, &mut c, snarl, frame.size());
                    }
                });
            });
            shot.absorb(&out.textures_delta);
            shapes = out.shapes;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/module_shots/rws_flick_circle.png");
        shot.save(&ctx, &shapes, screen, 1.0, &path).expect("the picture saves");
        println!("{}", path.display());
    }
}
