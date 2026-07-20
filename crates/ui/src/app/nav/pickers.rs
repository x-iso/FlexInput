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
}
