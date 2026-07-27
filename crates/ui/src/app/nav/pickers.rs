//! Gamepad-nav driving of the popup pickers (keyboard/mouse cell grid and
//! the press-mode list).

use super::*;

impl FlexInputApp {

    pub(crate) fn drive_kbm_picker(
        &mut self,
        step_dir: Option<crate::gamepad_nav::NavDir>,
        nav: &crate::gamepad_nav::NavInput,
    ) {
        use crate::gamepad_nav::NavDir;
        use crate::kbm_picker::{clamp_index, nearest_in_dir, picker_cells};

        // East closes the picker.
        if nav.is_rising("btn_east") {
            self.gamepad_nav.kbm_picker_open = false;
            self.gamepad_nav.kbm_picker_viewport = None;
            return;
        }
        // Spatial navigation over the cells actually shown for this mode (the
        // Touch-Zones variant hides the touchpad cluster and adds analog outputs),
        // so focus never lands on a hidden cell and the analog cells are reachable.
        let cells = picker_cells(self.gamepad_nav.kbm_picker_touch_zones, &self.macro_display_entries());
        let mut idx = clamp_index(&cells, self.gamepad_nav.kbm_picker_idx);
        idx = match step_dir {
            Some(NavDir::Left)  => nearest_in_dir(&cells, idx, -1.0, 0.0),
            Some(NavDir::Right) => nearest_in_dir(&cells, idx, 1.0, 0.0),
            Some(NavDir::Up)    => nearest_in_dir(&cells, idx, 0.0, -1.0),
            Some(NavDir::Down)  => nearest_in_dir(&cells, idx, 0.0, 1.0),
            None => idx,
        };
        self.gamepad_nav.kbm_picker_idx = idx;

        // An empty path is valid (top-level node); only `inner` is required.
        let path = self.gamepad_nav.kbm_picker_path.clone();
        let Some(inner) = self.gamepad_nav.kbm_picker_node else {
            self.gamepad_nav.kbm_picker_open = false;
            self.gamepad_nav.kbm_picker_viewport = None; return; };
        let draft_key = self.gamepad_nav.kbm_picker_draft_key.clone();

        // North resets the output chord.
        if nav.is_rising("btn_north") {
            self.picker_set_draft(&path, inner, &draft_key, &[]);
            return;
        }
        // South appends the focused pin (de-duped) + flips the widget into the
        // phase that shows the draft + enables Add, WITHOUT running the gamepad
        // capture machine (so the South used to pick isn't swept into the chord).
        // Analog-only cells (swipe) are ignored when the input isn't analog;
        // excluded cells (a menu's own targets) are never appendable.
        if nav.is_rising("btn_south") {
            let pin = cells[idx].pin.clone();
            let excluded = self.gamepad_nav.kbm_picker_exclude.as_deref()
                .is_some_and(|p| pin.starts_with(p));
            if !excluded {
                self.picker_append_pin(&pin);
            }
        }
    }

    /// Drive the press-mode picker modal: up/down move the highlight, South
    /// applies the highlighted mode to the target card (and closes), East
    /// cancels.
    pub(crate) fn drive_press_mode_picker(
        &mut self,
        step_dir: Option<crate::gamepad_nav::NavDir>,
        nav: &crate::gamepad_nav::NavInput,
    ) {
        use crate::gamepad_nav::NavDir;
        if nav.is_rising("btn_east") {
            self.gamepad_nav.press_mode_open = false;
            return;
        }
        let n = Self::PRESS_MODES.len();
        let mut i = self.gamepad_nav.press_mode_idx.min(n - 1);
        match step_dir {
            Some(NavDir::Up)   => i = i.saturating_sub(1),
            Some(NavDir::Down) => i = (i + 1).min(n - 1),
            _ => {}
        }
        self.gamepad_nav.press_mode_idx = i;

        if nav.is_rising("btn_south") {
            if let Some(outer) = self.gamepad_nav.press_mode_outer {
                let card = self.gamepad_nav.press_mode_card;
                let mode = Self::PRESS_MODES[i];
                self.nav_remap_set_mode(outer, card, mode);
            }
            self.gamepad_nav.press_mode_open = false;
        }
    }

    /// Render the virtual KB/M picker modal: a keyboard-ish grid of KBM icons
    /// with the focused cell highlighted, the current output chord shown above,
    /// and control hints. Input is handled in `drive_kbm_picker`; this is
    /// display-only. Rendered top-level (not in a sublayer), so painting here is
    /// safe.
    /// Resolve the picker's spatial-nav step this frame: D-pad = one discrete
    /// step per press; left stick = continuous auto-repeat whose rate rises with
    /// deflection (so a big push scrolls fast, a small one steps slowly). Uses
    /// the shared `stick_dir` for direction so up/down match every other nav
    /// (no Y inversion). Shared by the main-window picker loop and the config
    /// overlay's (which owns nav while summoned).
    pub(crate) fn picker_step_dir(
        &mut self,
        nav: &crate::gamepad_nav::NavInput,
        dt: f32,
    ) -> Option<crate::gamepad_nav::NavDir> {
        use crate::gamepad_nav::{self as gn, NavDir};
        if nav.is_rising("dpad_up") { self.gamepad_nav.repeat_dir = None; return Some(NavDir::Up); }
        if nav.is_rising("dpad_down") { self.gamepad_nav.repeat_dir = None; return Some(NavDir::Down); }
        if nav.is_rising("dpad_left") { self.gamepad_nav.repeat_dir = None; return Some(NavDir::Left); }
        if nav.is_rising("dpad_right") { self.gamepad_nav.repeat_dir = None; return Some(NavDir::Right); }
        match gn::stick_dir(nav.lstick) {
            Some(d) => {
                if self.gamepad_nav.repeat_dir != Some(d) {
                    self.gamepad_nav.repeat_dir = Some(d);
                    self.gamepad_nav.repeat_accum = 1.0; // immediate first step
                }
                let mag = nav.lstick.length();
                let rate = 5.0 + ((mag - 0.5) / 0.5).clamp(0.0, 1.0) * 13.0; // ~5..18 cells/s
                self.gamepad_nav.repeat_accum += dt * rate;
                if self.gamepad_nav.repeat_accum >= 1.0 {
                    self.gamepad_nav.repeat_accum -= 1.0;
                    return Some(d);
                }
                None
            }
            None => {
                self.gamepad_nav.repeat_dir = None;
                self.gamepad_nav.repeat_accum = 0.0;
                None
            }
        }
    }

    pub(crate) fn draw_kbm_picker(&mut self, ctx: &egui::Context) {
        let (clicked_pin, done) = self.kbm_picker_window(ctx);
        self.apply_kbm_picker_result(clicked_pin, done);
    }

    /// Render the KB/M picker in its OWN always-on-top viewport, floating over the
    /// game — used when the picker was summoned from a pinned body in the config
    /// overlay (the main-window picker would be behind the game, unreachable).
    /// Fullscreen transparent + interactable so the centered picker window shows
    /// over the game while the rest stays see-through; the config overlay's own
    /// nav still drives it (gamepad reads raw signals, not egui focus).
    pub(crate) fn draw_kbm_picker_over_game(&mut self, ctx: &egui::Context) {
        let monitor = ctx
            .input(|i| i.viewport().monitor_size)
            .filter(|s| s.x > 1.0 && s.y > 1.0)
            .unwrap_or(egui::vec2(1920.0, 1080.0));
        let builder = egui::ViewportBuilder::default()
            .with_title("FlexInput KB/M Picker")
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_taskbar(false)
            .with_resizable(false)
            .with_has_shadow(false)
            .with_active(false)
            .with_position(egui::pos2(0.0, 0.0))
            .with_inner_size(monitor);
        let mut result: (Option<String>, bool) = (None, false);
        ctx.show_viewport_immediate(
            crate::config_overlay::picker_viewport_id(),
            builder,
            |vctx, _class| {
                result = self.kbm_picker_window(vctx);
            },
        );
        self.apply_kbm_picker_result(result.0, result.1);
    }

    /// Apply a picker interaction collected by `kbm_picker_window` — split out
    /// so a sub-patch editor viewport (which holds `&self` during its closure)
    /// can render the window inline and apply the result once `&mut` is back.
    pub(crate) fn apply_kbm_picker_result(&mut self, clicked_pin: Option<String>, done: bool) {
        if let Some(pin) = clicked_pin {
            self.picker_append_pin(&pin);
        }
        if done {
            self.gamepad_nav.kbm_picker_open = false;
            self.gamepad_nav.kbm_picker_viewport = None;
        }
    }

    /// Render the picker window into `ctx` (read-only on `self`) and report
    /// what the user did: `(clicked pin, Done pressed)`.
    pub(crate) fn kbm_picker_window(&self, ctx: &egui::Context) -> (Option<String>, bool) {
        if !self.gamepad_nav.kbm_picker_open { return (None, false); }
        use crate::kbm_picker::{clamp_index, layout_extent, picker_cells, MACRO_Y};
        let macros = self.macro_display_entries();
        let cells = picker_cells(self.gamepad_nav.kbm_picker_touch_zones, &macros);
        let sel = clamp_index(&cells, self.gamepad_nav.kbm_picker_idx);
        let accent = ctx.style().visuals.selection.stroke.color;

        // Current output chord for the header preview (read from whichever draft
        // param this picker session targets — any sub-patch depth).
        let dk = self.gamepad_nav.kbm_picker_draft_key.clone();
        let chord: Vec<String> = match self.gamepad_nav.kbm_picker_node {
            Some(i) => self.picker_draft_vec(&self.gamepad_nav.kbm_picker_path, i, &dk),
            None => Vec::new(),
        };
        // Whether analog-only (swipe) cells are usable for this target.
        let analog_ok = self.picker_analog_input_ok();

        const UNIT: f32 = 30.0; // px per grid unit
        const GAP: f32 = 3.0;   // gap between adjacent keys
        let (ext_x, ext_y) = layout_extent(&cells);
        let board_w = ext_x * (UNIT + GAP);
        let board_h = ext_y * (UNIT + GAP);
        let skin = crate::canvas::remapper_icons::Skin::Kbm;

        // Collected from the closure (no &mut self inside the egui window body).
        let mut clicked_pin: Option<String> = None;
        let mut done = false;

        // Touch Zones variant: a touchpad can't remap to itself, so the touchpad
        // cluster is hidden and analog-output cells replace it (both handled by
        // `picker_cells`, which also keeps them navigable).
        let tz = self.gamepad_nav.kbm_picker_touch_zones;

        egui::Window::new(if tz { "⌨ KB/M + mouse picker" } else { "⌨ KB/M + touchpad picker" })
            .id(egui::Id::new("gp_kbm_picker"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(
                        "Click or LS/D-pad: move   South: add   North: clear   East/Done: close")
                        .small().color(egui::Color32::from_gray(150)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(egui::RichText::new("Done").size(13.0)).clicked() { done = true; }
                    });
                });
                ui.add_space(4.0);
                // Output chord preview.
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Output:").small().weak());
                    if chord.is_empty() {
                        ui.label(egui::RichText::new("(none)").small().italics().weak());
                    } else {
                        for (i, pin) in chord.iter().enumerate() {
                            if i > 0 { ui.label(egui::RichText::new("+").strong()); }
                            // Macro pins show the port's display name.
                            let label = macros.iter().find(|e| e.pin == *pin)
                                .map(|e| e.name.clone())
                                .unwrap_or_else(|| kbm_pin_label(pin));
                            ui.label(egui::RichText::new(label).strong());
                        }
                    }
                });
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);

                // Absolute-positioned board: allocate one area sized to the layout
                // extent, then place each cell at its (x,y)*unit origin. Cells are
                // individually clickable (mouse) AND highlight the gamepad focus.
                let (board, _) = ui.allocate_exact_size(
                    egui::vec2(board_w, board_h), egui::Sense::hover());
                // "MACROS" caption above the dynamic cluster (only when present).
                if cells.iter().any(|c| c.macro_meta.is_some()) {
                    ui.painter_at(board).text(
                        board.min + egui::vec2(2.0, (MACRO_Y - 0.42) * (UNIT + GAP)),
                        egui::Align2::LEFT_TOP, "MACROS",
                        egui::FontId::proportional(9.0), egui::Color32::from_gray(130));
                }
                for (i, cell) in cells.iter().enumerate() {
                    let min = board.min + egui::vec2(
                        cell.x * (UNIT + GAP), cell.y * (UNIT + GAP));
                    let size = egui::vec2(
                        cell.width * UNIT + (cell.width - 1.0) * GAP, UNIT);
                    let rect = egui::Rect::from_min_size(min, size);
                    // Analog-only cells (swipe) are disabled unless the input is
                    // analog; excluded cells (a menu's own targets from its own
                    // cards) are always disabled.
                    let self_target = self.gamepad_nav.kbm_picker_exclude.as_deref()
                        .is_some_and(|p| cell.pin.starts_with(p));
                    let disabled = (cell.analog_only && !analog_ok) || self_target;
                    let resp = if disabled {
                        ui.interact(rect, egui::Id::new(("kbm_cell", i)), egui::Sense::hover())
                    } else {
                        let r = ui.interact(rect, egui::Id::new(("kbm_cell", i)), egui::Sense::click());
                        if r.clicked() { clicked_pin = Some(cell.pin.clone()); }
                        r
                    };
                    let focused = i == sel;
                    let hovered = resp.hovered() && !disabled;
                    let painter = ui.painter_at(rect);
                    let bg = if disabled {
                        egui::Color32::from_gray(28)
                    } else if focused {
                        let [rr, gg, bb, _] = accent.to_array();
                        egui::Color32::from_rgba_unmultiplied(rr, gg, bb, 60)
                    } else if hovered {
                        egui::Color32::from_gray(60)
                    } else {
                        egui::Color32::from_gray(40)
                    };
                    painter.rect_filled(rect, 4.0, bg);
                    if focused {
                        painter.rect_stroke(rect, 4.0,
                            egui::Stroke::new(2.0, accent), egui::StrokeKind::Outside);
                    }
                    let tint = if disabled { egui::Color32::from_gray(110) } else { egui::Color32::WHITE };
                    // Macro cells: the port's icon (custom patch-embedded SVG
                    // or embedded set) or the port name + full-name tooltip.
                    // KBM cells: skin icon or text fallback.
                    if let Some(entry) = cell.macro_meta.as_ref() {
                        if let Some(tex) = crate::macro_icons::macro_port_icon_texture(
                            ctx, &entry.icon, &entry.icon_svg, UNIT - 6.0)
                        {
                            let s = (UNIT - 6.0).min(size.x - 6.0).max(8.0);
                            let img_rect = egui::Rect::from_center_size(
                                rect.center(), egui::vec2(s, s));
                            painter.image(tex.id(), img_rect,
                                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                tint);
                        } else {
                            let short: String = entry.name.chars().take(4).collect();
                            painter.text(rect.center(), egui::Align2::CENTER_CENTER,
                                short, egui::FontId::proportional(10.0), ui.visuals().text_color());
                        }
                        if self_target {
                            resp.on_hover_text(format!("{} — a menu can't target itself", entry.name));
                        } else {
                            resp.on_hover_text(&entry.name);
                        }
                        continue;
                    }
                    if let Some(tex) = kbm_cell_texture(ctx, skin, &cell.pin) {
                        let s = (UNIT - 6.0).min(size.x - 6.0).max(8.0);
                        let img_rect = egui::Rect::from_center_size(
                            rect.center(), egui::vec2(s, s));
                        painter.image(tex.id(), img_rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            tint);
                    } else {
                        // Compact labels — "L-Stick" won't fit a 30px cell.
                        let lbl = match cell.pin.as_str() {
                            "left_stick" => "LS".to_string(),
                            "right_stick" => "RS".to_string(),
                            _ => kbm_pin_label(&cell.pin),
                        };
                        painter.text(rect.center(), egui::Align2::CENTER_CENTER,
                            lbl,
                            egui::FontId::proportional(11.0),
                            if disabled { egui::Color32::from_gray(110) } else { ui.visuals().text_color() });
                    }
                }
            });

        ctx.request_repaint();
        (clicked_pin, done)
    }

    /// Modal press-mode picker: a vertical list of the press modes (glyph +
    /// label + short description) with the current/highlighted one accented.
    /// Opened from a mapping card's press-mode field; input handled in
    /// `drive_press_mode_picker`.
    pub(crate) fn draw_press_mode_picker(&mut self, ctx: &egui::Context) {
        if !self.gamepad_nav.press_mode_open { return; }
        let sel = self.gamepad_nav.press_mode_idx.min(Self::PRESS_MODES.len() - 1);
        // Current mode on the target card (to mark the active row).
        let cur_mode = self.gamepad_nav.press_mode_outer.map(|o|
            self.nav_remap_card_mode(o, self.gamepad_nav.press_mode_card)
                .unwrap_or_else(|| "down".to_string()))
            .unwrap_or_else(|| "down".to_string());
        let accent = ctx.style().visuals.selection.stroke.color;

        egui::Window::new("Press mode")
            .id(egui::Id::new("gp_press_mode_picker"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(
                    "LS/D-pad: move   South: apply   East: cancel")
                    .small().color(egui::Color32::from_gray(150)));
                ui.add_space(6.0);
                for (i, mode) in Self::PRESS_MODES.iter().enumerate() {
                    let glyph = crate::canvas::viewer::remapper_press_mode_glyph(mode);
                    let label = crate::canvas::viewer::remapper_press_mode_label(mode);
                    let focused = i == sel;
                    let is_cur = *mode == cur_mode;
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(220.0, 26.0), egui::Sense::hover());
                    let painter = ui.painter();
                    if focused {
                        let [r, g, b, _] = accent.to_array();
                        painter.rect_filled(rect, 4.0,
                            egui::Color32::from_rgba_unmultiplied(r, g, b, 55));
                        painter.rect_stroke(rect, 4.0,
                            egui::Stroke::new(1.5, accent), egui::StrokeKind::Inside);
                    }
                    painter.text(rect.left_center() + egui::vec2(10.0, 0.0),
                        egui::Align2::LEFT_CENTER, glyph,
                        egui::FontId::proportional(16.0), ui.visuals().text_color());
                    painter.text(rect.left_center() + egui::vec2(34.0, 0.0),
                        egui::Align2::LEFT_CENTER, label,
                        egui::FontId::proportional(13.0), ui.visuals().text_color());
                    if is_cur {
                        painter.text(rect.right_center() - egui::vec2(8.0, 0.0),
                            egui::Align2::RIGHT_CENTER, "●",
                            egui::FontId::proportional(10.0), accent);
                    }
                }
            });
        ctx.request_repaint();
    }
}
