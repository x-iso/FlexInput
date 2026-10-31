//! The main window's drawing: title bar, tabs, panels, the canvas and the
//! windows floating in it. Everything in here exists for someone looking at
//! the window, so `update()` can skip it while nobody is. Whatever has to keep
//! running with no window on screen stays in `update()`.

use std::ops::ControlFlow;

use super::*;

/// A pointer that moved over a frozen sub-patch editor this recently keeps it
/// live, so the click it's on its way to make lands: egui hits widgets from the
/// previous pass's layout, and a pass that drew nothing leaves none to hit. The
/// main window needn't: it holds the click back for a frame instead (see
/// [`FlexInputApp::hold_input_for_frozen_window`]), which immediate viewports like
/// the editors have no hook for.
const POINTER_LIVE_SECS: f32 = 5.0;

/// Passes a woken main window draws, for a click to land and be let go of.
pub(super) const WAKE_PASSES: u8 = 3;

/// How dark an unfocused window goes.
const VEIL_ALPHA: u8 = 110;

/// What a window does this pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WindowStep {
    Draw,
    /// Draw, with the veil on top.
    DrawDimmed,
    /// Draw nothing and leave the last frame on screen.
    KeepFrame,
}

/// The facts about a window that freezing it depends on.
pub(super) struct WindowFacts {
    minimized: bool,
    focused: bool,
    pointer_live: bool,
    size: egui::Vec2,
    pixels_per_point: f32,
}

pub(super) fn window_facts(ctx: &egui::Context, id: egui::ViewportId) -> WindowFacts {
    let (minimized, focused, size, pixels_per_point) = ctx.input(|i| {
        let info = i.raw.viewports.get(&id);
        (
            info.and_then(|v| v.minimized).unwrap_or(false),
            info.and_then(|v| v.focused).unwrap_or(true),
            info.and_then(|v| v.inner_rect).map_or(egui::Vec2::ZERO, |r| r.size()),
            info.and_then(|v| v.native_pixels_per_point).unwrap_or(1.0),
        )
    });
    let pointer_live = ctx.input_for(id, |i| {
        i.pointer.any_down()
            || (i.pointer.has_pointer() && i.pointer.time_since_last_movement() < POINTER_LIVE_SECS)
    });
    WindowFacts { minimized, focused, pointer_live, size, pixels_per_point }
}

impl WindowFacts {
    /// For a window that doesn't need waking by a hovering pointer.
    pub(super) fn without_hover(mut self, ctx: &egui::Context, id: egui::ViewportId) -> Self {
        self.pointer_live = ctx.input_for(id, |i| i.pointer.any_down());
        self
    }
}

/// One window's freeze state. Dimming takes one frame with the veil on it, which
/// is then the frame left on screen; it's drawn again if the window changes size,
/// since a resized surface has lost it.
#[derive(Default)]
pub(super) struct WindowFreeze {
    veiled_at: Option<(egui::Vec2, f32)>,
}

impl WindowFreeze {
    /// Whether [`step`](Self::step) would say [`WindowStep::KeepFrame`].
    fn would_keep(&self, window: &WindowFacts, may_freeze: bool, freeze_unfocused: bool) -> bool {
        may_freeze && (window.minimized || (
            freeze_unfocused && !window.focused && !window.pointer_live
                && self.veiled_at == Some((window.size, window.pixels_per_point))
        ))
    }

    /// `may_freeze` false overrides everything; `freeze_unfocused` is the setting.
    /// A minimized window freezes with or without it, since nothing can see it.
    pub(super) fn step(
        &mut self,
        window: &WindowFacts,
        may_freeze: bool,
        freeze_unfocused: bool,
    ) -> WindowStep {
        if !may_freeze {
            self.veiled_at = None;
            return WindowStep::Draw;
        }
        if window.minimized {
            self.veiled_at = None;
            return WindowStep::KeepFrame;
        }
        if !freeze_unfocused || window.focused {
            self.veiled_at = None;
            return WindowStep::Draw;
        }
        let here = Some((window.size, window.pixels_per_point));
        if !window.pointer_live && self.veiled_at == here {
            return WindowStep::KeepFrame;
        }
        self.veiled_at = here;
        WindowStep::DrawDimmed
    }

    pub(super) fn forget_veil(&mut self) {
        self.veiled_at = None;
    }
}

/// Ask for the next pass after `wait`, exactly. egui takes a predicted frame time
/// off any delay so that a frame lands by then, and a window that presents nothing
/// isn't held back by vsync: left as it is, a wait shorter than a frame becomes
/// "now" over and over, spinning through passes for that last frame of the wait.
fn request_pass_in(ctx: &egui::Context, wait: std::time::Duration) {
    let frame = std::time::Duration::from_secs_f32(ctx.input(|i| i.predicted_dt).max(0.0));
    ctx.request_repaint_after(wait + frame);
}

pub(super) fn paint_window_veil(ctx: &egui::Context) {
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("fxi_window_veil"),
    ));
    painter.rect_filled(ctx.content_rect(), 0.0, egui::Color32::from_black_alpha(VEIL_ALPHA));
}

/// One second of the main window's passes, for Settings -> Performance.
#[derive(Clone, Copy, Default)]
pub(super) struct UiPassRow {
    pub passes: u32,
    pub drawn: u32,
    /// Passes a frozen window let go by: pointer events under the background rate.
    pub waited: u32,
    pub avg_ms: f32,
}

/// The latest second, and the latest one with no focus: a frozen window can't
/// show its own figures, so they wait here for when it's looked at again.
pub(super) static UI_PASSES: std::sync::Mutex<(UiPassRow, UiPassRow)> =
    std::sync::Mutex::new((UiPassRow { passes: 0, drawn: 0, waited: 0, avg_ms: 0.0 },
                           UiPassRow { passes: 0, drawn: 0, waited: 0, avg_ms: 0.0 }));

#[derive(Default)]
pub(super) struct UiPassAcc {
    since: Option<std::time::Instant>,
    row: UiPassRow,
    busy: std::time::Duration,
    had_focus: bool,
}

impl UiPassAcc {
    pub(super) fn waited(&mut self) {
        self.row.waited += 1;
    }

    pub(super) fn end_pass(&mut self, start: std::time::Instant, drawn: bool, focused: bool) {
        let now = std::time::Instant::now();
        self.row.passes += 1;
        self.row.drawn += drawn as u32;
        self.busy += now - start;
        self.had_focus |= focused;
        let since = *self.since.get_or_insert(now);
        if now - since < std::time::Duration::from_secs(1) {
            return;
        }
        let secs = (now - since).as_secs_f32();
        let per_sec = |n: u32| (n as f32 / secs).round() as u32;
        let row = UiPassRow {
            passes: per_sec(self.row.passes),
            drawn: per_sec(self.row.drawn),
            waited: per_sec(self.row.waited),
            avg_ms: self.busy.as_secs_f32() * 1e3 / self.row.passes.max(1) as f32,
        };
        if let Ok(mut t) = UI_PASSES.lock() {
            t.0 = row;
            if !self.had_focus {
                t.1 = row;
            }
        }
        *self = Self { since: Some(now), ..Self::default() };
    }
}

impl FlexInputApp {
    /// While any of these is up a window has to keep drawing: pickers and the
    /// MIDI editor live in OS windows egui destroys the moment a pass doesn't
    /// show them, and the rest is the user working through a window that may not
    /// have focus.
    pub(super) fn windows_must_draw(&self, ctx: &egui::Context) -> bool {
        let nav = &self.gamepad_nav;
        nav.kbm_picker_open
            || nav.press_mode_open
            || nav.midi_modal.is_some()
            || nav.cursor_visible
            || crate::config_overlay::config_overlay_visible(ctx)
            || crate::overlay::overlay_edit(ctx)
            || crate::canvas::viewer::overlay_pick_active(ctx)
            || crate::menu_overlay::menu_edit_target(ctx).is_some()
            || ctx.input(|i| !i.raw.hovered_files.is_empty() || !i.raw.dropped_files.is_empty())
    }

    /// Called before the pass sees its input. A click or scroll on a frozen main
    /// window would hit nothing, the last pass having laid out no widgets. So it
    /// waits a frame: this pass draws, and the next one gets the input and finds
    /// the widgets there. Without this the pointer would have to wake the window
    /// just by passing over it.
    pub(super) fn hold_input_for_frozen_window(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        if !self.held_input.is_empty() {
            let mut held = std::mem::take(&mut self.held_input);
            held.append(&mut raw.events);
            raw.events = held;
            return;
        }
        let wakes = |e: &egui::Event| matches!(e,
            egui::Event::PointerButton { pressed: true, .. }
            | egui::Event::MouseWheel { .. }
            | egui::Event::Zoom(_));
        if self.root_kept_last_pass && raw.events.iter().any(wakes) {
            self.held_input = std::mem::take(&mut raw.events);
            self.wake_passes = WAKE_PASSES;
            ctx.request_repaint();
        }
    }

    /// Whether this pass of a frozen main window can be skipped altogether. Each
    /// pointer event over the window brings a pass, far more often than the
    /// background rate its logic runs at; those needn't do a thing.
    pub(super) fn frozen_pass_can_wait(&mut self, ctx: &egui::Context) -> bool {
        if !self.root_kept_last_pass || !self.held_input.is_empty() || self.wake_passes > 0 {
            return false;
        }
        let Some(last) = self.last_full_pass else { return false };
        let hz = self.settings.bg_repaint_hz.clamp(settings::BG_REPAINT_HZ_MIN, settings::BG_REPAINT_HZ_MAX);
        let interval = std::time::Duration::from_millis(1000 / hz as u64);
        let elapsed = last.elapsed();
        if elapsed >= interval {
            return false;
        }
        // Other OS windows vanish unless every pass shows them; and an event for
        // the window itself (a close request) is only there for this pass.
        let quiet = ctx.input(|i| i.raw.viewports.len() == 1 && i.viewport().events.is_empty());
        if !quiet {
            return false;
        }
        let root = egui::ViewportId::ROOT;
        let facts = window_facts(ctx, root).without_hover(ctx, root);
        let may_freeze = !self.windows_must_draw(ctx);
        if !self.main_window_freeze.would_keep(&facts, may_freeze, self.freeze_unfocused_windows()) {
            return false;
        }
        eframe::egui_wgpu::keep_frame_on_screen(root);
        request_pass_in(ctx, interval - elapsed);
        true
    }

    /// The Settings option, unless the window is being kept in view on purpose.
    pub(super) fn freeze_unfocused_windows(&self) -> bool {
        self.settings.freeze_background
            && !self.settings.pin_active
            && !self.settings.see_through_active
    }

    /// `Break` means the window is closing and the rest of the pass is moot.
    pub(super) fn draw_main_window(
        &mut self,
        ctx: &egui::Context,
        effective_bypass: &[bool],
        live_device_ids: &std::collections::HashSet<String>,
    ) -> ControlFlow<()> {
        // ── Custom title bar ──────────────────────────────────────────────────────
        let mut do_save = false;
        let mut do_load = false;
        let mut do_save_workspace = false;
        let mut do_load_workspace = false;
        let mut do_new  = false;
        let mut do_close = false;
        let mut do_bind  = false;
        let mut do_hidhide = false;
        let mut do_undo = false;
        let mut do_redo = false;
        let mut toggle_settings = false;
        let mut toggle_bluetooth = false;
        let mut do_pin_toggle = false;
        let mut do_set_mode: Option<settings::UiMode> = None;
        let pin_active_now = self.settings.pin_active;
        let ui_mode_now = self.settings.ui_mode;
        let can_undo = self.tabs[self.active_tab].canvas.can_undo();
        let can_redo = self.tabs[self.active_tab].canvas.can_redo();
        let title_frame = egui::Frame::NONE.fill(ctx.style().visuals.panel_fill);
        egui::TopBottomPanel::top("title_bar")
            .exact_height(32.0)
            .frame(title_frame)
            .show_separator_line(false)
            .show(ctx, |ui| {
                show_title_bar(
                    ui, ctx,
                    &mut do_save, &mut do_load,
                    &mut do_save_workspace, &mut do_load_workspace,
                    &mut do_new, &mut do_close, &mut do_bind,
                    &mut do_hidhide,
                    &mut self.auto_switch,
                    &mut do_undo, &mut do_redo,
                    can_undo, can_redo,
                    &self.logo_texture,
                    &mut self.panic_shortcut,
                    &mut self.panic_active,
                    &mut self.panic_learning,
                    &self.panic_shortcut_shared,
                    &mut toggle_settings,
                    &mut toggle_bluetooth,
                    self.bluetooth_present,
                    pin_active_now,
                    &mut do_pin_toggle,
                    ui_mode_now,
                    &mut do_set_mode,
                );
            });
        if toggle_settings {
            self.settings_open = !self.settings_open;
        }
        if toggle_bluetooth {
            self.bluetooth.open = !self.bluetooth.open;
        }
        if do_pin_toggle {
            self.toggle_pin(ctx);
        }
        if let Some(new_mode) = do_set_mode {
            if self.settings.ui_mode != new_mode {
                self.settings.ui_mode = new_mode;
                settings::save_settings(&self.settings);
            }
        }

        // ── Tab bar ───────────────────────────────────────────────────────────────
        let tab_bar_frame = egui::Frame::NONE.fill(ctx.style().visuals.widgets.noninteractive.bg_fill);
        let tab_actions = egui::TopBottomPanel::top("tab_bar")
            .exact_height(32.0)
            .frame(tab_bar_frame)
            .show_separator_line(false)
            .show(ctx, |ui| show_tab_bar(ui, &self.tabs, self.active_tab, &effective_bypass, &mut self.auto_switch))
            .inner;
        let TabBarActions {
            switch_to: tab_switch,
            close_idx: tab_close_idx,
            new_tab: tab_new,
            bypass_toggle: bypass_toggle_idx,
            do_save: tab_save,
            do_load: tab_load,
            do_save_workspace: tab_save_ws,
            do_load_workspace: tab_load_ws,
            do_bind: tab_bind,
            do_close: tab_close,
        } = tab_actions;
        do_new   = do_new   || tab_new;
        do_save  = do_save  || tab_save;
        do_load  = do_load  || tab_load;
        do_save_workspace = do_save_workspace || tab_save_ws;
        do_load_workspace = do_load_workspace || tab_load_ws;
        do_bind  = do_bind  || tab_bind;
        do_close = do_close || tab_close;
        if let Some(idx) = bypass_toggle_idx {
            if idx < self.tabs.len() {
                // Any manual bypass action disengages auto mode.
                self.auto_switch = false;
                if effective_bypass[idx] {
                    // Turn off bypass: clear manual first; if only auto-bypass was active, disable it.
                    if self.tabs[idx].bypassed {
                        self.tabs[idx].bypassed = false;
                    } else {
                        self.tabs[idx].auto_bypass = false;
                    }
                } else {
                    self.tabs[idx].bypassed = true;
                }
            }
        }

        // Open the bind-to-process picker.
        if do_bind {
            self.bind_window_open = true;
            self.bind_window_filter.clear();
            self.bind_window_procs = crate::process_list::enumerate_windows();
        }

        // Open the HidHide configuration window.
        if do_hidhide {
            self.hidhide_window_open = true;
            self.hidhide_filter.clear();
            self.hidhide_proc_list = crate::process_list::enumerate_processes_full();
            // Open a transient handle for the window's lifetime (released when it
            // closes — see the release guard before the window render).
            self.hidhide = HidHideClient::try_open();
            if let Some(hh) = &self.hidhide {
                self.hidhide_whitelist = hh.whitelist();
            }
        }

        // ── Bind-to-process window ────────────────────────────────────────────────
        self.draw_bind_window(ctx);

        // Release the transient HidHide handle whenever the window isn't open —
        // holding it would block the elevated helper from opening the control device.
        if !self.hidhide_window_open && self.hidhide.is_some() {
            self.hidhide = None;
        }

        // ── HidHide configuration window ──────────────────────────────────────────
        self.draw_hidhide_window(ctx);

        // ── Settings window ───────────────────────────────────────────────────
        self.draw_settings_window(ctx);
        self.draw_licenses_window(ctx);
        // Refreshed here rather than in the title bar: `present` rescans on
        // its own timer, and the title bar runs before this in the frame.
        self.bluetooth_present = self.bluetooth.present();
        let key_dir = self.settings.bt_key_dir.clone();
        bluetooth_window::show(ctx, &mut self.bluetooth, key_dir.as_deref());
        self.draw_gp_settings_panel(ctx);
        // Auto-close the picker when a Touch Zones assignment finishes: the card
        // renderer commits the mapping (or Cancel fires) by resetting the node's
        // `_tz_phase` out of "captured". Watching that here means the picker
        // closes on Add without a viewer→app close bridge. Only applies to the
        // TZ picker variant; the Remapper's picker is closed by its own Done.
        if self.gamepad_nav.kbm_picker_open && self.gamepad_nav.kbm_picker_touch_zones {
            let path = self.gamepad_nav.kbm_picker_path.clone();
            if let Some(inner) = self.gamepad_nav.kbm_picker_node {
                let phase = self.picker_target_param_str(&path, inner, "_tz_phase");
                if phase.as_deref() != Some("captured") {
                    self.gamepad_nav.kbm_picker_open = false;
                    self.gamepad_nav.kbm_picker_viewport = None;
                }
            }
        }

        // KB/M picker: drawn here only when the main window owns the session.
        // Editor-owned sessions render inside their own viewport (see
        // show_subpatch_editors); config-overlay sessions render in a dedicated
        // always-on-top viewport over the game. If the owning surface has closed,
        // fall back to the main window so the modal can't become unreachable.
        let picker_vp = crate::config_overlay::picker_viewport_id();
        // Universal routing: while the config overlay is up the main window is
        // behind the game and unreachable, so ANY picker opened without an
        // explicit viewport (gamepad "Choose", a body "Choose…" click, …) must
        // render OVER THE GAME. Promote it here, in the one central spot, so no
        // individual picker entry point has to special-case the overlay.
        if self.gamepad_nav.kbm_picker_open
            && self.gamepad_nav.kbm_picker_viewport.is_none()
            && crate::config_overlay::config_overlay_visible(ctx)
        {
            self.gamepad_nav.kbm_picker_viewport = Some(picker_vp);
        }
        if let Some(vp) = self.gamepad_nav.kbm_picker_viewport {
            if vp == picker_vp {
                // Config-overlay picker is valid only while the overlay is up.
                if !crate::config_overlay::config_overlay_visible(ctx) {
                    self.gamepad_nav.kbm_picker_open = false;
                    self.gamepad_nav.kbm_picker_viewport = None;
                }
            } else {
                let owner_open = self.sub_patch_editors.iter().any(|e| egui::ViewportId::from_hash_of(
                    ("subpatch_editor", e.tab_idx, e.node_id.0)) == vp);
                if !owner_open { self.gamepad_nav.kbm_picker_viewport = None; }
            }
        }
        if self.gamepad_nav.kbm_picker_open
            && self.gamepad_nav.kbm_picker_viewport == Some(picker_vp)
        {
            self.draw_kbm_picker_over_game(ctx);
        } else if self.gamepad_nav.kbm_picker_viewport.is_none() {
            self.draw_kbm_picker(ctx);
        }
        // The press-mode list has the KB/M picker's problem: summoned by a pad
        // from the config overlay, it drew in the main window — behind the game.
        if self.gamepad_nav.press_mode_open && crate::config_overlay::config_overlay_visible(ctx) {
            nav::show_over_game(ctx, |vctx| self.draw_press_mode_picker(vctx));
        } else {
            self.draw_press_mode_picker(ctx);
        }
        self.draw_midi_modal(ctx);
        self.draw_reinstall_confirm(ctx);
        self.draw_uninstall_confirm(ctx);
        // Modal device-op overlay — painted last so it sits above everything and
        // swallows input while a create/remove/reinstall is in flight.
        self.draw_device_op_overlay(ctx);
        // Capture the window's settled position/size so the next launch reopens
        // where it was (fixes the down-right cascade). Marks settings_dirty on a
        // genuine change, which the block just below flushes to settings.json.
        self.persist_window_geometry(ctx);
        if self.settings_dirty {
            settings::save_settings(&self.settings);
            self.settings_dirty = false;
        }

        // Close a specific tab from the tab bar X button.
        let close_idx = tab_close_idx.or(if do_close { Some(self.active_tab) } else { None });
        if let Some(idx) = close_idx {
            if self.tabs.len() == 1 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return ControlFlow::Break(());
            }
            self.tabs.remove(idx);
            let new_idx = self.active_tab.saturating_sub(if self.active_tab > idx { 1 } else { 0 })
                .min(self.tabs.len() - 1);
            // Clamp active_tab before set_active_tab reads self.tabs[self.active_tab].
            self.active_tab = self.active_tab.min(self.tabs.len() - 1);
            // Force the active-id set + I/O filter refresh even when
            // new_idx == active_tab (set_active_tab early-returns on no-op);
            // the closing tab may have referenced devices that are now
            // orphaned and we want to silence them before pruning.
            self.refresh_active_tab_device_ids();
            self.set_active_tab(new_idx);
            // Prune the shared pool: drop any device no remaining tab
            // references. Teardown runs on the worker (off the UI thread) since
            // a HIDMaestro device's Drop calls the blocking helper destroy.
            prune_devices_async(
                &self.shared_virtual_devices,
                &mut self.pending_device_ids,
                &self.device_ops.tx,
                &self.tabs,
            );
        }

        // Switch active tab — manual tab click disengages auto mode.
        if let Some(idx) = tab_switch {
            if idx < self.tabs.len() {
                self.set_active_tab(idx);
                self.auto_switch = false;
            }
        }

        // New tab.
        if do_new {
            let n = self.next_untitled;
            self.next_untitled += 1;
            self.tabs.push(PatchTab::new_untitled(n));
            let new_idx = self.tabs.len() - 1;
            self.set_active_tab(new_idx);
        }

        // Keyboard Undo / Redo in Easy mode. Advanced mode handles Ctrl+Z /
        // Ctrl+Shift+Z inside `Canvas::show()`, but that path doesn't run in
        // Easy mode (the central panel returns early before `canvas::show`), so
        // the shortcut would otherwise be dead there even though pinned-widget
        // edits are now undoable. Skip when a text field holds focus so typing
        // (pinned Label text, preset rename) isn't hijacked.
        if self.settings.ui_mode == settings::UiMode::Easy && !ctx.wants_keyboard_input() {
            let (kz, ksz) = ctx.input(|i| {
                let z = i.events.iter().any(|e| matches!(e,
                    egui::Event::Key { key: egui::Key::Z, pressed: true, modifiers, .. }
                    if modifiers.ctrl && !modifiers.shift));
                let sz = i.events.iter().any(|e| matches!(e,
                    egui::Event::Key { key: egui::Key::Z, pressed: true, modifiers, .. }
                    if modifiers.ctrl && modifiers.shift));
                (z, sz)
            });
            if kz { do_undo = true; }
            if ksz { do_redo = true; }
        }

        // Undo / Redo from title bar buttons (and the Easy-mode keyboard path).
        if do_undo { self.tabs[self.active_tab].canvas.undo(); }
        if do_redo { self.tabs[self.active_tab].canvas.redo(); }

        // Save / Load operate on the active tab.
        if do_save {
            // Saved patches record the device ids referenced by the active
            // tab's canvas — that's the contract `.fxp` consumers expect.
            // The shared pool may hold devices owned by other tabs that
            // shouldn't end up in this file.
            let vids: Vec<String> =
                snarl_virtual_device_ids(&self.tabs[self.active_tab].canvas.snarl);
            let bound = self.tabs[self.active_tab].bound_exes.clone();
            let auto_bypass = self.tabs[self.active_tab].auto_bypass;
            let preset_path = self.tabs[self.active_tab]
                .easy_state.loaded_preset.as_ref().map(|(p, _)| p.clone());
            let overlay = self.tabs[self.active_tab].overlay.clone();
            let config = self.tabs[self.active_tab].config.clone();
            if let Some(saved_path) = self.tabs[self.active_tab].canvas
                .save_patch(vids, bound, auto_bypass, preset_path, overlay, config)
            {
                let tab = &mut self.tabs[self.active_tab];
                tab.title = saved_path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Untitled".to_string());
                tab.file_path = Some(saved_path);
            }
        }
        if do_load {
            if let Some((new_canvas, vids, bound, auto_bypass, path, preset_path, overlay, config)) =
                crate::canvas::Canvas::load_patch()
            {
                // If the loaded file was a .fxsp wrapped into an
                // Easy-shaped canvas (single subpatch node) AND the
                // sub-patch declares the AutoMap inlet/outlet pair
                // plus has a non-empty layout, flip to Easy mode so
                // the user lands in the preset-driven UI directly.
                let loaded_was_fxsp = path.extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("fxsp"))
                    .unwrap_or(false);
                if loaded_was_fxsp && is_easy_compatible_canvas(&new_canvas)
                    && self.settings.ui_mode != settings::UiMode::Easy
                {
                    self.settings.ui_mode = settings::UiMode::Easy;
                    settings::save_settings(&self.settings);
                }
                let tab = &mut self.tabs[self.active_tab];
                tab.canvas = new_canvas;
                clear_canvas_capture_state(&mut tab.canvas);
                tab.title = path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Untitled".to_string());
                tab.file_path = Some(path);
                tab.bound_exes = bound;
                tab.auto_bypass = auto_bypass;
                tab.overlay = overlay;
                tab.config = config;
                // Restore Easy-mode preset link: rederive content hash
                // from the live sub-patch. If the saved path is gone,
                // EasyState will fall back to hash-matching against the
                // current preset index (see center_panel::restore_link).
                tab.easy_state.loaded_preset = preset_path.map(|p| (p, 0));
                // Queue the configured on-load camera action. The canvas
                // consumes this on its next render — it can't apply now
                // because SnarlState / snarl_rect aren't in scope here.
                tab.canvas.pending_view_action = match self.settings.on_patch_load {
                    settings::OnPatchLoad::Off       => None,
                    settings::OnPatchLoad::Center    =>
                        Some(crate::canvas::PendingViewAction::Center),
                    settings::OnPatchLoad::ZoomToFit =>
                        Some(crate::canvas::PendingViewAction::ZoomToFit),
                };
                // Reconcile the shared pool: union of the saved id list
                // and the canvas-referenced ids (canvas wins on disagreement
                // because that's what the user will see). Reuses existing
                // pool entries — no duplicate instances.
                let canvas_vids = snarl_virtual_device_ids(&tab.canvas.snarl);
                let mut needed: Vec<String> = vids;
                for cv in canvas_vids {
                    if !needed.iter().any(|v| v == &cv) {
                        needed.push(cv);
                    }
                }
                reconcile_shared_devices(
                    &self.shared_virtual_devices,
                    &mut self.pending_device_ids,
                    &self.failed_device_ids,
                    &self.device_ops.tx,
                    &needed,
                );
                // Active-tab canvas changed — refresh the I/O filter so the
                // new tab's devices start receiving signals this frame.
                self.refresh_active_tab_device_ids();
                // Prune any devices the previous canvas needed but the new
                // one (and no other tab) does.
                prune_devices_async(
                    &self.shared_virtual_devices,
                    &mut self.pending_device_ids,
                    &self.device_ops.tx,
                    &self.tabs,
                );
            }
        }

        // ── Save workspace (full tab set) ────────────────────────────────────
        // Mirrors the auto-persisted `workspace.json` path but lets the user
        // pick the destination — handy for A/B perf comparisons by quickly
        // swapping between an empty workspace and a loaded one without
        // restarting the app.
        if do_save_workspace {
            if let Some(path) = crate::overlay::with_overlay_not_topmost(|| {
                rfd::FileDialog::new()
                    .add_filter("FlexInput Workspace", &["fxw", "json"])
                    .set_file_name("workspace.fxw")
                    .save_file()
            }) {
                let ws = self.build_persisted_workspace();
                if let Err(e) = settings::save_workspace_to(&ws, &path) {
                    eprintln!("[workspace] save failed: {e}");
                }
            }
        }

        // ── Load workspace (full tab set, replacing current state) ───────────
        if do_load_workspace {
            if let Some(path) = crate::overlay::with_overlay_not_topmost(|| {
                rfd::FileDialog::new()
                    .add_filter("FlexInput Workspace", &["fxw", "json"])
                    .pick_file()
            }) {
                if let Some(ws) = settings::load_workspace_from(&path) {
                    let on_load_view = match self.settings.on_patch_load {
                        settings::OnPatchLoad::Off       => None,
                        settings::OnPatchLoad::Center    => Some(crate::canvas::PendingViewAction::Center),
                        settings::OnPatchLoad::ZoomToFit => Some(crate::canvas::PendingViewAction::ZoomToFit),
                    };
                    let new_tabs: Vec<PatchTab> = ws.tabs.into_iter().map(|pt| {
                        let mut canvas = Canvas::new();
                        canvas.snarl = pt.snarl;
                        crate::canvas::migrate_loaded_snarl(&mut canvas.snarl);
                        clear_canvas_capture_state(&mut canvas);
                        canvas.pending_view_action = on_load_view;
                        // Keep each tab's view independent; allocate a fresh
                        // salt for legacy (`0`) workspaces. See startup loader.
                        let view_salt = if pt.view_salt != 0 { pt.view_salt } else { crate::canvas::next_canvas_salt() };
                        canvas.set_view_salt(view_salt);
                        let easy_state = EasyState {
                            loaded_preset: pt.easy_preset_path.map(|p| (p, 0)),
                            ..EasyState::default()
                        };
                        let mut overlay = pt.overlay;
                        let mut config = pt.config;
                        crate::canvas::node::materialize_subpatch_overlays(
                            &canvas.snarl, &mut overlay, &mut config);
                        PatchTab {
                            title: pt.title,
                            file_path: pt.file_path,
                            bound_exes: pt.bound_exes,
                            canvas,
                            virtual_panel: VirtualDevicePanel::new(),
                            bypassed: false,
                            auto_bypass: pt.auto_bypass,
                            easy_state,
                            view_salt,
                            overlay,
                            config,
                        }
                    }).collect();
                    if !new_tabs.is_empty() {
                        self.tabs = new_tabs;
                        self.active_tab = ws.active_tab.min(self.tabs.len() - 1);
                        // Rebuild the shared virtual-device pool from the new
                        // tab set; prune what's no longer needed. Both go through
                        // the worker so the reload never blocks on helper IPC.
                        let mut needed: Vec<String> = Vec::new();
                        for tab in &self.tabs {
                            for v in snarl_virtual_device_ids(&tab.canvas.snarl) {
                                if !needed.iter().any(|x| x == &v) { needed.push(v); }
                            }
                        }
                        reconcile_shared_devices(
                            &self.shared_virtual_devices,
                            &mut self.pending_device_ids,
                            &self.failed_device_ids,
                            &self.device_ops.tx,
                            &needed,
                        );
                        prune_devices_async(
                            &self.shared_virtual_devices,
                            &mut self.pending_device_ids,
                            &self.device_ops.tx,
                            &self.tabs,
                        );
                        self.refresh_active_tab_device_ids();
                    }
                } else {
                    eprintln!("[workspace] load failed: file missing or invalid JSON");
                }
            }
        }

        // Optionally hide FlexInput's own ViGEm virtuals from the
        // physical-devices panel. The gilrs backend already dedups based
        // on SetupAPI counts, but its `phys_counts` cache is refreshed
        // every ~2s — a freshly-created virtual can therefore leak into
        // the displayed list for up to that window. Filter by kind here
        // using the shared pool as authoritative for "ours": one
        // virtual.xinput → drop one ControllerKind::XInput entry, etc.
        // Device ids that are FlexInput's own virtual pads. Two-tier: HIDMaestro
        // (PS) pads via the path-classified `:v` gilrs id, plus ViGEm
        // (xinput/ds4) pads via the pool-count fallback (no HID path) — see
        // `own_virtual_device_ids`. Used both to filter the physical list (below)
        // and to gray out their nav toggle (loopback feedback guard).
        let nav_excluded_ids = self.own_virtual_device_ids();
        let devices_owned;
        let devices: &[PhysicalDevice] = if self.settings.show_own_virtuals_as_physical {
            &self.devices
        } else {
            // Drop FlexInput's OWN emulated devices so they don't clutter the
            // physical list. A real controller is never dropped: PS pads are
            // path-classified (not plug-order), and the ViGEm fallback only
            // claims as many as we actually created.
            devices_owned = self.devices.iter()
                .filter(|d| !nav_excluded_ids.contains(&d.id))
                .cloned()
                .collect::<Vec<_>>();
            &devices_owned
        };
        // Belt-and-suspenders: ensure any excluded device is OFF in the nav map
        // (covers the case where the global default flipped it on before the
        // device was recognized as a loopback virtual).
        for id in &nav_excluded_ids {
            if let Some(v) = self.gamepad_nav.mode.get_mut(id) { *v = false; }
        }
        // Pre-compute the set of device ids referenced by *non-active* tab
        // canvases so the panel can grey out the close (X) button on any
        // chip another tab still needs.
        let referenced_by_other_tabs: std::collections::HashSet<String> = {
            let mut s = std::collections::HashSet::new();
            for (i, t) in self.tabs.iter().enumerate() {
                if i == self.active_tab { continue; }
                for id in snarl_virtual_device_ids(&t.canvas.snarl) {
                    s.insert(id);
                }
            }
            s
        };
        let shared_pool_for_panel = Arc::clone(&self.shared_virtual_devices);
        let ping_requests_for_panel = Arc::clone(&self.ping_requests);
        let easy_mode = self.settings.ui_mode == settings::UiMode::Easy;

        // Gamepad-nav legend bar — declared before the physical-devices panel so
        // it docks outermost/lowest at the very bottom. Drawn here (before the
        // `&mut self.gamepad_nav` disjoint borrow below) as a read-only `&self`
        // call. Visible only while a nav-enabled gamepad drives the UI.
        self.draw_gp_legend_bar(ctx);

        let device_defaults_for_easy = self.nav_device_defaults();
        let user_presets_folder = self.settings.user_presets_folder.clone();
        // Snapshots needed for the inner sub-patch render in Easy
        // center panel (mirrors what show_subpatch_editors passes to
        // inner_canvas.show in the Sub-Patch editor window).
        let descriptors_for_easy = self.descriptors.clone();
        let live_signals_for_easy = self.last_signals.clone();
        let panic_shortcut_for_easy = self.panic_shortcut.clone();
        // Pull favorites out so easy can mutate; we'll write back +
        // persist if the user starred or reordered something.
        let mut favorites_for_easy = self.settings.favorite_presets.clone();
        let favorites_before = favorites_for_easy.clone();
        let nav_mode_default = self.settings.gamepad_ui_nav_default;
        // Disjoint borrow: `tab` below borrows only `self.tabs`, so this
        // independent borrow of `self.gamepad_nav` is allowed. io_panel takes
        // `&mut .mode`; center_panel takes the whole `&mut gamepad_nav` for
        // preset-dropdown navigation.
        let gamepad_nav = &mut self.gamepad_nav;
        let tab = &mut self.tabs[self.active_tab];
        let (virtual_panel, canvas, easy_state, tab_overlay, tab_config) =
            (&mut tab.virtual_panel, &mut tab.canvas, &mut tab.easy_state,
             &mut tab.overlay, &mut tab.config);

        // Device panels use a darker fill so they read as separate from the
        // canvas surface and the floating heading tabs visually integrate.
        let panel_frame = {
            let bg = ctx.style().visuals.panel_fill;
            let dark = crate::panels::physical_devices::darken_color(bg, 0.35);
            egui::Frame::default()
                .fill(dark)
                .inner_margin(egui::Margin::symmetric(8, 2))
        };
        let collapsed_frame = egui::Frame::default().inner_margin(egui::Margin::ZERO);

        // Animation progress: 0.0 = fully collapsed, 1.0 = fully expanded.
        // ctx.animate_bool_with_time eases between targets smoothly.
        let virt_open = ctx.animate_bool_with_time(
            egui::Id::new("virt_panel_open_anim"),
            !self.virtual_panel_collapsed,
            0.18,
        );
        let phys_open = ctx.animate_bool_with_time(
            egui::Id::new("phys_panel_open_anim"),
            !self.physical_panel_collapsed,
            0.18,
        );

        const VIRT_NATURAL_H: f32 = 36.0;
        const PHYS_NATURAL_H: f32 = 44.0;
        const COLLAPSED_H:    f32 = 1.0;
        let virt_h = COLLAPSED_H.max(VIRT_NATURAL_H * virt_open);
        let phys_h = COLLAPSED_H.max(PHYS_NATURAL_H * phys_open);

        // Build a frame that scales its vertical margins with the animation
        // progress so the panel can actually shrink to ~0 height. Once fully
        // open we use the regular frame so margins are stable for layout.
        let scaled_frame = |progress: f32| -> egui::Frame {
            let bg = ctx.style().visuals.panel_fill;
            let dark = crate::panels::physical_devices::darken_color(bg, 0.35);
            let vpad = (2.0 * progress).round() as i8;
            egui::Frame::default()
                .fill(dark)
                .inner_margin(egui::Margin { left: 8, right: 8, top: vpad, bottom: vpad })
        };
        let top_frame = if virt_open < 0.001 { collapsed_frame }
                        else if virt_open > 0.999 { panel_frame }
                        else { scaled_frame(virt_open) };
        let bot_frame = if phys_open < 0.001 { collapsed_frame }
                        else if phys_open > 0.999 { panel_frame }
                        else { scaled_frame(phys_open) };

        let default_collapsed = self.settings.device_nodes_default_collapsed;
        // Built inline (not via `nav_device_defaults`) because a whole-`self`
        // method call can't coexist with the `&mut self.gamepad_nav` above;
        // field reads on `self.settings` can.
        let device_defaults = crate::canvas::DeviceParamDefaults {
            stick_deadzone: self.settings.default_stick_deadzone,
            gyro_mult: self.settings.default_gyro_mult,
            mouse_sensitivity: self.settings.default_mouse_sensitivity,
            rumble_floor: self.settings.default_rumble_floor,
            rumble_max: self.settings.default_rumble_max,
            rumble_exp: self.settings.default_rumble_exp,
        };

        // Both side panels are declared unconditionally so egui's
        // remembered panel stack ordering is stable across Easy ↔
        // Advanced toggles. In Easy mode the panel bodies are no-ops
        // (height collapses to a zero-content frame) and the floating
        // heading tabs are hidden.
        let top_resp = egui::TopBottomPanel::top("virtual_devices_panel")
            .resizable(false)
            .exact_height(if easy_mode { 0.0 } else { virt_h })
            .frame(if easy_mode { collapsed_frame } else { top_frame })
            .show(ctx, |ui| {
                if !easy_mode && virt_open > 0.01 {
                    virtual_panel.show(
                        ui,
                        &shared_pool_for_panel,
                        canvas,
                        default_collapsed,
                        device_defaults,
                        &|id| referenced_by_other_tabs.contains(id),
                    );
                }
            });

        let bottom_resp = egui::TopBottomPanel::bottom("physical_devices_panel")
            .resizable(false)
            .exact_height(if easy_mode { 0.0 } else { phys_h })
            .frame(if easy_mode { collapsed_frame } else { bot_frame })
            .show(ctx, |ui| {
                if !easy_mode && phys_open > 0.01 {
                    physical_devices::show(ui, devices, canvas, default_collapsed, device_defaults);
                }
            });
        if !easy_mode && phys_open > 0.99 {
            self.bottom_panel_height = bottom_resp.response.rect.height();
        }

        if !easy_mode {
            // Floating heading tabs hang off each panel's canvas-facing edge,
            // nudged 1px down to sit on the canvas inner border.
            let top_rect = top_resp.response.rect;
            let bottom_rect = bottom_resp.response.rect;
            let top_anchor_y = top_rect.bottom() + 5.0;
            let bottom_anchor_y = bottom_rect.top() - 1.0;
            let top_tab = crate::panels::physical_devices::draw_floating_heading(
                ctx,
                "heading_virtual_devices",
                "Virtual Devices",
                egui::pos2(top_rect.left() + 12.0, top_anchor_y),
                crate::panels::physical_devices::TabDirection::Down,
            );
            if top_tab.clicked() {
                self.virtual_panel_collapsed = !self.virtual_panel_collapsed;
            }
            let bottom_tab = crate::panels::physical_devices::draw_floating_heading(
                ctx,
                "heading_physical_devices",
                "Physical Devices",
                egui::pos2(bottom_rect.left() + 12.0, bottom_anchor_y),
                crate::panels::physical_devices::TabDirection::Up,
            );
            if bottom_tab.clicked() {
                self.physical_panel_collapsed = !self.physical_panel_collapsed;
            }
        }

        // Seed outer canvas clipboard from inner when app_clipboard_from_inner is set
        // (user copied inside a sub-patch editor last frame) or on first use.
        if self.app_clipboard_from_inner || canvas.clipboard().is_none() {
            if let Some(ref cb) = self.app_clipboard {
                canvas.set_clipboard(cb.clone());
            }
        }
        let outer_gen_before = canvas.clipboard_gen;

        let device_rates_snap = self.device_rates.read().map(|r| r.clone()).unwrap_or_default();
        let mut calibrate_request: Option<egui_snarl::NodeId> = None;
        // Two-way bridge between settings.see_through_alpha and the
        // temp data slot that the eye-button popover slider edits.
        // If the slot value differs from settings (popover user moved
        // the slider), pull the new value into settings and mark
        // dirty. Then publish settings back to the slot so any other
        // reader (canvas bg_frame fill) sees the up-to-date value.
        {
            let alpha_id = egui::Id::new(crate::canvas::SEE_THROUGH_ALPHA_KEY);
            let from_slot: Option<f32> = ctx.data(|d| d.get_temp::<f32>(alpha_id));
            if let Some(a) = from_slot {
                if (a - self.settings.see_through_alpha).abs() > f32::EPSILON {
                    self.settings.see_through_alpha = a.clamp(0.0, 1.0);
                    self.settings_dirty = true;
                }
            }
            ctx.data_mut(|d| {
                d.insert_temp(alpha_id, self.settings.see_through_alpha);
            });
        }
        // CentralPanel handling has two modes:
        //  * Opaque: default `Frame::central_panel(&style)` — its
        //    `panel_fill` paints the whole area, and the 8 px
        //    inner_margin gives the implicit "frame surround" band
        //    around the snarl rect.
        //  * See-through: we need the inner area to actually be
        //    transparent so the snarl `bg_frame` alpha can show the
        //    desktop. We use a TRANSPARENT frame fill (so the inner
        //    area doesn't paint), but keep `inner_margin(8)` so the
        //    snarl rect lands in the same place as opaque mode. Then
        //    we explicitly paint 4 opaque bands in the 8 px surround
        //    region — this is the FRAME, promoted from
        //    accidental-byproduct of the central panel fill to a
        //    deliberate UI element.
        let style_snapshot = ctx.style();
        let see_through_on = self.settings.see_through_active;
        let central_frame = if see_through_on {
            egui::Frame::central_panel(&style_snapshot)
                .fill(egui::Color32::TRANSPARENT)
        } else {
            egui::Frame::central_panel(&style_snapshot)
        };

        egui::CentralPanel::default().frame(central_frame).show(ctx, |ui| {
            if easy_mode {
                // Two-pane Easy layout built INSIDE the CentralPanel
                // (rather than using SidePanels) so the egui panel
                // topology — and the layer ordering the snarl canvas
                // relies on for set_sublayer — stays identical to
                // Advanced mode. Allocating fixed-width child UIs
                // sidesteps the "Background vs Middle" sublayer panic
                // that toggling SidePanel registrations triggers.
                //
                // Left panel: input/output picker (scrollable gamepad
                // list on top, output section on bottom). Central:
                // sub-patch preset picker + body.
                let total = ui.available_rect_before_wrap();
                // `total` is inset from the window edge by the central
                // panel's inner margin (~8px). The visible window edge
                // sits in that surround ring. Compute the full content
                // bounds (ring included) so (a) the surround doesn't go
                // fully transparent in see-through mode, and (b) the
                // inner shadow hugs the real window edge with no offset.
                let margin = style_snapshot.spacing.window_margin.left as f32;
                let outer_total = total.expand(margin.max(8.0));
                // Stash the FULL content area (everything below the tab
                // strip, out to the window edge) so the post-frame inner-
                // shadow pass hugs the real edge rather than the margin-
                // inset content rect.
                ctx.data_mut(|d| d.insert_temp(
                    egui::Id::new(INNER_SHADOW_RECT_KEY), outer_total));
                // Fixed left-panel width so cards / chips don't restyle
                // as the user resizes the window. Only shrinks if the
                // window itself is narrower than this baseline.
                let side_w_full = 280.0_f32.min(total.width() * 0.5);
                // Collapse animation: 1.0 = fully open, 0.0 = folded away left.
                // `side_w` is the on-screen SLICE width; the panel itself keeps its
                // full width and SLIDES left (translated + clipped) so its contents
                // move out of frame rather than being squeezed into nothing.
                let left_open = ctx.animate_bool_with_time(
                    egui::Id::new("easy_left_open_anim"),
                    !self.easy_left_panel_collapsed,
                    0.18,
                );
                let side_w = side_w_full * left_open;
                let left_visible = left_open > 0.01;
                let gap = 6.0_f32 * left_open;
                // Visible slice (also the clip + dark-fill + tab-anchor rect).
                let left_rect = egui::Rect::from_min_size(
                    total.min,
                    egui::vec2(side_w, total.height()),
                );
                // Full-width panel, translated left so its RIGHT edge lands on the
                // slice's right edge; the part left of `total.min.x` is clipped off
                // frame. At open=1 it coincides with `left_rect`.
                let panel_full_rect = egui::Rect::from_min_size(
                    egui::pos2(total.min.x + side_w - side_w_full, total.min.y),
                    egui::vec2(side_w_full, total.height()),
                );
                let center_rect = egui::Rect::from_min_size(
                    egui::pos2(total.min.x + side_w + gap, total.min.y),
                    egui::vec2((total.width() - side_w - gap).max(0.0), total.height()),
                );
                // See-through handling for Easy mode. In opaque mode the
                // left panel uses its solid dark fill and the central
                // area inherits the (opaque) CentralPanel frame. In
                // see-through mode BOTH backgrounds get the user-chosen
                // alpha so the desktop bleeds through evenly — the
                // central frame fill was set TRANSPARENT above, so we
                // paint the alpha-faded central fill ourselves here,
                // mirroring how the Advanced canvas fades its bg_frame.
                let st_alpha: f32 = ctx.data(|d|
                    d.get_temp::<f32>(egui::Id::new(crate::canvas::SEE_THROUGH_ALPHA_KEY))
                ).unwrap_or(1.0);
                let left_fill = if see_through_on {
                    let a = (st_alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
                    egui::Color32::from_rgba_unmultiplied(0x1a, 0x1a, 0x1a, a)
                } else {
                    egui::Color32::from_rgb(0x1a, 0x1a, 0x1a)
                };
                if see_through_on {
                    // Fill the FULL surround ring (out to the window edge)
                    // with the alpha-faded panel fill first, so the
                    // central-panel inner margin doesn't read as a fully-
                    // transparent border. The per-panel fills below paint
                    // on top of this, matching opacity throughout.
                    let base = style_snapshot.visuals.extreme_bg_color;
                    let a = (st_alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
                    ui.painter().rect_filled(
                        outer_total, 0.0,
                        egui::Color32::from_rgba_unmultiplied(
                            base.r(), base.g(), base.b(), a),
                    );
                    // Central area: alpha-faded panel fill so the
                    // sub-patch body floats over a translucent surface
                    // instead of a fully-transparent void.
                    ui.painter().rect_filled(
                        center_rect, 0.0,
                        egui::Color32::from_rgba_unmultiplied(
                            base.r(), base.g(), base.b(), a),
                    );
                }
                // Darker panel background fill so the left panel
                // visually groups itself apart from the central canvas.
                // Skipped once folded away (zero width) so nothing renders.
                if left_visible {
                    ui.painter().rect_filled(left_rect, 0.0, left_fill);
                    // Lay the panel out at FULL width in `panel_full_rect` (so cards
                    // never reflow), but clip to the visible slice — the content
                    // then slides left out of frame as the panel folds.
                    ui.scope_builder(egui::UiBuilder::new().max_rect(panel_full_rect), |ui| {
                        ui.set_clip_rect(left_rect);
                        puffin::profile_scope!("easy_io_panel_show");
                        crate::easy::io_panel::show(
                            ui,
                            devices,
                            canvas,
                            &shared_pool_for_panel,
                            default_collapsed,
                            device_defaults_for_easy,
                            &mut calibrate_request,
                            &device_rates_snap,
                            &ping_requests_for_panel,
                            &mut gamepad_nav.mode,
                            nav_mode_default,
                            &nav_excluded_ids,
                        );
                    });
                }

                // Floating "Devices" tab: hangs off the left panel's right edge
                // onto the preset bar, aligned with the preset-dropdown row. Rides
                // the panel as it collapses and pins at the window's top-left as
                // the re-open button. Publish its right edge so the center panel
                // clamps the "Preset:" label off it.
                let (tab_resp, tab_rect) = crate::panels::physical_devices::draw_devices_tab(
                    ctx,
                    "easy_devices_tab",
                    "Devices",
                    left_rect.right(),
                    // Match the preset pill's top: center content top + add_space(6)
                    // + the (34-row − 30-pill)/2 centering inset.
                    total.min.y + 8.0,
                );
                if tab_resp.clicked() {
                    self.easy_left_panel_collapsed = !self.easy_left_panel_collapsed;
                }
                ctx.data_mut(|d| d.insert_temp(
                    egui::Id::new("easy_devices_label_right_x"), tab_rect.right()));

                ui.scope_builder(egui::UiBuilder::new().max_rect(center_rect), |ui| {
                    puffin::profile_scope!("easy_center_panel_show");
                    crate::easy::center_panel::show(
                        ui,
                        canvas,
                        easy_state,
                        tab_overlay,
                        tab_config,
                        user_presets_folder.as_deref(),
                        device_defaults_for_easy,
                        &descriptors_for_easy,
                        devices,
                        &live_device_ids,
                        &live_signals_for_easy,
                        &panic_shortcut_for_easy,
                        &device_rates_snap,
                        &mut favorites_for_easy,
                        gamepad_nav,
                    );
                    // Keep the Easy I/O nodes arranged against the sub-patch using
                    // its REAL rendered size (just measured by the canvas show
                    // above via final_node_rect). Re-running each frame lets a
                    // freshly-deployed node converge to its measured size, and
                    // reflows automatically when the Layout editor resizes the
                    // sub-patch. Easy-mode only (this branch); Advanced never runs
                    // it, so manual node drags there are preserved.
                    crate::easy::layout::reposition_io_nodes_with_ctx(canvas, Some(ui.ctx()));
                });
                if favorites_for_easy != favorites_before {
                    self.settings.favorite_presets = favorites_for_easy;
                    settings::save_settings(&self.settings);
                }
                return;
            }
            if see_through_on {
                // `ui.max_rect()` here is the central panel's content
                // area AFTER the 8 px inner_margin has been applied,
                // so the visible "frame" surround band sits in the
                // 8 px ring OUTSIDE this rect. Paint that ring on the
                // ctx's background layer painter (which spans the
                // whole window) using the panel_fill color.
                let inner = ui.max_rect();
                let outer = inner.expand(8.0);
                let p = ctx.layer_painter(egui::LayerId::background());
                let frame_color = style_snapshot.visuals.panel_fill;
                // Top
                p.rect_filled(
                    egui::Rect::from_min_max(outer.left_top(),
                        egui::pos2(outer.right(), inner.top())),
                    egui::CornerRadius::ZERO, frame_color);
                // Bottom
                p.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(outer.left(), inner.bottom()),
                        outer.right_bottom()),
                    egui::CornerRadius::ZERO, frame_color);
                // Left (between top and bottom bands)
                p.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(outer.left(), inner.top()),
                        egui::pos2(inner.left(), inner.bottom())),
                    egui::CornerRadius::ZERO, frame_color);
                // Right
                p.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(inner.right(), inner.top()),
                        egui::pos2(outer.right(), inner.bottom())),
                    egui::CornerRadius::ZERO, frame_color);
            }
            // Stash the canvas content area so the post-frame inner-
            // shadow pass scopes its gradient to the canvas rather than
            // the whole window (Advanced mode).
            ctx.data_mut(|d| d.insert_temp(
                egui::Id::new(INNER_SHADOW_RECT_KEY), ui.max_rect()));
            puffin::profile_scope!("canvas_show");
            calibrate_request = crate::panels::canvas::show(
                canvas, &self.descriptors, &live_device_ids, &self.last_signals,
                &self.last_sink_bus,
                &self.panic_shortcut, devices, &device_rates_snap,
                device_defaults, ui, &ping_requests_for_panel,
            );
        });
        if let Some(node) = calibrate_request {
            self.calibration_open.insert(node);
        }
        {
            puffin::profile_scope!("calibration_show_windows");
            // Hands-free gyro auto-calibration watcher — runs whether or not
            // a calibration window is open.
            crate::panels::calibration::auto_cal_tick(ctx, canvas, &self.last_signals);
            crate::panels::calibration::show_windows(ctx, canvas, &mut self.calibration_open, &self.last_signals, &self.scope_taps, &self.spike_filter_settings, &self.gyro_drift_settings, &self.click_gate_settings);
        }

        // Only update app_clipboard from outer canvas when the user actually copied
        // (gen advanced). Clear from_inner flag regardless so seeding doesn't repeat.
        if canvas.clipboard_gen != outer_gen_before {
            self.last_outer_clipboard_gen = canvas.clipboard_gen;
            if let Some(cb) = canvas.clipboard() {
                self.app_clipboard = Some(cb);
            }
        }
        self.app_clipboard_from_inner = false;

        // A Special… button on a top-level (or main-canvas sub-patch) Remapper/
        // Lean body requested the shared picker. The request carries its own
        // `outer` addressing; editor-viewport requests are handled inside
        // show_subpatch_editors. Deferred to here so the `canvas` borrow above
        // has ended before this `&mut self` call.
        if let Some(req) = crate::canvas::viewer::take_special_picker_request(ctx) {
            self.open_special_picker(req, None);
        }
        // Same for a MIDI… button or a card's MIDI chip: the editor window.
        if let Some(req) = crate::canvas::viewer::take_midi_modal_request(ctx) {
            self.open_midi_modal(req, None);
        }

        // Overlay pick: a click on an exposable element of the MAIN canvas
        // (armed amber by the overlay's "Add element"). Drained here — before
        // the sub-patch editors run — so an editor drain can't misattribute
        // a top-level click. Path `[]` = tab canvas.
        if crate::canvas::viewer::overlay_pick_active(ctx) {
            if let Some((inner_uid, eid, size)) =
                crate::canvas::viewer::take_overlay_pick_pending(ctx)
            {
                crate::canvas::viewer::put_overlay_pick_result(ctx, vec![], inner_uid, eid, size);
            }
            // Esc in the main window cancels the pick back to overlay edit.
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                crate::canvas::viewer::set_overlay_pick_active(ctx, false);
            }
        }

        ControlFlow::Continue(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(minimized: bool, focused: bool, pointer_live: bool) -> WindowFacts {
        WindowFacts {
            minimized,
            focused,
            pointer_live,
            size: egui::vec2(800.0, 600.0),
            pixels_per_point: 1.0,
        }
    }

    #[test]
    fn a_minimized_window_freezes_without_the_setting() {
        let mut f = WindowFreeze::default();
        assert_eq!(f.step(&window(true, false, false), true, false), WindowStep::KeepFrame);
    }

    #[test]
    fn an_unfocused_window_only_freezes_with_the_setting() {
        let mut f = WindowFreeze::default();
        assert_eq!(f.step(&window(false, false, false), true, false), WindowStep::Draw);
        assert_eq!(f.step(&window(false, true, false), true, true), WindowStep::Draw);
    }

    #[test]
    fn the_veil_is_drawn_once_and_then_stays_on_screen() {
        let mut f = WindowFreeze::default();
        let w = window(false, false, false);
        assert_eq!(f.step(&w, true, true), WindowStep::DrawDimmed);
        assert_eq!(f.step(&w, true, true), WindowStep::KeepFrame);
        assert_eq!(f.step(&w, true, true), WindowStep::KeepFrame);
    }

    #[test]
    fn a_pointer_on_the_way_keeps_the_dimmed_window_live() {
        let mut f = WindowFreeze::default();
        assert_eq!(f.step(&window(false, false, false), true, true), WindowStep::DrawDimmed);
        assert_eq!(f.step(&window(false, false, true), true, true), WindowStep::DrawDimmed);
        assert_eq!(f.step(&window(false, false, false), true, true), WindowStep::KeepFrame);
    }

    #[test]
    fn a_resize_redraws_the_veil() {
        let mut f = WindowFreeze::default();
        let mut w = window(false, false, false);
        f.step(&w, true, true);
        w.size = egui::vec2(900.0, 600.0);
        assert_eq!(f.step(&w, true, true), WindowStep::DrawDimmed);
        assert_eq!(f.step(&w, true, true), WindowStep::KeepFrame);
    }

    #[test]
    fn coming_back_from_minimized_redraws_the_veil() {
        let mut f = WindowFreeze::default();
        f.step(&window(false, false, false), true, true);
        f.step(&window(true, false, false), true, true);
        assert_eq!(f.step(&window(false, false, false), true, true), WindowStep::DrawDimmed);
    }

    /// The delay egui will actually wait, after one pass that asks with `ask`.
    fn delay_after(ask: impl FnMut(&egui::Context)) -> std::time::Duration {
        let ctx = egui::Context::default();
        // A new context repaints at once for its first passes, whatever is asked.
        for _ in 0..4 {
            let _ = ctx.run(egui::RawInput::default(), |_| {});
        }
        let out = ctx.run(egui::RawInput::default(), ask);
        out.viewport_output[&egui::ViewportId::ROOT].repaint_delay
    }

    #[test]
    fn a_short_wait_does_not_turn_into_repainting_at_once() {
        let wait = std::time::Duration::from_millis(5);
        // What this guards against: egui's own handling of a short delay.
        assert!(delay_after(|c| c.request_repaint_after(wait)).is_zero());

        let got = delay_after(|c| request_pass_in(c, wait));
        let off = got.abs_diff(wait);
        assert!(off < std::time::Duration::from_millis(1), "{got:?}");
    }

    #[test]
    fn would_keep_says_what_step_will() {
        for (minimized, focused, pointer_live) in [
            (true, false, false), (false, false, false), (false, true, false), (false, false, true),
        ] {
            for may_freeze in [false, true] {
                for setting in [false, true] {
                    for veiled in [false, true] {
                        let w = window(minimized, focused, pointer_live);
                        let mut f = WindowFreeze::default();
                        if veiled {
                            f.step(&window(false, false, false), true, true);
                        }
                        let said = f.would_keep(&w, may_freeze, setting);
                        assert_eq!(said, f.step(&w, may_freeze, setting) == WindowStep::KeepFrame,
                            "{minimized} {focused} {pointer_live} {may_freeze} {setting} {veiled}");
                    }
                }
            }
        }
    }

    #[test]
    fn must_draw_overrides_everything() {
        let mut f = WindowFreeze::default();
        assert_eq!(f.step(&window(true, false, false), false, true), WindowStep::Draw);
        assert_eq!(f.step(&window(false, false, false), false, true), WindowStep::Draw);
    }
}
