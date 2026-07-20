//! The gamepad-native settings panel: its row model, value get/set, the
//! nav driver that walks it, and the panel renderer.

use super::*;

impl FlexInputApp {

    /// Ordered list of gamepad-navigable settings rows. Keeps the panel and the
    /// driver in lock-step (same indices). Each row is a kind + label; the
    /// numeric kinds carry their range/step so the driver can nudge them.
    pub(crate) fn gp_settings_rows(&self) -> Vec<GpSettingRow> {
        use GpSettingKind::*;
        vec![
            GpSettingRow { label: "Max polling rate".into(),
                kind: IntSlider { lo: settings::POLLING_HZ_MIN as f32,
                    hi: settings::POLLING_HZ_MAX as f32, step: 50.0,
                    key: GpSettingKey::PollingHz }, suffix: " Hz" },
            GpSettingRow { label: "Processing sample rate".into(),
                kind: IntSlider { lo: settings::SAMPLE_RATE_HZ_MIN as f32,
                    hi: settings::SAMPLE_RATE_HZ_MAX as f32, step: 50.0,
                    key: GpSettingKey::SampleRateHz }, suffix: " Hz" },
            GpSettingRow { label: "Background repaint rate".into(),
                kind: IntSlider { lo: settings::BG_REPAINT_HZ_MIN as f32,
                    hi: settings::BG_REPAINT_HZ_MAX as f32, step: 1.0,
                    key: GpSettingKey::BgRepaintHz }, suffix: " Hz" },
            GpSettingRow { label: "Gamepad UI nav by default".into(),
                kind: Toggle { key: GpSettingKey::NavDefault }, suffix: "" },
            GpSettingRow { label: "Cursor max speed".into(),
                kind: IntSlider { lo: 1000.0, hi: 30000.0, step: 250.0,
                    key: GpSettingKey::CursorMaxSpeed }, suffix: " px/s" },
            GpSettingRow { label: "Cursor acceleration".into(),
                kind: FloatSlider { lo: 1.0, hi: 4.0, step: 0.05,
                    key: GpSettingKey::CursorAccel }, suffix: "" },
            GpSettingRow { label: "Contrast".into(),
                kind: FloatSlider { lo: -1.0, hi: 1.0, step: 0.05,
                    key: GpSettingKey::Contrast }, suffix: "" },
            GpSettingRow { label: "Keep tabs on launch".into(),
                kind: Toggle { key: GpSettingKey::KeepWorkspace }, suffix: "" },
            GpSettingRow { label: "Collapse new device nodes".into(),
                kind: Toggle { key: GpSettingKey::CollapseNodes }, suffix: "" },
            GpSettingRow { label: "Show own virtuals as physical".into(),
                kind: Toggle { key: GpSettingKey::ShowVirtuals }, suffix: "" },
            GpSettingRow { label: "Default deadzone".into(),
                kind: FloatSlider { lo: 0.0, hi: 0.5, step: 0.005,
                    key: GpSettingKey::DefDeadzone }, suffix: "" },
            GpSettingRow { label: "Default gyro ×".into(),
                kind: FloatSlider { lo: 0.1, hi: 50.0, step: 0.05,
                    key: GpSettingKey::DefGyroMult }, suffix: "" },
            GpSettingRow { label: "Default mouse speed".into(),
                kind: FloatSlider { lo: 0.0, hi: 3000.0, step: 1.0,
                    key: GpSettingKey::DefMouseSens }, suffix: "" },
            GpSettingRow { label: "Shortcuts: nav-only".into(),
                kind: Toggle { key: GpSettingKey::ChordsNavOnly }, suffix: "" },
            GpSettingRow { label: "Shortcut: See-through".into(),
                kind: ChordLearn { target: crate::gamepad_nav::ChordTarget::SeeThrough }, suffix: "" },
            GpSettingRow { label: "Shortcut: Panic".into(),
                kind: ChordLearn { target: crate::gamepad_nav::ChordTarget::Panic }, suffix: "" },
            GpSettingRow { label: "Shortcut: Overlay".into(),
                kind: ChordLearn { target: crate::gamepad_nav::ChordTarget::Overlay }, suffix: "" },
        ]
    }

    /// Current numeric value of a settings key (bools as 0/1).
    pub(crate) fn gp_setting_value(&self, key: GpSettingKey) -> f32 {
        use GpSettingKey::*;
        match key {
            PollingHz => self.settings.polling_hz as f32,
            SampleRateHz => self.settings.sample_rate_hz as f32,
            BgRepaintHz => self.settings.bg_repaint_hz as f32,
            NavDefault => self.settings.gamepad_ui_nav_default as i32 as f32,
            CursorMaxSpeed => self.settings.cursor_max_speed,
            CursorAccel => self.settings.cursor_accel,
            Contrast => self.settings.contrast,
            KeepWorkspace => self.settings.keep_workspace as i32 as f32,
            CollapseNodes => self.settings.device_nodes_default_collapsed as i32 as f32,
            ShowVirtuals => self.settings.show_own_virtuals_as_physical as i32 as f32,
            DefDeadzone => self.settings.default_stick_deadzone,
            DefGyroMult => self.settings.default_gyro_mult,
            DefMouseSens => self.settings.default_mouse_sensitivity,
            ChordsNavOnly => self.settings.gamepad_chords_nav_only as i32 as f32,
        }
    }

    /// Write a numeric value to a settings key (bools from !=0), pushing any
    /// live-thread side effects, and mark settings dirty.
    pub(crate) fn gp_setting_set(&mut self, key: GpSettingKey, val: f32) {
        use GpSettingKey::*;
        match key {
            PollingHz => {
                // Snap to a valid whole-ms step (same quantization as the
                // Settings slider) and retune the virtual Xbox 360's pump.
                let v = settings::snap_polling_hz(val.round() as u32);
                self.settings.polling_hz = v;
                self.polling_hz.store(v, Ordering::Relaxed);
                flexinput_virtual::set_requested_poll_hz(v);
            }
            SampleRateHz => {
                let v = (val.round() as u32)
                    .clamp(settings::SAMPLE_RATE_HZ_MIN, settings::SAMPLE_RATE_HZ_MAX);
                self.settings.sample_rate_hz = v;
                self.sample_rate_hz.store(v, Ordering::Relaxed);
            }
            BgRepaintHz => {
                let v = (val.round() as u32)
                    .clamp(settings::BG_REPAINT_HZ_MIN, settings::BG_REPAINT_HZ_MAX);
                self.settings.bg_repaint_hz = v;
            }
            NavDefault => self.settings.gamepad_ui_nav_default = val != 0.0,
            CursorMaxSpeed => self.settings.cursor_max_speed = val.clamp(1000.0, 30000.0),
            CursorAccel => self.settings.cursor_accel = val.clamp(1.0, 4.0),
            Contrast => self.settings.contrast = val.clamp(-1.0, 1.0),
            KeepWorkspace => {
                self.settings.keep_workspace = val != 0.0;
                if !self.settings.keep_workspace { settings::delete_workspace(); }
            }
            CollapseNodes => self.settings.device_nodes_default_collapsed = val != 0.0,
            ShowVirtuals => self.settings.show_own_virtuals_as_physical = val != 0.0,
            DefDeadzone => self.settings.default_stick_deadzone = val.clamp(0.0, 0.5),
            DefGyroMult => self.settings.default_gyro_mult = val.clamp(0.1, 50.0),
            DefMouseSens => self.settings.default_mouse_sensitivity = val.clamp(0.0, 3000.0),
            ChordsNavOnly => self.settings.gamepad_chords_nav_only = val != 0.0,
        }
        self.settings_dirty = true;
    }

    /// Drive the gamepad settings panel (modal). dpad/stick up/down moves the
    /// highlighted row; South toggles a bool or enters/exits numeric edit; while
    /// editing, left/right nudges the value. East closes (or exits edit). West
    /// = fine step. North = (numeric) reset is not wired — values have explicit
    /// ranges; skip.
    pub(crate) fn nav_drive_gp_settings(
        &mut self,
        nav: &crate::gamepad_nav::NavInput,
        rt_rising: bool,
        lt_rising: bool,
    ) {
        use crate::gamepad_nav::{self as gn, NavDir};
        let rows = self.gp_settings_rows();
        if rows.is_empty() { return; }
        let editing = self.gamepad_nav.settings_editing;

        // ── Shortcut-chord capture (panel stays open, mirrors the widget Learn
        // flow exactly) ─────────────────────────────────────────────────────
        // When a ChordLearn row is "learning", the panel is listening: we wait
        // for the device to go idle ONCE (so the South press that started the
        // capture isn't swept in), then accumulate every held button (ANY pin
        // is bindable — North included), and the moment everything releases we
        // latch the combo into the target setting and exit capture, leaving the
        // panel open on the same row. East aborts capture (back), no binding
        // written. This is identical to how a widget's Learn captures input.
        if self.gamepad_nav.chord_learn.is_some() {
            self.drive_gp_chord_capture(nav);
            return;
        }

        // Directional intent (dpad discrete + fresh stick deflection).
        let mut dir: Option<NavDir> = None;
        if nav.is_rising("dpad_up") { dir = Some(NavDir::Up); }
        else if nav.is_rising("dpad_down") { dir = Some(NavDir::Down); }
        else if nav.is_rising("dpad_left") { dir = Some(NavDir::Left); }
        else if nav.is_rising("dpad_right") { dir = Some(NavDir::Right); }
        if dir.is_none() {
            if let Some(sd) = gn::stick_dir(nav.lstick) {
                if self.gamepad_nav.repeat_dir != Some(sd) {
                    self.gamepad_nav.repeat_dir = Some(sd);
                    dir = Some(sd);
                }
            } else {
                self.gamepad_nav.repeat_dir = None;
            }
        }

        if !editing {
            // Row navigation.
            match dir {
                Some(NavDir::Up) => {
                    self.gamepad_nav.settings_index =
                        self.gamepad_nav.settings_index.saturating_sub(1);
                }
                Some(NavDir::Down) => {
                    self.gamepad_nav.settings_index =
                        (self.gamepad_nav.settings_index + 1).min(rows.len() - 1);
                }
                _ => {}
            }
            let idx = self.gamepad_nav.settings_index.min(rows.len() - 1);
            let row = &rows[idx];
            // South / RT → toggle bool, enter numeric edit, or start a chord
            // capture (which closes the panel so the user can press the combo).
            if nav.is_rising("btn_south") || rt_rising {
                match &row.kind {
                    GpSettingKind::Toggle { key } => {
                        let cur = self.gp_setting_value(*key);
                        self.gp_setting_set(*key, if cur != 0.0 { 0.0 } else { 1.0 });
                    }
                    GpSettingKind::ChordLearn { target } => {
                        // Start listening — panel STAYS open and shows the
                        // listening state on this row. Capture runs in
                        // `drive_gp_chord_capture` (early-returned above while
                        // learning). Arm-idle = false so the South that started
                        // this isn't swept into the combo.
                        self.gamepad_nav.chord_learn = Some(*target);
                        self.gamepad_nav.chord_draft.clear();
                        self.gamepad_nav.chord_arm_idle = false;
                    }
                    _ => { self.gamepad_nav.settings_editing = true; }
                }
            }
            // North → clear the assigned binding on a ChordLearn row.
            if nav.is_rising("btn_north") {
                if let GpSettingKind::ChordLearn { target } = &row.kind {
                    use crate::gamepad_nav::ChordTarget;
                    match target {
                        ChordTarget::SeeThrough => self.settings.seethrough_chord = None,
                        ChordTarget::Panic      => self.settings.panic_chord = None,
                        ChordTarget::Overlay    => self.settings.overlay_chord = None,
                    }
                    self.settings_dirty = true;
                }
            }
            // East / LT → close panel.
            if nav.is_rising("btn_east") || lt_rising {
                self.gamepad_nav.settings_open = false;
            }
        } else {
            let idx = self.gamepad_nav.settings_index.min(rows.len() - 1);
            let row = &rows[idx];
            let fine = nav.pressed.contains("btn_west");
            // Cycle gets its own handler — left/right step by one option,
            // wrapping. No fine step / stick deflection (it's a discrete
            // choice). Done early so the slider math below doesn't fire.
            if let GpSettingKind::Cycle { key, opts } = &row.kind {
                let cur = self.gp_setting_value(*key);
                let cur_idx = opts.iter().position(|(v, _)| (v - cur).abs() < 0.5)
                    .unwrap_or(0) as i32;
                let n = opts.len() as i32;
                let new_idx = match dir {
                    Some(NavDir::Right) | Some(NavDir::Up) => (cur_idx + 1).rem_euclid(n),
                    Some(NavDir::Left)  | Some(NavDir::Down) => (cur_idx - 1).rem_euclid(n),
                    _ => cur_idx,
                };
                if new_idx != cur_idx {
                    self.gp_setting_set(*key, opts[new_idx as usize].0);
                }
                return;
            }
            // Left/right (dpad or stick) nudges the value.
            let (lo, hi, step, key) = match &row.kind {
                GpSettingKind::IntSlider { lo, hi, step, key }
                | GpSettingKind::FloatSlider { lo, hi, step, key } => (*lo, *hi, *step, *key),
                GpSettingKind::Toggle { .. } | GpSettingKind::ChordLearn { .. } | GpSettingKind::Cycle { .. } => {
                    self.gamepad_nav.settings_editing = false; return;
                }
            };
            let mut delta = 0.0f32;
            let s = step * if fine { 0.25 } else { 1.0 };
            match dir {
                Some(NavDir::Right) | Some(NavDir::Up) => delta += s,
                Some(NavDir::Left) | Some(NavDir::Down) => delta -= s,
                _ => {}
            }
            let mag = nav.lstick.length();
            if mag > 0.5 {
                let span = hi - lo;
                delta += nav.lstick.x * span * if fine { 0.15 } else { 0.6 }
                    * 0.016; // ~per-frame scale
            }
            if delta != 0.0 {
                let cur = self.gp_setting_value(key);
                self.gp_setting_set(key, (cur + delta).clamp(lo, hi));
            }
            // South / East / RT / LT → leave edit (back to row nav).
            if nav.is_rising("btn_south") || nav.is_rising("btn_east")
                || rt_rising || lt_rising
            {
                self.gamepad_nav.settings_editing = false;
            }
        }
    }

    /// Render the gamepad-native settings panel (driven by `nav_drive_gp_settings`).
    /// A self-contained modal mirroring the gamepad-relevant subset of global
    /// settings, navigable purely by controller (the real Settings window can't
    /// be). Display-only here — all mutation happens in the driver.
    pub(crate) fn draw_gp_settings_panel(&mut self, ctx: &egui::Context) {
        if !self.gamepad_nav.settings_open { return; }
        let rows = self.gp_settings_rows();
        let sel = self.gamepad_nav.settings_index.min(rows.len().saturating_sub(1));
        let editing = self.gamepad_nav.settings_editing;
        let accent = ctx.style().visuals.selection.stroke.color;
        // Skin for combo glyphs: the active nav device's, else Xbox.
        let glyph_skin = self.gamepad_nav.active_dev.as_deref()
            .map(crate::canvas::remapper_icons::skin_from_device_id)
            .unwrap_or(crate::canvas::remapper_icons::Skin::Xbox);

        egui::Window::new("🎮 Settings")
            .id(egui::Id::new("gp_settings_panel"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .default_width(380.0)
            .show(ctx, |ui| {
                if self.gamepad_nav.chord_learn.is_some() {
                    ui.label(egui::RichText::new(
                        "Listening… hold a 2+ button combo and release to bind   \
                         (East alone: cancel)")
                        .small().color(egui::Color32::from_rgb(230, 185, 95)));
                } else {
                    ui.label(egui::RichText::new(
                        "D-pad/stick: move   South: edit/toggle   ←/→: adjust   \
                         West: fine   North: clear shortcut   East: close")
                        .small().color(egui::Color32::from_gray(150)));
                }
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(4.0);
                for (i, row) in rows.iter().enumerate() {
                    let is_sel = i == sel;

                    // Is this row currently capturing a shortcut combo?
                    let learning_row = matches!(&row.kind, GpSettingKind::ChordLearn { target }
                        if self.gamepad_nav.chord_learn == Some(*target));

                    // For an idle ChordLearn row with a stored binding we draw
                    // the combo as glyph icons (trim + tooltip). Otherwise the
                    // value is a plain right-aligned string.
                    let mut combo_icons: Option<Vec<String>> = None;
                    let val_str = match &row.kind {
                        GpSettingKind::Toggle { key } =>
                            if self.gp_setting_value(*key) != 0.0 { "ON".to_string() } else { "OFF".to_string() },
                        GpSettingKind::IntSlider { key, .. } =>
                            format!("{}{}", self.gp_setting_value(*key).round() as i64, row.suffix),
                        GpSettingKind::FloatSlider { key, .. } =>
                            format!("{:.2}{}", self.gp_setting_value(*key), row.suffix),
                        GpSettingKind::Cycle { key, opts } => {
                            let v = self.gp_setting_value(*key);
                            opts.iter().find(|(val, _)| (val - v).abs() < 0.5)
                                .map(|(_, label)| (*label).to_string())
                                .unwrap_or_else(|| format!("?{:.0}", v))
                        }
                        GpSettingKind::ChordLearn { target } => {
                            use crate::gamepad_nav::ChordTarget;
                            if self.gamepad_nav.chord_learn == Some(*target) {
                                // Learning: live listening state. Show the combo
                                // captured so far as icons too.
                                if self.gamepad_nav.chord_draft.is_empty() {
                                    "◉ Listening…".to_string()
                                } else {
                                    combo_icons = Some(self.gamepad_nav.chord_draft.clone());
                                    String::new()
                                }
                            } else {
                                let assigned = match target {
                                    ChordTarget::SeeThrough => self.settings.seethrough_chord.as_ref(),
                                    ChordTarget::Panic => self.settings.panic_chord.as_ref(),
                                    ChordTarget::Overlay => self.settings.overlay_chord.as_ref(),
                                };
                                match assigned {
                                    Some(c) if !c.is_empty() => { combo_icons = Some(c.clone()); String::new() }
                                    _ => "(none)".to_string(),
                                }
                            }
                        }
                    };

                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 24.0), egui::Sense::hover());
                    let painter = ui.painter();
                    if learning_row {
                        // Warm "listening" highlight, distinct from the cool
                        // selection accent, so it's obvious the panel is capturing.
                        let warm = egui::Color32::from_rgb(220, 170, 80);
                        let [r, g, b, _] = warm.to_array();
                        painter.rect_filled(rect, 5.0,
                            egui::Color32::from_rgba_unmultiplied(r, g, b, 60));
                        painter.rect_stroke(rect, 5.0,
                            egui::Stroke::new(2.0, warm), egui::StrokeKind::Inside);
                    } else if is_sel {
                        let bright = editing && !matches!(row.kind, GpSettingKind::Toggle { .. });
                        let [r, g, b, _] = accent.to_array();
                        painter.rect_filled(rect, 5.0,
                            egui::Color32::from_rgba_unmultiplied(r, g, b,
                                if bright { 70 } else { 40 }));
                        painter.rect_stroke(rect, 5.0,
                            egui::Stroke::new(if bright { 2.0 } else { 1.0 }, accent),
                            egui::StrokeKind::Inside);
                    }
                    // Row label (left). Measure its width so combo glyphs know
                    // how far left they may extend before crowding it.
                    let label_galley = painter.layout_no_wrap(
                        row.label.clone(), egui::FontId::proportional(13.0),
                        ui.visuals().text_color());
                    let label_right = rect.left() + 10.0 + label_galley.size().x;
                    painter.galley(
                        egui::pos2(rect.left() + 10.0, rect.center().y - label_galley.size().y * 0.5),
                        label_galley, ui.visuals().text_color());

                    if let Some(pins) = combo_icons {
                        // Draw the combo as glyph icons right-to-left, with "+"
                        // separators. If they would crowd the label, trim the
                        // overflow (leftmost icons) and prefix a "…"; the full
                        // combo is always available in a hover tooltip.
                        const G: f32 = 18.0;       // glyph size
                        const SEP: f32 = 9.0;      // width budget for a "+"
                        const PAD: f32 = 16.0;     // min gap from the label text
                        let min_x = label_right + PAD;
                        let mut x = rect.right() - 10.0;
                        let cy = rect.center().y;
                        let icon_col = if learning_row { egui::Color32::from_rgb(230, 185, 95) }
                            else if is_sel { accent } else { egui::Color32::from_gray(210) };
                        let mut trimmed = false;
                        // Walk pins from last to first, placing each icon to the
                        // left of the previous, stopping when we run out of room.
                        for (j, pin) in pins.iter().enumerate().rev() {
                            // Reserve room for a leading "…" if there are still
                            // earlier pins we might not fit.
                            let need_ellipsis = j > 0;
                            let reserve = if need_ellipsis { SEP } else { 0.0 };
                            if x - G < min_x + reserve {
                                trimmed = true;
                                break;
                            }
                            let icon_rect = egui::Rect::from_min_size(
                                egui::pos2(x - G, cy - G * 0.5), egui::vec2(G, G));
                            if let Some(tex) = self.gp_legend_glyph(ctx, glyph_skin, pin) {
                                painter.image(tex.id(), icon_rect,
                                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                    egui::Color32::WHITE);
                            } else {
                                painter.text(icon_rect.center(), egui::Align2::CENTER_CENTER,
                                    gp_pin_token(pin), egui::FontId::proportional(11.0), icon_col);
                            }
                            x -= G;
                            if j > 0 {
                                x -= SEP;
                                painter.text(egui::pos2(x + SEP * 0.5, cy),
                                    egui::Align2::CENTER_CENTER, "+",
                                    egui::FontId::proportional(12.0), icon_col);
                            }
                        }
                        if trimmed {
                            painter.text(egui::pos2(x, cy), egui::Align2::RIGHT_CENTER, "…",
                                egui::FontId::proportional(13.0), icon_col);
                        }
                        // Full combo tooltip (always, since even untrimmed icons
                        // can be ambiguous).
                        resp.on_hover_text(pretty_chord_combo(&pins));
                    } else {
                        painter.text(
                            egui::pos2(rect.right() - 10.0, rect.center().y),
                            egui::Align2::RIGHT_CENTER, &val_str,
                            egui::FontId::proportional(13.0),
                            if learning_row { egui::Color32::from_rgb(230, 185, 95) }
                            else if is_sel { accent }
                            else { ui.visuals().weak_text_color() });
                    }
                    ui.add_space(2.0);
                }
            });
        ctx.request_repaint();
    }
}
