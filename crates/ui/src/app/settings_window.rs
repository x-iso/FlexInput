//! The desktop Settings modal, and the gamepad-shortcut row widget it
//! shares with the gamepad-native settings panel.

use super::*;

impl FlexInputApp {


    /// One gamepad-shortcut chord row: a label, a Learn button that captures a
    /// button combo (sets `gamepad_nav.chord_learn`), and a clear (✕). Shared by
    /// the desktop Settings window and the gamepad-native settings panel.
    /// Returns true if the setting changed (so the caller marks dirty).
    pub(crate) fn gamepad_shortcut_row(&mut self, ui: &mut egui::Ui, label: &str,
        target: crate::gamepad_nav::ChordTarget) -> bool
    {
        use crate::gamepad_nav::ChordTarget;
        let mut changed = false;
        let learning = self.gamepad_nav.chord_learn == Some(target);
        // Snapshot the assigned combo + presence as owned values so no borrow of
        // self.settings is held across the closure (which mutates self).
        let (assigned_label, has_assigned) = {
            let assigned: Option<&Vec<String>> = match target {
                ChordTarget::SeeThrough    => self.settings.seethrough_chord.as_ref(),
                ChordTarget::Panic         => self.settings.panic_chord.as_ref(),
                ChordTarget::Overlay       => self.settings.overlay_chord.as_ref(),
                ChordTarget::Pin           => self.settings.pin_chord.as_ref(),
                ChordTarget::ConfigOverlay => self.settings.config_overlay_chord.as_ref(),
            };
            (assigned.map(|c| pretty_chord_combo(c)), assigned.is_some())
        };
        let face = if learning {
            "Hold 2+ buttons, release…".to_string()
        } else {
            assigned_label.unwrap_or_else(|| "(none)".to_string())
        };
        ui.horizontal(|ui| {
            ui.label(format!("{label}:"));
            let mut btn = egui::Button::new(egui::RichText::new(face).size(12.0));
            if learning {
                btn = btn.fill(egui::Color32::from_rgb(80, 60, 30))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(200, 160, 80)));
            }
            if ui.add(btn).on_hover_text(
                "Click, then hold a 2+ button gamepad combo and release it to bind.\n\
                 Captured while held; latches when you let go. \
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
                match target {
                    ChordTarget::SeeThrough    => self.settings.seethrough_chord = None,
                    ChordTarget::Panic         => self.settings.panic_chord = None,
                    ChordTarget::Overlay       => self.settings.overlay_chord = None,
                    ChordTarget::Pin           => self.settings.pin_chord = None,
                    ChordTarget::ConfigOverlay => self.settings.config_overlay_chord = None,
                }
                if learning { self.gamepad_nav.chord_learn = None; }
                changed = true;
            }
        });
        changed
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
    #[cfg(windows)]
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

        let installed = self.hidhide_installed;
        let active = self.settings.hide_originals.unwrap_or(installed);
        let mut targets: Vec<(u16, u16)> = if active && installed {
            self.remapped_physical_hid_targets()
        } else {
            Vec::new()
        };
        targets.sort_unstable();
        targets.dedup();

        let prev_targets = std::mem::take(&mut self.hidhide_last_targets);
        if self.hidhide_last_active == Some(active) && prev_targets == targets {
            self.hidhide_last_targets = prev_targets; // unchanged: restore + bail
            return;
        }
        self.hidhide_last_active = Some(active);
        self.hidhide_last_targets = targets.clone();
        // Don't spawn the helper (UAC) just to apply or clear an empty set.
        if targets.is_empty() && prev_targets.is_empty() {
            return;
        }

        // FlexInput's own exe stays whitelisted so it keeps reading the hidden pads.
        let whitelist: Vec<String> = HidHideClient::current_exe_path().into_iter().collect();
        std::thread::spawn(move || {
            // (vid,pid) → HID instance id (slow SetupAPI; safe off the UI/IO thread).
            let blacklist: Vec<String> = targets
                .iter()
                .filter_map(|(vid, pid)| {
                    let id = flexinput_devices::hidhide::instance_id_for_vid_pid(*vid, *pid);
                    eprintln!(
                        "[hidhide] target {:04X}:{:04X} -> instance {:?}",
                        vid, pid, id
                    );
                    id
                })
                .collect();
            match flexinput_hidmaestro::helper::hidhide_apply(&blacklist, &whitelist, active) {
                Ok(st) if !st.present => eprintln!("[hidhide] driver not present at apply time"),
                Ok(st) => eprintln!(
                    "[hidhide] applied: active={} hidden={} (requested {})",
                    st.active, st.hidden.len(), blacklist.len()
                ),
                Err(e) => eprintln!("[hidhide] apply failed: {e}"),
            }
        });
    }

    #[cfg(not(windows))]
    pub(crate) fn reconcile_hidhide(&mut self) {}

    /// (vid,pid) of physical HID controllers in the active patch that feed a
    /// `virtual.*` output. XInput (Xbox) pads are excluded — HidHide can't hide
    /// their XUSB face — as are MIDI devices.
    #[cfg(windows)]
    pub(crate) fn remapped_physical_hid_targets(&self) -> Vec<(u16, u16)> {
        use std::collections::HashSet;
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
                    if let Some(dev_id) = find_automap_device_id_for_viewer(snarl, src, None) {
                        // Both backend prefixes are real physical pads that need
                        // cloaking: `gilrs:` (native path) and `sdl:` (the
                        // route-all-through-SDL switch). Without `sdl:` here, a
                        // pad read through SDL was never blacklisted, so HidHide
                        // silently did nothing in SDL mode and Steam kept seeing
                        // the physical device. The vid/pid → instance-id lookup
                        // downstream already handles both USB and Bluetooth.
                        if dev_id.starts_with("gilrs:") || dev_id.starts_with("sdl:") {
                            phys_ids.insert(dev_id);
                        }
                    }
                }
            }
        }
        // Map device ids → (vid,pid), keeping HID-class pads only.
        let mut out = Vec::new();
        for dev in &self.devices {
            if !phys_ids.contains(&dev.id) {
                continue;
            }
            if matches!(
                dev.kind,
                flexinput_devices::ControllerKind::XInput
                    | flexinput_devices::ControllerKind::MidiIn
                    | flexinput_devices::ControllerKind::MidiOut
            ) {
                continue; // Xbox/XInput can't be hidden; MIDI has no vid/pid
            }
            if let (Some(vid), Some(pid)) = (dev.vid, dev.pid) {
                out.push((vid, pid));
            }
        }
        out
    }

    /// Render the Settings modal. Reads/writes `self.settings`, mirrors live
    /// values into the engine/I-O atomics, and flips `settings_dirty` so the
    /// outer update loop persists settings.json at end of frame.
    pub(crate) fn draw_settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open { return; }
        let mut open = true;
        let mut dirty = false;
        let mut save_workspace = false;

        egui::Window::new("Settings")
            .id(egui::Id::new("settings_window"))
            .collapsible(false)
            .resizable(true)
            .default_size([460.0, 520.0])
            .max_size(egui::vec2(560.0, 720.0))
            .open(&mut open)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("FlexInput");
                    ui.label(egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                        .small().color(egui::Color32::from_gray(170)));
                });
                ui.add_space(8.0);
                // Settings content grew tall enough to fall off screen on
                // common 1080p / smaller displays — wrap in a scroll area
                // so users can reach the bottom sections (Workspace,
                // Device defaults, Links, Credits) without resizing the
                // window manually.
                egui::ScrollArea::vertical().show(ui, |ui| {
                ui.separator();
                ui.add_space(6.0);

                // ── Performance ─────────────────────────────────────────
                ui.label(egui::RichText::new("Performance").strong());
                ui.add_space(4.0);

                ui.horizontal(|ui| {
                    ui.label("Max polling rate")
                        .on_hover_text("Upper bound for the I/O loop, and the rate the virtual Xbox 360 (HIDMaestro XInput) delivers to games. Steps are whole-millisecond periods (the driver's resolution): 1000=1ms … 125=8ms. Actual per-device input rate depends on the device — see the live Hz on each device header.");
                    // The slider drives the STEP INDEX (0..=7), not the raw Hz, so
                    // the handle is evenly spaced and snaps to a valid whole-ms
                    // period *while* dragging (an in-between rate would just round
                    // on the driver side anyway). The custom formatter shows the Hz
                    // for the current step instead of the bare index.
                    //
                    // POLLING_HZ_STEPS is DESCENDING (index 0 = 1000 Hz), but the
                    // slider should read low→high left→right. So the slider drives
                    // a POSITION = last - index; 125 Hz lands at the left, 1000 Hz
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
                        flexinput_virtual::set_requested_poll_hz(self.settings.polling_hz);
                        dirty = true;
                    }
                });
                ui.label(egui::RichText::new(
                    "How often the I/O thread polls gamepads and MIDI devices, and how fast the virtual Xbox 360 reports to games."
                ).small().color(egui::Color32::from_gray(140)));

                ui.add_space(8.0);

                if ui.checkbox(&mut self.settings.sdl_all_pads, "Route all pads through SDL")
                    .on_hover_text("Diagnostic: read EVERY controller through SDL instead of the native gilrs/HID paths. Lets a pad with a native parser be compared against SDL, and surfaces SDL-only devices. Changes device IDs, so re-wire after toggling. Off = normal native handling.")
                    .changed()
                {
                    self.sdl_all_pads.store(self.settings.sdl_all_pads, Ordering::Relaxed);
                    dirty = true;
                }

                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    ui.label("MIDI devices");
                    if ui.button("Refresh MIDI").clicked() {
                        self.midi_refresh_requested.store(true, Ordering::Release);
                    }
                });
                ui.label(egui::RichText::new(
                    "MIDI ports are scanned once at startup and on demand. \
                     Auto-scanning is disabled because the Windows MIDI API periodically \
                     disrupts the audio stack (stream skips, Bluetooth noise). \
                     Click after plugging in or creating a MIDI/loopMIDI port."
                ).small().color(egui::Color32::from_gray(140)));

                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    ui.label("Processing sample rate");
                    let resp = ui.add(egui::Slider::new(
                        &mut self.settings.sample_rate_hz,
                        settings::SAMPLE_RATE_HZ_MIN..=settings::SAMPLE_RATE_HZ_MAX,
                    ).suffix(" Hz"));
                    if resp.changed() {
                        self.sample_rate_hz.store(self.settings.sample_rate_hz, Ordering::Relaxed);
                        dirty = true;
                    }
                });
                ui.label(egui::RichText::new(
                    "Engine tick rate. Higher = lower latency, more CPU."
                ).small().color(egui::Color32::from_gray(140)));

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("Background repaint rate")
                        .on_hover_text("Repaint rate applied when the window is minimized or another app has focus. The focused window always paints at vsync regardless.");
                    let resp = ui.add(egui::Slider::new(
                        &mut self.settings.bg_repaint_hz,
                        settings::BG_REPAINT_HZ_MIN..=settings::BG_REPAINT_HZ_MAX,
                    ).suffix(" Hz"));
                    if resp.changed() {
                        dirty = true;
                    }
                });
                ui.label(egui::RichText::new(
                    "How often the UI repaints while FlexInput sits in the background. Lower = less CPU while you play a game, higher = smoother glanceable visuals. Has no effect when the window is focused."
                ).small().color(egui::Color32::from_gray(140)));

                ui.add_space(8.0);
                if ui.checkbox(&mut self.settings.mouse_suppression_enabled,
                    "Pause virtual mouse when you move a physical mouse")
                    .on_hover_text(
                        "When on, the virtual mouse briefly yields if it detects the real \
                         cursor moving on its own, so a stick-driven cursor doesn't fight a \
                         physical mouse on the desktop. Automatically disabled while a virtual \
                         gamepad is also active (mixed mode), since games that warp the cursor \
                         would otherwise make virtual mouse aim stutter.")
                    .changed()
                {
                    flexinput_virtual::set_mouse_suppression_enabled(self.settings.mouse_suppression_enabled);
                    dirty = true;
                }
                ui.add_enabled_ui(self.settings.mouse_suppression_enabled, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Release after");
                        let resp = ui.add(egui::Slider::new(
                            &mut self.settings.mouse_suppress_release_ms,
                            settings::MOUSE_SUPPRESS_RELEASE_MS_MIN..=settings::MOUSE_SUPPRESS_RELEASE_MS_MAX,
                        ).suffix(" ms"));
                        if resp.changed() {
                            flexinput_virtual::set_mouse_suppression_release_ms(self.settings.mouse_suppress_release_ms);
                            dirty = true;
                        }
                    });
                });
                ui.label(egui::RichText::new(
                    "How long the virtual mouse stays paused after a physical-mouse move. Lower = recovers faster."
                ).small().color(egui::Color32::from_gray(140)));

                ui.add_space(8.0);
                if ui.checkbox(&mut self.settings.mixed_braid_enabled,
                    "Braid mixed output (experimental)")
                    .on_hover_text(
                        "Phase-offset WHEN the virtual gamepad and keyboard/mouse packets \
                         land, so they interleave instead of co-occurring — without muting \
                         either stream (an idle mouse won't chop the pad). For probing games \
                         whose input arbiter behaves differently under simultaneous mixed \
                         output. Effect is game-specific; leave off unless experimenting.")
                    .changed()
                {
                    flexinput_virtual::set_braid_enabled(self.settings.mixed_braid_enabled);
                    dirty = true;
                }
                ui.add_enabled_ui(self.settings.mixed_braid_enabled, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Braid pacing");
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
                            dirty = true;
                        }
                    });
                });
                ui.label(egui::RichText::new(
                    "Gamepad and mouse packets strictly alternate (never coincident). Real-time = fastest/lowest latency; lower rates pace the alternation. Sweep to probe the game."
                ).small().color(egui::Color32::from_gray(140)));

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // ── 3D controller models ───────────────────────────────
                ui.label(egui::RichText::new("3D controller models").strong());
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("User models folder")
                        .on_hover_text(
                            "Optional folder with your own controller models. Same \
                             structure as the bundled ones: <Name>/info.txt + .obj files \
                             (+ optional colors.fxcol). A same-named folder overrides the \
                             bundled model.",
                        );
                    let cur = self.settings.user_models_dir.clone().unwrap_or_default();
                    ui.label(
                        egui::RichText::new(if cur.is_empty() { "(none)" } else { &cur })
                            .small()
                            .color(egui::Color32::from_gray(160)),
                    );
                    if ui.small_button("Browse…").clicked() {
                        if let Some(p) = crate::overlay::with_overlay_not_topmost(|| rfd::FileDialog::new().pick_folder()) {
                            let s = p.to_string_lossy().to_string();
                            self.settings.user_models_dir = Some(s);
                            crate::model::set_user_models_dir(Some(p));
                            dirty = true;
                        }
                    }
                    if !cur.is_empty() && ui.small_button("Clear").clicked() {
                        self.settings.user_models_dir = None;
                        crate::model::set_user_models_dir(None);
                        dirty = true;
                    }
                });

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // ── Gamepad UI navigation ──────────────────────────────
                ui.label(egui::RichText::new("Gamepad UI navigation").strong());
                ui.add_space(4.0);
                if ui.checkbox(&mut self.settings.gamepad_ui_nav_default,
                    "Enable gamepad UI navigation by default")
                    .on_hover_text(
                        "When on, newly-seen gamepads start in UI-navigation mode: while \
                         FlexInput holds focus the controller drives FlexInput's own UI and \
                         its mapped output is suppressed. Alt-tab away and mappings resume \
                         automatically. Toggle per-device on each gamepad card."
                    )
                    .changed()
                {
                    dirty = true;
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("Cursor max speed")
                        .on_hover_text("Top speed of the right-stick/gyro nav cursor at full deflection (px/s). The actual speed follows the acceleration curve below up to this cap.");
                    let resp = ui.add(egui::Slider::new(
                        &mut self.settings.cursor_max_speed, 1000.0..=30000.0)
                        .suffix(" px/s"));
                    if resp.changed() { dirty = true; }
                });
                ui.horizontal(|ui| {
                    ui.label("Cursor acceleration")
                        .on_hover_text("How the cursor speed ramps with stick deflection. 1.0 = linear; higher = slower start and a faster top end (deflection raised to this power).");
                    let mut a = self.settings.cursor_accel;
                    let resp = ui.add(egui::Slider::new(&mut a, 1.0..=4.0)
                        .fixed_decimals(2));
                    if resp.double_clicked() { a = 2.0; }
                    if (a - self.settings.cursor_accel).abs() > f32::EPSILON {
                        self.settings.cursor_accel = a.clamp(1.0, 4.0);
                        dirty = true;
                    }
                });

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // ── Appearance ──────────────────────────────────────────
                ui.label(egui::RichText::new("Appearance").strong());
                ui.add_space(4.0);
                // Theme switching is not yet plumbed through every custom-
                // painted UI element (canvas viewer overlays, calibration
                // scope backgrounds, response-curve grid, etc.), so the
                // app is dark-only for now. The contrast slider still
                // works on the dark theme.
                ui.horizontal(|ui| {
                    ui.label("Contrast:");
                    let mut c = self.settings.contrast;
                    let resp = ui.add(egui::Slider::new(&mut c, -1.0_f32..=1.0)
                        .show_value(false)
                        .clamping(egui::SliderClamping::Always));
                    if resp.double_clicked() { c = 0.0; }
                    ui.add(egui::DragValue::new(&mut c)
                        .speed(0.01)
                        .range(-1.0_f32..=1.0)
                        .fixed_decimals(2));
                    if (c - self.settings.contrast).abs() > f32::EPSILON {
                        self.settings.contrast = c.clamp(-1.0, 1.0);
                        dirty = true;
                    }
                });
                ui.label(egui::RichText::new(
                    "Adjusts panel/widget background lightness. Negative = darker, positive = lighter. Double-click the slider to reset."
                ).small().color(egui::Color32::from_gray(140)));

                // See-through opacity is set via the popover slider that
                // appears when you hover the eye icon next to the zoom
                // controls — no longer surfaced here to keep Settings
                // focused on durable preferences.

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // ── Always-on-top pin ──────────────────────────────────
                ui.label(egui::RichText::new("Always-on-top pin").strong());
                ui.add_space(4.0);

                // Shortcut binder (mirrors the panic-mode binder pattern).
                ui.horizontal(|ui| {
                    ui.label("Pin shortcut:");
                    let btn_text = if self.pin_learning {
                        "Press chord…".to_string()
                    } else {
                        self.settings.pin_shortcut.label()
                    };
                    let mut btn = egui::Button::new(egui::RichText::new(btn_text).size(12.0));
                    if self.pin_learning {
                        btn = btn.fill(egui::Color32::from_rgb(80, 60, 30))
                                 .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(200, 160, 80)));
                    }
                    let resp = ui.add(btn).on_hover_text(
                        if self.pin_learning {
                            "Press the new shortcut (modifier + key).\nClick again to cancel."
                        } else {
                            "Click to re-bind. Press the shortcut anywhere on the system to toggle the pin."
                        }
                    );
                    if resp.clicked() {
                        self.pin_learning = !self.pin_learning;
                    }
                });
                if self.pin_learning {
                    let pressed: Option<egui::Key> = ctx.input(|i| {
                        i.events.iter().find_map(|e| match e {
                            egui::Event::Key { key, pressed: true, repeat: false, .. } => Some(*key),
                            _ => None,
                        })
                    });
                    if let Some(key) = pressed {
                        let m = ctx.input(|i| i.modifiers);
                        let key_name = format!("{:?}", key);
                        self.settings.pin_shortcut = settings::PinShortcut {
                            ctrl:  m.ctrl,
                            shift: m.shift,
                            alt:   m.alt,
                            win:   m.command && !m.ctrl,
                            key:   Some(key_name),
                        };
                        if let Ok(mut s) = self.pin_shortcut_shared.write() {
                            *s = self.settings.pin_shortcut.clone();
                        }
                        self.pin_learning = false;
                        dirty = true;
                    }
                }

                ui.add_space(6.0);
                if ui.checkbox(&mut self.settings.focus_flip_flop,
                    "Flip-flop focus on pin toggle")
                    .on_hover_text(
                        "Pin ON: remember the previously-focused window.\n\
                         Pin OFF: return focus to it.\n\
                         Lets you press the shortcut, tweak, press again, and instantly \
                         resume testing the target app."
                    )
                    .changed()
                {
                    dirty = true;
                }


                // Push live state changes to the Guide watcher (which now summons
                // the CONFIG overlay) whenever the user toggles its options below.
                if let Ok(mut cfg) = self.pin_guide_cfg.write() {
                    cfg.enabled = self.settings.config_via_guide;
                    cfg.require_double_tap = self.settings.config_guide_double_tap;
                    cfg.chord_signal = self.settings.config_guide_chord.clone();
                }

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // ── Overlays ────────────────────────────────────────────
                ui.label(egui::RichText::new("Overlays").strong());
                ui.add_space(4.0);

                // Show info-overlay shortcut (mirrors the pin binder above).
                ui.horizontal(|ui| {
                    ui.label("Show info overlay:");
                    let btn_text = if self.overlay_learning {
                        "Press chord…".to_string()
                    } else {
                        self.settings.overlay_shortcut.label()
                    };
                    let mut btn = egui::Button::new(egui::RichText::new(btn_text).size(12.0));
                    if self.overlay_learning {
                        btn = btn.fill(egui::Color32::from_rgb(80, 60, 30))
                                 .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(200, 160, 80)));
                    }
                    let resp = ui.add(btn).on_hover_text(
                        if self.overlay_learning {
                            "Press the new shortcut (modifier + key).\nClick again to cancel."
                        } else {
                            "Click to re-bind. Press the shortcut anywhere on the system to show/hide the overlay."
                        }
                    );
                    if resp.clicked() {
                        self.overlay_learning = !self.overlay_learning;
                    }
                });
                if self.overlay_learning {
                    let pressed: Option<egui::Key> = ctx.input(|i| {
                        i.events.iter().find_map(|e| match e {
                            egui::Event::Key { key, pressed: true, repeat: false, .. } => Some(*key),
                            _ => None,
                        })
                    });
                    if let Some(key) = pressed {
                        let m = ctx.input(|i| i.modifiers);
                        let key_name = format!("{:?}", key);
                        self.settings.overlay_shortcut = settings::PinShortcut {
                            ctrl:  m.ctrl,
                            shift: m.shift,
                            alt:   m.alt,
                            win:   m.command && !m.ctrl,
                            key:   Some(key_name),
                        };
                        if let Ok(mut s) = self.overlay_shortcut_shared.write() {
                            *s = self.settings.overlay_shortcut.clone();
                        }
                        self.overlay_learning = false;
                        dirty = true;
                    }
                }

                ui.add_space(4.0);

                // Edit info-overlay shortcut (mirrors the show binder; toggles
                // the overlay's layout-edit mode and shows it).
                ui.horizontal(|ui| {
                    ui.label("Edit info overlay:");
                    let btn_text = if self.edit_overlay_learning {
                        "Press chord…".to_string()
                    } else {
                        self.settings.edit_overlay_shortcut.label()
                    };
                    let mut btn = egui::Button::new(egui::RichText::new(btn_text).size(12.0));
                    if self.edit_overlay_learning {
                        btn = btn.fill(egui::Color32::from_rgb(80, 60, 30))
                                 .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(200, 160, 80)));
                    }
                    let resp = ui.add(btn).on_hover_text(
                        if self.edit_overlay_learning {
                            "Press the new shortcut (modifier + key).\nClick again to cancel."
                        } else {
                            "Click to re-bind. Press the shortcut anywhere on the system to enter/exit the overlay's edit mode."
                        }
                    );
                    if resp.clicked() {
                        self.edit_overlay_learning = !self.edit_overlay_learning;
                    }
                });
                if self.edit_overlay_learning {
                    let pressed: Option<egui::Key> = ctx.input(|i| {
                        i.events.iter().find_map(|e| match e {
                            egui::Event::Key { key, pressed: true, repeat: false, .. } => Some(*key),
                            _ => None,
                        })
                    });
                    if let Some(key) = pressed {
                        let m = ctx.input(|i| i.modifiers);
                        let key_name = format!("{:?}", key);
                        self.settings.edit_overlay_shortcut = settings::PinShortcut {
                            ctrl:  m.ctrl,
                            shift: m.shift,
                            alt:   m.alt,
                            win:   m.command && !m.ctrl,
                            key:   Some(key_name),
                        };
                        if let Ok(mut s) = self.edit_overlay_shortcut_shared.write() {
                            *s = self.settings.edit_overlay_shortcut.clone();
                        }
                        self.edit_overlay_learning = false;
                        dirty = true;
                    }
                }

                ui.add_space(4.0);

                // Config-overlay toggle shortcut (M3; mirrors the overlay binder).
                ui.horizontal(|ui| {
                    ui.label("Show config overlay:");
                    let btn_text = if self.config_overlay_learning {
                        "Press chord…".to_string()
                    } else {
                        self.settings.config_overlay_shortcut.label()
                    };
                    let mut btn = egui::Button::new(egui::RichText::new(btn_text).size(12.0));
                    if self.config_overlay_learning {
                        btn = btn.fill(egui::Color32::from_rgb(80, 60, 30))
                                 .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(200, 160, 80)));
                    }
                    let resp = ui.add(btn).on_hover_text(
                        if self.config_overlay_learning {
                            "Press the new shortcut (modifier + key).\nClick again to cancel."
                        } else {
                            "Click to re-bind. Press the shortcut anywhere on the system to show/hide the config overlay."
                        }
                    );
                    if resp.clicked() {
                        self.config_overlay_learning = !self.config_overlay_learning;
                    }
                });
                if self.config_overlay_learning {
                    let pressed: Option<egui::Key> = ctx.input(|i| {
                        i.events.iter().find_map(|e| match e {
                            egui::Event::Key { key, pressed: true, repeat: false, .. } => Some(*key),
                            _ => None,
                        })
                    });
                    if let Some(key) = pressed {
                        let m = ctx.input(|i| i.modifiers);
                        let key_name = format!("{:?}", key);
                        self.settings.config_overlay_shortcut = settings::PinShortcut {
                            ctrl:  m.ctrl,
                            shift: m.shift,
                            alt:   m.alt,
                            win:   m.command && !m.ctrl,
                            key:   Some(key_name),
                        };
                        if let Ok(mut s) = self.config_overlay_shortcut_shared.write() {
                            *s = self.settings.config_overlay_shortcut.clone();
                        }
                        self.config_overlay_learning = false;
                        dirty = true;
                    }
                }

                ui.add_space(4.0);
                // Guide-button summon for the config overlay. Unlike the gamepad
                // shortcut chords below (which only fire while FlexInput is
                // focused), this watches every pad from a background thread, so it
                // works while a GAME holds focus — the intended way to bring up the
                // overlay mid-play. Default on; the pin's old Guide binding moved
                // to a user-assignable gamepad chord (Settings → Gamepad shortcuts).
                if ui.checkbox(&mut self.settings.config_via_guide,
                    "Summon config overlay with controller Guide / PS / Home button")
                    .on_hover_text(
                        "Watches every connected gamepad for a Guide-button press, even \
                         while a game is focused.\nStandard XInput on Windows does NOT expose \
                         the Guide bit on Xbox controllers — it works for DualSense via HID, \
                         virtual ViGEm pads, and most non-Microsoft controllers."
                    )
                    .changed()
                {
                    dirty = true;
                }
                ui.add_enabled_ui(self.settings.config_via_guide, |ui| {
                    if ui.checkbox(&mut self.settings.config_guide_double_tap,
                        "    Require double-tap")
                        .on_hover_text(
                            "Recommended: dodges collisions with Steam / Game Bar's own \
                             single-press Guide-button handling.\nTwo taps within ~300 ms."
                        )
                        .changed()
                    {
                        dirty = true;
                    }

                    // Optional chord button held WITH Guide (AutoMap-style learn).
                    ui.horizontal(|ui| {
                        ui.label("    Chord button:");
                        let learning = self.pin_learn_chord.load(Ordering::Relaxed);
                        let face = if learning {
                            "Press a button…".to_string()
                        } else {
                            self.settings.config_guide_chord
                                .as_deref()
                                .map(pretty_chord_name)
                                .unwrap_or_else(|| "(none)".to_string())
                        };
                        let mut btn = egui::Button::new(
                            egui::RichText::new(face).size(12.0));
                        if learning {
                            btn = btn.fill(egui::Color32::from_rgb(80, 60, 30))
                                .stroke(egui::Stroke::new(1.0,
                                    egui::Color32::from_rgb(200, 160, 80)));
                        }
                        let resp = ui.add(btn).on_hover_text(
                            "Optional button that must be held WITH the Guide press\n\
                             for the overlay to summon. Useful to dodge Steam / Game Bar.\n\
                             Click to (re)bind; press any controller button to capture.");
                        if resp.clicked() {
                            let new_state = !learning;
                            self.pin_learn_chord.store(new_state, Ordering::Relaxed);
                            if let Ok(mut g) = self.pin_learned_chord.lock() {
                                *g = None;
                            }
                        }
                        if self.settings.config_guide_chord.is_some()
                            && ui.small_button("✕")
                                .on_hover_text("Clear chord — Guide alone fires")
                                .clicked()
                        {
                            self.settings.config_guide_chord = None;
                            dirty = true;
                        }
                    });

                    // Consume any newly-learned chord this frame.
                    let learned: Option<String> = self.pin_learned_chord
                        .lock().ok().and_then(|mut g| g.take());
                    if let Some(name) = learned {
                        self.settings.config_guide_chord = Some(name);
                        dirty = true;
                    }
                });

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Overlay frame rate")
                        .on_hover_text("Repaint rate of the overlay while it's visible. Separate from the background repaint rate — the overlay animates on top of your game.");
                    let resp = ui.add(egui::Slider::new(
                        &mut self.settings.overlay_fps,
                        settings::OVERLAY_FPS_MIN..=settings::OVERLAY_FPS_MAX,
                    ).suffix(" FPS"));
                    if resp.changed() {
                        dirty = true;
                    }
                });
                ui.label(egui::RichText::new(
                    "How smoothly pinned elements animate on the overlay. Higher = smoother glow, a bit more CPU/GPU while the overlay is shown."
                ).small().color(egui::Color32::from_gray(140)));

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // ── Gamepad shortcuts ───────────────────────────────────
                // Assign a gamepad button combo to toggle see-through / panic.
                // Learned by clicking Learn then pressing+releasing a combo.
                ui.label(egui::RichText::new("Gamepad shortcuts").strong());
                ui.add_space(4.0);
                if self.gamepad_shortcut_row(ui, "See-through", crate::gamepad_nav::ChordTarget::SeeThrough) {
                    dirty = true;
                }
                if self.gamepad_shortcut_row(ui, "Panic mode", crate::gamepad_nav::ChordTarget::Panic) {
                    dirty = true;
                }
                if self.gamepad_shortcut_row(ui, "Overlay", crate::gamepad_nav::ChordTarget::Overlay) {
                    dirty = true;
                }
                if self.gamepad_shortcut_row(ui, "Config overlay", crate::gamepad_nav::ChordTarget::ConfigOverlay) {
                    dirty = true;
                }
                if self.gamepad_shortcut_row(ui, "Pin (always-on-top)", crate::gamepad_nav::ChordTarget::Pin) {
                    dirty = true;
                }
                ui.add_space(2.0);
                if ui.checkbox(&mut self.settings.gamepad_chords_nav_only,
                    "Only when in gamepad navigation mode")
                    .on_hover_text(
                        "On: these combos fire only while the driving gamepad is in UI-navigation mode \
                         (so the same buttons stay free for in-game mappings otherwise).\n\
                         Off: they fire from any connected gamepad whenever FlexInput is focused.")
                    .changed()
                {
                    dirty = true;
                }

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // ── Workspace ───────────────────────────────────────────
                ui.label(egui::RichText::new("Workspace").strong());
                ui.add_space(4.0);
                let resp = ui.checkbox(&mut self.settings.keep_workspace,
                    "Keep open tabs on next launch");
                if resp.changed() {
                    dirty = true;
                    if self.settings.keep_workspace {
                        save_workspace = true;
                    } else {
                        settings::delete_workspace();
                    }
                }
                ui.label(egui::RichText::new(
                    "When enabled, the current tabs (including unsaved patches) are restored on the next launch."
                ).small().color(egui::Color32::from_gray(140)));

                ui.add_space(6.0);
                if ui.checkbox(&mut self.settings.device_nodes_default_collapsed,
                    "Collapse new device nodes by default").changed()
                {
                    dirty = true;
                }
                ui.label(egui::RichText::new(
                    "New physical / virtual device nodes spawn collapsed. The header (icon, status, Auto-Map) stays visible."
                ).small().color(egui::Color32::from_gray(140)));

                ui.add_space(6.0);
                if ui.checkbox(
                    &mut self.settings.show_own_virtuals_as_physical,
                    "Show FlexInput's own virtual controllers in the physical-devices panel",
                ).changed() {
                    dirty = true;
                }
                ui.label(egui::RichText::new(
                    "Off by default. Turn on to test patches against your own virtual output (loopback).",
                ).small().color(egui::Color32::from_gray(140)));

                ui.add_space(6.0);
                if ui.checkbox(
                    &mut self.settings.persist_virtual_devices,
                    "Keep HIDMaestro virtual controllers alive after FlexInput closes",
                ).changed() {
                    dirty = true;
                    #[cfg(windows)]
                    flexinput_hidmaestro::helper::set_persist(self.settings.persist_virtual_devices);
                }
                ui.label(egui::RichText::new(
                    "Off by default: virtual pads are removed when the app closes or crashes. \
                     Turn on to keep a HIDMaestro controller (DualShock 4 / DualSense) registered \
                     across an app restart or update, so a game doesn't lose the device \u{2014} \
                     FlexInput reclaims it on next launch. While FlexInput is closed the pad sends \
                     no input (it resumes when the app reopens).\n\
                     Does not apply to ViGEm devices (Virtual Xbox / DualShock 4 (ViGEmBus)): \
                     ViGEmBus removes those automatically when FlexInput exits and they can't be \
                     reclaimed — they're re-created fresh on next launch.",
                ).small().color(egui::Color32::from_gray(140)));

                // ── Hide originals from games (HidHide) ──────────────────
                ui.add_space(6.0);
                let hidhide_installed = self.hidhide_installed;
                // Effective value: explicit user choice, else default ON when the
                // HidHide driver is installed (the requested default), OFF otherwise.
                let mut hide_effective = self.settings.hide_originals.unwrap_or(hidhide_installed);
                let resp = ui.add_enabled(
                    hidhide_installed,
                    egui::Checkbox::new(
                        &mut hide_effective,
                        "Hide original controllers from games (HidHide)",
                    ),
                );
                if resp.changed() {
                    self.settings.hide_originals = Some(hide_effective);
                    dirty = true;
                    self.hidhide_dirty = true; // apply on the next frame's reconcile
                }
                if hidhide_installed {
                    ui.label(egui::RichText::new(
                        "Defaults ON because HidHide is installed. When on, any physical controller \
                         you remap to a virtual output is hidden from games, so the game sees only \
                         the virtual pad. FlexInput stays whitelisted so it still reads the original. \
                         Masking is cleared automatically when FlexInput closes. \u{2014} Note: only \
                         HID controllers (DualShock 4 / DualSense / Switch) can be hidden; the \
                         XInput/XUSB face of Xbox controllers cannot.",
                    ).small().color(egui::Color32::from_gray(140)));
                } else {
                    ui.label(egui::RichText::new(
                        "HidHide driver not installed. Install HidHide (nefarius/HidHide) to hide \
                         original controllers from games. Without it, games may see both the physical \
                         pad and the virtual one.",
                    ).small().color(egui::Color32::from_rgb(210, 150, 90)));
                }

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Reinstall HIDMaestro drivers").clicked() {
                        self.reinstall_confirm_open = true;
                    }
                    if ui.button("Uninstall HIDMaestro drivers").clicked() {
                        self.uninstall_confirm_open = true;
                    }
                });
                ui.label(egui::RichText::new(
                    "Reinstall: removes and reinstalls the driver, then re-deploys the virtual \
                     controllers on your canvas — use this if virtual DS4/DualSense stop working \
                     after a Windows or app update. Uninstall: removes the HIDMaestro driver and \
                     all its virtual controllers (gamepads will need a reinstall to work again). \
                     Both prompt for admin once.",
                ).small().color(egui::Color32::from_gray(140)));
                if let Some(err) = &self.last_device_op_error {
                    ui.label(egui::RichText::new(format!("Last device error: {err}"))
                        .small().color(egui::Color32::from_rgb(220, 120, 120)));
                }

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("On patch load:");
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
                                dirty = true;
                            }
                        });
                });
                ui.label(egui::RichText::new(
                    "Behavior when a .fxp is loaded into a tab. 'Off' preserves the current view."
                ).small().color(egui::Color32::from_gray(140)));

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // ── Easy mode ───────────────────────────────────────────
                ui.label(egui::RichText::new("Easy mode").strong());
                ui.add_space(4.0);
                ui.label(egui::RichText::new(
                    "Folder scanned for user-authored .fxsp sub-patch presets, in addition to the factory presets shipped under app/assets/sub-patches/."
                ).small().color(egui::Color32::from_gray(140)));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("User presets folder:");
                    let label = self.settings.user_presets_folder.as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "(none)".into());
                    ui.monospace(label);
                    if ui.button("Browse…").clicked() {
                        if let Some(p) = crate::overlay::with_overlay_not_topmost(|| rfd::FileDialog::new().pick_folder()) {
                            self.settings.user_presets_folder = Some(p);
                            dirty = true;
                        }
                    }
                    if self.settings.user_presets_folder.is_some()
                        && ui.button("Clear").clicked()
                    {
                        self.settings.user_presets_folder = None;
                        dirty = true;
                    }
                });

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // ── Device defaults ─────────────────────────────────────
                // Per-device sliders in the node header are seeded from these
                // when a node is first added. Editing them later updates only
                // the affected node, not these defaults.
                ui.label(egui::RichText::new("Device defaults").strong());
                ui.add_space(4.0);
                ui.label(egui::RichText::new(
                    "Applied to newly-added device nodes. Existing nodes keep their own values."
                ).small().color(egui::Color32::from_gray(140)));
                ui.add_space(4.0);

                ui.label(egui::RichText::new("Double-click the slider track to reset to factory default. Double-click the value to type a number.")
                    .small().italics().color(egui::Color32::from_gray(120)));
                ui.add_space(4.0);

                // egui Slider widgets only sense drag, so Response::double_clicked()
                // never fires on the track. Overlay a click-sense interact on the
                // same rect with a derived id and read the double-click from there.
                fn track_dbl(ui: &egui::Ui, r: &egui::Response) -> bool {
                    let id = r.id.with("__dblclick_overlay");
                    ui.interact(r.rect, id, egui::Sense::click()).double_clicked()
                }

                egui::Grid::new("device-defaults-grid")
                    .num_columns(3)
                    .spacing([10.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Default Stick deadzone");
                        let r = ui.add(egui::Slider::new(&mut self.settings.default_stick_deadzone, 0.0_f32..=0.5)
                            .show_value(false)
                            .clamping(egui::SliderClamping::Always));
                        if r.changed() { dirty = true; }
                        if track_dbl(ui, &r) {
                            self.settings.default_stick_deadzone = 0.1;
                            dirty = true;
                        }
                        if ui.add(egui::DragValue::new(&mut self.settings.default_stick_deadzone)
                            .speed(0.005)
                            .range(0.0_f32..=0.5)
                            .fixed_decimals(2))
                            .changed() { dirty = true; }
                        ui.end_row();

                        ui.label("Default Gyro ×");
                        let r = ui.add(egui::Slider::new(&mut self.settings.default_gyro_mult, 0.1_f32..=50.0)
                            .logarithmic(true)
                            .show_value(false)
                            .clamping(egui::SliderClamping::Always));
                        if r.changed() { dirty = true; }
                        if track_dbl(ui, &r) {
                            self.settings.default_gyro_mult = 1.0;
                            dirty = true;
                        }
                        if ui.add(egui::DragValue::new(&mut self.settings.default_gyro_mult)
                            .speed(0.05)
                            .range(0.1_f32..=50.0)
                            .fixed_decimals(2))
                            .changed() { dirty = true; }
                        ui.end_row();

                        ui.label("Default Mouse ×");
                        let r = ui.add(egui::Slider::new(&mut self.settings.default_mouse_sensitivity, 0.0_f32..=3000.0)
                            .logarithmic(true)
                            .smallest_positive(0.01)
                            .show_value(false)
                            .clamping(egui::SliderClamping::Always));
                        if r.changed() { dirty = true; }
                        if track_dbl(ui, &r) {
                            self.settings.default_mouse_sensitivity = 100.0;
                            dirty = true;
                        }
                        if ui.add(egui::DragValue::new(&mut self.settings.default_mouse_sensitivity)
                            .speed(0.5)
                            .range(0.0_f32..=3000.0)
                            .fixed_decimals(2))
                            .changed() { dirty = true; }
                        ui.end_row();

                        // Default rumble-forwarding shape for virtual pads whose
                        // Rumble control hasn't been touched. Same widgets as the
                        // node control; double-click resets to neutral.
                        ui.label("Default Rumble");
                        {
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
                                dirty = true;
                            }
                        }
                        ui.end_row();
                    });

                // ── Renderer ─────────────────────────────────────────────
                // Backend is fixed at startup (the wgpu instance/surface can't
                // be swapped live), so changes here take effect on restart.
                // Auto steers AMD GPUs on Windows to OpenGL — their Vulkan
                // swapchain stalls for seconds on window resize/restore
                // (see `auto_backends` in app/src/main.rs).
                {
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(6.0);

                    ui.label(egui::RichText::new("Renderer").strong());
                    ui.add_space(4.0);
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
                        dirty = true;
                    }
                    ui.label(egui::RichText::new(
                        "Takes effect after restarting FlexInput. Auto leads with \
                         DirectX 12 on AMD (the only backend there that both shows \
                         see-through mode and survives sleep/wake) and Vulkan on \
                         other GPUs, then falls back automatically if a backend fails \
                         to start. On AMD, see-through mode needs DirectX 12 — the AMD \
                         Vulkan driver doesn't support transparent windows and its \
                         OpenGL path crashes on wake from sleep."
                    ).small().weak());
                }

                // ── Profiler (dev tool, debug builds only) ──────────────
                // Toggle flips `puffin::set_scopes_on()` and starts/stops
                // a `puffin_http` server on 127.0.0.1:8585 so the
                // standalone `puffin_viewer` GUI can connect for a live
                // flamegraph. Not persisted — resets to off on every
                // launch. Hidden from release builds entirely so the
                // shipped UI doesn't expose an internal dev tool.
                #[cfg(debug_assertions)]
                {
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(6.0);

                    ui.label(egui::RichText::new("Profiler").strong());
                    ui.add_space(4.0);
                    let mut prof = self.settings.profiler_enabled;
                    if ui.checkbox(&mut prof, "Enable puffin profiler (127.0.0.1:8585)").changed() {
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
                    ui.label(egui::RichText::new(
                        "Install once: `cargo install puffin_viewer`.\n\
                         Then run `puffin_viewer --url 127.0.0.1:8585` with this toggle on."
                    ).small().weak());
                }

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // ── Links ───────────────────────────────────────────────
                ui.label(egui::RichText::new("Links").strong());
                ui.add_space(4.0);
                ui.hyperlink_to("FlexInput repository",      "https://github.com/x-iso/FlexInput");
                ui.hyperlink_to("HIDMaestro — virtual HID driver", "https://github.com/hifihedgehog/HIDMaestro");
                ui.hyperlink_to("HidHide — latest release",  "https://github.com/nefarius/HidHide/releases/latest");

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // ── Credits ─────────────────────────────────────────────
                ui.label(egui::RichText::new("Credits").strong());
                ui.add_space(4.0);
                ui.label(egui::RichText::new(
                    "Built with egui, eframe, egui-snarl, egui_extras, gilrs, SDL, midir, rfd, serde, HidHide."
                ).small());
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("Virtual devices powered by").small());
                    ui.hyperlink_to(
                        egui::RichText::new("HIDMaestro").small(),
                        "https://github.com/hifihedgehog/HIDMaestro",
                    );
                    ui.label(egui::RichText::new("by hifihedgehog (MIT).").small());
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("Input prompt SVG icons by Kenney —").small());
                    ui.hyperlink_to(
                        egui::RichText::new("kenney.nl/assets/input-prompts").small(),
                        "https://kenney.nl/assets/input-prompts",
                    );
                    ui.label(egui::RichText::new("(CC0).").small());
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("Macro & menu icons from").small());
                    ui.hyperlink_to(
                        egui::RichText::new("game-icons.net").small(),
                        "https://game-icons.net/",
                    );
                    ui.label(egui::RichText::new("by their respective authors, licensed").small());
                    ui.hyperlink_to(
                        egui::RichText::new("CC BY 3.0").small(),
                        "https://creativecommons.org/licenses/by/3.0/",
                    );
                    ui.label(egui::RichText::new("(per-icon authors listed on the site).").small());
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("3D controller models adapted from").small());
                    ui.hyperlink_to(
                        egui::RichText::new("3d-controller-overlay").small(),
                        "https://github.com/larfingshnew/3d-controller-overlay",
                    );
                    ui.label(egui::RichText::new("by larfingshnew (MIT).").small());
                });
                }); // ScrollArea
            });

        if dirty { self.settings_dirty = true; }
        if save_workspace { self.save_workspace_now(); }
        if !open { self.settings_open = false; }
    }
}
