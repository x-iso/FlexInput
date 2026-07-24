//! Workspace persistence: snapshotting tab state, the opt-in workspace
//! save, and the always-on crash-recovery autosave. Also the overlay
//! accessors the viewport host reads each frame.

use super::*;

impl FlexInputApp {

    /// Snapshot the full tab/canvas state into a `PersistedWorkspace`. Shared
    /// by the opt-in workspace save and the always-on crash-recovery save so
    /// both serialize identical state.
    pub(crate) fn build_persisted_workspace(&self) -> PersistedWorkspace {
        let tabs: Vec<PersistedTab> = self.tabs.iter().map(|t| PersistedTab {
            title: t.title.clone(),
            file_path: t.file_path.clone(),
            bound_exes: t.bound_exes.clone(),
            auto_bypass: t.auto_bypass,
            snarl: crate::canvas::sanitize_snarl_for_save(&t.canvas.snarl),
            easy_preset_path: t.easy_state.loaded_preset.as_ref().map(|(p, _)| p.clone()),
            view_salt: t.view_salt,
            overlay: t.overlay.clone(),
            config: t.config.clone(),
        }).collect();
        PersistedWorkspace {
            version: 1,
            active_tab: self.active_tab,
            tabs,
        }
    }

    /// Field-split accessor for the overlay viewport (`crate::overlay`):
    /// the active tab (mutably — its snarl renders the pinned elements and
    /// its `overlay` layout gets edited), the live signal map, and a clone
    /// of the panic shortcut. One method so the borrows stay disjoint.
    pub(crate) fn overlay_parts(&mut self) -> (
        &mut PatchTab,
        &HashMap<(String, String), Signal>,
        PanicShortcut,
    ) {
        let shortcut = self.panic_shortcut.clone();
        let idx = self.active_tab.min(self.tabs.len().saturating_sub(1));
        (&mut self.tabs[idx], &self.last_signals, shortcut)
    }

    /// Mirror a menu's `menu_rect` into any OPEN sub-patch editor that owns the
    /// menu's node, so the editor's full-snarl write-back (`show_subpatch_editors`)
    /// doesn't clobber a placement the menu overlay wrote to the embedded copy.
    /// `outer` is the first-level sub-patch node the menu lives in (`None` = the
    /// menu sits on the tab canvas, where no editor clone exists to reconcile);
    /// `inner` is the menu's NodeId, identical in the editor's clone (its slots
    /// were seeded aligned).
    pub(crate) fn write_menu_rect_to_editors(
        &mut self, outer: Option<NodeId>, inner: NodeId, r: [f32; 4],
    ) {
        let Some(outer) = outer else { return; };
        let val = serde_json::json!([r[0], r[1], r[2], r[3]]);
        let active = self.active_tab;
        for ed in self.sub_patch_editors.iter_mut() {
            if ed.tab_idx == active
                && ed.parent_editor_idx.is_none()
                && ed.node_id == outer
            {
                if let Some(node) = ed.canvas.snarl.get_node_mut(inner) {
                    node.params.insert("menu_rect".to_string(), val.clone());
                }
            }
        }
    }

    /// The gamepad-focused config tweak-pin index (M3.5), for the config
    /// overlay to light up + pass through. `None` = no gamepad focus.
    pub(crate) fn config_nav_focus(&self) -> Option<usize> {
        self.gamepad_nav.config_index
    }

    /// Whether the gamepad is value-editing the focused config pin (M3.6) and
    /// whether a nav-enabled gamepad is currently driving — for the overlay's
    /// legend to show the right hints (and only while a pad is active).
    pub(crate) fn config_nav_state(&self) -> (bool, bool) {
        (self.gamepad_nav.config_editing, self.gamepad_nav.active_dev.is_some())
    }

    /// The right-stick virtual cursor position + visibility (M3.6), for the
    /// config overlay to draw it in its own viewport.
    pub(crate) fn config_cursor(&self) -> (egui::Pos2, bool) {
        (self.gamepad_nav.cursor_pos, self.gamepad_nav.cursor_visible)
    }

    /// The overlay's live repaint rate (clamped to the settings bounds).
    pub(crate) fn overlay_fps(&self) -> u32 {
        self.settings.overlay_fps
            .clamp(settings::OVERLAY_FPS_MIN, settings::OVERLAY_FPS_MAX)
    }

    /// Serialize the current tab set to workspace.json. No-op if the user
    /// has not opted in to workspace persistence.
    pub(crate) fn save_workspace_now(&self) {
        if !self.settings.keep_workspace { return; }
        settings::save_workspace(&self.build_persisted_workspace());
    }

    /// Sum `mutation_gen` over all tabs. Used as the cheap dirty signal for the
    /// crash-recovery autosave — it advances on every snarl mutation (any
    /// push_undo / push_snapshot / undo / redo), so a change between frames
    /// means persistent state changed.
    pub(crate) fn total_mutation_gen(&self) -> u64 {
        self.tabs.iter().map(|t| t.canvas.mutation_gen).fold(0u64, u64::wrapping_add)
    }

    /// Write the crash-recovery snapshot if (and only if) a settled edit
    /// happened since the last write. Called once per frame from `update`.
    /// Independent of `keep_workspace`: even a user who never opted into tab
    /// persistence must not lose work to a GPU-loss relaunch. The write is
    /// atomic (temp + rename) so the panic-hook / relaunch path can never read
    /// a half-written file.
    pub(crate) fn maybe_write_recovery_snapshot(&mut self) {
        let gen = self.total_mutation_gen();
        if gen == self.last_recovery_mutation_gen {
            return;
        }
        self.last_recovery_mutation_gen = gen;
        settings::save_recovery(&self.build_persisted_workspace());
    }
}
