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
        use crate::kbm_picker::{clamp_index, nearest_in_dir};

        // RT's press edge: it inserts a gap.
        let rt_down = nav.rt > 0.5;
        let rt_rising = rt_down && !self.gamepad_nav.kbm_picker_prev_rt;
        self.gamepad_nav.kbm_picker_prev_rt = rt_down;
        // The chord list has the focus: the D-pad and the face buttons edit
        // the list, not the board.
        if self.gamepad_nav.kbm_picker_use == crate::gamepad_nav::PickerUse::Chord {
            if let Some(f) = self.gamepad_nav.kbm_picker_list {
                self.drive_picker_list(f, step_dir, nav, rt_rising);
                return;
            }
        }

        // East closes the picker. While typing that means cancel: what was
        // typed is dropped, which is why Enter and West exist to keep it.
        if nav.is_rising("btn_east") {
            self.close_kbm_picker();
            return;
        }
        // LB / RB step through the tabs.
        let tab_step = if nav.is_rising("btn_rb") { 1 } else if nav.is_rising("btn_lb") { -1 } else { 0 };
        if tab_step != 0 {
            let tabs = self.picker_tabs();
            let at = tabs.iter().position(|t| *t == self.picker_tab()).unwrap_or(0) as i32;
            self.picker_select_tab(tabs[(at + tab_step).rem_euclid(tabs.len() as i32) as usize]);
            return;
        }
        // Spatial navigation over the cells actually shown on this tab for this
        // mode (the Touch-Zones variant hides the touchpad cluster and adds
        // analog outputs), so focus never lands on a hidden cell and the analog
        // cells are reachable.
        let cells = self.picker_tab_cells(self.picker_tab(), &self.macro_display_entries());
        let usable = self.picker_usable();
        let purpose = self.gamepad_nav.kbm_picker_use;
        let vocab = JsmVocab::of(self.gamepad_nav.kbm_slot);
        let mut idx = clamp_index(&cells, self.gamepad_nav.kbm_picker_idx);
        let chord_purpose = purpose == crate::gamepad_nav::PickerUse::Chord;
        // Up past the board's top row goes into the chord list, on its last
        // element — or on "+ Gap" when the list has nothing yet — when there's
        // one to go to.
        if chord_purpose && matches!(step_dir, Some(NavDir::Up))
            && (cells.is_empty() || nearest_in_dir(&cells, idx, 0.0, -1.0, &usable) == idx)
        {
            let n = self.picker_chord().elems(self.picker_side_timed()).len();
            if self.picker_list_stops() > 0 {
                self.gamepad_nav.kbm_picker_list = Some(n.saturating_sub(1));
                self.gamepad_nav.fine_increment = false;
                return;
            }
        }
        // RT on the board: a gap at the end, for the next pick to wait on.
        if chord_purpose && rt_rising && self.picker_side_timed() {
            self.picker_edit_chord(crate::kbm_picker::ChordEdit::InsertGap(None));
            return;
        }
        if !cells.is_empty() {
            idx = match step_dir {
                Some(NavDir::Left)  => nearest_in_dir(&cells, idx, -1.0, 0.0, &usable),
                Some(NavDir::Right) => nearest_in_dir(&cells, idx, 1.0, 0.0, &usable),
                Some(NavDir::Up)    => nearest_in_dir(&cells, idx, 0.0, -1.0, &usable),
                Some(NavDir::Down)  => nearest_in_dir(&cells, idx, 0.0, 1.0, &usable),
                None => idx,
            };
        }
        self.gamepad_nav.kbm_picker_idx = idx;
        // The focused cell — none on an empty tab (Macros, before there are any).
        let focused = cells.get(idx).cloned();

        // Typing is the picker's other purpose: same grid, same navigation,
        // but an activated cell gives a character or a name rather than a pin.
        if purpose != crate::gamepad_nav::PickerUse::Chord {
            self.drive_kbm_typing(nav, focused.as_ref(), &vocab);
            return;
        }
        let stage = self.gamepad_nav.kbm_picker_stage;

        // An empty path is valid (top-level node); only `inner` is required.
        if self.gamepad_nav.kbm_picker_node.is_none() {
            self.gamepad_nav.kbm_picker_open = false;
            self.gamepad_nav.kbm_picker_viewport = None;
            return;
        }

        // West switches between the input and the output, on a card that has
        // both. Focus stays put: the board is the same, only what greys changes.
        if nav.is_rising("btn_west") && self.picker_has_both_stages() {
            use crate::kbm_picker::PickStage;
            self.picker_switch_stage(match stage {
                PickStage::Input => PickStage::Output,
                PickStage::Output => PickStage::Input,
            });
            return;
        }
        // North resets the chord being chosen.
        if nav.is_rising("btn_north") {
            self.picker_clear_draft();
            return;
        }
        // South appends the focused pin (de-duped) + flips the widget into the
        // phase that shows the draft + enables Add, WITHOUT running the gamepad
        // capture machine (so the South used to pick isn't swept into the chord).
        // Analog-only cells (swipe) are ignored when the input isn't analog;
        // excluded cells (a menu's own targets) are never appendable.
        if let Some(cell) = focused.filter(|c| nav.is_rising("btn_south") && usable(c)) {
            self.picker_append_pin(&cell.pin);
        }
    }

    /// How many stops the chord list has for the pad: its elements, then —
    /// on a side that takes time — the "+ Gap" button after them.
    pub(crate) fn picker_list_stops(&self) -> usize {
        let timed = self.picker_side_timed();
        self.picker_chord().elems(timed).len() + usize::from(timed)
    }

    /// Drive the chord list while it has the focus. Left / Right move along
    /// it, out to "+ Gap" at its end; Up / Down retime a gap or a pick's
    /// duration (a duration stepped below its smallest goes back to the
    /// input's); West toggles fine steps; North deletes; RT puts a gap after
    /// the focus, as South does on "+ Gap"; East — or South anywhere else —
    /// goes back to the board. An input's list has no time, so there Down
    /// goes back to the board too.
    fn drive_picker_list(
        &mut self,
        focus: usize,
        step_dir: Option<crate::gamepad_nav::NavDir>,
        nav: &crate::gamepad_nav::NavInput,
        rt_rising: bool,
    ) {
        use crate::gamepad_nav::NavDir;
        use crate::kbm_picker::ChordEdit;
        let timed = self.picker_side_timed();
        let chord = self.picker_chord();
        let elems = chord.elems(timed);
        let stops = elems.len() + usize::from(timed);
        let f = focus.min(stops.saturating_sub(1));
        // The focus is on "+ Gap" past the last element.
        let on_add_gap = timed && f == elems.len();
        let back = |app: &mut Self| {
            app.gamepad_nav.kbm_picker_list = None;
            app.gamepad_nav.fine_increment = false;
        };
        if stops == 0 || nav.is_rising("btn_east") {
            back(self);
            return;
        }
        if nav.is_rising("btn_south") {
            if on_add_gap {
                self.picker_edit_chord(ChordEdit::InsertGap(elems.last().copied()));
            } else {
                back(self);
            }
            return;
        }
        if nav.is_rising("btn_west") && timed {
            self.gamepad_nav.fine_increment = !self.gamepad_nav.fine_increment;
        }
        let fine = self.gamepad_nav.fine_increment;
        match step_dir {
            Some(NavDir::Left) => self.gamepad_nav.kbm_picker_list = Some(f.saturating_sub(1)),
            Some(NavDir::Right) => self.gamepad_nav.kbm_picker_list = Some((f + 1).min(stops - 1)),
            Some(d @ (NavDir::Up | NavDir::Down)) if timed && !on_add_gap => {
                let mut c = chord;
                c.nudge(elems[f], if d == NavDir::Up { 1 } else { -1 }, fine);
                self.picker_set_chord(&c);
            }
            Some(NavDir::Down) => back(self),
            _ => {}
        }
        if on_add_gap {
            if rt_rising {
                self.picker_edit_chord(ChordEdit::InsertGap(elems.last().copied()));
            }
            return;
        }
        let elem = elems[f];
        if nav.is_rising("btn_north") {
            self.picker_edit_chord(ChordEdit::Delete(elem));
        } else if rt_rising && timed {
            self.picker_edit_chord(ChordEdit::InsertGap(Some(elem)));
        }
    }

    /// Is a cell usable (focusable + mappable) for the open session? Not when
    /// it's an analog output on a discrete target (analog not OK), one of the
    /// target's own pins, something that can't sit on the side being chosen,
    /// or — by the picker's purpose — a key with no character to type or no
    /// JSM name. The renderer greys exactly these, so grey == unreachable.
    pub(crate) fn picker_usable(&self) -> impl Fn(&crate::kbm_picker::PickerCell) -> bool {
        let analog_ok = self.picker_analog_input_ok();
        let excl = self.gamepad_nav.kbm_picker_exclude.clone();
        let stage = self.gamepad_nav.kbm_picker_stage;
        let purpose = self.gamepad_nav.kbm_picker_use;
        let chord = purpose == crate::gamepad_nav::PickerUse::Chord;
        let vocab = JsmVocab::of(self.gamepad_nav.kbm_slot);
        // An analog-only cell needs an analog INPUT to drive it as an output;
        // as an input (a whole stick) it is itself the analog input.
        let choosing_input = chord && stage == crate::kbm_picker::PickStage::Input;
        move |cell| {
            let self_target = excl.as_deref().is_some_and(|p| cell.pin.starts_with(p));
            let wrong_side = chord && !crate::kbm_picker::stage_allows(stage, &cell.pin);
            !((cell.analog_only && !analog_ok && !choosing_input) || self_target || wrong_side)
                && cell_does_something(purpose, &cell.pin, &vocab)
        }
    }

    /// Show a tab, focus on its first usable cell.
    pub(crate) fn picker_select_tab(&mut self, tab: crate::kbm_picker::PickerTab) {
        let usable = self.picker_usable();
        let cells = self.picker_tab_cells(tab, &self.macro_display_entries());
        self.gamepad_nav.kbm_picker_idx = cells.iter().position(|c| usable(c)).unwrap_or(0);
        self.gamepad_nav.kbm_picker_tab = tab;
    }

    /// Choose for the other side of the mapping. An input can't be a key, so
    /// switching to it from the keyboard tab goes to the gamepad.
    pub(crate) fn picker_switch_stage(&mut self, stage: crate::kbm_picker::PickStage) {
        use crate::kbm_picker::{PickStage, PickerTab};
        self.gamepad_nav.kbm_picker_stage = stage;
        self.gamepad_nav.kbm_picker_list = None;
        let tab = match (stage, self.picker_tab()) {
            (PickStage::Input, PickerTab::Kbm) => PickerTab::Gamepad,
            (_, t) => t,
        };
        self.picker_select_tab(tab);
    }

    /// Close the picker and forget whatever session it was running.
    ///
    /// One place, because a session left half-set is a picker that opens next
    /// time still typing into the last thing that asked.
    pub(crate) fn close_kbm_picker(&mut self) {
        self.gamepad_nav.kbm_picker_open = false;
        self.gamepad_nav.kbm_picker_list = None;
        self.gamepad_nav.fine_increment = false;
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
        cell: Option<&crate::kbm_picker::PickerCell>,
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
                if let Some(name) = cell.and_then(|c| vocab.insert_for(&c.pin)) {
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
        let Some(cell) = cell.filter(|_| nav.is_rising("btn_south")) else {
            return;
        };
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
        let interaction = self.kbm_picker_window(ctx);
        self.apply_kbm_picker_result(interaction);
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
        let mut result = PickerInteraction::default();
        ctx.show_viewport_immediate(
            crate::config_overlay::picker_viewport_id(),
            builder,
            |vctx, _class| {
                result = self.kbm_picker_window(vctx);
            },
        );
        self.apply_kbm_picker_result(result);
    }

    /// Apply a picker interaction collected by `kbm_picker_window` — split out
    /// so a sub-patch editor viewport (which holds `&self` during its closure)
    /// can render the window inline and apply the result once `&mut` is back.
    pub(crate) fn apply_kbm_picker_result(&mut self, interaction: PickerInteraction) {
        if let Some(stage) = interaction.stage {
            self.picker_switch_stage(stage);
        }
        if let Some(tab) = interaction.tab {
            self.picker_select_tab(tab);
        }
        if let Some(sel) = interaction.select {
            self.gamepad_nav.kbm_picker_list = sel;
        }
        if let Some(edit) = interaction.edit {
            self.picker_edit_chord(edit);
        }
        if let Some(pin) = interaction.pin {
            self.picker_append_pin(&pin);
        }
        if interaction.done {
            self.gamepad_nav.kbm_picker_open = false;
            self.gamepad_nav.kbm_picker_viewport = None;
        }
    }

    /// Render the picker window into `ctx` (read-only on `self`) and report
    /// what the user did.
    pub(crate) fn kbm_picker_window(&self, ctx: &egui::Context) -> PickerInteraction {
        if !self.gamepad_nav.kbm_picker_open { return PickerInteraction::default(); }
        use crate::kbm_picker::{clamp_index, layout_extent, PickStage, PickerTab};
        let macros = self.macro_display_entries();
        let layout = self.picker_layout();
        let tabs = self.picker_tabs();
        let tab = self.picker_tab();
        let cells = self.picker_tab_cells(tab, &macros);
        let sel = clamp_index(&cells, self.gamepad_nav.kbm_picker_idx);
        let accent = ctx.style().visuals.selection.stroke.color;
        let stage = self.gamepad_nav.kbm_picker_stage;
        let both_stages = self.picker_has_both_stages();

        // The chords for the header preview (read from whichever draft params
        // this picker session targets — any sub-patch depth): the input, when
        // the card has one to choose, and the output, when it has that.
        let draft = |key: &str| -> Vec<String> {
            match self.gamepad_nav.kbm_picker_node {
                Some(i) if !key.is_empty() => self.picker_draft_vec(&self.gamepad_nav.kbm_picker_path, i, key),
                _ => Vec::new(),
            }
        };
        let in_chord: Option<Vec<String>> = self.gamepad_nav.kbm_picker_in_key.as_deref().map(draft);
        let has_out = !self.gamepad_nav.kbm_picker_draft_key.is_empty();
        let chord: Vec<String> = draft(&self.gamepad_nav.kbm_picker_draft_key);
        // What this session is for, and — when it is naming a key for a config —
        // the names JSM binds our pins by, so a key it can't name greys out.
        let purpose = self.gamepad_nav.kbm_picker_use;
        let vocab = JsmVocab::of(self.gamepad_nav.kbm_slot);
        let usable = self.picker_usable();

        // The whole board — cells, gaps, captions, labels and the header's
        // text — is drawn at this scale.
        const SCALE: f32 = 1.2;
        const UNIT: f32 = 30.0 * SCALE; // px per grid unit
        const GAP: f32 = 3.0 * SCALE;   // gap between adjacent keys
        // A pick's chip in the chord previews above the board.
        const PREVIEW_CHIP: f32 = 24.0 * SCALE;
        // Sized for the largest tab, so switching tabs doesn't resize the window.
        let (ext_x, ext_y) = tabs.iter()
            .map(|t| layout_extent(&self.picker_tab_cells(*t, &macros)))
            .fold((0.0f32, 0.0f32), |(w, h), (x, y)| (w.max(x), h.max(y)));
        let board_w = ext_x * (UNIT + GAP);
        let board_h = ext_y * (UNIT + GAP);
        let skin = crate::canvas::remapper_icons::Skin::Kbm;

        // Collected from the closure (no &mut self inside the egui window body).
        let mut clicked_pin: Option<String> = None;
        let mut done = false;
        let mut switch_to: Option<PickStage> = None;
        let mut tab_to: Option<PickerTab> = None;
        // A pad driving nav: its LB / RB switch tabs (drawn in its own glyphs),
        // and the focused cell says what it is, as a hover would.
        let nav_skin = self.gamepad_nav.active_dev.as_deref()
            .map(crate::canvas::remapper_icons::skin_from_device_id);
        let mut focus_tip: Option<(egui::Rect, String)> = None;
        let mut focus_rect: Option<egui::Rect> = None;
        // The chord list of the side being chosen, with its timing, and where
        // its focus is (it has the focus instead of the board when set).
        let list_chord = self.picker_chord();
        let list_timed = self.picker_side_timed();
        let list_elems = list_chord.elems(list_timed);
        // Its stops: the elements, and "+ Gap" after them on a timed side.
        let list_focus = self.gamepad_nav.kbm_picker_list
            .filter(|&k| k < list_elems.len() + usize::from(list_timed));
        let mut list_select: Option<Option<usize>> = None;
        let mut list_edit: Option<crate::kbm_picker::ChordEdit> = None;
        let mut list_focus_rect: Option<egui::Rect> = None;

        // Touch Zones variant: a touchpad can't remap to itself, so the touchpad
        // cluster is hidden and analog-output cells replace it (both handled by
        // `picker_cells`, which also keeps them navigable).
        let tz = self.gamepad_nav.kbm_picker_touch_zones;
        // The pad drawn in the dialect of the side being chosen: an input as
        // the pad it comes from, an output as the device it goes to (both as
        // the target's body publishes them), Xbox while there's neither. JSM
        // writes the Xbox names (`X_A`), so its board stays Xbox.
        let side_skins = match (purpose, self.gamepad_nav.kbm_picker_node) {
            (crate::gamepad_nav::PickerUse::Chord, Some(n)) => {
                crate::canvas::viewer::picker_skins(ctx, &self.gamepad_nav.kbm_picker_path, n)
            }
            _ => None,
        }.unwrap_or((crate::canvas::remapper_icons::Skin::Xbox, crate::canvas::remapper_icons::Skin::Xbox));
        let pad_skin = if stage == PickStage::Input { side_skins.0 } else { side_skins.1 };

        let title = match purpose {
            crate::gamepad_nav::PickerUse::Chord if stage == PickStage::Input => "Choose input",
            crate::gamepad_nav::PickerUse::Chord => "Choose output",
            _ if tz => "⌨ KB/M + mouse picker",
            _ => "⌨ KB/M + touchpad picker",
        };
        let picker_id = egui::Id::new("gp_kbm_picker");
        egui::Window::new(title)
            .id(picker_id)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                for font in ui.style_mut().text_styles.values_mut() {
                    font.size *= SCALE;
                }
                ui.horizontal(|ui| {
                    // What the pad's buttons do is the bottom legend bar's to
                    // say (`gp_legend_hints`); the window keeps only what the
                    // board is for, where that isn't plain from it.
                    let note = match (purpose, self.gamepad_nav.kbm_slot) {
                        // What the keys stand for depends on which side of
                        // the line the pick lands on, so say which.
                        (crate::gamepad_nav::PickerUse::JsmName, crate::gamepad_nav::JsmSlot::Name) =>
                            Some("A line starts with a BUTTON: pick the one it binds."),
                        // Everything greys out here, and a board full of grey
                        // has to say why rather than look broken.
                        (crate::gamepad_nav::PickerUse::JsmName, crate::gamepad_nav::JsmSlot::Neither) =>
                            Some("A NUMBER goes here, not a key."),
                        _ => None,
                    };
                    if let Some(note) = note {
                        ui.label(egui::RichText::new(note)
                            .small().color(egui::Color32::from_gray(150)));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(egui::RichText::new("Done").size(13.0 * SCALE)).clicked() { done = true; }
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
                // Chord previews: the input (when this card's can be chosen)
                // and the output, the side being chosen marked.
                // With both sides to fill, a caption is also the mouse's way
                // to switch to its side (West on the pad).
                let mut preview = |ui: &mut egui::Ui, caption: &str, pins: &[String], side: Option<PickStage>| {
                    let active = side == Some(stage);
                    ui.horizontal(|ui| {
                        let cap = egui::RichText::new(caption).small();
                        let cap = if active { cap.strong().color(accent) } else { cap.weak() };
                        match side {
                            Some(s) if both_stages && !active => {
                                if ui.add(egui::Button::new(cap).small())
                                    .on_hover_text("Choose this side instead (West)").clicked()
                                {
                                    switch_to = Some(s);
                                }
                            }
                            _ => { ui.label(cap); }
                        }
                        let label_of = |pin: &str| macros.iter().find(|e| e.pin == pin)
                            .map(|e| e.name.clone())
                            .unwrap_or_else(|| kbm_pin_label(pin));
                        // Picks as the cards draw them: the key's or button's
                        // glyph, the pad's in this side's dialect.
                        let side_pad = if side == Some(PickStage::Input) { side_skins.0 } else { side_skins.1 };
                        let pin_skin = |pin: &str| if crate::kbm_picker::is_pad_cell(pin) {
                            side_pad
                        } else {
                            crate::canvas::remapper_icons::Skin::Kbm
                        };
                        let chip = |ui: &mut egui::Ui, pin: &str, dur: Option<f32>, sense: egui::Sense| {
                            let ctx = ui.ctx().clone();
                            // Measured by painting it nowhere: a pick with no
                            // glyph is a text pill as wide as its name.
                            let w = crate::canvas::viewer::paint_chord_chip_to_rect(
                                &ui.painter().with_clip_rect(egui::Rect::NOTHING),
                                &ctx, egui::Pos2::ZERO, PREVIEW_CHIP, pin, pin_skin(pin));
                            let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, PREVIEW_CHIP), sense);
                            if resp.hovered() && sense.senses_click() {
                                ui.painter().rect_filled(rect.expand(2.0), 4.0, egui::Color32::from_white_alpha(18));
                            }
                            crate::canvas::viewer::paint_chord_chip_to_rect(
                                ui.painter(), &ctx, rect.min, PREVIEW_CHIP, pin, pin_skin(pin));
                            if let Some(ms) = dur {
                                crate::canvas::viewer::paint_dur_badge(ui.painter(), rect, ms);
                            }
                            let tip = match dur {
                                Some(ms) => format!("{} · on for {ms:.0} ms", label_of(pin)),
                                None => label_of(pin),
                            };
                            resp.on_hover_text(tip)
                        };
                        let plus = |ui: &mut egui::Ui| {
                            ui.label(egui::RichText::new("+").strong());
                        };
                        if purpose != crate::gamepad_nav::PickerUse::Chord {
                            ui.label(egui::RichText::new("—").small().weak());
                        } else if active {
                            // The list being built: every pick and gap can be
                            // focused (a click, or Up from the board) and edited.
                            if list_elems.is_empty() {
                                ui.label(egui::RichText::new("(none)").small().italics().weak());
                            }
                            for (k, e) in list_elems.iter().enumerate() {
                                use crate::kbm_picker::ChordElem;
                                let together = k > 0 && matches!((list_elems[k - 1], e),
                                    (ChordElem::Item(_), ChordElem::Item(_)));
                                if together { plus(ui); }
                                let r = match *e {
                                    ChordElem::Item(i) => chip(ui, &list_chord.pins[i],
                                        if list_timed { list_chord.durs.get(i).copied().flatten() } else { None },
                                        egui::Sense::click()),
                                    ChordElem::Gap(i) => {
                                        let ms = list_chord.gaps.get(i).copied().unwrap_or(0.0);
                                        let w = crate::canvas::viewer::gap_marker_width(ctx, PREVIEW_CHIP, ms);
                                        let (rect, r) = ui.allocate_exact_size(
                                            egui::vec2(w, PREVIEW_CHIP), egui::Sense::click());
                                        if r.hovered() {
                                            ui.painter().rect_filled(rect.expand(2.0), 4.0,
                                                egui::Color32::from_white_alpha(18));
                                        }
                                        crate::canvas::viewer::paint_gap_marker(
                                            ui.painter(), ctx, rect.min, PREVIEW_CHIP, ms);
                                        r.on_hover_text(format!("Waits {ms:.0} ms after the pick before it"))
                                    }
                                };
                                let focused = list_focus == Some(k);
                                if r.clicked() {
                                    list_select = Some(if focused { None } else { Some(k) });
                                }
                                if focused { list_focus_rect = Some(r.rect); }
                            }
                            if list_timed {
                                let after = list_focus.and_then(|k| list_elems.get(k).copied());
                                // The list's last stop for the pad, past its
                                // elements: South on it adds the gap.
                                let on_it = list_focus == Some(list_elems.len());
                                let r = ui.add(egui::Button::new(egui::RichText::new("+ Gap").small())
                                    .selected(on_it))
                                    .on_hover_text("A wait before the next pick (RT on the pad). \
                                                    After the focused element, or at the end.");
                                if on_it { list_focus_rect = Some(r.rect); }
                                if r.clicked() {
                                    list_edit = Some(crate::kbm_picker::ChordEdit::InsertGap(after));
                                }
                            }
                        } else if pins.is_empty() {
                            ui.label(egui::RichText::new("(none)").small().italics().weak());
                        } else {
                            for (i, pin) in pins.iter().enumerate() {
                                if i > 0 { plus(ui); }
                                chip(ui, pin, None, egui::Sense::hover());
                            }
                        }
                    });
                };
                let chord_purpose = purpose == crate::gamepad_nav::PickerUse::Chord;
                if let (Some(ins), true) = (in_chord.as_ref(), chord_purpose) {
                    preview(ui, "Input:", ins, Some(PickStage::Input));
                }
                if has_out || !chord_purpose {
                    preview(ui, "Output:", &chord, chord_purpose.then_some(PickStage::Output));
                }
                // The focused element of the list: its time, and Delete.
                if let (Some(e), true) = (list_focus.and_then(|k| list_elems.get(k).copied()), chord_purpose) {
                    use crate::kbm_picker::{ChordEdit, ChordElem, DEFAULT_DUR_MS};
                    ui.horizontal(|ui| {
                        match e {
                            ChordElem::Gap(i) => {
                                let mut ms = list_chord.gaps.get(i).copied().unwrap_or(0.0);
                                ui.label(egui::RichText::new("Gap").small());
                                if ui.add(egui::DragValue::new(&mut ms)
                                    .range(crate::kbm_picker::MIN_GAP_MS..=5000.0).speed(1.0).suffix(" ms"))
                                    .on_hover_text("How long after the pick before it this one starts.")
                                    .changed()
                                {
                                    list_edit = Some(ChordEdit::SetGap(i, ms));
                                }
                            }
                            ChordElem::Item(i) if list_timed => {
                                let dur = list_chord.durs.get(i).copied().flatten();
                                let mut own = dur.is_some();
                                if ui.checkbox(&mut own, egui::RichText::new("Own duration").small())
                                    .on_hover_text("Off: the pick stays on as long as the card's input does \
                                                    (the time gap's pulse, or the press replayed, by mode).")
                                    .changed()
                                {
                                    list_edit = Some(ChordEdit::SetDur(i, own.then_some(DEFAULT_DUR_MS)));
                                }
                                if let Some(mut ms) = dur {
                                    if ui.add(egui::DragValue::new(&mut ms).range(1.0..=10000.0).speed(1.0).suffix(" ms"))
                                        .changed()
                                    {
                                        list_edit = Some(ChordEdit::SetDur(i, Some(ms)));
                                    }
                                } else {
                                    ui.label(egui::RichText::new("as long as the input").small().weak());
                                }
                            }
                            ChordElem::Item(_) => {}
                        }
                        if ui.small_button("Delete").on_hover_text("Take it out of the list (North).").clicked() {
                            list_edit = Some(ChordEdit::Delete(e));
                        }
                    });
                }
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(4.0);
                // The tabs (LB / RB, or a click).
                if tabs.len() > 1 {
                    // The pad's own bumper glyph and an arrow, either side of
                    // the tabs — only while a pad is driving.
                    let bumper = |ui: &mut egui::Ui, pin: &str, arrow: &str, before: bool| {
                        let Some(skin) = nav_skin else { return };
                        let arrow = egui::RichText::new(arrow).color(egui::Color32::from_gray(150));
                        if !before { ui.label(arrow.clone()); }
                        if let Some(tex) = kbm_cell_texture(ctx, skin, pin) {
                            ui.add(egui::Image::new(&tex).fit_to_exact_size(egui::vec2(22.0, 22.0) * SCALE));
                        }
                        if before { ui.label(arrow); }
                    };
                    ui.horizontal(|ui| {
                        bumper(ui, "btn_lb", "◀", true);
                        for t in &tabs {
                            if ui.selectable_label(*t == tab, t.label()).clicked() && *t != tab {
                                tab_to = Some(*t);
                            }
                        }
                        bumper(ui, "btn_rb", "▶", false);
                    });
                    ui.add_space(6.0);
                }

                // Absolute-positioned board: allocate one area sized to the layout
                // extent, then place each cell at its (x,y)*unit origin. Cells are
                // individually clickable (mouse) AND highlight the gamepad focus.
                let (board, _) = ui.allocate_exact_size(
                    egui::vec2(board_w, board_h), egui::Sense::hover());
                // Cluster captions (GAMEPAD, STICKS, GYRO, …), for those shown.
                for (caption, px, py) in crate::kbm_picker::tab_captions(layout, tab) {
                    ui.painter_at(board).text(
                        board.min + egui::vec2(px * (UNIT + GAP), py * (UNIT + GAP)),
                        egui::Align2::LEFT_TOP, caption,
                        egui::FontId::proportional(9.0 * SCALE), egui::Color32::from_gray(130));
                }
                // An empty Macros tab stays, and says where macros come from.
                if tab == PickerTab::Macros && cells.is_empty() {
                    ui.painter_at(board).text(
                        board.left_top() + egui::vec2(4.0, 4.0),
                        egui::Align2::LEFT_TOP,
                        "No macros yet — create macros via the Macro Output module or a JSM @ binding.",
                        egui::FontId::proportional(11.0 * SCALE), egui::Color32::from_gray(140));
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
                    let wrong_side = purpose == crate::gamepad_nav::PickerUse::Chord
                        && !crate::kbm_picker::stage_allows(stage, &cell.pin);
                    let disabled = !usable(cell);
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
                    } else if hovered {
                        egui::Color32::from_gray(60)
                    } else {
                        egui::Color32::from_gray(40)
                    };
                    painter.rect_filled(rect, 4.0, bg);
                    // The focus is the gamepad-nav glow round the cell, not a
                    // fill: a tinted tile showed its square corners round every
                    // rounded glyph.
                    let focused = focused && list_focus.is_none();
                    if focused {
                        focus_rect = Some(rect);
                    }
                    let tint = if disabled { egui::Color32::from_gray(110) } else { egui::Color32::WHITE };
                    // Macro cells: the port's icon (custom patch-embedded SVG
                    // or embedded set) or the port name + full-name tooltip.
                    // KBM cells: skin icon or text fallback.
                    if let Some(entry) = cell.macro_meta.as_ref() {
                        // A menu's four targets share its icon (or its name's
                        // first letters): the corner badge tells them apart.
                        let badge_on = if let Some(tex) = crate::macro_icons::macro_port_icon_texture(
                            ctx, &entry.icon, &entry.icon_svg, UNIT - 6.0)
                        {
                            let s = (UNIT - 6.0).min(size.x - 6.0).max(8.0);
                            let img_rect = egui::Rect::from_center_size(
                                rect.center(), egui::vec2(s, s));
                            painter.image(tex.id(), img_rect,
                                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                tint);
                            img_rect
                        } else {
                            let short: String = entry.name.chars().take(4).collect();
                            painter.text(rect.center(), egui::Align2::CENTER_CENTER,
                                short, egui::FontId::proportional(10.0 * SCALE), ui.visuals().text_color());
                            rect.shrink(2.0)
                        };
                        crate::macro_icons::paint_menu_target_badge(&painter, badge_on, &cell.pin, tint);
                        let tip = if self_target {
                            format!("{} — a menu can't target itself", entry.name)
                        } else {
                            entry.name.clone()
                        };
                        if focused && nav_skin.is_some() {
                            focus_tip = Some((rect, tip.clone()));
                        }
                        resp.on_hover_text(tip);
                        continue;
                    }
                    // The gamepad cells in the side's dialect (`pad_skin`).
                    let skin = if crate::kbm_picker::is_pad_cell(&cell.pin) {
                        pad_skin
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
                        // A glyph shared with another cell (the touchpad's
                        // touch and click zones) gets its word under it.
                        if let Some(word) = crate::kbm_picker::cell_caption(&cell.pin) {
                            let at = rect.center_bottom() - egui::vec2(0.0, 1.0);
                            let font = egui::FontId::proportional(UNIT * 0.26);
                            for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
                                painter.text(at + egui::vec2(dx, dy), egui::Align2::CENTER_BOTTOM,
                                    word, font.clone(), egui::Color32::from_black_alpha(220));
                            }
                            painter.text(at, egui::Align2::CENTER_BOTTOM, word, font, tint);
                        }
                    } else {
                        // Compact labels — "L-Stick" won't fit a 30px cell.
                        let lbl = match cell.pin.as_str() {
                            "left_stick" => "LS".to_string(),
                            "right_stick" => "RS".to_string(),
                            p => match flexinput_core::imu::imu_pin(p) {
                                Some(imu) => imu.short.to_string(),
                                None => kbm_pin_label(p),
                            },
                        };
                        painter.text(rect.center(), egui::Align2::CENTER_CENTER,
                            lbl,
                            egui::FontId::proportional(11.0 * SCALE),
                            if disabled { egui::Color32::from_gray(110) } else { ui.visuals().text_color() });
                    }
                    // What a cell is when its face doesn't say, and why one is
                    // grey for this side. Hovered by the mouse; shown beside the
                    // focus while a pad drives.
                    if let Some(tip) = picker_cell_tip(&cell.pin, wrong_side, stage) {
                        if focused && nav_skin.is_some() {
                            focus_tip = Some((rect, tip.clone()));
                        }
                        resp.on_hover_text(tip);
                    }
                }
            });

        // The focused cell's glow — or the focused list element's — above the
        // board so neighbours can't cover it, but under a window over the picker.
        let host = egui::LayerId::new(egui::Order::Middle, picker_id);
        if let Some(rect) = list_focus_rect.or(focus_rect) {
            let painter = ctx.layer_painter(crate::widgets::nav_glow_layer(ctx,
                egui::Order::Foreground, egui::Id::new("gp_kbm_picker_focus_glow"), host));
            crate::widgets::paint_nav_bloom(&painter, rect, accent, 5.0, 150.0, 7.0, 6, 2.0, 1.5, 2.0);
        }
        // The focused cell's name, under it, above everything — the pad's
        // hover.
        if let Some((rect, tip)) = focus_tip {
            let painter = ctx.layer_painter(crate::widgets::nav_glow_layer(ctx,
                egui::Order::Tooltip, egui::Id::new("gp_kbm_picker_focus_tip"), host));
            let galley = painter.layout_no_wrap(
                tip, egui::FontId::proportional(11.0 * SCALE), egui::Color32::WHITE);
            let pos = rect.center_bottom() + egui::vec2(-galley.size().x * 0.5, 6.0);
            let bg = egui::Rect::from_min_size(pos, galley.size()).expand(4.0);
            painter.rect_filled(bg, 3.0, egui::Color32::from_black_alpha(230));
            painter.rect_stroke(bg, 3.0, egui::Stroke::new(1.0_f32, accent), egui::StrokeKind::Outside);
            painter.galley(pos, galley, egui::Color32::WHITE);
        }

        ctx.request_repaint();
        PickerInteraction {
            pin: clicked_pin,
            done,
            stage: switch_to,
            tab: tab_to,
            select: list_select,
            edit: list_edit,
        }
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
                            egui::Stroke::new(1.5_f32, accent), egui::StrokeKind::Inside);
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

/// What a picker cell is called where its face doesn't say it — and, when it
/// can't sit on the side being chosen, why it's grey. Keys say what they are.
fn picker_cell_tip(pin: &str, wrong_side: bool, stage: crate::kbm_picker::PickStage) -> Option<String> {
    let name = match flexinput_core::imu::imu_pin(pin) {
        Some(p) => p.name.to_string(),
        None if pin.starts_with("key_") => kbm_pin_label(pin),
        None => crate::canvas::viewer::remapper_pin_display(pin),
    };
    if wrong_side {
        let why = match stage {
            crate::kbm_picker::PickStage::Input => "an output only",
            crate::kbm_picker::PickStage::Output => "an input only",
        };
        return Some(format!("{name} — {why}"));
    }
    (!pin.starts_with("key_")).then_some(name)
}

/// What the user did in the picker window this frame, applied once `&mut` is
/// back ([`FlexInputApp::apply_kbm_picker_result`]).
#[derive(Default)]
pub(crate) struct PickerInteraction {
    /// A cell clicked.
    pub(crate) pin: Option<String>,
    /// Done pressed.
    pub(crate) done: bool,
    /// A side's caption clicked, to choose for it.
    pub(crate) stage: Option<crate::kbm_picker::PickStage>,
    /// A tab clicked.
    pub(crate) tab: Option<crate::kbm_picker::PickerTab>,
    /// An element of the chord list clicked (focus it), or `Some(None)` to
    /// give the focus back to the board.
    pub(crate) select: Option<Option<usize>>,
    /// An edit to the chord list.
    pub(crate) edit: Option<crate::kbm_picker::ChordEdit>,
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
        // lookup above finds nothing and `fi_insert` refuses it.
        let entry = crate::macro_icons::registry_entry(pin)?;
        let is_port = flexinput_core::macros::parse_macro_pin(&entry.pin).is_some();
        fi_insert(self.slot, &entry.name, is_port)
    }
}

/// How one of OUR targets goes into a config, on this side of a line.
///
/// Right of the `=`, a Macro Output port or a Virtual Menu entry is something
/// the binding drives. Left of it, a Macro Output port is also a button to read
/// (`@Jump,E = SPACE`) — but a menu entry can only be pressed, never read, so
/// it has no place there. Split out from the registry lookup so the rule can be
/// tested without a patch.
fn fi_insert(slot: crate::gamepad_nav::JsmSlot, name: &str, is_port: bool) -> Option<String> {
    use crate::gamepad_nav::JsmSlot;
    match slot {
        JsmSlot::Value => Some(flexinput_engine::eval::jsm_fi_tag(name)),
        JsmSlot::Name if is_port => Some(flexinput_engine::eval::jsm_fi_tag(name)),
        _ => None,
    }
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

    /// Right of the `=`, any of our targets is something the binding drives.
    /// Left of it, a Macro Output port is a button to read — but a Virtual Menu
    /// entry can only be pressed, so it isn't offered there.
    #[test]
    fn a_port_is_a_value_or_a_name_and_a_menu_entry_only_a_value() {
        assert_eq!(fi_insert(JsmSlot::Value, "Reload", true).as_deref(), Some("@Reload"));
        assert_eq!(
            fi_insert(JsmSlot::Value, "Reload sequence", true).as_deref(),
            Some("@\"Reload sequence\""),
            "and it is written the way the parser reads it back"
        );
        assert_eq!(fi_insert(JsmSlot::Value, "Menu — Show", false).as_deref(), Some("@\"Menu — Show\""));
        assert_eq!(fi_insert(JsmSlot::Name, "Reload", true).as_deref(), Some("@Reload"));
        assert_eq!(fi_insert(JsmSlot::Name, "Menu — Show", false), None);
        assert_eq!(fi_insert(JsmSlot::Neither, "Reload", true), None);
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

        // A media key goes in under JSM's own name for it, as a value only.
        assert_eq!(value.insert_for("media_volume_up").as_deref(), Some("VOLUME_UP"));
        assert_eq!(value.insert_for("media_play_pause").as_deref(), Some("PLAY_PAUSE"));
        assert_eq!(name.insert_for("media_mute"), None);

        // A setting's value takes a number, so the board has nothing for it.
        let neither = JsmVocab::of(JsmSlot::Neither);
        for pin in ["btn_south", "key_space", "mouse_left", "left_trigger"] {
            assert_eq!(neither.insert_for(pin), None, "{pin}");
        }
    }
}
