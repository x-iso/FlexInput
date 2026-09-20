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
            let l = n.params.get("field_labels").and_then(|v| v.as_bool()).unwrap_or(true);
            (a.clamp(0.0, 1.0), t.clamp(5.0, 90.0), l)
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

pub(crate) fn show_rws_body(node_id: NodeId, ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let (scale, rws, stick_max) = snarl
        .get_node(node_id)
        .map(|n| {
            let scale = n.params.get("scale").and_then(|v| v.as_f64()).unwrap_or(100.0) as f32;
            let rws = n.params.get("rws").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let stick_max = n.params.get("stick_out_dps").and_then(|v| v.as_f64()).unwrap_or(360.0) as f32;
            (scale, rws, stick_max)
        })
        .unwrap_or((100.0, 1.0, 360.0));
    let (bg_alpha, tick_deg, labels) = rws_field_style(snarl, node_id);
    let unit = RwsScaleUnit::of(snarl, node_id);

    let mut set: Vec<(&str, Value)> = Vec::new();

    // The snarl body ui is left-to-right by default; wrap so the rows stack.
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);

    // Each output's calibrated constant, side by side. Still two elements, so
    // each pins independently.
    ui.horizontal(|ui| {
        // Mouse Scale — mouse dots per degree (or per 360°); the calibrated ground
        // truth (the value box the calibration viewport edits in the config overlay).
        let r_scale = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Mouse Scale").small())
                .on_hover_text("Mouse Move (XY) output: mouse dots per degree of turn, or per 360°\n(unit switch in the header) — the calibrated 1:1 ground truth.\nCalibrate this (RWS 1 then feels like a 1:1 physical rotation).");
            let mut v = unit.shown(scale);
            if ui.add(unit.drag_value(&mut v).suffix(unit.suffix())).changed() {
                if let Some(p) = unit.to_param(v) { set.push(("scale", p)); }
            }
        });
        register_exposable_element(ui, node_id, "scale", r_scale.response.rect);
        ui.separator();
        // Stick output scaling (the game's camera turn rate at full stick; the
        // Stick-output equivalent of Mouse Scale).
        let r_sdps = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Stick °/s").small())
                .on_hover_text("Right-Stick output: the game's camera turn rate at full deflection.\nWire the Stick output to a virtual Right Stick for stick-aim games.");
            let mut sm = stick_max;
            if ui.add(egui::DragValue::new(&mut sm).speed(5.0).range(1.0..=100_000.0).suffix(" °/s")).changed() {
                if let Some(n) = Number::from_f64(sm as f64) { set.push(("stick_out_dps", Value::Number(n))); }
            }
        });
        register_exposable_element(ui, node_id, "stick_dps", r_sdps.response.rect);
    });

    // RWS multiplier + V/H bias on one row, again as separate elements.
    let (gyro_vh, stick_vh) = snarl.get_node(node_id).map(|n| (
        n.params.get("gyro_vh_ratio").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
        n.params.get("stick_vh_ratio").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
    )).unwrap_or((1.0, 1.0));
    ui.horizontal(|ui| {
        // RWS multiplier relative to the calibrated ground truth.
        let r_rws = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("RWS").small())
                .on_hover_text("Sensitivity as a multiple of the calibrated 1:1 ground truth.\n1.0 = matches your physical rotation; 2.0 = twice as fast.");
            let mut v = rws;
            if ui.add(egui::DragValue::new(&mut v).speed(0.01).range(0.01..=50.0)).changed() {
                if let Some(n) = Number::from_f64(v as f64) { set.push(("rws", Value::Number(n))); }
            }
        });
        register_exposable_element(ui, node_id, "rws", r_rws.response.rect);
        ui.separator();
        // V/H bias — vertical sensitivity relative to horizontal (the calibrated
        // reference), separately for the gyro source and stick sources. 1.0 = equal.
        let r_vh = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("V/H").small().weak())
                .on_hover_text("Vertical sensitivity relative to horizontal (1.0 = equal;\n>1 = look up/down faster). Horizontal is the calibrated reference,\nset per source: gyro vs stick.");
            ui.label(egui::RichText::new("gyro").small().weak());
            let mut g = gyro_vh;
            if ui.add(egui::DragValue::new(&mut g).speed(0.01).range(0.0..=8.0)).changed() {
                if let Some(n) = Number::from_f64(g as f64) { set.push(("gyro_vh_ratio", Value::Number(n))); }
            }
            ui.label(egui::RichText::new("stick").small().weak());
            let mut s = stick_vh;
            if ui.add(egui::DragValue::new(&mut s).speed(0.01).range(0.0..=8.0)).changed() {
                if let Some(n) = Number::from_f64(s as f64) { set.push(("stick_vh_ratio", Value::Number(n))); }
            }
        });
        register_exposable_element(ui, node_id, "vh", r_vh.response.rect);
    });

    // Measure-based auto-calibration (turn a known 180°/360° → back-solve the
    // constant), above the ruler. Stacked vertically so its guide and result
    // lines sit on rows of their own instead of widening the controls row.
    // (Body edits are real input in this window, so the canvas's own edit
    // tracking syncs them — the returned write flag is only needed for overlays.)
    let r_meas = ui.vertical(|ui| { let _ = rws_measure_controls(node_id, ui, snarl, true); });
    register_exposable_element(ui, node_id, "measure", r_meas.response.rect);

    // Calibration viewport (own row). Writes `scale` directly via its centre box,
    // so it must run before the `set` flush below (disjoint params — no conflict).
    let fw = ui.available_width().clamp(120.0, 260.0);
    let frect = render_rws_field(node_id, ui, snarl, egui::vec2(fw, 100.0), false);
    register_exposable_element(ui, node_id, "field", frect);

    // View + style row.
    let mode = snarl.get_node(node_id)
        .and_then(|n| n.params.get("field_mode").and_then(|v| v.as_str()))
        .unwrap_or("ruler").to_string();
    let fov = snarl.get_node(node_id)
        .and_then(|n| n.params.get("field_fov").and_then(|v| v.as_f64()))
        .unwrap_or(90.0) as f32;
    let r_style = ui.horizontal(|ui| {
        for (val, lbl) in [("ruler", "Ruler"), ("room", "Room"), ("both", "Both")] {
            if ui.selectable_label(mode == val, egui::RichText::new(lbl).small()).clicked() {
                set.push(("field_mode", Value::String(val.to_string())));
            }
        }
        ui.separator();
        let mut a = bg_alpha;
        if ui.add(egui::DragValue::new(&mut a).speed(0.01).range(0.0..=1.0).prefix("BG "))
            .on_hover_text("Viewport background opacity (0 = transparent).")
            .changed()
        {
            if let Some(n) = Number::from_f64(a as f64) { set.push(("field_bg_alpha", Value::Number(n))); }
        }
        if mode != "ruler" {
            let mut fv = fov;
            if ui.add(egui::DragValue::new(&mut fv).speed(1.0).range(30.0..=140.0).prefix("FOV ").suffix("°"))
                .on_hover_text("Horizontal field of view — set this to match your in-game FOV so the turn rate reads 1:1.")
                .changed()
            {
                if let Some(n) = Number::from_f64(fv as f64) { set.push(("field_fov", Value::Number(n))); }
            }
        }
        if mode != "room" {
            let mut td = tick_deg;
            if ui.add(egui::DragValue::new(&mut td).speed(1.0).range(5.0..=90.0).suffix("°"))
                .on_hover_text("Minor tick spacing.")
                .changed()
            {
                if let Some(n) = Number::from_f64(td as f64) { set.push(("field_tick_deg", Value::Number(n))); }
            }
            let mut lb = labels;
            if ui.checkbox(&mut lb, egui::RichText::new("labels").small()).changed() {
                set.push(("field_labels", Value::Bool(lb)));
            }
        }
    });
    register_exposable_element(ui, node_id, "style", r_style.response.rect);

    // Flick stick (input 2, optional): push to flick, hold + rotate to track.
    let (flick_on, flick_dz, flick_sm, aim_on, aim_rws) = snarl.get_node(node_id).map(|n| {
        (
            n.params.get("flick_enabled").and_then(|v| v.as_bool()).unwrap_or(false),
            n.params.get("flick_deadzone").and_then(|v| v.as_f64()).unwrap_or(0.85) as f32,
            n.params.get("flick_smooth_ms").and_then(|v| v.as_f64()).unwrap_or(100.0) as f32,
            n.params.get("stick_aim_enabled").and_then(|v| v.as_bool()).unwrap_or(false),
            n.params.get("stick_aim_rws").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
        )
    }).unwrap_or((false, 0.85, 100.0, false, 1.0));
    let r_flick = ui.horizontal(|ui| {
        let mut fe = flick_on;
        if ui.checkbox(&mut fe, egui::RichText::new("Flick").small())
            .on_hover_text("Flick stick on input 2: push the stick past the deadzone to\nsnap the camera to that direction; hold it out and rotate to track.\nFlicks are 1:1 (RWS does not apply).")
            .changed()
        {
            set.push(("flick_enabled", Value::Bool(fe)));
        }
        if flick_on {
            ui.label(egui::RichText::new("dz").small().weak());
            let mut dz = flick_dz;
            if ui.add(egui::DragValue::new(&mut dz).speed(0.01).range(0.1..=0.99))
                .on_hover_text("Deadzone — stick magnitude needed to engage a flick.")
                .changed()
            {
                if let Some(n) = Number::from_f64(dz as f64) { set.push(("flick_deadzone", Value::Number(n))); }
            }
            ui.label(egui::RichText::new("smooth").small().weak());
            let mut sm = flick_sm;
            if ui.add(egui::DragValue::new(&mut sm).speed(1.0).range(0.0..=500.0).suffix(" ms"))
                .on_hover_text("Smoothing window for the initial flick snap (0 = instant).")
                .changed()
            {
                if let Some(n) = Number::from_f64(sm as f64) { set.push(("flick_smooth_ms", Value::Number(n))); }
            }
        }
        // Stick-aim — available whether or not Flick is on (with Flick it lives
        // inside the deadzone; without, it uses the full stick range).
        let mut ae = aim_on;
        if ui.checkbox(&mut ae, egui::RichText::new("Stick aim").small())
            .on_hover_text("Use the stick wired to the Flick input as a rate aim feeding BOTH\noutputs (its own RWS + the stick V/H bias). With Flick on it acts INSIDE\nthe deadzone (past → flick); with Flick off it uses the full stick range.\nThe stick is suppressed from its default mapping while aiming.")
            .changed()
        {
            set.push(("stick_aim_enabled", Value::Bool(ae)));
        }
        if aim_on {
            let mut ar = aim_rws;
            if ui.add(egui::DragValue::new(&mut ar).speed(0.01).range(0.01..=50.0).prefix("×"))
                .on_hover_text("Stick-aim RWS multiplier (independent of the main RWS).")
                .changed()
            {
                if let Some(n) = Number::from_f64(ar as f64) { set.push(("stick_aim_rws", Value::Number(n))); }
            }
        }
    });
    register_exposable_element(ui, node_id, "flick", r_flick.response.rect);

    // Flick-stick source suppression: the stick wired into Flick is auto-detected
    // and blocked downstream (so it can't leak to its default mapping, e.g. the
    // virtual Right Stick), while this module keeps reading it internally.
    let suppress = snarl.get_node(node_id)
        .and_then(|n| n.params.get("suppress_source").and_then(|v| v.as_str()))
        .unwrap_or("off").to_string();
    let r_sup = ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Suppress flick stick").small().weak())
            .on_hover_text("Block the stick wired into Flick from leaking to its default\nmapping (e.g. the virtual Right Stick). Auto-detected from the wire;\nthis module still reads it (via the pre-block snapshot, like the\nVirtual Menu).\n• Off — no block.\n• Full — always block while Flick is enabled.\n• In deadzone — block only past the deadzone, so small movements\n  inside the deadzone still reach the default mapping.");
        egui::ComboBox::from_id_salt((node_id, "rws_suppress"))
            .selected_text(match suppress.as_str() {
                "full" => "Full", "deadzone" => "In deadzone", _ => "Off",
            })
            .width(104.0)
            .show_ui(ui, |ui| {
                for (val, lbl) in [("off", "Off"), ("full", "Full"), ("deadzone", "In deadzone")] {
                    if ui.selectable_label(suppress == val, lbl).clicked() {
                        set.push(("suppress_source", Value::String(val.to_string())));
                    }
                }
            });
    });
    register_exposable_element(ui, node_id, "suppress", r_sup.response.rect);

    ui.label(egui::RichText::new("→ wire Mouse Move (XY) to KB/M “Mouse XY (move)”").small().weak())
        .on_hover_text("RWS drives the mouse via the displacement pin, which ignores the\nKB/M card's mouse sensitivity — so the calibration is portable.");
    }); // ui.vertical

    if !set.is_empty() {
        if let Some(node) = snarl.get_node_mut(node_id) {
            for (k, v) in set {
                node.params.insert(k.to_string(), v);
            }
        }
    }
}

/// Flick-stick row (enable + deadzone + smoothing), as a standalone pinnable
/// element.
pub(crate) fn render_rws_flick(
    node_id: NodeId,
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    container: egui::Vec2,
) {
    let (flick_on, flick_dz, flick_sm, aim_on, aim_rws) = snarl.get_node(node_id).map(|n| {
        (
            n.params.get("flick_enabled").and_then(|v| v.as_bool()).unwrap_or(false),
            n.params.get("flick_deadzone").and_then(|v| v.as_f64()).unwrap_or(0.85) as f32,
            n.params.get("flick_smooth_ms").and_then(|v| v.as_f64()).unwrap_or(100.0) as f32,
            n.params.get("stick_aim_enabled").and_then(|v| v.as_bool()).unwrap_or(false),
            n.params.get("stick_aim_rws").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
        )
    }).unwrap_or((false, 0.85, 100.0, false, 1.0));
    let mut set: Vec<(&str, Value)> = Vec::new();
    ui.set_max_width(container.x);
    apply_widget_scale(ui, container, egui::vec2(190.0, 22.0));
    ui.horizontal(|ui| {
        let mut fe = flick_on;
        if ui.checkbox(&mut fe, egui::RichText::new("Flick").small()).changed() {
            set.push(("flick_enabled", Value::Bool(fe)));
        }
        if flick_on {
            ui.label(egui::RichText::new("dz").small().weak());
            let mut dz = flick_dz;
            if ui.add(egui::DragValue::new(&mut dz).speed(0.01).range(0.1..=0.99)).changed() {
                if let Some(n) = Number::from_f64(dz as f64) { set.push(("flick_deadzone", Value::Number(n))); }
            }
            ui.label(egui::RichText::new("ms").small().weak());
            let mut sm = flick_sm;
            if ui.add(egui::DragValue::new(&mut sm).speed(1.0).range(0.0..=500.0)).changed() {
                if let Some(n) = Number::from_f64(sm as f64) { set.push(("flick_smooth_ms", Value::Number(n))); }
            }
        }
        let mut ae = aim_on;
        if ui.checkbox(&mut ae, egui::RichText::new("aim").small()).changed() {
            set.push(("stick_aim_enabled", Value::Bool(ae)));
        }
        if aim_on {
            let mut ar = aim_rws;
            if ui.add(egui::DragValue::new(&mut ar).speed(0.01).range(0.01..=50.0).prefix("×")).changed() {
                if let Some(n) = Number::from_f64(ar as f64) { set.push(("stick_aim_rws", Value::Number(n))); }
            }
        }
    });
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
    "scale", "rws", "stick_out_dps", "max_rate_dps",
    "gyro_vh_ratio", "stick_vh_ratio",
    "flick_enabled", "flick_deadzone", "flick_smooth_ms",
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
    (axis, float_out(2), float_out(3))
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
    let guide = |ui: &mut egui::Ui, instruction: &str, pad_hint: &str| {
        if split_hints {
            ui.label(egui::RichText::new(instruction).small().weak());
            ui.label(egui::RichText::new(pad_hint).small().weak());
        } else {
            ui.label(egui::RichText::new(format!("{instruction}  ·  {pad_hint}")).small().weak());
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
        let instruction = match (axis.as_str(), is_stick) {
            ("pitch", false) => "Turn the camera fully UP, then Finish",
            ("pitch", true) => "Turn the camera fully UP (steadily, not too fast), then Finish",
            (_, false) => "Turn one full 360°, then Finish",
            (_, true) => "Turn one full 360° (steadily, not too fast), then Finish",
        };
        guide(ui, instruction, "A = Finish · B = Cancel");
    } else {
        let r = ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Auto-cal").small().weak())
                .on_hover_text("Turn the camera a known amount; the constant for the chosen output\nis solved from the measured rotation.\n\nNo reference value for this game? Set a LOW in-game mouse sensitivity\nfirst: in most games it sets the turn per mouse dot, so lower means\nfiner angular resolution (more dots per 360°).");
            // Method buttons; the gamepad-highlighted (pending) one is marked.
            let r_p = ui.selectable_label(pending == "pitch", egui::RichText::new("↕180°").small())
                .on_hover_text("Vertical: aim straight DOWN, then turn straight UP (horizontal blocked).");
            let r_y = ui.selectable_label(pending == "yaw", egui::RichText::new("↔360°").small())
                .on_hover_text("Horizontal: turn one full 360° (vertical blocked).");
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
            guide(ui, "Aim the camera straight DOWN first", "A = Start · ◄► method · ▲▼ output");
        } else {
            guide(ui, "Aim straight ahead first", "A = Start · ◄► method · ▲▼ output · Y = snapshot");
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
    let (bg_alpha, tick_deg, labels) = rws_field_style(snarl, node_id);
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
            let mut lb = labels;
            if ui.checkbox(&mut lb, egui::RichText::new("labels").small()).changed() {
                set.push(("field_labels", Value::Bool(lb)));
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
        egui::Stroke::new(1.5, egui::Color32::from_rgb(120, 200, 255)),
    );

    // Status label (bottom-left).
    painter.text(
        egui::pos2(rect.left() + 4.0, rect.bottom() - 3.0),
        egui::Align2::LEFT_BOTTOM,
        "live",
        egui::FontId::proportional(9.0),
        egui::Color32::from_gray(120),
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
        egui::Stroke::new(1.0, egui::Color32::from_gray(90)),
    );

    // Minor ticks at the chosen spacing.
    let minor_stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(115));
    let mut d = ((phase - half) / tick_deg).ceil() * tick_deg;
    while d <= phase + half {
        let x = cx + (d - phase) * ppd;
        painter.line_segment([egui::pos2(x, cy - minor_h), egui::pos2(x, cy + minor_h)], minor_stroke);
        d += tick_deg;
    }

    // Major ticks + labels at each 90° (independent of the minor spacing).
    let major_stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(170));
    let font = egui::FontId::proportional((rect.height() * 0.16).clamp(7.0, 11.0));
    let mut d = ((phase - half) / 90.0).ceil() * 90.0;
    while d <= phase + half {
        let x = cx + (d - phase) * ppd;
        painter.line_segment([egui::pos2(x, cy - major_h), egui::pos2(x, cy + major_h)], major_stroke);
        if labels {
            let deg = (d.rem_euclid(360.0)).round() as i32 % 360;
            painter.text(
                egui::pos2(x, cy + major_h + 1.0),
                egui::Align2::CENTER_TOP,
                format!("{deg}°"),
                font.clone(),
                egui::Color32::from_gray(200),
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
    let floor = egui::Color32::from_gray(70);
    let ceil = egui::Color32::from_gray(150);

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
    let s = |c: egui::Color32| egui::Stroke::new(1.0, c);
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
}
