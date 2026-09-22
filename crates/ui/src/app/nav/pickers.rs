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

        // East closes the picker. While typing that means cancel: what was
        // typed is dropped, which is why Enter and West exist to keep it.
        if nav.is_rising("btn_east") {
            self.close_kbm_picker();
            return;
        }
        // Spatial navigation over the cells actually shown for this mode (the
        // Touch-Zones variant hides the touchpad cluster and adds analog outputs),
        // so focus never lands on a hidden cell and the analog cells are reachable.
        let pad = self.gamepad_nav.kbm_picker_use != crate::gamepad_nav::PickerUse::Chord;
        let cells = picker_cells(
            self.gamepad_nav.kbm_picker_touch_zones, pad, &self.macro_display_entries());
        // A cell is usable (focusable + mappable) unless it's an analog output on
        // a discrete target (analog not OK) or one of the target's own pins. Same
        // predicate the renderer uses to grey cells — so grey == unreachable.
        let analog_ok = self.picker_analog_input_ok();
        let excl = self.gamepad_nav.kbm_picker_exclude.clone();
        let usable = |cell: &crate::kbm_picker::PickerCell| {
            let self_target = excl.as_deref().is_some_and(|p| cell.pin.starts_with(p));
            !((cell.analog_only && !analog_ok) || self_target)
        };
        // What a cell is worth depends on what the picker is for: a key with no
        // character types nothing, and a key JSM has no name for can't be a
        // binding. Both grey the same way the chord purpose greys its own.
        let purpose = self.gamepad_nav.kbm_picker_use;
        let vocab = JsmVocab::of(self.gamepad_nav.kbm_slot);
        let usable = |cell: &crate::kbm_picker::PickerCell| {
            usable(cell) && cell_does_something(purpose, &cell.pin, &vocab)
        };
        let mut idx = clamp_index(&cells, self.gamepad_nav.kbm_picker_idx);
        idx = match step_dir {
            Some(NavDir::Left)  => nearest_in_dir(&cells, idx, -1.0, 0.0, &usable),
            Some(NavDir::Right) => nearest_in_dir(&cells, idx, 1.0, 0.0, &usable),
            Some(NavDir::Up)    => nearest_in_dir(&cells, idx, 0.0, -1.0, &usable),
            Some(NavDir::Down)  => nearest_in_dir(&cells, idx, 0.0, 1.0, &usable),
            None => idx,
        };
        self.gamepad_nav.kbm_picker_idx = idx;

        // Typing is the picker's other purpose: same grid, same navigation,
        // but an activated cell gives a character or a name rather than a pin.
        if purpose != crate::gamepad_nav::PickerUse::Chord {
            self.drive_kbm_typing(nav, &cells[idx], &vocab);
            return;
        }

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
        if nav.is_rising("btn_south") && usable(&cells[idx]) {
            let pin = cells[idx].pin.clone();
            self.picker_append_pin(&pin);
        }
    }

    /// Close the picker and forget whatever session it was running.
    ///
    /// One place, because a session left half-set is a picker that opens next
    /// time still typing into the last thing that asked.
    pub(crate) fn close_kbm_picker(&mut self) {
        self.gamepad_nav.kbm_picker_open = false;
        self.gamepad_nav.kbm_picker_viewport = None;
        self.gamepad_nav.kbm_picker_use = crate::gamepad_nav::PickerUse::Chord;
        self.gamepad_nav.kbm_text.clear();
        self.gamepad_nav.kbm_text_caps = false;
    }

    /// Hand a finished session's text back to the node that asked for it, and
    /// close. Whoever opened the session drains `kbm_text_done`.
    fn finish_kbm_typing(&mut self, text: String) {
        if let Some(node) = self.gamepad_nav.kbm_picker_node {
            self.gamepad_nav.kbm_text_done = Some((node, text));
        }
        self.close_kbm_picker();
    }

    /// Drive the picker while it is typing rather than binding.
    ///
    /// South activates the focused key. West and North finish, keeping what was
    /// typed — North because it is the button that opened the picker, and a
    /// toggle you can only close with a different button is one you close by
    /// guessing. East and the Escape key drop it instead.
    fn drive_kbm_typing(
        &mut self,
        nav: &crate::gamepad_nav::NavInput,
        cell: &crate::kbm_picker::PickerCell,
        vocab: &JsmVocab,
    ) {
        use crate::gamepad_nav::PickerUse;
        use crate::kbm_picker::{cell_typed, Typed};
        if self.gamepad_nav.kbm_picker_use == PickerUse::JsmName {
            // A binding can carry an event modifier or be a chord (`\SPACE`,
            // `A+B`), and no single key expresses that — so the same board drops
            // into typing, already holding whatever the token said.
            if nav.is_rising("btn_west") {
                self.gamepad_nav.kbm_picker_use = PickerUse::Text;
                return;
            }
            if nav.is_rising("btn_north") {
                self.close_kbm_picker();
                return;
            }
            // Otherwise one key, one name, done — a keyboard whose keys had to
            // be spelled out letter by letter would be a worse list than the
            // list is.
            if nav.is_rising("btn_south") {
                if let Some(name) = vocab.insert_for(&cell.pin) {
                    self.finish_kbm_typing(name);
                }
            }
            return;
        }
        // West is the caps latch — held-shift rather than caps-lock, so it
        // changes the digits into their symbols too. North finishes (it is the
        // button that opened the board, and a toggle you close with a different
        // button is one you close by guessing); East drops what was typed.
        if nav.is_rising("btn_west") {
            self.gamepad_nav.kbm_text_caps = !self.gamepad_nav.kbm_text_caps;
            return;
        }
        if nav.is_rising("btn_north") {
            let typed = std::mem::take(&mut self.gamepad_nav.kbm_text);
            self.finish_kbm_typing(typed);
            return;
        }
        if !nav.is_rising("btn_south") {
            return;
        }
        let caps = self.gamepad_nav.kbm_text_caps;
        match cell_typed(&cell.pin, caps) {
            Some(Typed::Char(c)) => self.gamepad_nav.kbm_text.push(c),
            Some(Typed::Backspace) => {
                self.gamepad_nav.kbm_text.pop();
            }
            Some(Typed::Caps) => self.gamepad_nav.kbm_text_caps = !caps,
            Some(Typed::Commit) => {
                let typed = std::mem::take(&mut self.gamepad_nav.kbm_text);
                self.finish_kbm_typing(typed);
            }
            Some(Typed::Cancel) => self.close_kbm_picker(),
            // A key with nothing to type is already greyed and unfocusable; this
            // is only reachable by a click.
            None => {}
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
        let modes = self.gamepad_nav.press_mode_outer
            .map_or(Self::PRESS_MODES, |o| self.nav_press_modes(o, self.gamepad_nav.press_mode_card));
        let n = modes.len();
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
                let mode = modes[i];
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
        let pad = self.gamepad_nav.kbm_picker_use != crate::gamepad_nav::PickerUse::Chord;
        let cells = picker_cells(self.gamepad_nav.kbm_picker_touch_zones, pad, &macros);
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
        // What this session is for, and — when it is naming a key for a config —
        // the names JSM binds our pins by, so a key it can't name greys out.
        let purpose = self.gamepad_nav.kbm_picker_use;
        let vocab = JsmVocab::of(self.gamepad_nav.kbm_slot);

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
                    // The hints are the session's, not the window's: the same
                    // board means three different things to the buttons.
                    let hints = match purpose {
                        crate::gamepad_nav::PickerUse::Chord =>
                            "Click or LS/D-pad: move   South: add   North: clear   East/Done: close",
                        crate::gamepad_nav::PickerUse::JsmName => match self.gamepad_nav.kbm_slot {
                            // What the keys stand for depends on which side of
                            // the line the pick lands on, so say which.
                            crate::gamepad_nav::JsmSlot::Name =>
                                "A line starts with a BUTTON. South: insert its name   West: type instead   East: cancel",
                            crate::gamepad_nav::JsmSlot::Value =>
                                "Click or LS/D-pad: move   South: insert this key's name   West: type instead   East: cancel",
                            // Everything greys out here, and a board full of grey
                            // has to say why rather than look broken.
                            crate::gamepad_nav::JsmSlot::Neither =>
                                "A NUMBER goes here: hold South and push the stick. West: type a word instead   East: cancel",
                        },
                        crate::gamepad_nav::PickerUse::Text =>
                            "South: type   West or Shift: caps   Backspace   North or Enter: done   East or Esc: cancel",
                    };
                    ui.label(egui::RichText::new(hints)
                        .small().color(egui::Color32::from_gray(150)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(egui::RichText::new("Done").size(13.0)).clicked() { done = true; }
                    });
                });
                ui.add_space(4.0);
                // What has been typed, while typing. Shown as it will land, with
                // a caret and the caps latch, because a virtual keyboard with no
                // sign of what you have pressed is a keyboard you type twice.
                if purpose == crate::gamepad_nav::PickerUse::Text {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Typing:").small().weak());
                        ui.label(
                            egui::RichText::new(format!("{}_", self.gamepad_nav.kbm_text))
                                .monospace()
                                .strong(),
                        );
                        if self.gamepad_nav.kbm_text_caps {
                            ui.label(egui::RichText::new("CAPS").small().strong());
                        }
                    });
                }
                // Which name the focused key would write, while picking one.
                if purpose == crate::gamepad_nav::PickerUse::JsmName {
                    let focused = cells.get(sel).and_then(|c| vocab.insert_for(&c.pin));
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Binds as:").small().weak());
                        match focused {
                            Some(n) => {
                                ui.label(egui::RichText::new(&n).monospace().strong());
                            }
                            None => {
                                ui.label(
                                    egui::RichText::new("(this key has no JSM name)")
                                        .small()
                                        .italics()
                                        .weak(),
                                );
                            }
                        }
                    });
                }
                // Output chord preview.
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Output:").small().weak());
                    if purpose != crate::gamepad_nav::PickerUse::Chord {
                        ui.label(egui::RichText::new("—").small().weak());
                    } else if chord.is_empty() {
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
                // "GAMEPAD" caption above its cluster (only when offered).
                if cells.iter().any(|c| crate::kbm_picker::is_pad_cell(&c.pin)) {
                    let (px, py) = crate::kbm_picker::pad_caption_at();
                    ui.painter_at(board).text(
                        board.min + egui::vec2(px * (UNIT + GAP), py * (UNIT + GAP)),
                        egui::Align2::LEFT_TOP, "GAMEPAD",
                        egui::FontId::proportional(9.0), egui::Color32::from_gray(130));
                }
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
                    let disabled = (cell.analog_only && !analog_ok)
                        || self_target
                        || !cell_does_something(purpose, &cell.pin, &vocab);
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
                    // The gamepad cluster in the Xbox dialect, which is the
                    // one whose names this picker writes (`X_A`, `X_LB`): a
                    // board showing a circle while inserting `X_B` reads as a
                    // mistake, whichever pad you actually hold.
                    let skin = if crate::kbm_picker::is_pad_cell(&cell.pin) {
                        crate::canvas::remapper_icons::Skin::Xbox
                    } else {
                        skin
                    };
                    // While typing, a key wears the character it would ACTUALLY
                    // add, which the caps latch changes for half of them: a
                    // number row that doesn't change is a caps key you can't
                    // tell you pressed. A character with no glyph in the icon set
                    // (@ # $ % & ( ) { } |) is drawn over the blank key shape,
                    // in the cell's own background colour so it reads as cut out
                    // of the key the way the drawn ones are.
                    let typed = (purpose == crate::gamepad_nav::PickerUse::Text)
                        .then(|| crate::kbm_picker::cell_typed(
                            &cell.pin, self.gamepad_nav.kbm_text_caps))
                        .flatten()
                        .and_then(|t| match t {
                            crate::kbm_picker::Typed::Char(c) if c != ' ' => Some(c),
                            _ => None,
                        });
                    let face = match typed {
                        // A letter wears the glyph the icon set drew for it —
                        // `A` and `a` are the same key face — so only the
                        // characters with no glyph at all get drawn.
                        Some(c) if c.is_ascii_alphabetic() => {
                            (kbm_cell_texture(ctx, skin, &cell.pin), None)
                        }
                        Some(c) => match crate::canvas::remapper_icons::char_svg(c) {
                            Some(svg) => (kbm_svg_texture(ctx, svg), None),
                            None => (
                                kbm_svg_texture(ctx, crate::canvas::remapper_icons::kb_blank()),
                                Some(c),
                            ),
                        },
                        None => (kbm_cell_texture(ctx, skin, &cell.pin), None),
                    };
                    if let Some(c) = face.1 {
                        if let Some(tex) = face.0.as_ref() {
                            let s = (UNIT - 6.0).min(size.x - 6.0).max(8.0);
                            let img_rect = egui::Rect::from_center_size(
                                rect.center(), egui::vec2(s, s));
                            painter.image(
                                tex.id(),
                                img_rect,
                                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                tint,
                            );
                        }
                        // Sized and weighted to sit beside the drawn glyphs
                        // rather than shout over them, and always dark: the key
                        // shape is white in every state, and painting the
                        // character in the cell's own background made it vanish
                        // on the focused key — which already has a ring round it
                        // to say where it is.
                        let glyph = egui::Color32::from_gray(40);
                        let font = egui::FontId::proportional(UNIT * 0.42);
                        // There is no bold face in the default font set, so the
                        // weight comes from drawing the glyph over itself a
                        // fraction of a pixel out. Cheap, and it matches the
                        // drawn symbols' stroke closely enough to pass.
                        for off in [
                            egui::vec2(-0.45, 0.0), egui::vec2(0.45, 0.0),
                            egui::vec2(0.0, -0.45), egui::vec2(0.0, 0.45),
                        ] {
                            painter.text(
                                rect.center() + off,
                                egui::Align2::CENTER_CENTER,
                                c.to_string(),
                                font.clone(),
                                glyph,
                            );
                        }
                        painter.text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            c.to_string(),
                            font,
                            glyph,
                        );
                        continue;
                    }
                    if let Some(tex) = face.0 {
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
        let modes = self.gamepad_nav.press_mode_outer
            .map_or(Self::PRESS_MODES, |o| self.nav_press_modes(o, self.gamepad_nav.press_mode_card));
        let sel = self.gamepad_nav.press_mode_idx.min(modes.len() - 1);
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
                for (i, mode) in modes.iter().enumerate() {
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

/// The names a JSM config could use for our pins, on whichever side of a line
/// the picker is inserting into.
///
/// Built once per frame rather than per cell: there are 150 cells and the
/// vocabulary is 200 names.
#[derive(Default)]
pub(crate) struct JsmVocab {
    slot: crate::gamepad_nav::JsmSlot,
    names: std::collections::HashMap<String, String>,
}

impl JsmVocab {
    pub(crate) fn of(slot: crate::gamepad_nav::JsmSlot) -> JsmVocab {
        use crate::gamepad_nav::JsmSlot as Slot;
        let names = match slot {
            Slot::Name => flexinput_engine::eval::jsm_input_names_by_pin(),
            Slot::Value => flexinput_engine::eval::jsm_names_by_pin(),
            Slot::Neither => Default::default(),
        };
        JsmVocab { slot, names }
    }

    /// What inserting this cell writes into a config, if anything.
    ///
    /// Left of the `=`, a pad button goes in as the name JSM READS it as (`S`);
    /// right of it, as the name a virtual pad REPORTS (`X_A`) — and a key or a
    /// mouse button only belongs on the right at all. One of ours — a Macro
    /// Output port, a Virtual Menu entry — has no JSM name, so it goes in under
    /// the `@` tag, and that is a value too. One function, so what the board
    /// offers, what it previews and what it inserts cannot disagree.
    pub(crate) fn insert_for(&self, pin: &str) -> Option<String> {
        if let Some(n) = self.names.get(crate::kbm_picker::pin_as_bound(pin)) {
            return Some(n.clone());
        }
        // `Neither` needs no guard of its own: it builds no vocabulary, so the
        // lookup above finds nothing and a FlexInput target is a value.
        fi_insert(self.slot, &crate::macro_icons::registry_entry(pin)?.name)
    }
}

/// How one of OUR targets goes into a config, on this side of a line.
///
/// A Macro Output port or a Virtual Menu entry is something a mapping ENDS at,
/// so it belongs right of the `=` and nowhere else. Split out from the registry
/// lookup so the rule can be tested without a patch.
fn fi_insert(slot: crate::gamepad_nav::JsmSlot, name: &str) -> Option<String> {
    (slot == crate::gamepad_nav::JsmSlot::Value)
        .then(|| flexinput_engine::eval::jsm_fi_tag(name))
}

/// Does this cell do anything for what the picker is being used for?
///
/// The chord purpose's own exclusions (analog-only cells, a menu's own targets)
/// are separate and stay where they are — this is only about the purpose.
pub(crate) fn cell_does_something(
    purpose: crate::gamepad_nav::PickerUse,
    pin: &str,
    vocab: &JsmVocab,
) -> bool {
    use crate::gamepad_nav::PickerUse;
    match purpose {
        PickerUse::Chord => true,
        // `false` for the caps latch: whether a key types something doesn't
        // depend on which of its two characters it would give.
        PickerUse::Text => crate::kbm_picker::cell_typed(pin, false).is_some(),
        PickerUse::JsmName => vocab.insert_for(pin).is_some(),
    }
}

#[cfg(test)]
mod vocab_tests {
    use super::{fi_insert, JsmVocab};
    use crate::gamepad_nav::JsmSlot;

    /// One of our own targets is something a mapping ENDS at, so it belongs
    /// right of the `=` and nowhere else — a line can't START with a macro port.
    #[test]
    fn one_of_our_targets_is_a_value_and_not_a_name() {
        assert_eq!(fi_insert(JsmSlot::Value, "Reload").as_deref(), Some("@Reload"));
        assert_eq!(
            fi_insert(JsmSlot::Value, "Reload sequence").as_deref(),
            Some("@\"Reload sequence\""),
            "and it is written the way the parser reads it back"
        );
        assert_eq!(fi_insert(JsmSlot::Name, "Reload"), None);
        assert_eq!(fi_insert(JsmSlot::Neither, "Reload"), None);
    }

    /// Which side of the line the pick lands on decides what the board's keys
    /// stand for. JSM keeps the two vocabularies apart and so must this.
    #[test]
    fn a_cell_means_a_different_name_on_each_side_of_the_line() {
        let name = JsmVocab::of(JsmSlot::Name);
        let value = JsmVocab::of(JsmSlot::Value);

        // A pad button: the name JSM reads inputs as, or the one a virtual pad
        // reports.
        assert_eq!(name.insert_for("btn_south").as_deref(), Some("S"));
        assert_eq!(value.insert_for("btn_south").as_deref(), Some("X_A"));
        assert_eq!(name.insert_for("left_trigger").as_deref(), Some("ZL"));
        assert_eq!(value.insert_for("left_trigger").as_deref(), Some("X_LT"));

        // A key or a mouse button belongs only on the right: nothing on a
        // keyboard can START a JSM line.
        assert_eq!(value.insert_for("key_space").as_deref(), Some("SPACE"));
        assert_eq!(name.insert_for("key_space"), None);
        assert_eq!(value.insert_for("mouse_left").as_deref(), Some("LMOUSE"));
        assert_eq!(name.insert_for("mouse_left"), None);

        // A setting's value takes a number, so the board has nothing for it.
        let neither = JsmVocab::of(JsmSlot::Neither);
        for pin in ["btn_south", "key_space", "mouse_left", "left_trigger"] {
            assert_eq!(neither.insert_for(pin), None, "{pin}");
        }
    }
}
