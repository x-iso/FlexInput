//! The desktop Settings modal, and the gamepad-shortcut row widget it
//! shares with the gamepad-native settings panel.

use super::*;

/// A clickable gamepad shortcut display: the assigned combo rendered as button
/// icon chips (like the remapper chords), a "(none)" pill when unassigned, or an
/// amber "capturing" pill while learning. Clicking anywhere on it (re)binds.
fn shortcut_chord_widget(
    ui: &mut egui::Ui, combo: &[String], learning: bool,
    skin: crate::canvas::remapper_icons::Skin,
) -> egui::Response {
    if learning {
        return ui.add(egui::Button::new(
            egui::RichText::new("Hold combo, release…").size(crate::settings::ui_size(12.0)))
            .fill(egui::Color32::from_rgb(80, 60, 30))
            .stroke(egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(200, 160, 80))));
    }
    if combo.is_empty() {
        return ui.add(egui::Button::new(egui::RichText::new("(none)").size(crate::settings::ui_size(12.0))));
    }
    let chip_h = 20.0;
    let gap = 4.0;
    let pad = 4.0;
    let w = combo.len() as f32 * chip_h
        + combo.len().saturating_sub(1) as f32 * gap
        + pad * 2.0;
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(w, chip_h + pad * 2.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        let bg = if resp.hovered() {
            crate::settings::tinted_gray(64)
        } else {
            crate::settings::tinted_gray(46)
        };
        painter.rect_filled(rect, 3.0, bg);
        let mut x = rect.left() + pad;
        let y = rect.top() + pad;
        for pin in combo {
            let cw = crate::canvas::viewer::paint_chord_chip_to_rect(
                &painter, ui.ctx(), egui::pos2(x, y), chip_h, pin, skin);
            x += cw + gap;
        }
    }
    resp
}

/// Double-click on a slider. `egui::Slider` senses drag only, so egui never
/// flags it clicked and `Response::double_clicked()` is always false — read the
/// double-click from the pointer while the slider is hovered instead.
fn slider_double_clicked(resp: &egui::Response) -> bool {
    resp.hovered()
        && resp.ctx.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary))
}

impl FlexInputApp {


    /// One gamepad-shortcut chord row: a label, a Learn button that captures a
    /// button combo (sets `gamepad_nav.chord_learn`), and a clear (✕). Shared by
    /// the desktop Settings window and the gamepad-native settings panel.
    /// Returns true if the setting changed (so the caller marks dirty).
    pub(crate) fn gamepad_shortcut_row(&mut self, ui: &mut egui::Ui, label: &str,
        target: crate::gamepad_nav::ChordTarget) -> bool
    {
        let mut changed = false;
        let learning = self.gamepad_nav.chord_learn == Some(target);
        // Snapshot the assigned combo as an owned value so no borrow of
        // self.settings is held across the closure (which mutates self).
        let combo_now: Vec<String> = self.settings.chord(target).clone().unwrap_or_default();
        let has_assigned = !combo_now.is_empty();
        // A single-button shortcut (a standalone Guide/Capture/Mic) may NOT use
        // "on press": the same button is meant to also be combinable in other
        // shortcuts, and an on-press single would fire the instant it's touched.
        let is_single = combo_now.len() == 1;
        let skin = self.shortcut_display_skin();
        form_row(ui, label, "", |ui| {
            let resp = shortcut_chord_widget(ui, &combo_now, learning, skin);
            if resp.on_hover_text(
                "Click, then hold a gamepad combo and release it to bind.\n\
                 A single Guide or Capture/Mic button is allowed; otherwise use \
                 2+ buttons. Captured while held; latches when you let go. \
                 (Press East alone to cancel.)").clicked()
            {
                // Toggle learn for this target; clear any in-flight draft and
                // require a full release before accumulating.
                self.gamepad_nav.chord_learn = if learning { None } else { Some(target) };
                self.gamepad_nav.chord_draft.clear();
                self.gamepad_nav.chord_arm_idle = false;
            }
            if has_assigned
                && ui.small_button("✕").on_hover_text("Clear shortcut").clicked()
            {
                *self.settings.chord_mut(target) = None;
                if learning { self.gamepad_nav.chord_learn = None; }
                changed = true;
            }

            // ── Press mode + time gap (mirrors the remapper card controls) ──
            // The layout-tab buttons have neither: always on press.
            if target.in_app() {
                return;
            }
            use crate::gamepad_nav::{SHORTCUT_PRESS_MODES, shortcut_press_mode_label,
                shortcut_press_mode_glyph};
            let Some(mut mode_now) = self.settings.chord_mode_mut(target).map(|m| m.clone()) else { return };
            // Single-button shortcuts can't be "on press" — migrate a stored
            // "down" to "long" so the value shown (and persisted below) is valid.
            if is_single && mode_now == "down" {
                mode_now = "long".to_string();
            }
            egui::ComboBox::from_id_salt(("fxi_shortcut_mode", label))
                .width(110.0)
                .selected_text(format!("{}  {}",
                    shortcut_press_mode_glyph(&mode_now),
                    shortcut_press_mode_label(&mode_now)))
                .show_ui(ui, |ui| {
                    for (val, glyph, mlabel) in SHORTCUT_PRESS_MODES {
                        // Hide "on press" for single-button shortcuts.
                        if is_single && *val == "down" { continue; }
                        if ui.selectable_label(mode_now == *val,
                            format!("{glyph}  {mlabel}")).clicked()
                        {
                            mode_now = (*val).to_string();
                        }
                    }
                });
            if let Some(m) = self.settings.chord_mode_mut(target) {
                if *m != mode_now {
                    *m = mode_now.clone();
                    changed = true;
                }
            }

            // Time gap applies to long / double (mirrors remapper `window_ms`).
            let gap_applies = mode_now != "down";
            let Some(mut gap_ms) = self.settings.chord_gap_ms_mut(target).map(|g| *g) else { return };
            ui.add_enabled_ui(gap_applies, |ui| {
                let resp = ui.add(
                    egui::DragValue::new(&mut gap_ms)
                        .speed(5.0).range(10.0f32..=5000.0)
                        .custom_formatter(|n, _| format!("{n:.0} ms")),
                ).on_hover_text("Long-press hold time / double-tap max gap.");
                if resp.changed() {
                    if let Some(g) = self.settings.chord_gap_ms_mut(target) {
                        *g = gap_ms;
                    }
                    changed = true;
                }
            });
        });
        changed
    }

    /// Skin used to render gamepad shortcut chord icons: inferred from the first
    /// connected physical pad so the glyphs match the user's controller, falling
    /// back to Xbox when none is present.
    pub(crate) fn shortcut_display_skin(&self) -> crate::canvas::remapper_icons::Skin {
        use crate::canvas::remapper_icons::{phys_pad_slug, skin_from_device_id, Skin};
        self.devices.iter()
            .find(|d| phys_pad_slug(&d.id).is_some())
            .map(|d| skin_from_device_id(&d.id))
            .unwrap_or(Skin::Xbox)
    }

    /// Skin of the currently-connected PHYSICAL gamepad, from its VID/PID-detected
    /// `ControllerKind` (robust where the id string is ambiguous, e.g. a Switch Pro
    /// whose gilrs name isn't "switch"), skipping our OWN re-enumerated virtual
    /// outputs (a virtual DualSense sink would otherwise mask the real pad) and
    /// MIDI. Drives dynamic `gp:<pin>` icon rendering; falls back to Xbox.
    pub(crate) fn current_gamepad_skin(&self) -> crate::canvas::remapper_icons::Skin {
        use crate::canvas::remapper_icons::{phys_pad_slug, Skin};
        use flexinput_devices::ControllerKind as CK;
        self.devices.iter()
            .find(|d| phys_pad_slug(&d.id).is_some()
                && !crate::app::is_own_virtual_gilrs_id(&d.id)
                && !matches!(d.kind, CK::MidiIn | CK::MidiOut))
            .map(|d| match d.kind {
                CK::DualShock4 | CK::DualSense => Skin::Playstation,
                CK::SwitchPro | CK::JoyConL | CK::JoyConR | CK::JoyConPair
                | CK::JoyCon2L | CK::JoyCon2R => Skin::SwitchPro,
                _ => Skin::Xbox,
            })
            .unwrap_or(Skin::Xbox)
    }


    /// Device id of a deployed virtual XInput (Virtual Xbox) sink in the active
    /// patch, if any — the target for a player-slot re-arrive.
    pub(crate) fn active_virtual_xinput_device_id(&self) -> Option<String> {
        let snarl = &self.tabs[self.active_tab].canvas.snarl;
        for (_id, node_ref) in snarl.nodes_ids_data() {
            let node = &node_ref.value;
            if node.module_id != "device.sink" {
                continue;
            }
            if let Some(dev) = node.params.get("device_id").and_then(|v| v.as_str()) {
                // Accept every Virtual Xbox kind id — both the legacy
                // `virtual.xinput*` and the Easy-mode HIDMaestro `virtual.hm.xinput*`.
                if crate::easy::io_panel::device_id_is_xinput(dev) {
                    return Some(dev.to_string());
                }
            }
        }
        None
    }

    /// Recompute and apply HidHide masking for the active patch. Cheap to call
    /// every frame: a last-applied snapshot debounces redundant applies, and the
    /// slow SetupAPI instance-id lookup + blocking helper IPC run on a spawned
    /// thread. Never spawns the elevated helper just to apply/clear *nothing*
    /// (avoids a spurious UAC when the feature is on but nothing is mapped yet).
    /// Linux: the same rules, applied by its own hide helper
    /// (`flexinput_devices::linux_hide`), which has no keep-on-exit.
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) fn reconcile_hidhide(&mut self) {
        // Only do real work on a relevant change: an explicit dirty (toggle /
        // startup), a device plug/unplug, or a slow fallback that catches patch
        // wiring edits. This replaces the per-frame walk.
        let device_sig: u64 = self.devices.iter().fold(0u64, |acc, d| {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            d.id.hash(&mut h);
            acc ^ h.finish() // XOR: order-independent
        });
        let due = self.hidhide_dirty
            || device_sig != self.hidhide_last_device_sig
            || self.hidhide_last_reconcile.elapsed() >= std::time::Duration::from_millis(750);
        if !due {
            return;
        }
        self.hidhide_dirty = false;
        self.hidhide_last_device_sig = device_sig;
        self.hidhide_last_reconcile = std::time::Instant::now();

        // Refresh the session vid/pid cache from the devices present right now.
        // Entries are never evicted while the app runs — that's the point: a pad
        // that drops out of enumeration must still resolve to a vid/pid so the
        // sticky path in `remapped_physical_hid_targets` can keep masking it.
        // Only maskable pads go in, so a cache hit needs no further filtering:
        // same exclusions as the live path (Xbox's XUSB face can't be hidden,
        // MIDI has no vid/pid).
        for dev in &self.devices {
            if matches!(
                dev.kind,
                flexinput_devices::ControllerKind::MidiIn
                    | flexinput_devices::ControllerKind::MidiOut
            ) {
                continue;
            }
            // Linux hides an Xbox pad's evdev node like any other.
            #[cfg(windows)]
            if dev.kind == flexinput_devices::ControllerKind::XInput {
                continue;
            }
            // Joy-Con 2 is read by OUR OWN hidapi transport (`joycon2::usb`),
            // not by SDL/gilrs, so masking its HID interface hides it from
            // FlexInput as well as from everything else. HidHide's per-app
            // whitelist is unreliable on this machine, and the observed result
            // is a pad that enumerates, opens, and then streams nothing at all
            // — the exact "connected at 0 Hz" symptom. There is also nothing to
            // gain: over USB the vendor protocol lives on interface 1, which no
            // game reads, so a wired Joy-Con 2 does not produce the duplicate
            // gamepad that hiding exists to suppress.
            if matches!(
                dev.kind,
                flexinput_devices::ControllerKind::JoyCon2L
                    | flexinput_devices::ControllerKind::JoyCon2R
            ) {
                self.hidhide_vidpid_cache.remove(&dev.id);
                continue;
            }
            if let (Some(vid), Some(pid)) = (dev.vid, dev.pid) {
                self.hidhide_vidpid_cache.insert(dev.id.clone(), (vid, pid));
            }
        }

        let installed = self.hidhide_installed;
        let auto_on = self.settings.hide_originals.unwrap_or(installed);
        self.hidhide_auto_ids = if auto_on && installed {
            self.remapped_physical_ids()
        } else {
            HashSet::new()
        };
        let targets: Vec<(u16, u16, bool)> = if installed {
            self.hidhide_targets()
        } else {
            Vec::new()
        };
        // A manual hide needs HidHide on even with "Hide original controllers" off.
        let active = auto_on || !targets.is_empty();
        // Pads a previous session left hidden at exit, to unhide unless still wanted.
        // (Linux keeps nothing hidden past exit.)
        let release = if cfg!(windows) { self.settings.hidhide_kept_ids.clone() } else { Vec::new() };
        // Right after launch the device list may still be empty; releasing then
        // would unhide a kept pad for the moment until it enumerates.
        if targets.is_empty()
            && !release.is_empty()
            && self.hidhide_started.elapsed() < std::time::Duration::from_secs(4)
        {
            return;
        }

        let prev_targets = std::mem::take(&mut self.hidhide_last_targets);
        if self.hidhide_last_active == Some(active) && prev_targets == targets {
            self.hidhide_last_targets = prev_targets; // unchanged: restore + bail
            return;
        }
        self.hidhide_last_active = Some(active);
        self.hidhide_last_targets = targets.clone();
        // Don't spawn the helper (UAC) just to apply or clear an empty set.
        if targets.is_empty() && prev_targets.is_empty() && release.is_empty() {
            return;
        }

        #[cfg(target_os = "linux")]
        {
            // The helper takes the set as is; an empty one shows everything.
            let set: Vec<(u16, u16)> = targets.iter().map(|&(vid, pid, _)| (vid, pid)).collect();
            eprintln!("[linux-hide] hiding {} controller(s)", set.len());
            self.linux_hide.apply(&set);
        }

        // FlexInput's own exe stays whitelisted so it keeps reading the hidden pads.
        #[cfg(windows)]
        {
            let whitelist: Vec<String> = HidHideClient::current_exe_path().into_iter().collect();
            let sticky = self.settings.hidhide_sticky;
            let inst_cache = std::sync::Arc::clone(&self.hidhide_instance_cache);
            let kept_report = std::sync::Arc::clone(&self.hidhide_kept_report);
            std::thread::spawn(move || {
                // (vid,pid) → HID instance ids (slow SetupAPI; safe off the UI/IO thread).
                let resolved: Vec<(String, bool)> = targets
                    .iter()
                    .flat_map(|&(vid, pid, keep)| {
                        let mut ids = flexinput_devices::hidhide::instance_ids_to_hide(vid, pid);
                        // Remember every id we resolve; when sticky is on and the pad
                        // is currently absent, fall back to the remembered ones so the
                        // entries survive the gap instead of being unmasked.
                        if !ids.is_empty() {
                            if let Ok(mut c) = inst_cache.lock() {
                                c.insert((vid, pid), ids.clone());
                            }
                        } else if sticky {
                            ids = inst_cache.lock().ok()
                                .and_then(|c| c.get(&(vid, pid)).cloned())
                                .unwrap_or_default();
                        }
                        eprintln!(
                            "[hidhide] target {:04X}:{:04X} -> instances {:?} (keep on exit: {keep})",
                            vid, pid, ids
                        );
                        ids.into_iter().map(move |id| (id, keep))
                    })
                    .collect();
                let blacklist: Vec<String> = resolved.iter().map(|(id, _)| id.clone()).collect();
                let keep: Vec<String> = resolved.iter()
                    .filter(|(_, k)| *k)
                    .map(|(id, _)| id.clone())
                    .collect();
                match flexinput_hidmaestro::helper::hidhide_apply(
                    &blacklist, &whitelist, active, &keep, &release,
                ) {
                    Ok(st) if !st.present => eprintln!("[hidhide] driver not present at apply time"),
                    Ok(st) => {
                        eprintln!(
                            "[hidhide] applied: active={} hidden={} (requested {}, kept on exit {})",
                            st.active, st.hidden.len(), blacklist.len(), keep.len()
                        );
                        if let Ok(mut r) = kept_report.lock() {
                            *r = Some(keep);
                        }
                    }
                    Err(e) => eprintln!("[hidhide] apply failed: {e}"),
                }
            });
        }
    }

    /// Fold eye-button clicks into the overrides (forcing a reconcile), and pick
    /// up the keep list the last apply reported. Every frame, before the reconcile.
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) fn drain_hidhide_requests(&mut self) {
        use crate::hidhide_toggle::HideRequest;
        for (id, req) in crate::hidhide_toggle::drain_requests() {
            match req {
                HideRequest::Set(v) => { self.settings.hidhide_overrides.insert(id, v); }
                HideRequest::Auto => { self.settings.hidhide_overrides.remove(&id); }
            }
            self.settings_dirty = true;
            self.hidhide_dirty = true;
            self.allow_hide_prompt();
        }
        let reported = self.hidhide_kept_report.lock().ok().and_then(|mut r| r.take());
        if let Some(kept) = reported {
            if kept != self.settings.hidhide_kept_ids {
                self.settings.hidhide_kept_ids = kept;
                self.settings_dirty = true;
            }
        }
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    pub(crate) fn drain_hidhide_requests(&mut self) {}

    /// The user asked to hide or show something: on Linux a dismissed password
    /// prompt may come up again, and the set is re-sent even if it's the same.
    fn allow_hide_prompt(&mut self) {
        #[cfg(target_os = "linux")]
        {
            self.linux_hide.allow_prompt();
            self.hidhide_last_active = None;
        }
    }

    /// Publish every device's eye-button state. Every frame, after the reconcile:
    /// it only reads the cached auto set, never walks the patch.
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) fn publish_hidhide_ui(&mut self) {
        use crate::hidhide_toggle::HideUiState;
        let own_virtual = self.own_virtual_device_ids();
        let states: HashMap<String, HideUiState> = self.devices.iter()
            .filter(|d| !matches!(d.kind,
                flexinput_devices::ControllerKind::MidiIn
                | flexinput_devices::ControllerKind::MidiOut))
            .map(|d| {
                let blocked = self.hidhide_block_reason(d, &own_virtual);
                let (hidden, manual) = self.hidhide_effective(&d.id);
                (d.id.clone(), HideUiState { hidden: hidden && blocked.is_none(), manual, blocked })
            })
            .collect();
        crate::hidhide_toggle::publish(states);
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    pub(crate) fn publish_hidhide_ui(&mut self) {}

    /// Fold light-bar swatch edits into `settings.led_defaults` (mirrored for
    /// the I/O thread), and publish each lightbar pad's default for the swatches.
    pub(crate) fn sync_led_defaults(&mut self) {
        use crate::device_led::LedRequest;
        let reqs = crate::device_led::drain_requests();
        if !reqs.is_empty() {
            for (id, req) in reqs {
                match req {
                    LedRequest::Set(rgb) => { self.settings.led_defaults.insert(id, rgb); }
                    LedRequest::Clear => { self.settings.led_defaults.remove(&id); }
                }
            }
            self.settings_dirty = true;
            if let Ok(mut m) = self.led_defaults.write() {
                m.clone_from(&self.settings.led_defaults);
            }
        }
        let own_virtual = self.own_virtual_device_ids();
        let states: HashMap<String, Option<[u8; 3]>> = self.devices.iter()
            .filter(|d| matches!(d.kind,
                flexinput_devices::ControllerKind::DualShock4
                | flexinput_devices::ControllerKind::DualSense))
            .filter(|d| crate::app::graph::is_physical_pad_id(&d.id) && !own_virtual.contains(&d.id))
            .map(|d| (d.id.clone(), self.settings.led_defaults.get(&d.id).copied()))
            .collect();
        crate::device_led::publish(states);
    }

    /// Fold Learn edits made on raw-pad device nodes into
    /// `settings.raw_pad_profiles` (mirrored for the I/O thread), and publish
    /// the layouts with which connected raw device uses which.
    pub(crate) fn sync_raw_pad_profiles(&mut self) {
        use flexinput_core::raw_pad::{is_raw_device_id, profile_key};
        let edits = crate::raw_learn::drain_requests();
        let mut changed = !edits.is_empty()
            && crate::raw_learn::apply_edits(&mut self.settings.raw_pad_profiles, edits);
        // A raw pad nobody has taught yet starts from the community controller
        // database's layout for its model, where it has one (`sdl_db`) —
        // looked up once per model and session. A profile that exists is
        // never replaced, even an emptied one (`from_db` keeps it), so a
        // layout the user cleared on purpose isn't offered again.
        for d in self.devices.iter().filter(|d| is_raw_device_id(&d.id)) {
            let key = profile_key(d.vid, d.pid, &d.display_name);
            if self.settings.raw_pad_profiles.contains_key(&key) || !self.raw_db_tried.insert(key.clone()) {
                continue;
            }
            if let Some(mut layout) = flexinput_core::sdl_db::layout_for(d.vid, d.pid, &d.display_name) {
                eprintln!("[raw-pad] {}: starting from SDL_GameControllerDB's \"{}\" layout ({} binds)",
                    d.id, layout.name, layout.binds.len());
                layout.name = d.display_name.clone();
                self.settings.raw_pad_profiles.insert(key, layout);
                changed = true;
            }
        }
        if changed {
            self.settings_dirty = true;
            if let Ok(mut m) = self.raw_pad_profiles.write() {
                *m = Arc::new(self.settings.raw_pad_profiles.clone());
            }
        }
        let keys: HashMap<String, String> = self.devices.iter()
            .filter(|d| is_raw_device_id(&d.id))
            .map(|d| (d.id.clone(), profile_key(d.vid, d.pid, &d.display_name)))
            .collect();
        let profiles = self.raw_pad_profiles.read().map(|m| Arc::clone(&m)).unwrap_or_default();
        crate::raw_learn::publish(profiles, keys);
    }

    /// `(hidden, manual)` for a device: its eye-button override when it has one,
    /// else whether the auto rule ("Hide original controllers" + remapped) hides it.
    #[cfg(any(windows, target_os = "linux"))]
    fn hidhide_effective(&self, device_id: &str) -> (bool, bool) {
        match self.settings.hidhide_overrides.get(device_id) {
            Some(&v) => (v, true),
            None => (self.hidhide_auto_ids.contains(device_id), false),
        }
    }

    /// Why a device can't be hidden on Linux, or `None` when it can. FlexInput
    /// keeps reading a hidden pad through private nodes its gilrs, hidraw and
    /// SDL readers fall back to (SDL's through the wrapped `open`, see
    /// `linux_hide::open_or_private`); the Joy-Con 2 transport doesn't.
    #[cfg(target_os = "linux")]
    fn hidhide_block_reason(
        &self,
        dev: &PhysicalDevice,
        own_virtual: &HashSet<String>,
    ) -> Option<&'static str> {
        use flexinput_devices::ControllerKind as K;
        if !self.hidhide_installed {
            return Some("Hiding needs pkexec (polkit), which isn't installed.");
        }
        if own_virtual.contains(&dev.id) {
            return Some("This is FlexInput's own virtual controller.");
        }
        match dev.kind {
            K::JoyCon2L | K::JoyCon2R => {
                Some("FlexInput reads Joy-Con 2 directly; hiding it would cut FlexInput off too.")
            }
            _ if dev.vid.is_none() || dev.pid.is_none() => {
                Some("This controller reports no USB vendor/product id to hide it by.")
            }
            _ => None,
        }
    }

    /// Why a device can't be hidden at all, or `None` when it can.
    #[cfg(windows)]
    fn hidhide_block_reason(
        &self,
        dev: &PhysicalDevice,
        own_virtual: &HashSet<String>,
    ) -> Option<&'static str> {
        use flexinput_devices::ControllerKind as K;
        if !self.hidhide_installed {
            return Some("The HidHide driver isn't installed (nefarius/HidHide).");
        }
        if own_virtual.contains(&dev.id) {
            return Some("This is FlexInput's own virtual controller.");
        }
        match dev.kind {
            K::XInput => Some("Xbox controllers can't be hidden: HidHide can't mask their XInput face."),
            // Joy-Con 2 is excluded BY KIND, not by device-id prefix: over USB
            // (charging grip) it DOES get a HID node, Windows binds it as a plain
            // gamepad, and it reaches us as `sdl:`/`gilrs:` — past the prefix gate
            // in `remapped_physical_ids`. Masking it is actively harmful, because
            // our own hidapi transport (`joycon2::usb`) is what reads it, and
            // HidHide's per-app whitelist is unreliable here: the pad enumerates,
            // opens, then streams nothing at all. Nothing is gained either — the
            // vendor protocol lives on interface 1, which no game reads, so there
            // is no phantom gamepad to suppress.
            K::JoyCon2L | K::JoyCon2R => {
                Some("FlexInput reads Joy-Con 2 directly; hiding it would cut FlexInput off too.")
            }
            _ if dev.vid.is_none() || dev.pid.is_none() => {
                Some("This controller reports no USB vendor/product id to hide it by.")
            }
            _ => None,
        }
    }

    /// (vid, pid, keep-after-exit) of every controller to hide right now: each
    /// live device that can be hidden and is (by override or the auto rule),
    /// plus — with sticky masking on — remembered devices that are briefly
    /// absent but still wanted hidden. Sorted and deduped by (vid, pid).
    #[cfg(any(windows, target_os = "linux"))]
    fn hidhide_targets(&self) -> Vec<(u16, u16, bool)> {
        let own_virtual = self.own_virtual_device_ids();
        // Linux keeps nothing hidden past exit: its helper gives every node back.
        let keep_mode = if cfg!(windows) {
            self.settings.hidhide_keep_on_exit
        } else {
            crate::settings::HidHideKeepOnExit::Never
        };
        let mut out: Vec<(u16, u16, bool)> = Vec::new();
        let mut live: HashSet<&str> = HashSet::new();
        for dev in &self.devices {
            live.insert(dev.id.as_str());
            if matches!(dev.kind,
                flexinput_devices::ControllerKind::MidiIn
                | flexinput_devices::ControllerKind::MidiOut)
            {
                continue;
            }
            if self.hidhide_block_reason(dev, &own_virtual).is_some() {
                continue;
            }
            let (hidden, manual) = self.hidhide_effective(&dev.id);
            if let (true, Some(vid), Some(pid)) = (hidden, dev.vid, dev.pid) {
                out.push((vid, pid, keep_mode.keeps(manual)));
            }
        }
        // Sticky masking: a pad that isn't in the live list right now (Bluetooth
        // dropout, or a reconnect that arrives already blacklisted and so never
        // reaches our enumeration) keeps its mask, resolved from the session
        // vid/pid cache. Without this the mask is dropped the instant the pad
        // blips out — which UNHIDES it system-wide, handing a running game the
        // physical pad alongside the virtual one until the pad returns and we
        // re-hide it. The patch wiring / override is still the source of truth:
        // a device unmapped (or shown) drops out here and is unmasked normally.
        // The cache only ever holds maskable pads (see `reconcile_hidhide`).
        if self.settings.hidhide_sticky {
            for (id, &(vid, pid)) in &self.hidhide_vidpid_cache {
                if live.contains(id.as_str()) {
                    continue;
                }
                let (hidden, manual) = self.hidhide_effective(id);
                if hidden {
                    out.push((vid, pid, keep_mode.keeps(manual)));
                }
            }
        }
        // One entry per (vid, pid); kept if any device behind it is.
        out.sort_unstable();
        out.dedup_by(|b, a| {
            let same = (a.0, a.1) == (b.0, b.1);
            if same { a.2 |= b.2; }
            same
        });
        out
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    pub(crate) fn reconcile_hidhide(&mut self) {}

    /// Ids of the physical controllers in the active patch that feed a
    /// `virtual.*` output — what "Hide original controllers" hides. Whether each
    /// can actually be hidden is `hidhide_block_reason`'s call.
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) fn remapped_physical_ids(&self) -> HashSet<String> {
        let snarl = &self.tabs[self.active_tab].canvas.snarl;
        // Physical device ids that feed a virtual.* sink (via an AutoMap wire).
        let mut phys_ids: HashSet<String> = HashSet::new();
        for (node_id, node_ref) in snarl.nodes_ids_data() {
            let node = &node_ref.value;
            if node.module_id != "device.sink" {
                continue;
            }
            let sink_dev = node.params.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
            if !sink_dev.starts_with("virtual.") {
                continue;
            }
            for i in 0..node.inputs.len() {
                if node.inputs.get(i).map(|p| p.signal_type) != Some(SignalType::AutoMap) {
                    continue;
                }
                let in_pin = snarl.in_pin(InPinId { node: node_id, input: i });
                for &src in &in_pin.remotes {
                    // Every device behind the wire: a Combiner merging two pads
                    // into one virtual sink remaps BOTH, so both are hidden — the
                    // single-device resolver only ever named one of them.
                    for dev_id in crate::app::find_automap_device_ids_for_viewer(snarl, src, None) {
                        // Both backend prefixes are real physical pads that need
                        // cloaking: `gilrs:` (native path) and `sdl:` (the
                        // route-all-through-SDL switch). Without `sdl:` here, a
                        // pad read through SDL was never blacklisted, so HidHide
                        // silently did nothing in SDL mode and Steam kept seeing
                        // the physical device. The vid/pid → instance-id lookup
                        // downstream already handles both USB and Bluetooth.
                        // NOT `jc2:` — deliberately. Joy-Con 2 speaks a vendor
                        // BLE GATT protocol with no HID-over-GATT service, so
                        // Windows binds no driver and creates no HID node.
                        // There is no phantom gamepad for other apps to see and
                        // nothing for HidHide to act on; adding the prefix here
                        // would only send it hunting for an instance path that
                        // does not exist.
                        if dev_id.starts_with("gilrs:") || dev_id.starts_with("sdl:") {
                            phys_ids.insert(dev_id);
                        }
                    }
                }
            }
        }
        phys_ids
    }

    /// Render the Settings modal. Reads/writes `self.settings`, mirrors live
    /// values into the engine/I-O atomics, and flips `settings_dirty` so the
    /// outer update loop persists settings.json at end of frame.
    ///
    /// Layout: a jump list of [`SettingsSection`]s on the left, every section
    /// stacked on one scrolling page on the right. Clicking an entry scrolls its
    /// section to the top of the page; scrolling the page highlights whichever
    /// section is at the top, so the list doubles as a "you are here" marker.
    pub(crate) fn draw_settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            // Closing Settings discards an un-applied UI-scale pick.
            self.ui_scale_pending = None;
            return;
        }
        let mut open = true;
        let mut dirty = false;
        let mut save_workspace = false;

        let nav_id = egui::Id::new("settings_window_nav");
        let mut nav: SettingsNav = ctx.data(|d| d.get_temp(nav_id)).unwrap_or_default();
        let sections = SettingsSection::visible();
        let now = ctx.input(|i| i.time);
        // HidHide is Windows-only; Linux has nothing to install.
        let hidhide_missing = cfg!(windows) && !self.hidhide_installed;

        egui::Window::new("Settings")
            .id(egui::Id::new("settings_window"))
            .collapsible(false)
            .resizable(true)
            .default_size([720.0, 560.0])
            .min_size([520.0, 300.0])
            .open(&mut open)
            .show(ctx, |ui| {
                // Width of the jump list. Points, so it tracks UI scale.
                const NAV_W: f32 = 150.0;
                let avail_h = ui.available_height();
                let mut jump: Option<SettingsSection> = None;
                ui.horizontal_top(|ui| {
                    // Both columns ask for `top_down` explicitly: a child ui built
                    // inside a horizontal layout would otherwise inherit it.
                    ui.allocate_ui_with_layout(
                        egui::vec2(NAV_W, avail_h),
                        egui::Layout::top_down_justified(egui::Align::Min),
                        |ui| {
                            ui.set_width(NAV_W);
                            ui.label(egui::RichText::new("FlexInput").strong());
                            ui.label(egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                                .small().color(crate::settings::tinted_gray(150)));
                            ui.add_space(6.0);
                            // A section clicked a moment ago stays lit while the
                            // page scrolls there, instead of the highlight
                            // sweeping through every section in between.
                            let lit = nav.pending.map(|(s, _)| s).unwrap_or(nav.active);
                            egui::ScrollArea::vertical()
                                .id_salt("settings_nav")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    for &s in &sections {
                                        let flagged = s == SettingsSection::HidHide && hidhide_missing;
                                        let mut text = egui::RichText::new(
                                            if flagged { format!("{}  ⚠", s.label()) } else { s.label().to_string() })
                                            .size(crate::settings::ui_size(13.0));
                                        if flagged { text = text.color(egui::Color32::from_rgb(230, 170, 80)); }
                                        let resp = ui.selectable_label(lit == s, text);
                                        let resp = if flagged {
                                            resp.on_hover_text("HidHide is not installed")
                                        } else { resp };
                                        if resp.clicked() {
                                            jump = Some(s);
                                        }
                                    }
                                });
                        },
                    );
                    ui.separator();

                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), avail_h),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("settings_page")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    form_page(ui.available_width());
                                    let view = ui.clip_rect();
                                    let mut at_top = sections[0];
                                    let mut last_top = view.top();
                                    for (i, &s) in sections.iter().enumerate() {
                                        if i > 0 { ui.add_space(2.4 * FORM_GAP); }
                                        let top = ui.cursor().top();
                                        let header = settings_section_header(ui, s.label());
                                        if jump == Some(s) {
                                            header.scroll_to_me(Some(egui::Align::TOP));
                                        }
                                        // The section owning the top of the view is
                                        // the last one whose header has reached it.
                                        if top <= view.top() + 30.0 { at_top = s; }
                                        last_top = top;
                                        self.settings_section(ui, s, &mut dirty, &mut save_workspace);
                                    }
                                    // Room after the last section so a jump to it can
                                    // still bring its header to the top of the view.
                                    let tail = view.height() - (ui.cursor().top() - last_top);
                                    if tail > 0.0 { ui.add_space(tail); }
                                    nav.active = at_top;
                                });
                        },
                    );
                });
                if let Some(s) = jump {
                    nav.pending = Some((s, now));
                }
            });

        // The pin on a clicked section lifts once the page has arrived there, or
        // after the scroll animation has had ample time to.
        if let Some((s, t)) = nav.pending {
            if nav.active == s || now - t > 1.0 {
                nav.pending = None;
            }
        }
        ctx.data_mut(|d| d.insert_temp(nav_id, nav));

        if dirty { self.settings_dirty = true; }
        if save_workspace { self.save_workspace_now(); }
        if !open { self.settings_open = false; }
    }

    /// Body of one Settings section, below its header.
    fn settings_section(
        &mut self, ui: &mut egui::Ui, section: SettingsSection,
        dirty: &mut bool, save_workspace: &mut bool,
    ) {
        use SettingsSection as S;
        match section {
            S::Display           => self.settings_display(ui, dirty),
            S::Workspace         => self.settings_workspace(ui, dirty, save_workspace),
            S::GamepadNav        => self.settings_gamepad_nav(ui, dirty),
            S::GamepadShortcuts  => self.settings_gamepad_shortcuts(ui, dirty),
            S::KeyboardShortcuts => self.settings_keyboard_shortcuts(ui, dirty),
            S::PinOverlays       => self.settings_pin_overlays(ui, dirty),
            S::Controllers       => self.settings_controllers(ui, dirty),
            S::HidHide           => self.settings_hidhide(ui, dirty),
            S::VirtualOutput     => self.settings_virtual_output(ui, dirty),
            S::DeviceDefaults    => self.settings_device_defaults(ui, dirty),
            S::Performance       => self.settings_performance(ui, dirty),
            S::Midi              => self.settings_midi(ui),
            S::Folders           => self.settings_folders(ui, dirty),
            S::Developer         => {
                #[cfg(debug_assertions)]
                self.settings_developer(ui);
            }
            S::About             => self.settings_about(ui),
        }
    }

    fn settings_display(&mut self, ui: &mut egui::Ui, dirty: &mut bool) {
        let ctx = ui.ctx().clone();
        form_subheading(ui, "Appearance");
        // Theme switching is not yet plumbed through every custom-
        // painted UI element (canvas viewer overlays, calibration
        // scope backgrounds, response-curve grid, etc.), so the
        // app is dark-only for now. The contrast slider still
        // works on the dark theme.
        form_row(ui, "Contrast", "", |ui| {
            let mut c = self.settings.contrast;
            let resp = ui.add(egui::Slider::new(&mut c, -1.0_f32..=1.0)
                .show_value(false)
                .clamping(egui::SliderClamping::Always));
            if slider_double_clicked(&resp) { c = settings::CONTRAST_DEFAULT; }
            form_value(ui, egui::DragValue::new(&mut c)
                .speed(0.01)
                .range(-1.0_f32..=1.0)
                .fixed_decimals(2));
            if (c - self.settings.contrast).abs() > f32::EPSILON {
                self.settings.contrast = c.clamp(-1.0, 1.0);
                *dirty = true;
            }
        });
        form_note(ui, "Adjusts panel/widget background lightness. Negative = darker, positive = lighter. Double-click the slider to reset to 0.50.");

        // The accent, and a tint the greys lean toward.
        form_row(ui, "Accent", "The colour of what is selected, lit or on: switches, chosen segments, the pills' name cells.", |ui| {
            let [r, g, b] = self.settings.accent_color;
            let mut c = egui::Color32::from_rgb(r, g, b);
            if egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::Opaque).changed() {
                self.settings.accent_color = [c.r(), c.g(), c.b()];
                *dirty = true;
            }
            if ui.add_enabled(self.settings.accent_color != settings::ACCENT_DEFAULT, egui::Button::new("Reset")).clicked() {
                self.settings.accent_color = settings::ACCENT_DEFAULT;
                *dirty = true;
            }
        });
        form_row(ui, "Grey tint", "A colour the greys of panels and widgets lean toward, keeping their lightness.", |ui| {
            let [r, g, b] = self.settings.gray_tint;
            let mut c = egui::Color32::from_rgb(r, g, b);
            if egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::Opaque).changed() {
                self.settings.gray_tint = [c.r(), c.g(), c.b()];
                *dirty = true;
            }
            // The slider gives the swatch its room, so the row ends where the others do.
            let swatch = ui.spacing().interact_size.x + ui.spacing().item_spacing.x;
            ui.spacing_mut().slider_width = (ui.spacing().slider_width - swatch).max(60.0);
            let mut amt = self.settings.gray_tint_amount * 100.0;
            let resp = ui.add(egui::Slider::new(&mut amt, 0.0_f32..=100.0).show_value(false).clamping(egui::SliderClamping::Always));
            if slider_double_clicked(&resp) { amt = 0.0; }
            form_value(ui, egui::DragValue::new(&mut amt).speed(0.5).range(0.0_f32..=100.0).suffix("%").fixed_decimals(0));
            if (amt / 100.0 - self.settings.gray_tint_amount).abs() > f32::EPSILON {
                self.settings.gray_tint_amount = (amt / 100.0).clamp(0.0, 1.0);
                *dirty = true;
            }
        });
        form_note(ui, "How strongly the greys take the tint: 0% = plain grey. Double-click the slider to turn it off.");

        form_subheading(ui, "Text and scale");
        // The controls' font: one for every pill, switch, segment and the buttons and
        // dropdowns beside them.
        form_row(ui, "Control font", "The font of the controls modules draw: value pills, switches, segments, and the buttons and dropdowns beside them.", |ui| {
            let mut mono = self.settings.control_font_mono;
            ui.selectable_value(&mut mono, true, "Console");
            ui.selectable_value(&mut mono, false, "Normal");
            let mut bold = self.settings.control_font_bold;
            ui.separator();
            ui.selectable_value(&mut bold, false, "Regular");
            ui.selectable_value(&mut bold, true, "Bold");
            if mono != self.settings.control_font_mono || bold != self.settings.control_font_bold {
                self.settings.control_font_mono = mono;
                self.settings.control_font_bold = bold;
                *dirty = true;
            }
        });
        form_row(ui, "Control font size", "", |ui| {
            // Applied when the drag ends (or the typing does): applied as it moved, every
            // step resized the text beside the slider and moved the slider under the pointer.
            let pending_id = egui::Id::new("control_font_size_pending");
            let mut size = ui.ctx().data(|d| d.get_temp::<f32>(pending_id)).unwrap_or(self.settings.control_font_size);
            let range = settings::CONTROL_FONT_SIZE_MIN..=settings::CONTROL_FONT_SIZE_MAX;
            let resp = ui.add(egui::Slider::new(&mut size, range.clone()).step_by(0.5).show_value(false).clamping(egui::SliderClamping::Always));
            if slider_double_clicked(&resp) { size = settings::CONTROL_FONT_SIZE_DEFAULT; }
            let dv = form_value(ui, egui::DragValue::new(&mut size).speed(0.1).range(range).suffix(" pt").fixed_decimals(1));
            if resp.dragged() || dv.dragged() || dv.has_focus() {
                ui.ctx().data_mut(|d| d.insert_temp(pending_id, size));
            } else {
                ui.ctx().data_mut(|d| d.remove::<f32>(pending_id));
                if (size - self.settings.control_font_size).abs() > f32::EPSILON {
                    self.settings.control_font_size = size.clamp(settings::CONTROL_FONT_SIZE_MIN, settings::CONTROL_FONT_SIZE_MAX);
                    *dirty = true;
                }
            }
        });
        form_note(ui, "Every button, dropdown, pill and switch, and a module's words beside them - section titles, captions, labels. The size is the normal font's: console and bold are drawn as wide as normal text at that size, so the controls keep their room. A pinned widget can keep its text from growing past a size, and size its graph's labels, in the Inspector (Text). It applies when you let go of the slider. Double-click the slider to return to 15 pt.");
        form_row(ui, "UI scale", "Scales the whole interface, on top of the Windows display scale. Text is re-rendered at the new size, so it stays sharp.", |ui| {
            // Stepped like the polling-rate slider: the slider drives an
            // index into UI_SCALE_STEPS, so the handle snaps while
            // dragging. The pick stays PENDING until Apply — rescaling
            // live would move the slider out from under the pointer.
            let applied = self.settings.ui_scale;
            let last = settings::UI_SCALE_STEPS.len() - 1;
            let mut idx = settings::ui_scale_to_index(
                self.ui_scale_pending.unwrap_or(applied)) as i64;
            let resp = ui.add(egui::Slider::new(&mut idx, 0..=last as i64)
                .integer()
                .show_value(false));
            let picked = if slider_double_clicked(&resp) {
                settings::UI_SCALE_DEFAULT
            } else {
                settings::ui_scale_from_index(idx as usize)
            };
            self.ui_scale_pending = (picked != applied).then_some(picked);
            // As wide as a value box, so Apply doesn't shift as the digits change.
            ui.add_sized([FORM_VALUE_W, ui.spacing().interact_size.y],
                egui::Label::new(format!("{:.0}%", picked * 100.0)));
            if ui.add_enabled(picked != applied, egui::Button::new("Apply")).clicked() {
                self.settings.ui_scale = picked;
                ctx.set_zoom_factor(picked);
                self.ui_scale_pending = None;
                *dirty = true;
            }
        });
        form_note(ui, "50%–300%. Takes effect when you press Apply. Double-click the slider to return to 100%.");

        form_subheading(ui, "Renderer");
        // Backend is fixed at startup (the wgpu instance/surface can't
        // be swapped live), so changes here take effect on restart.
        // Auto leads with DirectX 12 on Windows (see `auto_cascade` in
        // app/src/main.rs).
        form_row(ui, "Graphics API", "", |ui| {
            let mut choice = self.settings.renderer;
            egui::ComboBox::from_id_salt("renderer_choice")
                .selected_text(choice.label())
                .show_ui(ui, |ui| {
                    let mut choices = vec![
                        settings::RendererChoice::Auto,
                        settings::RendererChoice::Vulkan,
                        settings::RendererChoice::OpenGl,
                    ];
                    if cfg!(windows) {
                        choices.push(settings::RendererChoice::Dx12);
                    }
                    for c in choices {
                        ui.selectable_value(&mut choice, c, c.label());
                    }
                });
            if choice != self.settings.renderer {
                self.settings.renderer = choice;
                *dirty = true;
            }
        });
        form_note(ui,
            "Takes effect after restarting FlexInput. On Windows, Auto \
             uses DirectX 12, falling back to Vulkan and then OpenGL only \
             if it fails to start — Vulkan drivers on Windows have proved \
             unreliable across versions and devices. On AMD, see-through \
             mode needs DirectX 12 — the AMD Vulkan driver doesn't support \
             transparent windows and its OpenGL path crashes on wake from \
             sleep.");
    }

    fn settings_workspace(&mut self, ui: &mut egui::Ui, dirty: &mut bool, save_workspace: &mut bool) {
        let resp = form_check(ui, "On launch", true, &mut self.settings.keep_workspace,
            "Keep open tabs", "");
        if resp.changed() {
            *dirty = true;
            if self.settings.keep_workspace {
                *save_workspace = true;
            } else {
                settings::delete_workspace();
            }
        }
        form_note(ui, "When enabled, the current tabs (including unsaved patches) are restored on the next launch.");

        if form_check(ui, "New device nodes", true, &mut self.settings.device_nodes_default_collapsed,
            "Start collapsed", "").changed()
        {
            *dirty = true;
        }
        form_note(ui, "New physical / virtual device nodes spawn collapsed. The header (icon, status, Auto-Map) stays visible.");
        form_row(ui, "On patch load", "", |ui| {
            let cur = self.settings.on_patch_load;
            let label = match cur {
                settings::OnPatchLoad::Off         => "Do nothing",
                settings::OnPatchLoad::Center      => "Center on patch",
                settings::OnPatchLoad::ZoomToFit   => "Zoom to fit",
            };
            egui::ComboBox::from_id_salt("on_patch_load_combo")
                .selected_text(label)
                .show_ui(ui, |ui| {
                    let mut sel = cur;
                    ui.selectable_value(&mut sel, settings::OnPatchLoad::Off,
                        "Do nothing");
                    ui.selectable_value(&mut sel, settings::OnPatchLoad::Center,
                        "Center on patch");
                    ui.selectable_value(&mut sel, settings::OnPatchLoad::ZoomToFit,
                        "Zoom to fit");
                    if sel != cur {
                        self.settings.on_patch_load = sel;
                        *dirty = true;
                    }
                });
        });
        form_note(ui, "What the view does when a .fxp is loaded into a tab. Do nothing keeps the current view.");
    }

    fn settings_gamepad_nav(&mut self, ui: &mut egui::Ui, dirty: &mut bool) {
        if form_check(ui, "New gamepads", true, &mut self.settings.gamepad_ui_nav_default,
            "Start in UI navigation",
            "When on, newly-seen gamepads start in UI-navigation mode: while \
             FlexInput holds focus the controller drives FlexInput's own UI and \
             its mapped output is suppressed. Alt-tab away and mappings resume \
             automatically. Toggle per-device on each gamepad card.")
            .changed()
        {
            *dirty = true;
        }
        form_row(ui, "Cursor max speed", "Top speed of the right-stick/gyro nav cursor at full deflection (px/s). The actual speed follows the acceleration curve below up to this cap.", |ui| {
            let resp = ui.add(egui::Slider::new(
                &mut self.settings.cursor_max_speed, 1000.0..=30000.0)
                .suffix(" px/s"));
            if resp.changed() { *dirty = true; }
        });
        form_row(ui, "Cursor acceleration", "How the cursor speed ramps with stick deflection. 1.0 = linear; higher = slower start and a faster top end (deflection raised to this power).", |ui| {
            let mut a = self.settings.cursor_accel;
            let resp = ui.add(egui::Slider::new(&mut a, 1.0..=4.0)
                .fixed_decimals(2));
            if slider_double_clicked(&resp) { a = 2.0; }
            if (a - self.settings.cursor_accel).abs() > f32::EPSILON {
                self.settings.cursor_accel = a.clamp(1.0, 4.0);
                *dirty = true;
            }
        });
        form_note(ui, "Double-click the slider to return to 2.00.");
    }

    fn settings_gamepad_shortcuts(&mut self, ui: &mut egui::Ui, dirty: &mut bool) {
        // ── Gamepad shortcuts ───────────────────────────────────
        // Assign a gamepad button combo to toggle see-through / panic.
        // Learned by clicking Learn then pressing+releasing a combo.
        use crate::gamepad_nav::ChordTarget as T;
        for (name, target) in [
            ("See-through", T::SeeThrough),
            ("Panic mode", T::Panic),
            ("Overlay", T::Overlay),
            ("Config overlay", T::ConfigOverlay),
            ("Pin (always-on-top)", T::Pin),
            ("Previous patch tab", T::PrevTab),
            ("Next patch tab", T::NextTab),
        ] {
            if self.gamepad_shortcut_row(ui, name, target) {
                *dirty = true;
            }
        }
        form_subheading(ui, "Layout tabs");
        form_group_note(ui,
            "Read by FlexInput's own gamepad navigation, on press, in the patch shown: a layout \
             without tabs leaves them alone.");
        for (name, target) in [("Previous layout tab", T::LayoutTabPrev), ("Next layout tab", T::LayoutTabNext)] {
            if self.gamepad_shortcut_row(ui, name, target) {
                *dirty = true;
            }
        }
        form_subheading(ui, "Behavior");
        if form_check(ui, "When", true, &mut self.settings.gamepad_chords_nav_only,
            "Only in gamepad navigation mode",
            "On: these combos fire only while the driving gamepad is in UI-navigation mode \
             (so the same buttons stay free for in-game mappings otherwise).\n\
             Off: they fire from any connected gamepad whenever FlexInput is focused.")
            .changed()
        {
            *dirty = true;
        }
        if form_check(ui, "Home button", true, &mut self.settings.gamepad_home_exclusive,
            "FlexInput's only",
            "Applies when Home/Guide/PS is part of a shortcut.\n\
             On: the game never sees Home.\n\
             Off: Home is held back while it might still become a shortcut; \
             if no shortcut used it, the game gets a Home tap when you let go.")
            .changed()
        {
            *dirty = true;
        }
    }

    fn settings_keyboard_shortcuts(&mut self, ui: &mut egui::Ui, dirty: &mut bool) {
        form_group_note(ui, "System-wide hotkeys: they work even while another app (a game) has focus. Click one, then press the new modifier + key.");
        if key_shortcut_row(ui, "Pin (always-on-top)", "toggle the pin",
            &mut self.pin_learning, &mut self.settings.pin_shortcut, &self.pin_shortcut_shared)
        {
            *dirty = true;
        }
        // Show info-overlay shortcut (mirrors the pin binder above).
        if key_shortcut_row(ui, "Show info overlay", "show/hide the overlay",
            &mut self.overlay_learning, &mut self.settings.overlay_shortcut, &self.overlay_shortcut_shared)
        {
            *dirty = true;
        }
        // Edit info-overlay shortcut (mirrors the show binder; toggles
        // the overlay's layout-edit mode and shows it).
        if key_shortcut_row(ui, "Edit info overlay", "enter/exit the overlay's edit mode",
            &mut self.edit_overlay_learning, &mut self.settings.edit_overlay_shortcut, &self.edit_overlay_shortcut_shared)
        {
            *dirty = true;
        }
        // Config-overlay toggle shortcut (M3; mirrors the overlay binder).
        if key_shortcut_row(ui, "Show config overlay", "show/hide the config overlay",
            &mut self.config_overlay_learning, &mut self.settings.config_overlay_shortcut, &self.config_overlay_shortcut_shared)
        {
            *dirty = true;
        }
    }

    fn settings_pin_overlays(&mut self, ui: &mut egui::Ui, dirty: &mut bool) {
        if form_check(ui, "Pin toggle", true, &mut self.settings.focus_flip_flop,
            "Flip-flop focus",
            "Pin ON: remember the previously-focused window.\n\
             Pin OFF: return focus to it.\n\
             Lets you press the shortcut, tweak, press again, and instantly \
             resume testing the target app.")
            .changed()
        {
            *dirty = true;
        }
        form_row(ui, "Overlay frame rate", "Repaint rate of the overlay while it's visible. Separate from the background repaint rate — the overlay animates on top of your game.", |ui| {
            let resp = ui.add(egui::Slider::new(
                &mut self.settings.overlay_fps,
                settings::OVERLAY_FPS_MIN..=settings::OVERLAY_FPS_MAX,
            ).suffix(" FPS"));
            if resp.changed() {
                *dirty = true;
            }
        });
        form_note(ui, "How smoothly pinned elements animate on the overlay. Higher = smoother glow, a bit more CPU/GPU while the overlay is shown.");
    }

    fn settings_controllers(&mut self, ui: &mut egui::Ui, dirty: &mut bool) {
        if form_check(ui, "Reading", true, &mut self.settings.sdl_all_pads, "Route all pads through SDL",
            "Diagnostic: read EVERY controller through SDL instead of the native gilrs/HID paths. Lets a pad with a native parser be compared against SDL, and surfaces SDL-only devices. Changes device IDs, so re-wire after toggling. Off = normal native handling. A Steam Controller keeps its own reader either way — SDL's driver for it stalls the pad — and that reader names its controls; only a pad read through SDL is named by SDL's Misc slots.")
            .changed()
        {
            self.sdl_all_pads.store(self.settings.sdl_all_pads, Ordering::Relaxed);
            *dirty = true;
        }
        form_note(ui, "A diagnostic: it changes device IDs, so re-wire after turning it on or off.");
        if form_check(ui, "Physical devices", true, &mut self.settings.show_own_virtuals_as_physical,
            "List FlexInput's own virtual controllers too", "").changed()
        {
            *dirty = true;
        }
        form_note(ui, "Off by default. Turn on to test patches against your own virtual output (loopback).");
    }

    /// Warning box at the top of "Hide controllers" while the HidHide driver
    /// is missing: what that costs, where to get it, and a re-probe so a fresh
    /// install is picked up without restarting FlexInput.
    fn hidhide_missing_banner(&mut self, ui: &mut egui::Ui) {
        let amber = FORM_WARN;
        egui::Frame::new()
            .fill(egui::Color32::from_rgba_unmultiplied(230, 170, 80, 28))
            .stroke(egui::Stroke::new(1.0_f32, amber))
            .corner_radius(4.0)
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(egui::RichText::new("⚠ HidHide is not installed").strong().color(amber));
                ui.label(egui::RichText::new(
                    "Without it FlexInput can't hide the physical controller you remap, so \
                     games may see both it and the virtual one and read double input. The \
                     options below stay unavailable until it's installed.",
                ).size(crate::settings::ui_size(FORM_NOTE_PT)));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.hyperlink_to("Download HidHide",
                        "https://github.com/nefarius/HidHide/releases/latest")
                        .on_hover_text("nefarius/HidHide on GitHub — run the installer, then \
                                        reboot if it asks you to.");
                    ui.separator();
                    if ui.button("Check again")
                        .on_hover_text("Look for the HidHide driver again, e.g. after installing it.")
                        .clicked()
                    {
                        // Transient open, same as startup: holding the control
                        // device would lock the elevated helper out of it.
                        self.hidhide_installed = HidHideClient::try_open().is_some();
                        self.hidhide_dirty = true;
                    }
                });
            });
        form_gap(ui);
    }

    fn settings_hidhide(&mut self, ui: &mut egui::Ui, dirty: &mut bool) {
        if !self.hidhide_installed {
            if cfg!(windows) {
                self.hidhide_missing_banner(ui);
            } else {
                form_group_line(ui, egui::RichText::new(
                    "Hiding controllers needs pkexec (polkit), which this system doesn't have.",
                ).color(FORM_WARN));
            }
        }
        let hidhide_installed = self.hidhide_installed;
        // Effective value: explicit user choice, else default ON when the
        // HidHide driver is installed (the requested default), OFF otherwise.
        let mut hide_effective = self.settings.hide_originals.unwrap_or(hidhide_installed);
        let resp = form_check(ui, "Remapped controllers", hidhide_installed, &mut hide_effective,
            if cfg!(windows) {
                "Hide them from games (HidHide)"
            } else {
                "Hide them from games"
            },
            "");
        if resp.changed() {
            self.settings.hide_originals = Some(hide_effective);
            *dirty = true;
            self.hidhide_dirty = true; // apply on the next frame's reconcile
            self.allow_hide_prompt();
        }
        if hidhide_installed && !cfg!(windows) {
            form_note(ui,
                "On by default. When on, any physical controller you remap to a virtual \
                 output is hidden from games, so the game sees only the virtual pad, while \
                 FlexInput keeps reading the original. Hiding asks for your password once \
                 per session. Start FlexInput before the game: a game that already has the \
                 controller open keeps it. The eye button on a device card overrides this per \
                 controller (right-click it to go back to automatic). Joy-Con 2 can't be \
                 hidden.");
        }
        if hidhide_installed && cfg!(windows) {
            form_note(ui,
                "Defaults ON because HidHide is installed. When on, any physical controller \
                 you remap to a virtual output is hidden from games, so the game sees only \
                 the virtual pad. FlexInput stays whitelisted so it still reads the original. \
                 The eye button on a device card overrides this per controller (right-click \
                 it to go back to automatic). \u{2014} Note: only HID controllers \
                 (DualShock 4 / DualSense / Switch) can be hidden; the XInput/XUSB face of \
                 Xbox controllers cannot.");
        }

        // ── Sticky masking (rides on the toggle above) ───────────────
        let resp = form_check(ui, "Disconnects", hidhide_installed && hide_effective,
            &mut self.settings.hidhide_sticky, "Keep hiding through them", "");
        if resp.changed() {
            *dirty = true;
            self.hidhide_dirty = true; // re-apply with the new policy
        }
        form_note(ui,
            "On (default): a controller still mapped in your patch stays hidden across the gap, \
             and is masked the instant it reconnects. Unmapping it, turning the option above \
             off, or closing FlexInput still unhides it.\n\
             Off: the mask is rebuilt from the controllers connected right now, so a pad that \
             briefly drops out \u{2014} a Bluetooth gap, or a reconnect that arrives already \
             hidden \u{2014} is UNHIDDEN for every app until it comes back. With a game running \
             that hands it the physical pad alongside the virtual one, which shows up as button \
             glyphs flickering between controller and keyboard.");

        // ── What stays hidden after FlexInput exits ──────────────────
        // Linux's helper gives everything back when FlexInput closes.
        if !cfg!(windows) {
            form_row(ui, "After FlexInput exits", "", |ui| {
                ui.label("Everything it hid is shown again.");
            });
            return;
        }
        ui.add_enabled_ui(hidhide_installed, |ui| {
            form_row(ui, "After FlexInput exits", "Which controllers stay hidden once FlexInput has closed", |ui| {
                let cur = self.settings.hidhide_keep_on_exit;
                egui::ComboBox::from_id_salt("hidhide_keep_on_exit")
                    .selected_text(cur.label())
                    .show_ui(ui, |ui| {
                        for opt in crate::settings::HidHideKeepOnExit::ALL {
                            if ui.selectable_label(cur == opt, opt.label()).clicked()
                                && cur != opt
                            {
                                self.settings.hidhide_keep_on_exit = opt;
                                *dirty = true;
                                self.hidhide_dirty = true; // re-apply with the new policy
                            }
                        }
                    });
            });
        });
        form_note(ui,
            "Which controllers stay hidden once FlexInput has closed.\n\
             Never: everything FlexInput hid is unhidden when it closes.\n\
             Only manually hidden (default): controllers you hid with the eye button on \
             their device card stay hidden; the automatically hidden ones are unhidden.\n\
             Always: every controller FlexInput hid stays hidden.\n\
             A kept controller is unhidden on FlexInput's next launch if it's no longer \
             wanted hidden then.");
    }

    fn settings_virtual_output(&mut self, ui: &mut egui::Ui, dirty: &mut bool) {
        form_subheading(ui, "HIDMaestro virtual controllers");
        if form_check(ui, "After FlexInput closes", true, &mut self.settings.persist_virtual_devices,
            "Keep them alive", "").changed()
        {
            *dirty = true;
            #[cfg(windows)]
            flexinput_hidmaestro::helper::set_persist(self.settings.persist_virtual_devices);
        }
        form_note(ui,
            "Off by default: virtual pads are removed when the app closes or crashes. \
             Turn on to keep a HIDMaestro controller (DualShock 4 / DualSense) registered \
             across an app restart or update, so a game doesn't lose the device \u{2014} \
             FlexInput reclaims it on next launch. While FlexInput is closed the pad sends \
             no input (it resumes when the app reopens).\n\
             Does not apply to ViGEm devices (Virtual Xbox / DualShock 4 (ViGEmBus)): \
             ViGEmBus removes those automatically when FlexInput exits and they can't be \
             reclaimed — they're re-created fresh on next launch.");
        form_row(ui, "Drivers", "", |ui| {
            if ui.button("Reinstall…").on_hover_text("Reinstall the HIDMaestro drivers").clicked() {
                self.reinstall_confirm_open = true;
            }
            if ui.button("Uninstall…").on_hover_text("Uninstall the HIDMaestro drivers").clicked() {
                self.uninstall_confirm_open = true;
            }
        });
        if let Some(err) = &self.last_device_op_error {
            form_line(ui, egui::RichText::new(format!("Last device error: {err}"))
                .size(crate::settings::ui_size(FORM_NOTE_PT)).color(FORM_ERROR));
        }
        form_note(ui,
            "Reinstall: removes and reinstalls the driver, then re-deploys the virtual \
             controllers on your canvas — use this if virtual DS4/DualSense stop working \
             after a Windows or app update. Uninstall: removes the HIDMaestro driver and \
             all its virtual controllers (gamepads will need a reinstall to work again). \
             Both prompt for admin once.");

        form_subheading(ui, "Virtual keyboard & mouse");
        if form_check(ui, "Physical mouse", true, &mut self.settings.mouse_suppression_enabled,
            "Pause the virtual mouse when it moves",
            "When on, the virtual mouse's pointer briefly yields whenever another \
             mouse is used — a physical mouse, a touchpad, or another program's \
             mouse input — so a stick-driven cursor doesn't fight it on the \
             desktop. Only motion pauses: virtual clicks and scrolling go through. \
             The cursor being moved by a game doesn't count. Automatically \
             disabled while a virtual gamepad is also active (mixed mode).")
            .changed()
        {
            flexinput_virtual::set_mouse_suppression_enabled(self.settings.mouse_suppression_enabled);
            *dirty = true;
        }
        ui.add_enabled_ui(self.settings.mouse_suppression_enabled, |ui| {
            form_row(ui, "Release after", "", |ui| {
                let resp = ui.add(egui::Slider::new(
                    &mut self.settings.mouse_suppress_release_ms,
                    settings::MOUSE_SUPPRESS_RELEASE_MS_MIN..=settings::MOUSE_SUPPRESS_RELEASE_MS_MAX,
                ).suffix(" ms"));
                if resp.changed() {
                    flexinput_virtual::set_mouse_suppression_release_ms(self.settings.mouse_suppress_release_ms);
                    *dirty = true;
                }
            });
        });
        form_note(ui, "How long the virtual mouse stays paused after a physical-mouse move. Lower = recovers faster.");

        if form_check(ui, "Mixed output", true, &mut self.settings.mixed_braid_enabled,
            "Braid it (experimental)",
            "Phase-offset WHEN the virtual gamepad and keyboard/mouse packets \
             land, so they interleave instead of co-occurring — without muting \
             either stream (an idle mouse won't chop the pad). For probing games \
             whose input arbiter behaves differently under simultaneous mixed \
             output. Effect is game-specific; leave off unless experimenting.")
            .changed()
        {
            flexinput_virtual::set_braid_enabled(self.settings.mixed_braid_enabled);
            *dirty = true;
        }
        ui.add_enabled_ui(self.settings.mixed_braid_enabled, |ui| {
            form_row(ui, "Braid pacing", "", |ui| {
                // Stepped slider over BRAID_RATE_STEPS (left = Real-time /
                // fastest, right = 125 Hz / slowest). The slider drives an
                // index; the formatter shows the step label.
                let last = settings::BRAID_RATE_STEPS.len() - 1;
                let mut idx = settings::braid_rate_to_index(self.settings.mixed_braid_rate_hz) as i64;
                let resp = ui.add(egui::Slider::new(&mut idx, 0..=last as i64)
                    .integer()
                    .custom_formatter(move |n, _| {
                        let i = (n as usize).min(last);
                        settings::braid_rate_label(settings::BRAID_RATE_STEPS[i])
                    }));
                if resp.changed() {
                    let i = (idx as usize).min(last);
                    self.settings.mixed_braid_rate_hz = settings::BRAID_RATE_STEPS[i];
                    flexinput_virtual::set_braid_rate_hz(self.settings.mixed_braid_rate_hz);
                    *dirty = true;
                }
            });
        });
        form_note(ui, "Gamepad and mouse packets strictly alternate (never coincident). Real-time = fastest/lowest latency; lower rates pace the alternation. Sweep to probe the game.");
    }

    fn settings_device_defaults(&mut self, ui: &mut egui::Ui, dirty: &mut bool) {
        // Per-device sliders in the node header are seeded from these
        // when a node is first added. Editing them later updates only
        // the affected node, not these defaults.
        form_group_note(ui,
            "Applied to newly-added device nodes. Existing nodes keep their own values. \
             Double-click a slider to put it back to the factory default, or its value to type a number.");
        form_row(ui, "Stick deadzone", "", |ui| {
            let r = ui.add(egui::Slider::new(&mut self.settings.default_stick_deadzone, 0.0_f32..=0.5)
                .show_value(false)
                .clamping(egui::SliderClamping::Always));
            if r.changed() { *dirty = true; }
            if slider_double_clicked(&r) {
                self.settings.default_stick_deadzone = 0.1;
                *dirty = true;
            }
            if form_value(ui, egui::DragValue::new(&mut self.settings.default_stick_deadzone)
                .speed(0.005)
                .range(0.0_f32..=0.5)
                .fixed_decimals(2))
                .changed() { *dirty = true; }
        });
        form_row(ui, "Gyro ×", "", |ui| {
            let r = ui.add(egui::Slider::new(&mut self.settings.default_gyro_mult, 0.1_f32..=50.0)
                .logarithmic(true)
                .show_value(false)
                .clamping(egui::SliderClamping::Always));
            if r.changed() { *dirty = true; }
            if slider_double_clicked(&r) {
                self.settings.default_gyro_mult = 1.0;
                *dirty = true;
            }
            if form_value(ui, egui::DragValue::new(&mut self.settings.default_gyro_mult)
                .speed(0.05)
                .range(0.1_f32..=50.0)
                .fixed_decimals(2))
                .changed() { *dirty = true; }
        });
        form_row(ui, "Mouse ×", "", |ui| {
            let r = ui.add(egui::Slider::new(&mut self.settings.default_mouse_sensitivity, 0.0_f32..=3000.0)
                .logarithmic(true)
                .smallest_positive(0.01)
                .show_value(false)
                .clamping(egui::SliderClamping::Always));
            if r.changed() { *dirty = true; }
            if slider_double_clicked(&r) {
                self.settings.default_mouse_sensitivity = 100.0;
                *dirty = true;
            }
            if form_value(ui, egui::DragValue::new(&mut self.settings.default_mouse_sensitivity)
                .speed(0.5)
                .range(0.0_f32..=3000.0)
                .fixed_decimals(2))
                .changed() { *dirty = true; }
        });
        // Default rumble-forwarding shape for virtual pads whose
        // Rumble control hasn't been touched. Same widgets as the
        // node control; double-click resets to neutral.
        form_row(ui, "Rumble", "", |ui| {
            use crate::canvas::header_controls::{
                curve_box, range_slider,
                RUMBLE_DEF_EXP, RUMBLE_DEF_FLOOR, RUMBLE_DEF_MAX,
            };
            let (f0, m0, e0) = (
                self.settings.default_rumble_floor,
                self.settings.default_rumble_max,
                self.settings.default_rumble_exp,
            );
            let mut floor = f0.clamp(0.0, 1.0);
            let mut max = m0.clamp(0.0, 1.0);
            let mut exp = e0.clamp(0.2, 3.0);
            range_slider(ui, &mut floor, &mut max,
                RUMBLE_DEF_FLOOR, RUMBLE_DEF_MAX,
                ui.spacing().slider_width)
                .on_hover_text(
                    "Default band for game rumble forwarded by virtual \
                     pads: left handle = floor (lifts faint rumble), \
                     right = ceiling. Applies to pads whose own Rumble \
                     control hasn't been changed. Double-click resets \
                     to neutral (full range).");
            curve_box(ui, &mut exp, RUMBLE_DEF_EXP);
            if max < floor { max = floor; }
            if (floor - f0).abs() > f32::EPSILON
                || (max - m0).abs() > f32::EPSILON
                || (exp - e0).abs() > f32::EPSILON
            {
                self.settings.default_rumble_floor = floor;
                self.settings.default_rumble_max = max;
                self.settings.default_rumble_exp = exp;
                *dirty = true;
            }
        });
    }

    /// Display-synced output: reports on a grid locked to the display's frames
    /// instead of at the output rate (see `flexinput_virtual::display_clock`).
    fn settings_output_sync(&mut self, ui: &mut egui::Ui, dirty: &mut bool) {
        use flexinput_virtual::display_clock;
        let supported = display_clock::SUPPORTED;
        let mut on = self.settings.output_display_sync && supported;
        if form_check(ui, "Display sync", supported, &mut on, "Sync output to the display",
            "Report on the display's frames instead of at a fixed rate: a few reports a frame, evenly spaced and locked to the frames, so each frame gets the same number. A game reads the mouse once a frame, and at a fixed rate the number of reports that land in each frame changes from frame to frame, so a mouse moved by a held stick or gyro judders, which only a very high output rate hides. Follows the display the foreground window is on; on Linux, which can't tell, the fastest display that's on.")
            .on_disabled_hover_text("Windows and Linux only.")
            .changed()
        {
            self.settings.output_display_sync = on;
            *dirty = true;
        }
        if !on {
            return;
        }
        form_row(ui, "Reports per frame", "How many times the virtual devices report in each frame. More is more forgiving: if a report lands just past the moment the game reads its input, it moves a smaller share of the frame's motion to the next frame. The output rate is this many times the display's refresh rate.", |ui| {
            for n in settings::OUTPUT_SYNC_PER_FRAME {
                if ui.selectable_label(self.settings.output_sync_per_frame == n, n.to_string()).clicked() {
                    self.settings.output_sync_per_frame = n;
                    *dirty = true;
                }
            }
        });
        form_row(ui, "Phase in frame", "Where in the frame the reports sit: the first this far into the frame after the display's refresh, the rest evenly after it. The game reads its input at a moment of its own in the frame; if motion still judders, move the reports away from it.", |ui| {
            let max = (settings::OUTPUT_SYNC_PHASE_MAX * 100.0).round() as i32;
            let mut pct = (self.settings.output_sync_phase * 100.0).round() as i32;
            if ui.add(egui::Slider::new(&mut pct, 0..=max).suffix("%")).changed() {
                self.settings.output_sync_phase = pct as f32 / 100.0;
                *dirty = true;
            }
        });
        // Tuned by eye in the game: a slider over it, on a gamepad shortcut.
        if self.gamepad_shortcut_row(ui, "Tune over the game", crate::gamepad_nav::ChordTarget::PhaseOverlay) {
            *dirty = true;
        }
        form_note(ui, "Opens the phase over the game: left stick or D-pad moves it, East closes it. The game keeps everything else, so aim while you tune.");
        let mut follow = self.settings.output_sync_follow_vrr;
        if form_check(ui, "Variable refresh", true, &mut follow, "Follow it (VRR)",
            "With G-SYNC or FreeSync the display refreshes when the game shows a frame, so the reports follow the frames as they come. Off, they go out at the display's maximum rate as if it were fixed.")
            .changed()
        {
            self.settings.output_sync_follow_vrr = follow;
            *dirty = true;
        }
        form_status(ui, &crate::phase_overlay::sync_status(&self.settings), crate::settings::tinted_gray(170));
    }

    fn settings_performance(&mut self, ui: &mut egui::Ui, dirty: &mut bool) {
        form_subheading(ui, "Input and output");
        form_row(ui, "Max polling rate", "How often controllers and MIDI devices are read. Unless Output rate is set, also how fast the virtual devices report to games. Steps are whole periods: 2000 = 0.5 ms, 1000 = 1 ms … 125 = 8 ms. A controller's own input rate depends on the controller — see the live Hz on each device header.", |ui| {
            // The slider drives the STEP INDEX, not the raw Hz, so the handle
            // is evenly spaced and snaps to a valid period *while* dragging.
            // The custom formatter shows the Hz for the current step instead
            // of the bare index.
            //
            // POLLING_HZ_STEPS is DESCENDING (index 0 = fastest), but the
            // slider should read low→high left→right. So the slider drives
            // a POSITION = last - index; 125 Hz lands at the left, 2000 Hz
            // at the right. Convert back to an index with the same mirror.
            let last = settings::POLLING_HZ_STEPS.len() - 1;
            let idx = settings::polling_hz_to_index(self.settings.polling_hz);
            let mut pos = (last - idx) as i64;
            let resp = ui.add(egui::Slider::new(&mut pos, 0..=last as i64)
                .integer()
                .custom_formatter(move |n, _| {
                    let i = last - (n as usize).min(last);
                    format!("{} Hz", settings::polling_hz_from_index(i))
                }));
            if resp.changed() {
                let i = last - (pos as usize).min(last);
                self.settings.polling_hz = settings::polling_hz_from_index(i);
                self.polling_hz.store(self.settings.polling_hz, Ordering::Relaxed);
                *dirty = true;
            }
        });
        form_note(ui, "How often the I/O thread reads gamepads and MIDI devices.");

        let synced = self.settings.output_sync().is_some();
        ui.add_enabled_ui(!synced, |ui| {
            form_row(ui, "Output rate", "How fast the virtual gamepads and the virtual mouse report to games, separately from the polling rate. Lower than polling saves work (or suits an old game that misbehaves with fast mice); higher gives virtual output finer steps than your controllers' own rate — mouse motion from a held gyro or stick moves in smaller, more frequent steps. Output can't change faster than the processing sample rate. \"Same as polling\" reports once per poll, as before, with the SendInput virtual mouse at its own 1000 Hz.", |ui| {
                // Position 0 = "Same as polling", then the polling steps low→high.
                let steps = settings::POLLING_HZ_STEPS.len();
                let mut pos = if self.settings.output_hz == settings::OUTPUT_HZ_SAME_AS_POLLING {
                    0i64
                } else {
                    (steps - settings::polling_hz_to_index(self.settings.output_hz)) as i64
                };
                let hz_at = move |pos: usize| -> u32 {
                    if pos == 0 {
                        settings::OUTPUT_HZ_SAME_AS_POLLING
                    } else {
                        settings::polling_hz_from_index(steps - pos.min(steps))
                    }
                };
                let resp = ui.add(egui::Slider::new(&mut pos, 0..=steps as i64)
                    .integer()
                    .custom_formatter(move |n, _| match hz_at(n as usize) {
                        settings::OUTPUT_HZ_SAME_AS_POLLING => "Same as polling".to_string(),
                        hz => format!("{hz} Hz"),
                    }))
                    .on_disabled_hover_text("Set by the display while output is synced to it.");
                if resp.changed() {
                    self.settings.output_hz = hz_at(pos.max(0) as usize);
                    self.output_hz.store(self.settings.output_hz, Ordering::Relaxed);
                    *dirty = true;
                }
            });
        });
        form_note(ui, "How fast the virtual gamepads and virtual mouse report to games.");
        self.settings_output_sync(ui, dirty);
        if self.settings.effective_output_hz() > self.settings.effective_sample_rate_hz() {
            form_status(ui, &format!(
                "Above the processing sample rate ({} Hz): outputs from the patch only change that often.",
                self.settings.effective_sample_rate_hz(),
            ), FORM_WARN);
        }
        let live = live_rates(ui.ctx());
        let mouse = match live.mouse_hz {
            Some(0) | None => "idle".to_string(),
            Some(hz) => format!("{hz} reports/s"),
        };
        let output = live.output_hz.map_or_else(|| "…".to_string(), |hz| format!("{hz} Hz"));
        form_status(ui, &format!(
            "Now: polling {} Hz · output {output} · virtual mouse {mouse}",
            flexinput_engine::current_io_rate(),
        ), crate::settings::tinted_gray(170))
            .on_hover_text("Measured live. The virtual mouse sends a report only when the pointer moves at least one whole count, so slow motion sends fewer than the output rate.");
        form_gap(ui);

        form_subheading(ui, "Processing");
        form_row(ui, "Sample rate", "", |ui| {
            use settings::SampleRateMode as Mode;
            let label = |m: Mode| match m {
                Mode::Manual => "Set by hand",
                Mode::SameAsOutput => "Same as output",
                Mode::TwiceOutput => "2x output",
            };
            // Narrow, with the rate beside it.
            ui.spacing_mut().combo_width = ui.spacing().combo_width.min(130.0);
            egui::ComboBox::from_id_salt("sample_rate_mode")
                .selected_text(label(self.settings.sample_rate_mode))
                .show_ui(ui, |ui| {
                    for (m, tip) in [
                        (Mode::Manual, "Pick the rate yourself, no lower than polling."),
                        (Mode::SameAsOutput, "One tick per output: the least CPU. Timers, gestures and PWM are only as fine as the output rate."),
                        (Mode::TwiceOutput, "Two ticks per output: twice the CPU of Same as output, and twice as fine for timers, gestures and PWM."),
                    ] {
                        if ui.selectable_value(&mut self.settings.sample_rate_mode, m, label(m))
                            .on_hover_text(tip)
                            .changed()
                        {
                            *dirty = true;
                        }
                    }
                });
            if self.settings.sample_rate_mode == Mode::Manual {
                let floor = self.settings.sample_rate_floor_hz();
                let resp = ui.add(egui::Slider::new(
                    &mut self.settings.sample_rate_hz,
                    floor..=settings::SAMPLE_RATE_HZ_MAX,
                ).suffix(" Hz"));
                if resp.changed() {
                    *dirty = true;
                }
            } else {
                ui.label(format!("{} Hz", self.settings.effective_sample_rate_hz()));
            }
        });
        if let (Some(passes), Some(ticks)) = (live.passes_hz, live.ticks_hz) {
            // Below ~90% of the setting, catch-up ticks are being dropped: the
            // patch costs more per tick than this machine has at this rate.
            let rate = self.settings.effective_sample_rate_hz();
            let behind = ticks < rate * 9 / 10;
            // A rate it would sustain: under what it manages now, with room.
            let suggest = (ticks * 4 / 5 / 100 * 100)
                .clamp(self.settings.sample_rate_floor_hz().min(rate), rate);
            let text = format!(
                "Now: engine {ticks} ticks/s in {passes} passes/s{}",
                if !behind {
                    String::new()
                } else if suggest < rate && self.settings.sample_rate_mode == settings::SampleRateMode::Manual {
                    format!(" — can't keep up at this rate; try {suggest} Hz")
                } else {
                    " — can't keep up at this rate".to_string()
                },
            );
            form_status(ui, &text, if behind { FORM_WARN } else { crate::settings::tinted_gray(170) })
                .on_hover_text("The engine publishes its outputs once per pass, so passes/s is how often outputs can change.");
        }
        form_note(ui, "The engine's tick rate. Higher = finer timing, more CPU.");

        form_subheading(ui, "Window");
        form_row(ui, "Background repaint rate", "Repaint rate applied when the window is minimized or another app has focus. The focused window always paints at vsync regardless.", |ui| {
            let resp = ui.add(egui::Slider::new(
                &mut self.settings.bg_repaint_hz,
                settings::BG_REPAINT_HZ_MIN..=settings::BG_REPAINT_HZ_MAX,
            ).suffix(" Hz"));
            if resp.changed() {
                *dirty = true;
            }
        });
        form_note(ui, "How often the UI repaints while FlexInput sits in the background. Lower = less CPU while you play a game, higher = smoother glanceable visuals. Has no effect when the window is focused.");
        if form_check(ui, "Unused windows", true, &mut self.settings.freeze_background, "Freeze and dim them",
            "Windows without focus stop drawing and stay dimmed on their last frame until you click them. A sub-patch editor also wakes when the pointer moves over it. Mappings and overlays keep running. A pinned or see-through window stays live.")
            .changed()
        {
            *dirty = true;
        }

        form_subheading(ui, "Where the time goes");
        // Inline rather than a tooltip: it's read while a controller moves the
        // pointer, which a hover can't survive.
        let small = |s: &str| egui::RichText::new(s).size(crate::settings::ui_size(FORM_NOTE_PT));
        egui::CollapsingHeader::new(small("The I/O loop"))
            .id_salt("io_timings_details")
            .show(ui, |ui| {
                let rows = super::threads::IO_TIMINGS.lock().map(|t| t.clone()).unwrap_or_default();
                egui::Grid::new("io_timings").num_columns(3).striped(true).show(ui, |ui| {
                    let head = |s: &str| small(s).strong();
                    ui.label(head("per loop pass, last second"));
                    ui.label(head("avg ms"));
                    ui.label(head("worst ms"));
                    ui.end_row();
                    for (name, avg, worst) in rows.iter().take(20) {
                        ui.label(small(name.as_str()));
                        ui.label(small(&format!("{avg:.3}")));
                        ui.label(small(&format!("{worst:.2}")));
                        ui.end_row();
                    }
                });
            });
        // Whether the engine is computing, blocked, or short of ticks.
        egui::CollapsingHeader::new(small("The engine"))
            .id_salt("engine_timings_details")
            .show(ui, |ui| {
                let rows = flexinput_engine::ENGINE_TIMINGS.lock().map(|t| t.clone()).unwrap_or_default();
                egui::Grid::new("engine_timings").num_columns(3).striped(true).show(ui, |ui| {
                    let head = |s: &str| small(s).strong();
                    ui.label(head("per engine pass, last second"));
                    ui.label(head("avg ms"));
                    ui.label(head("worst ms"));
                    ui.end_row();
                    for (name, avg, worst) in &rows {
                        ui.label(small(name.as_str()));
                        ui.label(small(&format!("{avg:.3}")));
                        ui.label(small(&format!("{worst:.2}")));
                        ui.end_row();
                    }
                });
            });
        // The main window's passes, before it paints. The second line keeps the
        // last figures from without focus, when the window can't show them live.
        egui::CollapsingHeader::new(small("The window"))
            .id_salt("ui_pass_details")
            .show(ui, |ui| {
                let (now, away) = super::main_window::UI_PASSES.lock().map(|t| *t).unwrap_or_default();
                let line = |r: super::main_window::UiPassRow| format!(
                    "{} passes/s, {} drawn, {} let go, {:.2} ms each",
                    r.passes, r.drawn, r.waited, r.avg_ms,
                );
                ui.label(small(&format!("Now: {}", line(now))));
                ui.label(small(&format!("Last time without focus: {}", line(away))));
            });
        // Which thread the process's CPU time goes to; nice shows whether the
        // input threads got their raised priority (thread_priority).
        #[cfg(target_os = "linux")]
        egui::CollapsingHeader::new(small("The CPU, by thread"))
            .id_salt("thread_cpu_details")
            .show(ui, |ui| {
                let rows = crate::thread_cpu::snapshot();
                let total: f32 = rows.iter().map(|r| r.percent).sum();
                ui.label(small(&format!(
                    "FlexInput: {total:.0}% of one core ({} cores here)",
                    std::thread::available_parallelism().map_or(1, |n| n.get()),
                )));
                egui::Grid::new("thread_cpu").num_columns(3).striped(true).show(ui, |ui| {
                    let head = |s: &str| small(s).strong();
                    ui.label(head("thread, last second"));
                    ui.label(head("% of a core"));
                    ui.label(head("nice"));
                    ui.end_row();
                    for r in rows.iter().take(14) {
                        let name = if r.count > 1 { format!("{} ×{}", r.name, r.count) } else { r.name.clone() };
                        ui.label(small(&name));
                        ui.label(small(&format!("{:.1}", r.percent)));
                        ui.label(small(&r.nice.to_string()));
                        ui.end_row();
                    }
                });
            });
    }

    fn settings_midi(&mut self, ui: &mut egui::Ui) {
        form_row(ui, "MIDI devices", "", |ui| {
            if ui.button("Refresh").on_hover_text("Scan for MIDI ports again").clicked() {
                self.midi_refresh_requested.store(true, Ordering::Release);
            }
        });
        form_note(ui,
            "MIDI ports are scanned once at startup and on demand. \
             Auto-scanning is disabled because the Windows MIDI API periodically \
             disrupts the audio stack (stream skips, Bluetooth noise). \
             Click after plugging in or creating a MIDI/loopMIDI port.");
        form_subheading(ui, "Loop guard");
        // What the loop guard knows: which In/Out ports it treats as one
        // port coming back round, and which Out ports its breaker muted.
        {
            use flexinput_devices::midi::PairStatus;
            let guard = crate::canvas::viewer::midi_guard_view(ui.ctx());
            let name = |id: &str| self.devices.iter()
                .find(|d| d.id == id)
                .map(|d| d.display_name.clone())
                .unwrap_or_else(|| id.to_string());
            if guard.pairs.is_empty() && guard.muted.is_empty() {
                form_group_note(ui,
                    "No MIDI ports in use are paired. A pair forms when a patch uses a MIDI In \
                     and a MIDI Out that are the same port (same name, or echoes seen coming back).");
            }
            for (out_id, in_id, status) in &guard.pairs {
                let (what, tip) = match status {
                    PairStatus::Seeded => ("same name — echoes cancelled",
                        "Named alike, so treated as one port until sends go unanswered."),
                    PairStatus::Confirmed => ("echoes seen — cancelled",
                        "Messages sent here came straight back in, so their echoes are dropped."),
                    PairStatus::Demoted => ("no echoes — nothing cancelled",
                        "Plenty sent and nothing came back, so this is two separate ports."),
                };
                form_data_row(ui, &format!("{} → {}", name(out_id), name(in_id)), "", |ui| {
                    ui.label(egui::RichText::new(what).color(crate::settings::tinted_gray(170)))
                        .on_hover_text(tip);
                });
            }
            for out_id in &guard.muted {
                form_data_row(ui, &name(out_id), "", |ui| {
                    ui.label(egui::RichText::new("⛔ muted by the loop breaker").color(egui::Color32::from_rgb(226, 104, 92)));
                    if ui.button("Unmute").clicked() {
                        crate::canvas::viewer::request_midi_unmute(ui.ctx(), out_id);
                    }
                });
            }
        }
    }

    fn settings_folders(&mut self, ui: &mut egui::Ui, dirty: &mut bool) {
        let presets = self.settings.user_presets_folder.as_ref().map(|p| p.display().to_string());
        form_row(ui, "User presets", "", |ui| {
            if ui.button("Browse…").clicked() {
                if let Some(p) = crate::overlay::with_overlay_not_topmost(|| rfd::FileDialog::new().pick_folder()) {
                    self.settings.user_presets_folder = Some(p);
                    *dirty = true;
                }
            }
            if ui.add_enabled(presets.is_some(), egui::Button::new("Clear")).clicked() {
                self.settings.user_presets_folder = None;
                *dirty = true;
            }
            form_path(ui, presets.as_deref(), "(none)");
        });
        form_note(ui, "Folder scanned for your own .fxsp sub-patch presets, beside the factory presets shipped under app/assets/sub-patches/.");

        let models = self.settings.user_models_dir.clone().filter(|s| !s.is_empty());
        form_row(ui, "3D controller models", "", |ui| {
            if ui.button("Browse…").clicked() {
                if let Some(p) = crate::overlay::with_overlay_not_topmost(|| rfd::FileDialog::new().pick_folder()) {
                    let s = p.to_string_lossy().to_string();
                    self.settings.user_models_dir = Some(s);
                    crate::model::set_user_models_dir(Some(p));
                    *dirty = true;
                }
            }
            if ui.add_enabled(models.is_some(), egui::Button::new("Clear")).clicked() {
                self.settings.user_models_dir = None;
                crate::model::set_user_models_dir(None);
                *dirty = true;
            }
            form_path(ui, models.as_deref(), "(none)");
        });
        form_note(ui,
            "Optional folder with your own controller models. Same \
             structure as the bundled ones: <Name>/info.txt + .obj files \
             (+ optional colors.fxcol). A same-named folder overrides the \
             bundled model.");

        let keys = self.settings.bt_key_dir.clone().filter(|s| !s.is_empty());
        form_row(ui, "Bluetooth link keys", "", |ui| {
            if ui.button("Browse…").clicked() {
                if let Some(p) = crate::overlay::with_overlay_not_topmost(
                    || rfd::FileDialog::new().pick_folder(),
                ) {
                    // ⭐ Checked NOW, not at pairing time. A cloud folder can be
                    // offline, read-only mid-sync, or not yet created — and
                    // finding that out during a pairing means losing the key for
                    // a bond that has already replaced the controller's previous
                    // host. That is the worst possible moment to discover it.
                    match flexinput_btle::keystore::check_writable(&p) {
                        Ok(()) => {
                            self.settings.bt_key_dir =
                                Some(p.to_string_lossy().to_string());
                            flexinput_btle::keystore::set_dir(Some(p));
                            *dirty = true;
                        }
                        Err(e) => {
                            // Kept on screen rather than flashed: the
                            // whole point is that the user learns the
                            // folder is unusable BEFORE relying on it.
                            ui.ctx().data_mut(|d| {
                                d.insert_temp(
                                    egui::Id::new("bt_key_dir_error"),
                                    format!("Cannot use that folder — {e}"),
                                )
                            });
                        }
                    }
                }
            }
            if ui.add_enabled(keys.is_some(), egui::Button::new("Clear")).clicked() {
                self.settings.bt_key_dir = None;
                flexinput_btle::keystore::set_dir(None);
                ui.ctx().data_mut(|d| {
                    d.remove::<String>(egui::Id::new("bt_key_dir_error"))
                });
                *dirty = true;
            }
            form_path(ui, keys.as_deref(), "(working directory)");
        });
        if let Some(err) = ui
            .ctx()
            .data(|d| d.get_temp::<String>(egui::Id::new("bt_key_dir_error")))
        {
            form_status(ui, &err, egui::Color32::from_rgb(230, 140, 60));
        }
        form_note(ui,
            "Where link keys for controllers paired to FlexInput's own \
             Bluetooth dongle are kept. A pairing is between the controller and \
             the DONGLE, not this PC — so the same dongle works on another machine. \
             Both ends have to hold the key, though, and this file is the host's \
             copy. Point this at a cloud-synced folder and the key follows the \
             dongle everywhere with nothing to copy by hand.");
    }

    /// Profiler toggle: flips `puffin::set_scopes_on()` and starts/stops a
    /// `puffin_http` server on 127.0.0.1:8585 so the standalone `puffin_viewer`
    /// GUI can connect for a live flamegraph. Not persisted — resets to off on
    /// every launch. Hidden from release builds entirely so the shipped UI
    /// doesn't expose an internal dev tool.
    #[cfg(debug_assertions)]
    fn settings_developer(&mut self, ui: &mut egui::Ui) {
        let mut prof = self.settings.profiler_enabled;
        if form_check(ui, "Profiler", true, &mut prof, "puffin, on 127.0.0.1:8585", "").changed() {
            self.settings.profiler_enabled = prof;
            if prof {
                match puffin_http::Server::new("127.0.0.1:8585") {
                    Ok(server) => {
                        puffin::set_scopes_on(true);
                        self.profiler_server = Some(server);
                        eprintln!("[profiler] listening on 127.0.0.1:8585 — connect with `puffin_viewer --url 127.0.0.1:8585`");
                    }
                    Err(e) => {
                        eprintln!("[profiler] failed to start server: {e}");
                        self.settings.profiler_enabled = false;
                    }
                }
            } else {
                puffin::set_scopes_on(false);
                self.profiler_server = None;
                eprintln!("[profiler] stopped");
            }
        }
        form_note(ui,
            "Install once: `cargo install puffin_viewer`.\n\
             Then run `puffin_viewer --url 127.0.0.1:8585` with this toggle on.");
    }

    fn settings_about(&mut self, ui: &mut egui::Ui) {
        form_row(ui, "Version", "", |ui| {
            ui.label(format!("FlexInput v{}", env!("CARGO_PKG_VERSION")));
        });
        form_row(ui, "Source", "", |ui| {
            ui.hyperlink_to("FlexInput repository", "https://github.com/x-iso/FlexInput");
        });
        form_row(ui, "Virtual HID driver", "", |ui| {
            ui.hyperlink_to("HIDMaestro", "https://github.com/hifihedgehog/HIDMaestro");
        });
        form_row(ui, "Controller hiding", "", |ui| {
            ui.hyperlink_to("HidHide — latest release", "https://github.com/nefarius/HidHide/releases/latest");
        });
        form_subheading(ui, "Credits");
        // Each credit a paragraph of its own, in the notes' size, with the same gap after it.
        let t = |s: &str| egui::RichText::new(s).size(crate::settings::ui_size(FORM_NOTE_PT));
        let credit = |ui: &mut egui::Ui, add: &mut dyn FnMut(&mut egui::Ui)| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                add(ui);
            });
            ui.add_space(2.0);
        };
        credit(ui, &mut |ui| {
            ui.label(t("Built with egui, eframe, egui-snarl, egui_extras, gilrs, SDL, midir, rfd, serde, HidHide."));
        });
        credit(ui, &mut |ui| {
            ui.label(t("Virtual devices powered by"));
            ui.hyperlink_to(t("HIDMaestro"), "https://github.com/hifihedgehog/HIDMaestro");
            ui.label(t("by hifihedgehog (MIT)."));
        });
        credit(ui, &mut |ui| {
            ui.label(t("Input prompt SVG icons by Kenney —"));
            ui.hyperlink_to(t("kenney.nl/assets/input-prompts"), "https://kenney.nl/assets/input-prompts");
            ui.label(t("(CC0)."));
        });
        credit(ui, &mut |ui| {
            ui.label(t("Macro & menu icons from"));
            ui.hyperlink_to(t("game-icons.net"), "https://game-icons.net/");
            ui.label(t("by their respective authors, licensed"));
            ui.hyperlink_to(t("CC BY 3.0"), "https://creativecommons.org/licenses/by/3.0/");
            ui.label(t("(per-icon authors listed on the site)."));
        });
        credit(ui, &mut |ui| {
            ui.label(t("3D controller models adapted from"));
            ui.hyperlink_to(t("3d-controller-overlay"), "https://github.com/larfingshnew/3d-controller-overlay");
            ui.label(t("by larfingshnew (MIT)."));
        });
        credit(ui, &mut |ui| {
            ui.label(t("Steam Controller 3D model © Valve Corporation, modified by FlickStick Videos and"));
            ui.hyperlink_to(t("ceski"), "https://github.com/ceski-1/3d-controller-overlay");
            ui.label(t("—"));
            ui.hyperlink_to(t("CC BY-NC-SA 4.0"), "https://creativecommons.org/licenses/by-nc-sa/4.0/");
            ui.label(t("(non-commercial use only). Not affiliated with or endorsed by Valve."));
        });
        credit(ui, &mut |ui| {
            ui.label(t("JSM Config follows"));
            ui.hyperlink_to(t("JoyShockMapper"), "https://github.com/Electronicks/JoyShockMapper");
            ui.label(t("by Jibb Smart and Electronicks, and its"));
            ui.hyperlink_to(t("JSM_custom_curve"), "https://github.com/evan1mclean/JSM_custom_curve");
            ui.label(t("fork by evan1mclean (both MIT)."));
        });
        credit(ui, &mut |ui| {
            ui.label(t("Adaptive-trigger effects follow Nielk1's"));
            ui.hyperlink_to(t("TriggerEffectGenerator"), "https://gist.github.com/Nielk1/6d54cc2c00d2201ccb8c2720ad7538db");
            ui.label(t("(MIT)."));
        });
        credit(ui, &mut |ui| {
            ui.label(t("Controller protocol research by"));
            ui.hyperlink_to(t("dekuNukem"), "https://github.com/dekuNukem/Nintendo_Switch_Reverse_Engineering");
            ui.label(t("(Switch Pro) and"));
            ui.hyperlink_to(t("ndeadly"), "https://github.com/ndeadly/switch2_controller_research");
            ui.label(t("(Switch 2 / Joy-Con 2)."));
        });
        form_gap(ui);
        if ui.button("Third-party licenses…")
            .on_hover_text("Full licence text for everything FlexInput redistributes")
            .clicked()
        {
            self.licenses_open = true;
        }
    }

    /// The third-party licence viewer, opened from Settings → Credits.
    ///
    /// Entry list on the left, that entry's licence on the right. The texts are
    /// embedded verbatim (see `crate::licenses`) and rendered monospace without
    /// truncation — a licence notice that's been reflowed or cut short is no
    /// longer the notice the licence asks us to reproduce.
    ///
    /// Drawn from `update` rather than from inside the Settings window so it
    /// stands on its own: closing Settings leaves it up.
    pub(crate) fn draw_licenses_window(&mut self, ctx: &egui::Context) {
        if !self.licenses_open { return; }
        let mut open = true;
        let entries = crate::licenses::LICENSES;
        // The index outlives any single frame, so don't trust it blindly.
        if self.licenses_selected >= entries.len() { self.licenses_selected = 0; }

        egui::Window::new("Third-party licenses")
            .id(egui::Id::new("licenses_window"))
            .collapsible(false)
            .resizable(true)
            .default_size([760.0, 560.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(
                    "FlexInput redistributes the software and assets below — linked \
                     into flexinput.exe, or bundled alongside it. Tools you install \
                     separately (HidHide, ViGEmBus) aren't redistributed and so \
                     aren't listed here."
                ).small().weak());
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(4.0);

                // Width of the entry list. Points, so it tracks UI scale.
                const LIST_W: f32 = 210.0;
                let avail_h = ui.available_height();
                ui.horizontal_top(|ui| {
                    // Both columns ask for `top_down` EXPLICITLY: a child ui built
                    // inside a horizontal layout inherits that direction, which lays
                    // the entry list out sideways across the window and leaves the
                    // text column a few pixels wide.
                    ui.allocate_ui_with_layout(
                        egui::vec2(LIST_W, avail_h),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("licenses_list")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    for (i, entry) in entries.iter().enumerate() {
                                        if ui.selectable_label(
                                            self.licenses_selected == i, entry.name).clicked()
                                        {
                                            self.licenses_selected = i;
                                        }
                                    }
                                });
                        },
                    );
                    ui.separator();

                    let entry = &entries[self.licenses_selected];
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), avail_h),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(egui::RichText::new(entry.name).strong());
                                ui.label(egui::RichText::new(entry.spdx).small().weak());
                                ui.hyperlink_to(egui::RichText::new("source").small(), entry.url);
                                if ui.small_button("Copy")
                                    .on_hover_text("Copy this licence text to the clipboard")
                                    .clicked()
                                {
                                    ui.ctx().copy_text(entry.text.to_owned());
                                }
                            });
                            ui.label(egui::RichText::new(entry.what).small());
                            if let Some(note) = entry.note {
                                ui.add_space(2.0);
                                ui.label(egui::RichText::new(note).small().weak());
                            }
                            ui.add_space(6.0);
                            egui::ScrollArea::vertical()
                                .id_salt("licenses_text")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.add(egui::Label::new(
                                        egui::RichText::new(entry.text).monospace().size(crate::settings::ui_size(11.0))
                                    ).wrap());
                                });
                        },
                    );
                });
            });

        if !open { self.licenses_open = false; }
    }
}

/// The sections of the Settings window, in page order. The jump list and the
/// scrolling page both walk [`SettingsSection::visible`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SettingsSection {
    #[default]
    Display,
    Workspace,
    GamepadNav,
    GamepadShortcuts,
    KeyboardShortcuts,
    PinOverlays,
    Controllers,
    HidHide,
    VirtualOutput,
    DeviceDefaults,
    Performance,
    Midi,
    Folders,
    /// Debug builds only — see [`SettingsSection::visible`].
    Developer,
    About,
}

impl SettingsSection {
    const ALL: [Self; 15] = [
        Self::Display, Self::Workspace, Self::GamepadNav, Self::GamepadShortcuts,
        Self::KeyboardShortcuts, Self::PinOverlays, Self::Controllers, Self::HidHide,
        Self::VirtualOutput, Self::DeviceDefaults, Self::Performance, Self::Midi,
        Self::Folders, Self::Developer, Self::About,
    ];

    /// Sections shown in this build: the profiler is an internal dev tool, so
    /// release builds leave Developer out entirely.
    fn visible() -> Vec<Self> {
        Self::ALL.into_iter()
            .filter(|s| *s != Self::Developer || cfg!(debug_assertions))
            .collect()
    }

    fn label(self) -> &'static str {
        match self {
            Self::Display           => "Display",
            Self::Workspace         => "Workspace",
            Self::GamepadNav        => "Gamepad navigation",
            Self::GamepadShortcuts  => "Gamepad shortcuts",
            Self::KeyboardShortcuts => "Keyboard shortcuts",
            Self::PinOverlays       => "Pin & overlays",
            Self::Controllers       => "Controllers",
            Self::HidHide           => "Hide controllers",
            Self::VirtualOutput     => "Virtual output",
            Self::DeviceDefaults    => "Device defaults",
            Self::Performance       => "Performance",
            Self::Midi              => "MIDI",
            Self::Folders           => "Folders",
            Self::Developer         => "Developer",
            Self::About             => "About",
        }
    }
}

/// Jump-list state carried across frames in egui temp data.
#[derive(Clone, Copy, Default)]
struct SettingsNav {
    /// Section at the top of the page as of the last frame.
    active: SettingsSection,
    /// Section just clicked in the jump list, and when (egui input time).
    pending: Option<(SettingsSection, f64)>,
}

/// A section's title on the Settings page. The response is what a jump
/// scrolls to the top of the view.
fn settings_section_header(ui: &mut egui::Ui, title: &str) -> egui::Response {
    let resp = ui.label(egui::RichText::new(title).size(crate::settings::ui_size(17.0)).strong());
    ui.separator();
    ui.add_space(4.0);
    // A subheading straight under the title adds no space of its own.
    form_trail(2.0 * FORM_GAP);
    resp
}

// ── The page's form ──────────────────────────────────────────────────────────
//
// Every setting on the page is laid out alike: its name in a column of its own, its controls
// beside it in a second - a checkbox too, its words saying what it turns on - and a note in
// the hint colour under the controls, after which comes the same gap before the next setting.
// A section's groups open with a subheading. Laid out row by row, the page had names with a
// colon and without, controls starting wherever their name ended, notes in two greys at
// egui's smallest size, and gaps of anything from 2 to 8 points: nothing lined up.

/// The gap after a setting's note, and before a subheading (twice it).
const FORM_GAP: f32 = 10.0;
/// The widest a slider is drawn, and a dropdown: as wide for every setting.
const FORM_SLIDER_W: f32 = 200.0;
const FORM_COMBO_W: f32 = 190.0;
/// A value box beside a slider, as wide for every one.
const FORM_VALUE_W: f32 = 64.0;
/// A note's text size, before the text size setting: egui's small style is 9 points, too
/// small for a paragraph.
const FORM_NOTE_PT: f32 = 11.0;
/// A warning's colour, and an error's.
const FORM_WARN: egui::Color32 = egui::Color32::from_rgb(230, 170, 80);
const FORM_ERROR: egui::Color32 = egui::Color32::from_rgb(220, 120, 120);

/// The room between the widest name and the controls, besides egui's own item spacing.
const FORM_NAME_GAP: f32 = 6.0;

thread_local! {
    /// The names' column's width on the page being drawn ([`form_page`]).
    static FORM_NAME_W: std::cell::Cell<f32> = const { std::cell::Cell::new(150.0) };
    /// The widest setting's name drawn on the page so far this pass, which the column is
    /// sized to on the next.
    static FORM_WIDEST: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
    /// The space already left under what was drawn last (a note's gap), which a subheading
    /// counts toward its own.
    static FORM_TRAIL: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
}

/// Start a page `w` wide: its names' column as wide as the widest setting's name on it was
/// last pass, and no more than two fifths of the page (a longer name wraps). It was a third
/// of the page, which left a short name's controls far off beside it.
fn form_page(w: f32) {
    let widest = FORM_WIDEST.with(|c| c.replace(0.0));
    let fit = if widest > 0.0 { widest + FORM_NAME_GAP } else { 150.0 };
    FORM_NAME_W.with(|c| c.set(fit.clamp(80.0, (w * 0.4).max(80.0))));
    form_trail(0.0);
}

fn form_name_w() -> f32 {
    FORM_NAME_W.with(|c| c.get())
}

fn form_trail(space: f32) {
    FORM_TRAIL.with(|c| c.set(space));
}

/// The gap that ends a setting.
fn form_gap(ui: &mut egui::Ui) {
    ui.add_space(FORM_GAP);
    form_trail(FORM_GAP);
}

/// A group's title inside a section.
fn form_subheading(ui: &mut egui::Ui, title: &str) {
    let have = FORM_TRAIL.with(|c| c.get());
    ui.add_space((2.0 * FORM_GAP - have).max(0.0));
    ui.label(egui::RichText::new(title).strong().color(crate::settings::tinted_gray(200)));
    ui.add_space(2.0);
    form_trail(0.0);
}

/// One setting's row: `name` in the names' column - its hover saying more, where `hover`
/// does - and `add`'s controls beside it, its sliders and dropdowns as wide as every other's
/// (or as the room left holds).
fn form_row<R>(ui: &mut egui::Ui, name: &str, hover: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let need = ui.painter()
        .layout_no_wrap(name.to_string(), egui::TextStyle::Body.resolve(ui.style()), egui::Color32::PLACEHOLDER)
        .size()
        .x;
    FORM_WIDEST.with(|c| c.set(c.get().max(need)));
    form_data_row(ui, name, hover, add)
}

/// [`form_row`] for a row named by something it lists - a MIDI port pair - rather than a
/// setting: a long name wraps in the column rather than widening it for the whole page.
fn form_data_row<R>(ui: &mut egui::Ui, name: &str, hover: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let w = form_name_w();
    let r = ui.horizontal(|ui| {
        let gap = ui.spacing().item_spacing.x;
        let room = ui.available_width() - w - gap;
        ui.spacing_mut().slider_width = (room - FORM_VALUE_W - 2.0 * gap).clamp(60.0, FORM_SLIDER_W);
        ui.spacing_mut().combo_width = FORM_COMBO_W.min(room.max(60.0));
        let h = ui.spacing().interact_size.y;
        let shown = ui.allocate_ui_with_layout(
            egui::vec2(w, h),
            egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
            |ui| {
                ui.set_width(w);
                ui.add(egui::Label::new(name).wrap())
            },
        ).inner;
        if !hover.is_empty() {
            shown.on_hover_text(hover).on_disabled_hover_text(hover);
        }
        add(ui)
    }).inner;
    form_trail(0.0);
    r
}

/// A setting that's a checkbox: `name` in the names' column (empty for the next of a group),
/// the box and its `text` - wrapping to the room - in the controls'.
fn form_check(ui: &mut egui::Ui, name: &str, enabled: bool, on: &mut bool, text: &str, hover: &str) -> egui::Response {
    form_row(ui, name, "", |ui| {
        ui.vertical(|ui| {
            let r = ui.add_enabled(enabled, egui::Checkbox::new(on, text));
            if hover.is_empty() { r } else { r.on_hover_text(hover) }
        }).inner
    })
}

/// `add` in the controls' column, under the row above: a note, a live readout, a warning.
fn form_column<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let indent = form_name_w() + ui.spacing().item_spacing.x;
    let mut rect = ui.available_rect_before_wrap();
    rect.min.x = (rect.min.x + indent).min(rect.max.x - 40.0);
    let r = ui.scope_builder(
        egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)),
        add,
    ).inner;
    form_trail(0.0);
    r
}

/// A line of text in the controls' column, wrapping to it.
fn form_line(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) -> egui::Response {
    let text = text.into();
    form_column(ui, |ui| ui.label(text))
}

/// A live readout, or a warning, under a setting: `text` in `color`.
fn form_status(ui: &mut egui::Ui, text: &str, color: egui::Color32) -> egui::Response {
    form_line(ui, egui::RichText::new(text).size(crate::settings::ui_size(FORM_NOTE_PT)).color(color))
}

/// The note that ends a setting: under its controls, in the hint colour.
fn form_note(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let r = form_status(ui, text, crate::settings::tinted_gray(140));
    form_gap(ui);
    r
}

/// A line under a section's title or a group's, from the page's edge: what the group is.
fn form_group_line(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) {
    ui.label(text);
    ui.add_space(FORM_GAP * 0.5);
    form_trail(FORM_GAP * 0.5);
}

/// [`form_group_line`] as a note, in the hint colour.
fn form_group_note(ui: &mut egui::Ui, text: &str) {
    form_group_line(ui, egui::RichText::new(text).size(crate::settings::ui_size(FORM_NOTE_PT)).color(crate::settings::tinted_gray(140)));
}

/// A value box beside a slider, as wide as every other.
fn form_value(ui: &mut egui::Ui, value: egui::DragValue<'_>) -> egui::Response {
    ui.add_sized([FORM_VALUE_W, ui.spacing().interact_size.y], value)
}

/// A folder's path after its buttons, cut to the room left - all of it in its hover - or
/// `none` when there's none.
fn form_path(ui: &mut egui::Ui, path: Option<&str>, none: &str) {
    let text = path.unwrap_or(none);
    let shown = ui.add(egui::Label::new(
        egui::RichText::new(text).size(crate::settings::ui_size(FORM_NOTE_PT)).color(crate::settings::tinted_gray(170)),
    ).truncate());
    if path.is_some() {
        shown.on_hover_text(text);
    }
}

/// One system-wide hotkey: its name, and a button showing its chord - click it, then press
/// the new modifier + key, which goes to `value` and to `shared` (the hotkey thread's copy).
/// `what` finishes the button's hover: what pressing the hotkey does. True when it changed.
fn key_shortcut_row(
    ui: &mut egui::Ui, name: &str, what: &str, learning: &mut bool,
    value: &mut settings::PinShortcut, shared: &std::sync::RwLock<settings::PinShortcut>,
) -> bool {
    form_row(ui, name, "", |ui| {
        let text = if *learning { "Press chord…".to_string() } else { value.label() };
        let mut btn = egui::Button::new(egui::RichText::new(text).size(crate::settings::ui_size(12.0)));
        if *learning {
            btn = btn.fill(egui::Color32::from_rgb(80, 60, 30))
                     .stroke(egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(200, 160, 80)));
        }
        let hover = if *learning {
            "Press the new shortcut (modifier + key).\nClick again to cancel.".to_string()
        } else {
            format!("Click to re-bind. Press the shortcut anywhere on the system to {what}.")
        };
        if ui.add(btn).on_hover_text(hover).clicked() {
            *learning = !*learning;
        }
    });
    if !*learning {
        return false;
    }
    let ctx = ui.ctx().clone();
    let pressed: Option<egui::Key> = ctx.input(|i| {
        i.events.iter().find_map(|e| match e {
            egui::Event::Key { key, pressed: true, repeat: false, .. } => Some(*key),
            _ => None,
        })
    });
    let Some(key) = pressed else { return false };
    let m = ctx.input(|i| i.modifiers);
    *value = settings::PinShortcut {
        ctrl:  m.ctrl,
        shift: m.shift,
        alt:   m.alt,
        win:   m.command && !m.ctrl,
        key:   Some(format!("{:?}", key)),
    };
    if let Ok(mut s) = shared.write() {
        *s = value.clone();
    }
    *learning = false;
    true
}

/// The Performance section's live readout: per-second rates differenced from
/// the engine's and the virtual mouse's running counters over half-second
/// windows. `None` until a first window completes.
#[derive(Clone, Copy, Default)]
struct LiveRates {
    at: Option<std::time::Instant>,
    passes: u64,
    ticks: u64,
    mouse: u64,
    outputs: u64,
    passes_hz: Option<u32>,
    ticks_hz: Option<u32>,
    mouse_hz: Option<u32>,
    output_hz: Option<u32>,
}

fn live_rates(ctx: &egui::Context) -> LiveRates {
    use std::time::{Duration, Instant};
    let id = egui::Id::new("settings_live_rates");
    let now = Instant::now();
    let (passes, ticks) = flexinput_engine::engine_counters();
    let mouse = flexinput_virtual::mouse_reports_sent();
    let outputs = super::threads::OUTPUT_PASSES.load(std::sync::atomic::Ordering::Relaxed);
    let mut r: LiveRates = ctx.data(|d| d.get_temp(id)).unwrap_or_default();
    let elapsed = r.at.map(|at| now - at);
    if elapsed.is_none_or(|e| e >= Duration::from_millis(500)) {
        // A window much longer than asked for means the section wasn't on
        // screen: start over rather than show an average over the gap.
        if let Some(e) = elapsed.filter(|e| *e < Duration::from_secs(2)) {
            let per_s = |now: u64, then: u64| {
                Some((now.saturating_sub(then) as f64 / e.as_secs_f64()).round() as u32)
            };
            r.passes_hz = per_s(passes, r.passes);
            r.ticks_hz = per_s(ticks, r.ticks);
            r.mouse_hz = per_s(mouse, r.mouse);
            r.output_hz = per_s(outputs, r.outputs);
        }
        r = LiveRates { at: Some(now), passes, ticks, mouse, outputs, ..r };
        ctx.data_mut(|d| d.insert_temp(id, r));
    }
    ctx.request_repaint_after(Duration::from_millis(250));
    r
}

#[cfg(test)]
mod form_tests {
    use super::*;

    /// A page drawn as the Settings page is: a setting of each kind, names short and long.
    /// Where each control starts goes to `edges`.
    fn sample_page(ui: &mut egui::Ui, edges: &mut Vec<(&'static str, f32)>) {
        form_page(ui.available_width());
        let (mut v, mut w, mut on, mut off) = (0.5_f32, 120.0_f32, true, false);
        settings_section_header(ui, "Display");
        form_subheading(ui, "Appearance");
        form_row(ui, "Contrast", "", |ui| {
            let r = ui.add(egui::Slider::new(&mut v, -1.0..=1.0).show_value(false));
            edges.push(("a slider", r.rect.left()));
            form_value(ui, egui::DragValue::new(&mut v).fixed_decimals(2));
        });
        let r = form_note(ui, "Adjusts panel/widget background lightness. Negative = darker, positive = lighter. Double-click the slider to reset to 0.50.");
        edges.push(("a note", r.rect.left()));
        form_row(ui, "Background repaint rate", "", |ui| {
            let r = ui.add(egui::Slider::new(&mut w, 1.0..=240.0).suffix(" Hz"));
            edges.push(("a slider beside a long name", r.rect.left()));
        });
        form_subheading(ui, "Workspace");
        let r = form_check(ui, "On launch", true, &mut on, "Keep open tabs", "");
        edges.push(("a checkbox", r.rect.left()));
        let r = form_check(ui, "", true, &mut off, "Show FlexInput's own virtual controllers in the physical-devices panel, which wraps", "");
        edges.push(("a checkbox with no name", r.rect.left()));
        form_row(ui, "After FlexInput exits", "", |ui| {
            let r = egui::ComboBox::from_id_salt("keep").selected_text("Only manually hidden").show_ui(ui, |_| {}).response;
            edges.push(("a dropdown", r.rect.left()));
        });
        form_data_row(ui, "loopMIDI Port 1 (a long port name) → loopMIDI Port 1 (a long port name)", "", |ui| {
            let r = ui.label("echoes seen — cancelled");
            edges.push(("a line beside a listed name that wraps", r.rect.left()));
        });
        let r = form_status(ui, "Now: polling 1000 Hz · output 1000 Hz · virtual mouse idle", crate::settings::tinted_gray(170));
        edges.push(("a live readout", r.rect.left()));
        form_row(ui, "User presets", "", |ui| {
            let r = ui.button("Browse…");
            edges.push(("a button", r.rect.left()));
            let _ = ui.add_enabled(false, egui::Button::new("Clear"));
            form_path(ui, Some("C:/Users/someone/Documents/FlexInput/presets/a folder with a long name"), "(none)");
        });
        form_note(ui, "Folder scanned for your own .fxsp sub-patch presets, beside the factory presets shipped under app/assets/sub-patches/.");
    }

    fn draw(ctx: &egui::Context, size: egui::Vec2) -> (Vec<(&'static str, f32)>, egui::Rect, Vec<egui::epaint::ClippedShape>, egui::TexturesDelta) {
        let mut edges = Vec::new();
        let mut page = egui::Rect::NOTHING;
        let mut delta = egui::TexturesDelta::default();
        let mut shapes = Vec::new();
        for _ in 0..3 {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), ..Default::default() };
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    edges.clear();
                    page = ui.max_rect();
                    sample_page(ui, &mut edges);
                });
            });
            delta.append(out.textures_delta);
            shapes = out.shapes;
        }
        (edges, page, shapes, delta)
    }

    /// Every setting's controls start at one edge - a slider, a checkbox, a dropdown, a
    /// button, a note and a live readout alike - just past the widest setting's name: a port
    /// pair's long name wraps in the column rather than pushing its line out or widening the
    /// column for the page.
    #[test]
    fn a_settings_controls_line_up_past_the_names() {
        let ctx = egui::Context::default();
        crate::app::fonts::setup_fonts(&ctx);
        for width in [560.0, 900.0] {
            let (edges, page, _, _) = draw(&ctx, egui::vec2(width, 900.0));
            let (_, first) = edges[0];
            for (what, x) in &edges {
                assert!((x - first).abs() < 0.5, "at {width} wide {what} starts at {x}, the others at {first}: {edges:?}");
            }
            let body = egui::TextStyle::Body.resolve(&ctx.style());
            let widest = ctx.fonts_mut(|f| f.layout_no_wrap("Background repaint rate".into(), body, egui::Color32::WHITE)).size().x;
            let gap = first - page.left() - widest;
            assert!(gap > 4.0 && gap < 20.0, "at {width} wide the controls start {gap} past the widest name");
        }
    }

    /// A picture of the page, for looking at: `cargo test -p flexinput-ui --lib shoot_settings_form -- --ignored`
    /// writes `target/module_shots/settings_form.png`.
    #[test]
    #[ignore = "writes a picture, asserts nothing"]
    fn shoot_settings_form() {
        let ctx = egui::Context::default();
        crate::app::fonts::setup_fonts(&ctx);
        let size = egui::vec2(620.0, 760.0);
        let (_, _, shapes, delta) = draw(&ctx, size);
        let mut shot = crate::canvas::shot::Shot::default();
        shot.absorb(&delta);
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/module_shots/settings_form.png");
        shot.save(&ctx, &shapes, egui::Rect::from_min_size(egui::Pos2::ZERO, size), 1.5, &path).expect("the picture saves");
        println!("{}", path.display());
    }
}
