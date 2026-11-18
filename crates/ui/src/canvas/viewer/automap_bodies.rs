//! AutoMap routing node bodies: Splitter, Collector, Fork, Selector,
//! Combiner + device-family capability helpers.

use super::*;

// ── AutoMap Splitter body ─────────────────────────────────────────────────────

/// Parse the family slug from a gilrs source ID (`"gilrs:<slug>:<inst>"`) or
/// Per-device-source capability flags for the header control rows.
/// Returns `(has_deadzone, has_gyro_mult, has_sticks_cal)`.
/// - MIDI ports: nothing
/// - XInput (Xbox): deadzone + sticks calibration, no gyro
/// - DualShock4 / DualSense / Switch Pro / Joy-Con 2: deadzone + gyro + sticks
/// - Generic HID / unknown: deadzone + sticks (conservative)
/// Whether a `gilrs:<slug>:<inst>` device has pressure-sensitive analog
/// triggers. Switch Pro (digital-only ZL/ZR) returns false. Unknown slugs are
/// treated as analog-capable (conservative — they keep the opt-in toggle).
pub(crate) fn slug_has_analog_triggers(dev_id: &str) -> bool {
    // phys_pad_slug handles both gilrs: and sdl: — so an SDL-surfaced Switch Pro
    // is correctly treated as digital-only (forced ON), not analog-capable.
    let slug = crate::canvas::remapper_icons::phys_pad_slug(dev_id).unwrap_or("");
    !crate::canvas::remapper_icons::slug_has_digital_triggers(slug)
}

/// Advanced-mode device.source body toggle for the digital-trigger override.
/// Mirrors the Easy-mode `digital_trigger_toggle`: forced ON + disabled for
/// digital-only pads, opt-in (default OFF) for pads with real analog triggers.
pub(crate) fn digital_trigger_header_toggle(
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    node: NodeId,
    dev_id: &str,
) {
    let forced = !slug_has_analog_triggers(dev_id);
    let stored = snarl.get_node(node)
        .and_then(|n| n.params.get("digital_triggers"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut checked = forced || stored;

    // Persist the forced value so the engine sees it even without a click.
    if forced && !stored {
        if let Some(n) = snarl.get_node_mut(node) {
            n.params.insert("digital_triggers".into(), Value::Bool(true));
        }
    }

    ui.add_enabled_ui(!forced, |ui| {
        let label = if forced { "Digital triggers (only option)" } else { "Digital triggers \u{2192} analog" };
        let resp = ui.checkbox(&mut checked, egui::RichText::new(label).small());
        if resp.changed() {
            if let Some(n) = snarl.get_node_mut(node) {
                n.params.insert("digital_triggers".into(), Value::Bool(checked));
            }
        }
        resp.on_hover_text(
            "Drive the virtual pad's analog triggers from this controller's digital \
             ZL/ZR buttons. A press maps to a full pull; otherwise the real analog \
             value is used.",
        );
    });
}

/// Feedback Control header toggle: whether wired feedback ADDS to what the game
/// asks of the pad (default) or OVERRIDES it. Stored on `fb_override`; the
/// engine's feedback layers (eval/feedback.rs) read it.
pub(crate) fn feedback_override_header_toggle(
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    node: NodeId,
) {
    let mut checked = snarl.get_node(node)
        .and_then(|n| n.params.get("fb_override"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // A lit word switch, the pills' shape, where it was a checkbox. The whole phrase when
    // the header has room for it, one word when not - it wrapped onto two lines inside its
    // button.
    let pad = 2.0 * ui.spacing().button_padding.x + 2.0;
    let word = super::simple_bodies::fitting_word(ui, "override|override game feedback", ui.available_width() - pad);
    let resp = ui.add(
        egui::Button::selectable(checked, super::simple_bodies::control_text(word))
            .frame_when_inactive(true)
            .wrap_mode(egui::TextWrapMode::Truncate),
    );
    if resp.clicked() {
        checked = !checked;
        if let Some(n) = snarl.get_node_mut(node) {
            n.params.insert("fb_override".into(), Value::Bool(checked));
        }
    }
    resp.on_hover_text(
        "Off: what you wire in is added on top of what the game sends to the pad.\n\
         On: each kind of feedback you wire in replaces the game's — rumble (classic \
         and HD alike), light bar, player LEDs, mic LED, left or right adaptive \
         trigger. Kinds you don't wire stay the game's, so wiring only a light bar \
         colour keeps the game's rumble.\n\
         A wire sitting at 0 still counts: it silences that kind of feedback.",
    );
}

/// Whether the "Suppress capacitive touch" toggle is worth showing for this
/// device: an SDL-read pad with capacitive stick caps or grips
/// (`automap::capacitive_misc_mask`). The SDL backend is what publishes the
/// mask the mute acts on, so a pad read any other way has nothing to mute.
pub(crate) fn has_capacitive_suppression(dev: &flexinput_devices::PhysicalDevice) -> bool {
    dev.id.starts_with("sdl:")
        && match (dev.vid, dev.pid) {
            (Some(v), Some(p)) => flexinput_core::automap::capacitive_misc_mask(v, p) != 0,
            _ => false,
        }
}

/// Label and hover text for that toggle, shared by the Advanced node header and
/// the Easy-mode input card.
pub(crate) const CAPACITIVE_LABEL: &str = "Ignore capacitive touch in Learn";
pub(crate) const CAPACITIVE_HOVER: &str =
    "Learn won't capture this pad's capacitive sensors: touch on the stick caps \
     and, on a Steam Controller, the grips. They fire just from holding the \
     controller, which would steal the capture from the button you meant.\n\
     Only Learn ignores them. To map one, choose it from the picker: it fires as \
     usual with this on.";

/// "Ignore capacitive touch in Learn" until set otherwise: on. It only shows on,
/// and only acts on, a pad with capacitive inputs (a Steam Controller above all),
/// whose grips and stick caps would take every Learn otherwise.
pub(crate) const TOUCH_MISC_DEFAULT: bool = true;

/// Whether a device.source node's params have "Ignore capacitive touch in
/// Learn" on (`suppress_touch_misc`), the default where unset.
pub(crate) fn touch_misc_suppressed(params: &std::collections::HashMap<String, Value>) -> bool {
    params.get("suppress_touch_misc").and_then(|v| v.as_bool()).unwrap_or(TOUCH_MISC_DEFAULT)
}

/// A device.source on/off param's value where the node has none set.
pub(crate) fn bool_param_default(key: &str) -> bool {
    key == "suppress_touch_misc" && TOUCH_MISC_DEFAULT
}

/// Advanced-mode device.source body toggle hiding the capacitive stick-cap / grip
/// pins from Learn. Stored on `suppress_touch_misc`; read by
/// `devices_pool::collect_touch_misc_suppressed`.
pub(crate) fn touch_misc_header_toggle(
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    node: NodeId,
) {
    let mut checked = snarl.get_node(node).is_some_and(|n| touch_misc_suppressed(&n.params));

    let resp = ui.checkbox(
        &mut checked,
        egui::RichText::new(CAPACITIVE_LABEL).small(),
    );
    if resp.changed() {
        if let Some(n) = snarl.get_node_mut(node) {
            n.params.insert("suppress_touch_misc".into(), Value::Bool(checked));
        }
    }
    resp.on_hover_text(CAPACITIVE_HOVER);
}

/// Label and hover text for the Virtual KB/M "Route gamepad touchpad" toggle
/// (`route_touchpad`), shared by the Advanced node header and the Easy-mode
/// Keyboard and Mouse card.
pub(crate) const TOUCHPAD_ROUTE_LABEL: &str = "Gamepad touchpad → trackpad";
pub(crate) const TOUCHPAD_ROUTE_HOVER: &str =
    "Use the touchpad of the pad feeding this output as a laptop trackpad:\n\
     • one finger moves the pointer\n\
     • two fingers scroll smoothly\n\
     • tap = left click, two-finger tap = right click\n\
     • pressing the pad = left click (right click with two fingers down)\n\
     On a Steam Controller each trackpad has a job of its own:\n\
     • the right pad moves the pointer, and pressing it = left click\n\
     • the left pad scrolls both ways, and pressing it = right click\n\
     • pressing both = middle click (no tap clicks on these pads)\n\
     Adds to whatever else drives the mouse. Windows' own trackpad gestures \
     (three-finger swipes, pinch zoom) aren't available.";

/// Advanced-mode Virtual KB/M header controls for the touchpad routing: the
/// toggle, and while it's on, the trackpad's speed and scroll direction.
pub(crate) fn touchpad_route_header_controls(
    ui: &mut egui::Ui,
    params: &mut std::collections::HashMap<String, Value>,
    label_cell_w: f32,
) {
    let flag = |params: &std::collections::HashMap<String, Value>, key: &str| {
        params.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
    };
    let mut on = flag(params, "route_touchpad");
    let resp = ui.checkbox(&mut on, egui::RichText::new(TOUCHPAD_ROUTE_LABEL).small());
    if resp.changed() {
        params.insert("route_touchpad".into(), Value::Bool(on));
    }
    resp.on_hover_text(TOUCHPAD_ROUTE_HOVER);
    if !on { return; }

    // Pointer and two-finger scroll each get their own multiplier (1.0 =
    // the trackpad's built-in rate; double-click the track to reset).
    for (label, key) in [("Pointer ×", "touchpad_speed"), ("Scroll ×", "touchpad_scroll_speed")] {
        let mut speed = params.get(key).and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
        let initial = speed;
        ui.horizontal(|ui| {
            slider_label(ui, label, label_cell_w);
            let resp = ui.add(egui::Slider::new(&mut speed, 0.1_f32..=10.0)
                .logarithmic(true)
                .show_value(false)
                .clamping(egui::SliderClamping::Always));
            if slider_track_double_clicked(ui, &resp) { speed = 1.0; }
            ui.add(egui::DragValue::new(&mut speed).speed(0.01).range(0.1_f32..=10.0).fixed_decimals(2));
        });
        if (speed - initial).abs() > f32::EPSILON {
            params.insert(key.into(), Value::from(speed as f64));
        }
    }

    let mut reverse = flag(params, "touchpad_reverse_scroll");
    let resp = ui.checkbox(&mut reverse, egui::RichText::new("Reverse scroll direction").small());
    if resp.changed() {
        params.insert("touchpad_reverse_scroll".into(), Value::Bool(reverse));
    }
    resp.on_hover_text(
        "Off: the page follows your fingers (Windows' trackpad default).\n\
         On: fingers up scroll up, like turning a mouse wheel.");
}

/// Label and hover text for the Virtual KB/M "Joy-Con 2 mouse sensor" toggle
/// (`route_optical_mouse`), shared like the touchpad's above.
pub(crate) const OPTICAL_MOUSE_ROUTE_LABEL: &str = "Joy-Con 2 mouse sensor → pointer";
pub(crate) const OPTICAL_MOUSE_ROUTE_HOVER: &str =
    "Use the optical sensor of a Joy-Con 2 feeding this output as a mouse:\n\
     stand it on its rail side on a desk and slide it.\n\
     Adds to whatever else drives the mouse. Mouse speed applies (100 = one \
     pixel per sensor count); Advanced mode adds a Sensor × on top.\n\
     The pad's Mouse Sensor pins can also be wired or mapped directly, and \
     Mouse Sensor Contact works as a button (on a surface / finger over it).";

/// Advanced-mode Virtual KB/M header controls for the Joy-Con 2 optical
/// sensor: the toggle and, while it's on, its own speed.
pub(crate) fn optical_mouse_route_header_controls(
    ui: &mut egui::Ui,
    params: &mut std::collections::HashMap<String, Value>,
    label_cell_w: f32,
) {
    let mut on = params.get("route_optical_mouse").and_then(|v| v.as_bool()).unwrap_or(false);
    let resp = ui.checkbox(&mut on, egui::RichText::new(OPTICAL_MOUSE_ROUTE_LABEL).small());
    if resp.changed() {
        params.insert("route_optical_mouse".into(), Value::Bool(on));
    }
    resp.on_hover_text(OPTICAL_MOUSE_ROUTE_HOVER);
    if !on { return; }

    // A multiplier on top of Mouse speed; double-click the track to reset.
    let key = "optical_mouse_speed";
    let mut speed = params.get(key).and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
    let initial = speed;
    ui.horizontal(|ui| {
        slider_label(ui, "Sensor ×", label_cell_w);
        let resp = ui.add(egui::Slider::new(&mut speed, 0.05_f32..=20.0)
            .logarithmic(true)
            .show_value(false)
            .clamping(egui::SliderClamping::Always));
        if slider_track_double_clicked(ui, &resp) { speed = 1.0; }
        ui.add(egui::DragValue::new(&mut speed).speed(0.01).range(0.05_f32..=20.0).fixed_decimals(2));
    });
    if (speed - initial).abs() > f32::EPSILON {
        params.insert(key.into(), Value::from(speed as f64));
    }
}

/// The Combiner's Motion-section mode row: SORT takes gyro, accel and
/// orientation whole from the top port carrying motion; CORRELATE fuses
/// rigidly joined IMUs. A per-pin policy below still overrides its one pin.
/// Controls put against the port strips' right edge (`right`), in a cell `w` wide of their
/// own. The row reserves the cell and nothing more: a dropdown drawn wider than it was asked
/// to be (its text past what fits at a large control font) stays in the cell, clipped,
/// instead of pushing the row past the edge - which moved the module's frame, and the edge
/// with it, out by as much every frame, without end.
fn right_cell(ui: &mut egui::Ui, right: f32, w: f32, add: impl FnOnce(&mut egui::Ui)) {
    let spare = right - ui.cursor().left() - w;
    if spare > 0.0 {
        ui.add_space(spare);
    }
    let h = (ui.text_style_height(&egui::TextStyle::Button) + 2.0 * ui.spacing().button_padding.y)
        .max(ui.spacing().interact_size.y);
    let (r, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    let mut cell = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(r)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    cell.set_clip_rect(egui::Rect::from_x_y_ranges(r.x_range(), ui.clip_rect().y_range()).intersect(ui.clip_rect()));
    add(&mut cell);
}

fn motion_section_mode(
    ui: &mut egui::Ui,
    node_id: NodeId,
    current: &str,
    next: &mut Option<&'static str>,
    gate: f32,
    next_gate: &mut Option<f32>,
    strip_right: f32,
) {
    const MODES: &[(&str, &str, &str)] = &[
        ("SORT", "Sort (top port)",
         "The whole Motion section — gyro, accel and orientation together — \
          from the top port that carries motion, even while it reads zero. \
          Two IMUs are never mixed axis by axis."),
        ("CORRELATE", "Correlate",
         "Fuse every port's motion. Only for IMUs that move as one: Joy-Cons in \
          a grip, or an extra IMU strapped to a pad with its own.\n\
          Each lower port's mounting rotation is learned by correlating its \
          gyro with the top port's — turn the assembly about a few different \
          axes for a second or two. Then the aligned readings are fused, each \
          weighted by its own measured noise (a noisier half counts for less), \
          and a noise gate removes what can't be told from noise. Until the fit \
          is good (or if it degrades), the top port alone, as Sort. \
          Orientation always comes from the top port."),
    ];
    let label = MODES.iter().find(|(m, _, _)| *m == current).map(|(_, l, _)| *l).unwrap_or("Sort (top port)");
    // One dropdown as wide as the two on the pin rows below it, ending where they end.
    let span = 58.0 + 54.0 + ui.spacing().item_spacing.x;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Section").small());
        right_cell(ui, strip_right, span, |ui| {
            let resp = egui::ComboBox::from_id_salt(egui::Id::new(("automap_combiner_motion", node_id)))
                .selected_text(super::simple_bodies::combo_text(ui, label, span))
                .width(span)
                .show_ui(ui, |ui| {
                    for &(mode, name, hover) in MODES {
                        if ui.selectable_label(current == mode, egui::RichText::new(name).small())
                            .on_hover_text(hover).clicked()
                        {
                            *next = Some(mode);
                        }
                    }
                });
            resp.response.on_hover_text(
                MODES.iter().find(|(m, _, _)| *m == current).map(|(_, _, h)| *h).unwrap_or(MODES[0].2));
        });
    });
    // The noise gate has a row of its own, its number in the right-hand dropdown's column:
    // after the dropdown it ran the row past the edge every other row keeps.
    if current != "CORRELATE" {
        return;
    }
    ui.horizontal(|ui| {
        {
            let mut g = gate;
            ui.label(egui::RichText::new("noise gate ×").small().weak());
            let mut r = None;
            right_cell(ui, strip_right, 54.0, |ui| {
                r = Some(ui.add_sized([54.0, ui.spacing().interact_size.y], egui::DragValue::new(&mut g).speed(0.02).range(0.0..=4.0).fixed_decimals(2)));
            });
            let Some(r) = r else { return };
            let r = r
                .on_hover_text(
                    "How much measured noise the fused gyro rate sheds.\n\
                     The noise is measured live from how much the IMUs DISAGREE \
                     — motion is the same in both, so their difference is noise \
                     alone — and rates within that are removed smoothly.\n\
                     1 = the default (a resting pair reads zero ~95% of the \
                     time; real turns pass whole). 0 = off: weighting only. \
                     Higher trims more of the slowest aim.");
            if r.changed() {
                *next_gate = Some(g);
            }
        }
    });
}

/// A device.source's sticks have different deadzones: the right stick's own
/// `deadzone_r` (Calibration window) differs from `deadzone`. A gamepad card's
/// single Deadzone value then shows in orange, and editing it sets both sticks
/// again (see [`set_both_stick_deadzones`]).
pub(crate) fn stick_deadzones_differ(params: &std::collections::HashMap<String, Value>) -> bool {
    let left = params.get("deadzone").and_then(|v| v.as_f64()).unwrap_or(0.1);
    params.get("deadzone_r").and_then(|v| v.as_f64()).is_some_and(|r| (r - left).abs() > 1e-6)
}

/// The gamepad card's Deadzone: one value for both sticks.
pub(crate) fn set_both_stick_deadzones(params: &mut std::collections::HashMap<String, Value>, dz: f32) {
    params.insert("deadzone".into(), Value::from(dz as f64));
    params.remove("deadzone_r");
}

/// Orange, for a card value that stands for two that differ.
pub(crate) const MIXED_VALUE_COLOR: egui::Color32 = egui::Color32::from_rgb(220, 160, 40);

pub(crate) fn device_source_caps(dev_id: &str, is_device_source: bool) -> (bool, bool, bool) {
    if !is_device_source { return (false, false, false); }
    if dev_id.starts_with("midi_in") || dev_id.starts_with("midi_out") {
        return (false, false, false);
    }
    // Same slug table for gilrs and SDL pads — a DualSense read through SDL
    // gets the same deadzone/gyro/stick calibration surface as a native one.
    if let Some(slug) = crate::canvas::remapper_icons::phys_pad_slug(dev_id) {
        return match slug {
            "xinput"                          => (true, false, true),
            "ds4" | "dualsense" | "switch_pro"
            | "joycon2_l" | "joycon2_r"
            | "joycon_l" | "joycon_r"         => (true, true,  true),
            _                                 => (true, false, true),
        };
    }
    (false, false, false)
}

/// virtual sink ID (`"virtual.<slug>:<inst>"` / `"virtual.keymouse"`). Returns
/// the canonical slug accepted by `am_canon::family_label` ("dualsense", "ds4",
/// "switch_pro", "xinput") or None if the device family is irrelevant for
/// button-glyph labelling.
pub(crate) fn family_slug_from_device_id(dev_id: &str) -> Option<&'static str> {
    // Physical: gilrs:<slug>:<inst> OR sdl:<slug>:<inst> (same slug table, so an
    // SDL-surfaced DualSense gets its own glyphs, not the Xbox fallback).
    if let Some(slug) = crate::canvas::remapper_icons::phys_pad_slug(dev_id) {
        return match slug {
            "dualsense"  => Some("dualsense"),
            "ds4"        => Some("ds4"),
            // A Joy-Con of either generation prints the Switch Pro's labels
            // (B/A/Y/X, ZL/ZR, +/−/Home) on its buttons.
            "switch_pro" | "joycon_l" | "joycon_r" | "joycon_pair"
            | "joycon2_l" | "joycon2_r" => Some("switch_pro"),
            "xinput"     => Some("xinput"),
            _            => None,
        };
    }
    // Virtual sink: virtual.<kind>[:inst]
    if let Some(rest) = dev_id.strip_prefix("virtual.") {
        let kind = rest.split(':').next()?;
        return match kind {
            "xinput"   => Some("xinput"),
            "ds4"      => Some("ds4"),
            // virtual.keymouse: KB/M, no gamepad-glyph family
            _          => None,
        };
    }
    None
}

/// Trace upstream from a node's AutoMap input to find the originating
/// device.source's family slug. Walks through Splitter/Fork/Selector
/// passthrough; returns `None` if no device is connected or the upstream
/// isn't a recognised gamepad family.
pub(crate) fn splitter_upstream_family(snarl: &Snarl<NodeData>, node_id: NodeId) -> Option<&'static str> {
    fn rec(snarl: &Snarl<NodeData>, src: OutPinId, depth: u32) -> Option<&'static str> {
        if depth > 16 { return None; }
        let node = snarl.get_node(src.node)?;
        if node.module_id == "device.source" {
            let dev_id = node.params.get("device_id").and_then(|v| v.as_str())?;
            return family_slug_from_device_id(dev_id);
        }
        // Passthrough: AutoMap Splitter/Fork/Selector forward the bus on their
        // first AutoMap input pin.
        let in_idx = node.inputs.iter().position(|p| p.signal_type == SignalType::AutoMap)?;
        let pin = snarl.in_pin(InPinId { node: src.node, input: in_idx });
        let upstream = *pin.remotes.first()?;
        rec(snarl, upstream, depth + 1)
    }
    let in_pin = snarl.in_pin(InPinId { node: node_id, input: 0 });
    let upstream = *in_pin.remotes.first()?;
    rec(snarl, upstream, 0)
}

/// Trace downstream from a Collector's AutoMap output (pin 0) to find the
/// destination `device.sink`'s family slug. Walks through Splitter/Fork/
/// Selector passthrough on the AutoMap output. Returns the first matching
/// sink — if a Collector fans out to multiple sinks of different families,
/// the first one wins.
pub(crate) fn collector_downstream_family(snarl: &Snarl<NodeData>, node_id: NodeId) -> Option<&'static str> {
    fn rec(snarl: &Snarl<NodeData>, dst: InPinId, depth: u32) -> Option<&'static str> {
        if depth > 16 { return None; }
        let node = snarl.get_node(dst.node)?;
        if node.module_id == "device.sink" {
            let dev_id = node.params.get("device_id").and_then(|v| v.as_str())?;
            return family_slug_from_device_id(dev_id);
        }
        // Passthrough: Splitter/Fork/Selector forward the bus on output pin 0.
        let out_pin = snarl.out_pin(OutPinId { node: dst.node, output: 0 });
        for &downstream in &out_pin.remotes {
            if let Some(fam) = rec(snarl, downstream, depth + 1) {
                return Some(fam);
            }
        }
        None
    }
    let out_pin = snarl.out_pin(OutPinId { node: node_id, output: 0 });
    for &downstream in &out_pin.remotes {
        if let Some(fam) = rec(snarl, downstream, 0) {
            return Some(fam);
        }
    }
    None
}

/// Compose a splitter/collector label for a pin: neutral name when nothing is
/// connected, "Neutral (Family)" when a known controller family is on the
/// other end. Suppresses the suffix if it would just duplicate the neutral.
pub(crate) fn splitter_pin_label(pin_id: &str, neutral: &str, family: Option<&str>) -> String {
    match family.and_then(|f| am_canon::family_label(pin_id, Some(f))) {
        Some(gly) if gly != neutral => format!("{neutral} ({gly})"),
        _ => neutral.to_string(),
    }
}

/// The Splitter's and Collector's actions, as a Remapper's action row draws its own: a
/// button each, one width, across the strips' width from `left`. `(label, enabled, on,
/// hover)`; returns the one clicked. A button with nothing to do is greyed rather than
/// gone, so the row doesn't change shape under the pointer.
pub(crate) fn automap_action_row(ui: &mut egui::Ui, left: f32, w: f32, actions: &[(&str, bool, bool, &str)]) -> Option<usize> {
    let gap = ui.spacing().item_spacing.x;
    let bw = super::remapper_bodies::action_width(ui, w, actions.len());
    let (row, _) = ui.allocate_exact_size(egui::vec2(w, super::remapper_bodies::ACTION_H), egui::Sense::hover());
    let mut clicked = None;
    for (k, (label, enabled, on, hover)) in actions.iter().enumerate() {
        let r = egui::Rect::from_min_size(egui::pos2(left + k as f32 * (bw + gap), row.top()), egui::vec2(bw, row.height()));
        let mut cell = ui.new_child(egui::UiBuilder::new().max_rect(r).id_salt(("am_action", k)));
        let resp = cell
            .add_enabled(*enabled, super::remapper_bodies::action_button(label, bw).selected(*on))
            .on_hover_text(*hover)
            .on_disabled_hover_text(*hover);
        if resp.clicked() {
            clicked = Some(k);
        }
    }
    clicked
}

/// [`automap_action_row`] as a column, one width - the widest word's - for a body that is a
/// narrow strip beside a long list of pins (the Virtual Keyboard & Mouse's), where a row
/// of them ran off its edge.
pub(crate) fn automap_action_column(ui: &mut egui::Ui, actions: &[(&str, bool, bool, &str)]) -> Option<usize> {
    let font = super::simple_bodies::control_font_at(super::simple_bodies::control_font_size() + 1.0);
    let w = actions
        .iter()
        .map(|(label, ..)| ui.painter().layout_no_wrap(label.to_string(), font.clone(), egui::Color32::PLACEHOLDER).size().x)
        .fold(0.0f32, f32::max)
        + 2.0 * ui.spacing().button_padding.x
        + 16.0;
    let mut clicked = None;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        for (k, (label, enabled, on, hover)) in actions.iter().enumerate() {
            let resp = ui
                .add_enabled(*enabled, super::remapper_bodies::action_button(label, w).selected(*on))
                .on_hover_text(*hover)
                .on_disabled_hover_text(*hover);
            if resp.clicked() {
                clicked = Some(k);
            }
        }
    });
    clicked
}

/// Whether the MIDI picker under an Auto-Map list's action row is open.
fn automap_midi_open_id(node_id: NodeId, salt: &str) -> egui::Id {
    egui::Id::new((node_id, salt, "midi_open"))
}

/// Row label for a bus pin: canonical name, MIDI name, or the raw id.
fn bus_pin_neutral_label(pin_id: &str) -> String {
    if let Some(name) = crate::model::connected_pad_pin_names(pin_id) {
        return name.0.to_string();
    }
    if let Some(p) = am_canon::ALL_PINS.iter().find(|p| p.id == pin_id) {
        return p.display_name.to_string();
    }
    if flexinput_core::midi::is_midi_pin(pin_id) {
        return midi_pin_label(pin_id);
    }
    if flexinput_core::raw_pad::is_extra_pin(pin_id) {
        return crate::raw_learn::extra_label(pin_id);
    }
    pin_id.to_string()
}

/// AutoMap curve header: which signal on the connected bus the curve reshapes
/// (`am_curve_pin`). Offers the Float and Vec2 signals the bus's devices carry
/// — what they report right now, so a pad without analog triggers offers none —
/// or, with no device resolvable upstream, every canonical one. A MIDI port's
/// controllers show up once they move. Gyro, Accel and each touch point are
/// offered whole too, ahead of their single axes: every axis on one graph, one
/// channel each.
///
/// Only the pick is stored: the curve stays exactly as drawn whatever signal it
/// is moved to (the engine reads a Vec2 by its length on its own). Returns true
/// when the pick changed, so the caller can make it an undo step.
pub(crate) fn automap_curve_pin_picker(
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    node: NodeId,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    automap_parent: Option<&AutomapGlowParent<'_>>,
) -> bool {
    let devs = remapper_upstream_device_ids(snarl, node, 0, automap_parent);
    let family = devs.iter().find_map(|d| family_slug_from_device_id(d));
    let carried = |pin: &str| devs.iter().any(|d| live_signals.contains_key(&(d.clone(), pin.to_string())));
    let shapeable = |t: SignalType| matches!(t, SignalType::Float | SignalType::Vec2);

    let mut options: Vec<String> = am_canon::ALL_PINS.iter()
        .filter(|p| shapeable(p.signal_type) && carried(p.id))
        .map(|p| p.id.to_string())
        .collect();
    if options.is_empty() {
        options = am_canon::ALL_PINS.iter()
            .filter(|p| shapeable(p.signal_type))
            .map(|p| p.id.to_string())
            .collect();
    }
    // A multi-axis signal as one pick, ahead of its first axis.
    for group in ["gyro", "accel", "touch1", "touch2"] {
        let Some(first) = flexinput_engine::eval::automap_curve_group(group).and_then(|a| a.first()) else { continue };
        if let Some(at) = options.iter().position(|p| p == first) {
            options.insert(at, group.to_string());
        }
    }
    let mut midi: Vec<String> = live_signals.iter()
        .filter(|((d, pin), sig)| devs.contains(d)
            && flexinput_core::midi::is_midi_pin(pin)
            && matches!(sig, Signal::Float(_)))
        .map(|((_, pin), _)| pin.clone())
        .collect();
    midi.sort();
    options.extend(midi);

    let current = snarl.get_node(node).map(|n| automap_curve_pin(n).to_string()).unwrap_or_default();
    let label_of = |pin: &str| match pin {
        "gyro"  => "Gyro (X, Y, Z)".to_string(),
        "accel" => "Accel (X, Y, Z)".to_string(),
        "touch1" => "Touch 1 (X, Y)".to_string(),
        "touch2" => "Touch 2 (X, Y)".to_string(),
        _ => splitter_pin_label(pin, &bus_pin_neutral_label(pin), family),
    };
    let selected = if current.is_empty() { "Pick signal…".to_string() } else { label_of(&current) };

    let mut picked: Option<String> = None;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Signal").small().weak());
        egui::ComboBox::from_id_salt((node, "am_curve_pin"))
            .selected_text(super::simple_bodies::combo_text(ui, selected, 150.0))
            .width(150.0)
            .show_ui(ui, |ui| {
                for pin in &options {
                    let text = egui::RichText::new(label_of(pin)).small();
                    if ui.selectable_label(*pin == current, text).clicked() && *pin != current {
                        picked = Some(pin.clone());
                    }
                }
            })
            .response
            .on_hover_text("The signal on the AutoMap bus this curve reshapes. Every other signal passes through unchanged.");
    });

    let Some(pin) = picked else { return false };
    let Some(n) = snarl.get_node_mut(node) else { return false };
    n.params.insert(flexinput_engine::eval::AUTOMAP_CURVE_PIN_PARAM.into(), Value::String(pin));
    true
}

/// One row of an Auto-Map port list: a drag handle, the port's name, and the `×` at the row's
/// right edge where the port strips keep theirs, so the names line up down the list instead of
/// staggering behind a column of buttons.
///
/// The handle drags the port to another row. These lists are ordered and the engine reads the
/// pins by position, so a move takes the wire with it (`move_automap_port`). The name is never
/// abbreviated here, unlike a small module's `in_2`: telling "Left Stick" from "Right Stick" at
/// a glance is the whole point of the module. What is cut short is only what won't fit the
/// row's width, and the whole name stays on hover.
///
/// The handle is dragged the way the port strips drag theirs - an interaction on the handle's
/// own rect, not a drag-and-drop payload - and what it hands back is where the pointer was let
/// go. The caller turns that into a target row once every row has been laid out, because the
/// row the pointer is over is below this one and has no rect yet. Returns whether the `×` was
/// pressed and the release position, if the drag ended here.
pub(crate) fn automap_port_row(
    ui: &mut egui::Ui,
    node_id: NodeId,
    salt: &str,
    band: egui::Rect,
    pin_id: &str,
    name: &str,
    skin: crate::canvas::remapper_icons::Skin,
    index: usize,
) -> (bool, Option<egui::Pos2>, egui::Rect) {
    const PAD: f32 = 4.0;
    let h = band.height();
    let mid = band.center().y;
    let mut released: Option<egui::Pos2> = None;
    let painter = ui.painter().clone();
    // A strip's plate, as a Split's and a Selector's wear; everything on it centred in its
    // row, painted where it goes (laid out in a row, the parts sat low in the plate).
    painter.rect_filled(band, 3.0, ui.visuals().widgets.inactive.weak_bg_fill);

    // The handle that moves the port.
    let handle = egui::Rect::from_center_size(egui::pos2(band.left() + h * 0.5, mid), egui::vec2(h * 0.7, h));
    painter.text(handle.center(), egui::Align2::CENTER_CENTER, "⠿", egui::TextStyle::Small.resolve(ui.style()), ui.visuals().weak_text_color());
    let grabbed = ui.interact(handle, ui.id().with((salt, "handle", node_id, index)), egui::Sense::drag());
    if grabbed.drag_stopped() {
        released = grabbed.interact_pointer_pos();
    }
    grabbed.on_hover_cursor(egui::CursorIcon::Grab).on_hover_text("Drag to move this port");

    // The `×` at the right edge.
    let x_size = (h - 6.0).max(12.0);
    let x_rect = egui::Rect::from_center_size(egui::pos2(band.right() - PAD - x_size * 0.5, mid), egui::vec2(x_size, x_size));
    let remove = ui
        .put(x_rect, egui::Button::new(egui::RichText::new("×").font(super::simple_bodies::control_font())))
        .on_hover_text("Remove this port")
        .clicked();

    // The pin's glyph, as a mapping card shows it, where it has one; then its name.
    let mut x = handle.right() + PAD;
    let icon_h = (h - 6.0).max(10.0);
    // The bus names some pins other than the pickers do (`bus_glyph_pin`).
    if let Some(glyph) = bus_pin_glyph(skin, pin_id) {
        let w = super::remapper_support::paint_chord_chip_tinted(
            &painter,
            ui.ctx(),
            egui::pos2(x, mid - icon_h * 0.5),
            icon_h,
            glyph,
            skin,
            egui::Color32::WHITE,
        );
        x += w + PAD;
    }
    let name_rect = egui::Rect::from_min_max(egui::pos2(x, band.top()), egui::pos2(x_rect.left() - PAD, band.bottom()));
    let font = super::simple_bodies::control_font();
    // Strong: the plate is the widgets' grey, and the plain text colour is a grey too.
    let colour = ui.visuals().strong_text_color();
    let shown = super::simple_bodies::elide_to_width(ui, name, font.clone(), colour, name_rect.width().max(0.0));
    painter.text(egui::pos2(name_rect.left(), mid), egui::Align2::LEFT_CENTER, shown, font, colour);
    ui.interact(name_rect, ui.id().with((salt, "name", node_id, index)), egui::Sense::hover()).on_hover_text(name);

    // Traced so a test can hold the rows to one width and find the handle and the `×` - and,
    // as every port strip is, level with its socket.
    super::simple_bodies::trace_rect(ui, node_id, &format!("{salt}_row_{index}"), band);
    super::simple_bodies::trace_rect(ui, node_id, &format!("strip_{index}"), band);
    (remove, released, band)
}


/// The glyph a bus pin is drawn with, under the id it is drawn under (`bus_glyph_pin`), if
/// it has one in any family.
fn bus_pin_glyph(skin: crate::canvas::remapper_icons::Skin, pin_id: &str) -> Option<&str> {
    use crate::canvas::remapper_icons::{self, Skin};
    let glyph = remapper_icons::bus_glyph_pin(pin_id);
    (remapper_icons::pin_svg(skin, glyph).is_some()
        || [Skin::Xbox, Skin::Playstation, Skin::SwitchPro, Skin::Kbm].iter().any(|s| remapper_icons::pin_svg(*s, glyph).is_some()))
    .then_some(glyph)
}

/// `add pin`'s list, each pin with its glyph as its strip will show it.
fn add_pin_combo(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    opts: &[(usize, &str)],
    pins: &[&str],
    skin: crate::canvas::remapper_icons::Skin,
    hover: &str,
    salt: (&'static str, NodeId),
) -> Option<usize> {
    let ctx = ui.ctx().clone();
    let draw = |p: &egui::Painter, square: egui::Rect, i: usize| -> bool {
        let Some(glyph) = pins.get(i).and_then(|id| bus_pin_glyph(skin, id)) else { return false };
        super::remapper_support::paint_chord_chip_tinted(p, &ctx, square.min, square.height(), glyph, skin, egui::Color32::WHITE);
        true
    };
    super::curve_rows::pill_combo_glyphs(ui, rect, "add pin", &usize::MAX, opts, hover, salt, Some(&draw))
}

/// Move one port of an Auto-Map list to another row, taking its wires with it and permuting
/// the id list kept beside the pins. The splitter's ids run alongside all its outputs (the
/// passthrough first); the collector's alongside its inputs past the bus pin, hence `offset`.
pub(crate) fn move_automap_port<S: PinSide>(
    node_id: NodeId,
    from: usize,
    to: usize,
    pins: &[S::Pin],
    ids_key: &str,
    offset: usize,
    snarl: &mut Snarl<NodeData>,
) {
    if from == to || from >= pins.len() || to >= pins.len() {
        return;
    }
    // The id list first: read it as it was, then hand the pins over and write it back moved.
    let old: Vec<Value> = snarl
        .get_node(node_id)
        .and_then(|n| n.params.get(ids_key).and_then(|v| v.as_array().cloned()))
        .unwrap_or_default();
    crate::canvas::viewer::pins::move_dynamic_pin::<S>(node_id, from, to, pins, snarl);
    if old.is_empty() {
        return;
    }
    // The same permutation over the ids. The list's row `i` is pin `i + offset` - the splitter
    // keeps its Auto-Map passthrough at output 0 and the collector its bus pin at input 0, and
    // neither is a row - so the id indices are the pin indices less that offset.
    let (a, b) = (from.saturating_sub(offset), to.saturating_sub(offset));
    if a < old.len() && b < old.len() {
        let (lo, hi) = (a.min(b), a.max(b));
        let mut order: Vec<usize> = (lo..=hi).collect();
        let moved = order.remove(a - lo);
        order.insert(b - lo, moved);
        let mut next = old.clone();
        for (k, &old_idx) in order.iter().enumerate() {
            next[lo + k] = old[old_idx].clone();
        }
        if let Some(node) = snarl.get_node_mut(node_id) {
            node.params.insert(ids_key.into(), Value::Array(next));
        }
    }
}

pub(crate) fn show_automap_split_body(
    node_id: NodeId,
    outputs: &[OutPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
) {
    // Current individual outputs — output_pin_ids[0] = "automap_pass" (skip it).
    let current_ids: Vec<String> = snarl.get_node(node_id)
        .and_then(|n| n.params.get("output_pin_ids").and_then(|v| v.as_array()))
        .map(|a| a.iter().skip(1).map(|v| v.as_str().unwrap_or("").to_string()).collect())
        .unwrap_or_default();

    let family_ref = splitter_upstream_family(snarl, node_id);
    // Own ports of the raw pads upstream (`extra:` pins): not canonical, so
    // offered by name from the pads' learned layouts.
    let upstream_extras: Vec<(String, String, SignalType)> = snarl.get_node(node_id)
        .and_then(|n| n.inputs.iter().position(|p| p.signal_type == SignalType::AutoMap))
        .and_then(|i| snarl.in_pin(egui_snarl::InPinId { node: node_id, input: i }).remotes.first().copied())
        .map(|src| crate::app::find_automap_device_ids_for_viewer(snarl, src, None))
        .unwrap_or_default()
        .iter()
        .flat_map(|d| crate::raw_learn::extra_pins_of(d))
        .collect();

    // The list's width is this constant, not the longest pin name on show: a splitter pointed
    // at a pad with long learned names was 263 points wide, wider than most of the modules it
    // feeds. A name that doesn't fit is cut with an ellipsis and the whole of it is on hover -
    // The list's width. The names in it are never abbreviated - telling one learned port from
    // another is the module's whole job - so the row is wide enough to read them, and only what
    // still won't fit is cut with the whole name on hover.
    const LIST_W: f32 = 170.0;
    ui.vertical(|ui| {
        ui.set_min_width(LIST_W);

        // The sockets' rows, as a Split's strips stand on (`kit::StripRows`): each port's strip
        // is on the row of its own socket - the strip names the port, so the socket carries no
        // label beside it - from the top of the body (the bus is on the header and takes no
        // row), and `add pin` after them.
        let rows_at = super::kit::StripRows::at_cursor("module.automap_split", ui).past_empty_row();
        let n = current_ids.len();
        let skin = crate::canvas::remapper_icons::Skin::from_str(family_ref.unwrap_or(""));
        if n > 0 {
            ui.allocate_exact_size(egui::vec2(LIST_W, rows_at.height(n) - (rows_at.advance - rows_at.row_h)), egui::Sense::hover());
        }
        let (left, list_w) = (ui.max_rect().left(), ui.max_rect().width().max(LIST_W));
        if !super::simple_bodies::node_open(snarl, node_id) {
            return;
        }

        // Each port: a handle to move it, its name, and the `×` at the row's right edge where
        // the port strips keep theirs.
        let mut to_remove: Option<usize> = None;
        let mut rows: Vec<(usize, egui::Rect)> = Vec::new();
        let mut released: Option<(usize, egui::Pos2)> = None;
        for (i, pin_id) in current_ids.iter().enumerate() {
            let neutral = bus_pin_neutral_label(pin_id);
            let display = splitter_pin_label(pin_id, &neutral, family_ref);
            let band = rows_at.band(i, left, list_w);
            let (remove, where_dropped, rect) = automap_port_row(ui, node_id, "am_split", band, pin_id, &display, skin, i);
            if remove { to_remove = Some(i + 1); }
            if let Some(p) = where_dropped { released = Some((i, p)); }
            rows.push((i, rect));
        }
        if let Some(rm_idx) = to_remove {
            remove_automap_split_output(node_id, rm_idx, outputs, snarl);
        }
        // Which row the handle was let go over - worked out now that every row has a rect -
        // and the pins the list shows start one past the Auto-Map passthrough.
        if let Some((from, p)) = released {
            let to = rows.iter().find(|(_, r)| r.contains(p)).map(|(i, _)| *i).unwrap_or(from);
            if to != from {
                move_automap_port::<Outputs>(node_id, from + 1, to + 1, outputs, "output_pin_ids", 0, snarl);
            }
        }

        // `add pin` after the strips, a pill their width, opening the choice of pins. The
        // dropdown is a popup, so it can be as wide as the names need without the module
        // paying for it.
        let mut adds: Vec<(String, String, String, SignalType)> = Vec::new();
        for ap in am_canon::ALL_PINS {
            if current_ids.iter().any(|id| id == ap.id) { continue; }
            adds.push((ap.id.to_string(), splitter_pin_label(ap.id, ap.display_name, family_ref), ap.display_name.to_string(), ap.signal_type));
        }
        for (id, name, ty) in &upstream_extras {
            if current_ids.iter().any(|c| c == id) { continue; }
            adds.push((id.clone(), name.clone(), name.clone(), *ty));
        }
        let opts: Vec<(usize, &str)> = adds.iter().enumerate().map(|(k, a)| (k, a.1.as_str())).collect();
        ui.add_space(4.0);
        let (add_rect, _) = ui.allocate_exact_size(egui::vec2(list_w, rows_at.row_h), egui::Sense::hover());
        let add_ids: Vec<&str> = adds.iter().map(|a| a.0.as_str()).collect();
        if let Some(k) = add_pin_combo(ui, add_rect, &opts, &add_ids, skin, "Add an output for one pin of the bus.", ("am_split_add", node_id)) {
            let (id, _, name, ty) = &adds[k];
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.outputs.push(PinDescriptor::new(name.as_str(), *ty));
                match node.params.get_mut("output_pin_ids") {
                    Some(Value::Array(ids)) => ids.push(Value::String(id.clone())),
                    _ => {
                        node.params.insert("output_pin_ids".to_string(), Value::Array(vec![
                            Value::String("automap_pass".to_string()),
                            Value::String(id.clone()),
                        ]));
                    }
                }
            }
        }

        ui.add_space(4.0);
        let has_unused = outputs.iter().skip(1).any(|o| o.remotes.is_empty());
        let midi_id = automap_midi_open_id(node_id, "am_split");
        let midi_open = ui.ctx().data(|d| d.get_temp::<bool>(midi_id)).unwrap_or(false);
        let action = automap_action_row(ui, left, list_w, &[
            ("MIDI…", true, midi_open, "Add an output for a MIDI message on the bus: pick it under this row."),
            ("Clear", has_unused, false, "Remove every output with no wire on it."),
        ]);
        if action == Some(0) {
            ui.ctx().data_mut(|d| d.insert_temp(midi_id, !midi_open));
        }
        // A note comes with its velocity and aftertouch.
        let picked = if midi_open { midi_pin_picker(ui, (node_id, "am_split"), false) } else { None };
        for pin in picked.iter().flat_map(|p| p.node_pins()) {
            let id = pin.to_id();
            if let Some(node) = snarl.get_node_mut(node_id) {
                let has = node.params.get("output_pin_ids").and_then(|v| v.as_array())
                    .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(id.as_str())));
                if !has {
                    node.outputs.push(PinDescriptor::new(&pin.display_name(), pin.signal_type()));
                    match node.params.get_mut("output_pin_ids") {
                        Some(Value::Array(ids)) => ids.push(Value::String(id)),
                        _ => {
                            node.params.insert("output_pin_ids".to_string(), Value::Array(vec![
                                Value::String("automap_pass".to_string()),
                                Value::String(id),
                            ]));
                        }
                    }
                }
            }
        }

        if action == Some(1) && has_unused {
            let to_clear: Vec<usize> = outputs.iter().enumerate().skip(1)
                .filter(|(_, o)| o.remotes.is_empty())
                .map(|(i, _)| i)
                .rev().collect();
            for rm_idx in to_clear {
                let fresh_outputs: Vec<OutPin> = (0..snarl.get_node(node_id).map_or(0, |n| n.outputs.len()))
                    .map(|i| snarl.out_pin(OutPinId { node: node_id, output: i }))
                    .collect();
                remove_automap_split_output(node_id, rm_idx, &fresh_outputs, snarl);
            }
        }
    });
}

pub(crate) fn remove_automap_split_output(node_id: NodeId, rm_idx: usize, outputs: &[OutPin], snarl: &mut Snarl<NodeData>) {
    // Never remove index 0 (the AutoMap passthrough output).
    if rm_idx == 0 { return; }
    // Splitter ids cover every output, passthrough included — offset 0.
    remove_dynamic_pin::<Outputs>(node_id, rm_idx, outputs, snarl, "output_pin_ids", 0);
}

// ── AutoMap Collector body ────────────────────────────────────────────────────

pub(crate) fn show_automap_collect_body(
    node_id: NodeId,
    inputs: &[InPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
) {
    // Individual input pin IDs stored in params["collect_input_pin_ids"] (parallel to inputs[1..]).
    let current_ids: Vec<String> = snarl.get_node(node_id)
        .and_then(|n| n.params.get("collect_input_pin_ids").and_then(|v| v.as_array()))
        .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
        .unwrap_or_default();

    let is_learning = snarl.get_node(node_id)
        .and_then(|n| n.params.get("learning").and_then(|v| v.as_bool()))
        .unwrap_or(false);

    let family_ref = collector_downstream_family(snarl, node_id);

    // Same list as the splitter's: a handle per port, its full name, the `×` at the right -
    // each on its socket's row, the bus pin's row holding `add pin`.
    const LIST_W: f32 = 170.0;
    ui.vertical(|ui| {
        ui.set_min_width(LIST_W);
        let rows_at = super::kit::StripRows::at_cursor("module.automap_collect", ui).past_empty_row();
        let n = current_ids.len();
        let skin = crate::canvas::remapper_icons::Skin::from_str(family_ref.unwrap_or(""));
        if n > 0 {
            ui.allocate_exact_size(egui::vec2(LIST_W, rows_at.height(n) - (rows_at.advance - rows_at.row_h)), egui::Sense::hover());
        }
        let (left, list_w) = (ui.max_rect().left(), ui.max_rect().width().max(LIST_W));
        if !super::simple_bodies::node_open(snarl, node_id) {
            return;
        }

        // Existing individual inputs.
        let mut to_remove: Option<usize> = None;
        let mut rows: Vec<(usize, egui::Rect)> = Vec::new();
        let mut released: Option<(usize, egui::Pos2)> = None;
        for (i, pin_id) in current_ids.iter().enumerate() {
            // Resolve display name: canonical list, then MIDI, otherwise the raw
            // key name (learned keys use their egui Key Debug name as both id
            // and display).
            let neutral = bus_pin_neutral_label(pin_id);
            let display = splitter_pin_label(pin_id, &neutral, family_ref);
            let band = rows_at.band(i, left, list_w);
            let (remove, where_dropped, rect) = automap_port_row(ui, node_id, "am_collect", band, pin_id, &display, skin, i);
            if remove { to_remove = Some(i + 1); }
            if let Some(p) = where_dropped { released = Some((i, p)); }
            rows.push((i, rect));
        }
        if let Some(rm_idx) = to_remove {
            remove_automap_collect_input(node_id, rm_idx, inputs, snarl);
        }
        // Input 0 is the Auto-Map bus pin, which isn't in the list: the pins it shows and the
        // id list beside them both run one behind it.
        if let Some((from, p)) = released {
            let to = rows.iter().find(|(_, r)| r.contains(p)).map(|(i, _)| *i).unwrap_or(from);
            if to != from {
                move_automap_port::<Inputs>(node_id, from + 1, to + 1, inputs, "collect_input_pin_ids", 1, snarl);
            }
        }

        // ── `add pin`: one input per canonical pin, as on the Splitter ─────────
        let adds: Vec<&am_canon::AutoMapPin> = am_canon::ALL_PINS
            .iter()
            .filter(|ap| !current_ids.iter().any(|id| id == ap.id))
            .collect();
        let labels: Vec<String> = adds.iter().map(|ap| splitter_pin_label(ap.id, ap.display_name, family_ref)).collect();
        let opts: Vec<(usize, &str)> = labels.iter().enumerate().map(|(k, l)| (k, l.as_str())).collect();
        ui.add_space(4.0);
        let (add_rect, _) = ui.allocate_exact_size(egui::vec2(list_w, rows_at.row_h), egui::Sense::hover());
        let add_ids: Vec<&str> = adds.iter().map(|ap| ap.id).collect();
        if let Some(k) = add_pin_combo(ui, add_rect, &opts, &add_ids, skin, "Add an input for one pin of the bus.", ("am_collect_add", node_id)) {
            let ap = adds[k];
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.inputs.push(PinDescriptor::new(ap.display_name, ap.signal_type));
                match node.params.get_mut("collect_input_pin_ids") {
                    Some(Value::Array(ids)) => { ids.push(Value::String(ap.id.to_string())); }
                    _ => {
                        node.params.insert("collect_input_pin_ids".to_string(), Value::Array(vec![
                            Value::String(ap.id.to_string()),
                        ]));
                    }
                }
            }
        }

        ui.add_space(4.0);
        let has_unwired = inputs.iter().skip(1).any(|p| p.remotes.is_empty());
        let midi_id = automap_midi_open_id(node_id, "am_collect");
        let midi_open = ui.ctx().data(|d| d.get_temp::<bool>(midi_id)).unwrap_or(false);
        let action = automap_action_row(ui, left, list_w, &[
            ("Learn", true, is_learning, if is_learning { "Press a key… (Esc cancels)" } else { "Add an input for the next key pressed." }),
            ("MIDI…", true, midi_open, "Add an input for a MIDI message: pick it under this row."),
            ("Clear", has_unwired, false, "Remove every input with no wire on it."),
        ]);
        match action {
            Some(0) => {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert("learning".to_string(), Value::Bool(!is_learning));
                }
            }
            Some(1) => ui.ctx().data_mut(|d| d.insert_temp(midi_id, !midi_open)),
            _ => {}
        }
        // A MIDI pin collected here is PRODUCED: a MIDI Out sink downstream
        // sends it even with Thru off.
        // A note comes with its velocity and aftertouch.
        let picked = if midi_open { midi_pin_picker(ui, (node_id, "am_collect"), true) } else { None };
        for pin in picked.iter().flat_map(|p| p.node_pins()) {
            let id = pin.to_id();
            if !current_ids.iter().any(|c| c == &id) {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.inputs.push(PinDescriptor::new(&pin.display_name(), pin.signal_type()));
                    match node.params.get_mut("collect_input_pin_ids") {
                        Some(Value::Array(ids)) => ids.push(Value::String(id)),
                        _ => {
                            node.params.insert("collect_input_pin_ids".to_string(),
                                Value::Array(vec![Value::String(id)]));
                        }
                    }
                }
            }
        }

        // ── Learn-key (capture next keypress; works for any key egui knows) ──
        if is_learning && action != Some(0) {
            ui.label(super::simple_bodies::control_text("Press a key… (Esc cancels)").weak());

            let key_pressed = ui.input(|i| {
                i.events.iter().find_map(|e| {
                    if let egui::Event::Key { key, pressed: true, .. } = e {
                        Some(*key)
                    } else { None }
                })
            });

            if let Some(key) = key_pressed {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert("learning".to_string(), Value::Bool(false));
                }
                if key != egui::Key::Escape {
                    let pin_name = format!("{key:?}");
                    let already_has = current_ids.iter().any(|id| id == &pin_name);
                    if !already_has {
                        if let Some(node) = snarl.get_node_mut(node_id) {
                            // Bool — every key is a digital signal.  Pin id == display name == egui Key Debug.
                            node.inputs.push(PinDescriptor::new(&pin_name, SignalType::Bool));
                            match node.params.get_mut("collect_input_pin_ids") {
                                Some(Value::Array(ids)) => { ids.push(Value::String(pin_name)); }
                                _ => {
                                    node.params.insert("collect_input_pin_ids".to_string(),
                                        Value::Array(vec![Value::String(pin_name)]));
                                }
                            }
                        }
                    }
                }
            }
        }

        if action == Some(2) && has_unwired {
            let to_clear: Vec<usize> = inputs.iter().enumerate().skip(1)
                .filter(|(_, p)| p.remotes.is_empty())
                .map(|(i, _)| i)
                .rev().collect();
            for rm_idx in to_clear {
                let fresh_inputs: Vec<InPin> = (0..snarl.get_node(node_id).map_or(0, |n| n.inputs.len()))
                    .map(|i| snarl.in_pin(InPinId { node: node_id, input: i }))
                    .collect();
                remove_automap_collect_input(node_id, rm_idx, &fresh_inputs, snarl);
            }
        }
    });
}

pub(crate) fn remove_automap_collect_input(node_id: NodeId, rm_idx: usize, inputs: &[InPin], snarl: &mut Snarl<NodeData>) {
    if rm_idx == 0 { return; } // Never remove the AutoMap passthrough input.
    // `collect_input_pin_ids` does NOT include the passthrough `input[0]`, so
    // pin `n`'s id lives at `n - 1` — hence offset 1.
    remove_dynamic_pin::<Inputs>(node_id, rm_idx, inputs, snarl, "collect_input_pin_ids", 1);
}

// ── AutoMap Fork body ─────────────────────────────────────────────────────────

/// AutoMap Fork (`module.automap_fork`): one bus out of as many sockets as are asked for. Its
/// outputs are a column of port strips, as a Split's are - the same rows, handles, `×` and
/// header `+` - where it used to be a stack of `× out_0` labels over a "+ output" button that
/// lined up with none of its sockets.
pub(crate) fn show_automap_fork_body(
    node_id: NodeId,
    inputs: &[InPin],
    outputs: &[OutPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
) {
    use super::simple_bodies::{show_port_strips, InterpRow, StripSide};
    show_port_strips(node_id, inputs, outputs, ui, snarl, "module.automap_fork", StripSide::Outputs, 0, 0, InterpRow::NONE);
}

// ── AutoMap Selector body ─────────────────────────────────────────────────────

/// AutoMap Selector (`module.automap_selector`): `select` picks which bus goes through. Its
/// buses are a column of port strips under the `select` pin's row, as a Selector's values are.
pub(crate) fn show_automap_selector_body(
    node_id: NodeId,
    inputs: &[InPin],
    outputs: &[OutPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
) {
    use super::simple_bodies::{show_port_strips, InterpRow, StripSide};
    // inputs[0] = select (fixed); inputs[1..] = in_0, in_1, ... (dynamic AutoMap)
    show_port_strips(node_id, inputs, outputs, ui, snarl, "module.automap_selector", StripSide::Inputs, 1, 1, InterpRow::NONE);
}

// ── AutoMap Combiner body ─────────────────────────────────────────────────────

/// Per-input structural snapshot: which canonical AutoMap pins this port's
/// upstream emits, and a short human label for the device. Used by the
/// Combiner body to compute overlap-by-connection (independent of live values)
/// and to annotate input rows with the device they're wired to.
pub(crate) struct CombinerInputInfo {
    /// Pins this port's upstream offers. Empty if the port is unwired or the
    /// upstream couldn't be traced.
    offered: std::collections::HashSet<&'static str>,
    /// Short display label (e.g. "DualSense", "Xbox / XInput", "Remapper").
    /// Empty when the port is unwired.
    label: String,
}

/// A node that hands on a whole AutoMap bus of its own, and what to call it in
/// the Combiner's list. Any canonical pin could come out of one of these, so each
/// offers the full set — which is what makes a pin show up as overlapping.
///
/// Every module in `module_ui_info::republishes_automap_bus` has to be here, or
/// the Combiner treats that input as offering nothing and quietly lists no
/// conflicts to resolve (the JSM module's first outing did exactly that). The
/// bus-shaped nodes that don't republish — Fork, Selector, Combiner, Remapper —
/// belong here too.
pub(crate) fn bus_label(module_id: &str) -> Option<&'static str> {
    Some(match module_id {
        "module.automap_collect" => "Collector",
        "module.automap_fork" => "Fork",
        "module.automap_selector" => "Selector",
        "module.automap_combiner" => "Combiner",
        "module.remapper" => "Remapper",
        "module.jsm" => "JSM Config",
        "module.audio_stream_haptics" => "Audio Haptics",
        "module.network_send" => "Network Send",
        "module.network_recv" => "Network Receive",
        "module.automap_response_curve" => "Response Curve",
        "module.automap_twoway_response_curve" => "Two-way Curve",
        "module.area_mapper" => "Area Mapper",
        "module.stick_rotation" => "Stick Rotation",
        "processing.rws" => "RWS Aim",
        _ => return None,
    })
}

/// What one Combiner port traces back to.
enum TracedInput {
    /// A physical `device.source`: its id and the canonical pins it offers.
    Device { dev_id: String, pins: std::collections::HashSet<&'static str> },
    /// A bus-shaped node, which may offer any pin.
    Bus(&'static str),
}

/// `parent` is the enclosing canvas when the Combiner sits inside a sub-patch.
///
/// ❗ Inside a sub-patch every input arrives through a `subpatch.inlet`, and the
/// walk used to stop there: each port traced to nothing, offered no pins, and
/// the Combiner listed no overlap at all — no Resolution rows, no Motion
/// section, so nothing to choose CORRELATE from. It now pops out through the
/// inlet into the enclosing canvas (and descends through a sub-patch's outlet
/// on the way), as the 3D viewer's trace does.
pub(crate) fn combiner_inputs_info(
    snarl: &Snarl<NodeData>,
    node_id: NodeId,
    parent: Option<&AutomapGlowParent<'_>>,
) -> Vec<CombinerInputInfo> {
    /// Walks the AutoMap chain to the originating `device.source`, or stops at
    /// a bus-shaped node (Remapper/Collector/Combiner/Fork/Selector/...).
    fn trace(
        snarl: &Snarl<NodeData>,
        src: OutPinId,
        parent: Option<&AutomapGlowParent<'_>>,
        depth: u32,
    ) -> Option<TracedInput> {
        if depth > 16 { return None; }
        let node = snarl.get_node(src.node)?;
        match node.module_id.as_str() {
            "device.source" => {
                let dev_id = node.params.get("device_id").and_then(|v| v.as_str())?.to_string();
                // Read off the node the walk reached — it may live in another
                // canvas than the Combiner, so searching by id would miss it.
                let pins = node.params.get("output_pin_ids")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter()
                        .filter_map(|v| v.as_str())
                        .filter_map(|s| am_canon::ALL_PINS.iter().find(|ap| ap.id == s).map(|ap| ap.id))
                        .collect())
                    .unwrap_or_default();
                Some(TracedInput::Device { dev_id, pins })
            }
            // Splitter is a transparent passthrough on its AutoMap input.
            "module.automap_split" => {
                let in_idx = node.inputs.iter().position(|p| p.signal_type == SignalType::AutoMap)?;
                let pin = snarl.in_pin(InPinId { node: src.node, input: in_idx });
                let upstream = *pin.remotes.first()?;
                trace(snarl, upstream, parent, depth + 1)
            }
            // Out of the sub-patch, through the outer wire into this inlet.
            "subpatch.inlet" => {
                let pin_idx = node.params.get("pin_index").and_then(|v| v.as_u64())? as usize;
                let p = parent?;
                let outer = p.snarl.in_pin(InPinId { node: p.subpatch_node_id, input: pin_idx });
                trace(p.snarl, *outer.remotes.first()?, p.prev, depth + 1)
            }
            // Into a sub-patch, through the outlet feeding this output.
            "subpatch" => {
                let sp = node.subpatch.as_ref()?;
                let outlet = sp.snarl.nodes_ids_data()
                    .find(|(_, n)| n.value.module_id == "subpatch.outlet"
                        && n.value.params.get("pin_index").and_then(|v| v.as_u64()) == Some(src.output as u64))
                    .map(|(id, _)| id)?;
                let inner = *sp.snarl.in_pin(InPinId { node: outlet, input: 0 }).remotes.first()?;
                let frame = AutomapGlowParent { snarl, subpatch_node_id: src.node, prev: parent };
                trace(&sp.snarl, inner, Some(&frame), depth + 1)
            }
            id => bus_label(id).map(TracedInput::Bus),
        }
    }

    let n = snarl.get_node(node_id).map(|n| n.inputs.len()).unwrap_or(0);
    let mut out: Vec<CombinerInputInfo> = Vec::with_capacity(n);
    for i in 0..n {
        let in_pin = snarl.in_pin(InPinId { node: node_id, input: i });
        let traced = in_pin.remotes.first().and_then(|&src| trace(snarl, src, parent, 0));
        let (offered, label) = match traced {
            // Physical device.source: pin set is whatever its output_pin_ids declares.
            Some(TracedInput::Device { dev_id, pins }) => {
                // Joy-Cons by their pad slug: their glyph family
                // (`family_slug_from_device_id`) would call them a Switch Pro.
                let label = match crate::canvas::remapper_icons::phys_pad_slug(&dev_id) {
                    Some("joycon2_l") => "Joy-Con 2 (L)".to_string(),
                    Some("joycon2_r") => "Joy-Con 2 (R)".to_string(),
                    Some("joycon_l") => "Joy-Con (L)".to_string(),
                    Some("joycon_r") => "Joy-Con (R)".to_string(),
                    Some("joycon_pair") => "Joy-Con pair".to_string(),
                    _ => match family_slug_from_device_id(&dev_id) {
                        Some("dualsense")  => "DualSense".to_string(),
                        Some("ds4")        => "DualShock 4".to_string(),
                        Some("xinput")     => "Xbox / XInput".to_string(),
                        Some("switch_pro") => "Switch Pro".to_string(),
                        _ => dev_id,
                    },
                };
                (pins, label)
            }
            // Collector-shaped node: offers the full canonical pin set (any
            // pin could be overridden into the bus).
            Some(TracedInput::Bus(tag)) => {
                let pins: std::collections::HashSet<&'static str> =
                    am_canon::ALL_PINS.iter().map(|p| p.id).collect();
                (pins, tag.to_string())
            }
            None => (std::collections::HashSet::new(), String::new()),
        };
        out.push(CombinerInputInfo { offered, label });
    }
    out
}

/// Move an input pin from `from_idx` to `to_idx`, preserving wire connections.
/// Captures every input's remotes, drops all inputs, reorders descriptors,
/// then reconnects each in the new positions.
pub(crate) fn combiner_move_input(
    node_id: NodeId,
    inputs: &[InPin],
    snarl: &mut Snarl<NodeData>,
    from_idx: usize,
    to_idx: usize,
) {
    if from_idx == to_idx { return; }
    let n = inputs.len();
    if from_idx >= n || to_idx >= n { return; }
    let remotes: Vec<Vec<OutPinId>> = inputs.iter().map(|p| p.remotes.clone()).collect();
    // Drop all wires.
    for i in 0..n { snarl.drop_inputs(InPinId { node: node_id, input: i }); }
    // Reorder descriptors and remotes-by-old-index together.
    let mut order: Vec<usize> = (0..n).collect();
    let moved = order.remove(from_idx);
    order.insert(to_idx, moved);
    if let Some(node) = snarl.get_node_mut(node_id) {
        let new_inputs: Vec<PinDescriptor> = order.iter().map(|&i| node.inputs[i].clone()).collect();
        node.inputs = new_inputs;
        // Rename to in_0, in_1, … so the pin labels stay tidy.
        for (i, desc) in node.inputs.iter_mut().enumerate() {
            desc.name = format!("in_{i}");
        }
    }
    // Reconnect using the new index of each old input.
    for (new_idx, &old_idx) in order.iter().enumerate() {
        let new_in = InPinId { node: node_id, input: new_idx };
        for &src in &remotes[old_idx] {
            snarl.connect(src, new_in);
        }
    }
    // Two settings are stored by port NUMBER, so they have to follow the ports they
    // describe: `combiner_port_default` (how that port's pins merge unless a pin says
    // otherwise) and `combiner_pin_port` (a pin resolved against one particular port). The
    // engine reads ports by position - the lowest one offering a pin wins - so leaving these
    // keyed to the position rather than the port silently hands a device's merge policy to
    // whichever device lands in its slot, and unpins a pin from the controller it was set on.
    if let Some(node) = snarl.get_node_mut(node_id) {
        // Keys that are port indices: the entry at the old index belongs at the new one.
        for key in ["combiner_port_default"] {
            let moved: serde_json::Map<String, Value> = node
                .params
                .get(key)
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default();
            if moved.is_empty() {
                continue;
            }
            let mut next = serde_json::Map::new();
            for (new_idx, &old_idx) in order.iter().enumerate() {
                if let Some(v) = moved.get(&old_idx.to_string()) {
                    next.insert(new_idx.to_string(), v.clone());
                }
            }
            for (k, v) in moved {
                if k.parse::<usize>().is_err() {
                    next.insert(k, v); // not a port index: leave it be
                }
            }
            node.params.insert(key.to_string(), Value::Object(next));
        }
        // Values that are port indices: the pin keeps pointing at the same port, which now
        // sits somewhere else.
        let pinned: serde_json::Map<String, Value> = node
            .params
            .get("combiner_pin_port")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        if !pinned.is_empty() {
            // `where_new[old]` - the position an old port index lands at.
            let mut where_new = vec![usize::MAX; n];
            for (new_idx, &old_idx) in order.iter().enumerate() {
                where_new[old_idx] = new_idx;
            }
            let mut next = pinned.clone();
            for (_, v) in next.iter_mut() {
                let old = match v {
                    Value::Number(num) => num.as_u64().map(|x| x as usize),
                    Value::String(s) => s.parse::<usize>().ok(),
                    _ => None,
                };
                if let Some(new) = old.and_then(|o| where_new.get(o).copied()) {
                    *v = Value::Number(serde_json::Number::from(new));
                }
            }
            node.params.insert("combiner_pin_port".to_string(), Value::Object(next));
        }
    }
}

pub(crate) fn show_automap_combiner_body(
    node_id: NodeId,
    inputs: &[InPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    parent: Option<&AutomapGlowParent<'_>>,
    pinned: bool,
) {
    let _ = live_signals; // no longer used: structural overlap is connection-based
    // Folded, no row is drawn, but the frame keeps the width the rows would ask for: a collapsed
    // node whose body is skipped would otherwise shrink to its header alone, and a module three
    // blocks across has no room left for its own name. Snarl runs a collapsed node's body and
    // shifts it up under the header's clip, which is how these rows came to sit in the space
    // above the frame - out of sight, and still taking the pointer there. A pinned copy has no
    // folded state of its own to answer for.
    if !pinned && !super::simple_bodies::node_open(snarl, node_id) {
        const COMBINER_W: f32 = 194.0; // handle, port name, resolution dropdown, `×` and pads
        ui.allocate_exact_size(egui::vec2(COMBINER_W, 0.0), egui::Sense::hover());
        return;
    }
    let n = snarl.get_node(node_id).map(|n| n.inputs.len()).unwrap_or(0);
    let infos = combiner_inputs_info(snarl, node_id, parent);

    // overlap[pin] = list of port indices whose upstream offers this pin.
    // A pin counts as conflicting when ≥2 inputs offer it (structural overlap,
    // not a live-value check — stays stable while the user is just looking).
    let mut overlap: std::collections::HashMap<&'static str, Vec<usize>> = std::collections::HashMap::new();
    for (i, info) in infos.iter().enumerate() {
        for &pin in &info.offered {
            overlap.entry(pin).or_default().push(i);
        }
    }
    let is_conflict = |pin_id: &str| -> bool {
        overlap.get(pin_id).map_or(false, |w| w.len() >= 2)
    };

    let body_resp = ui.vertical(|ui| {
        ui.set_min_width(140.0);

        let mut to_remove: Option<usize> = None;
        let mut to_move: Option<(usize, usize)> = None;
        #[allow(unused_assignments)]
        let mut strip_right = 0.0f32;

        // Per-port default merge policy (SORT default). Applies to every pin a
        // port offers that has no per-pin override. Collected here; written
        // after the loop to avoid a mutable-borrow conflict with `snarl`.
        let port_default_map: serde_json::Map<String, Value> = snarl.get_node(node_id)
            .and_then(|node| node.params.get("combiner_port_default"))
            .and_then(|v| v.as_object()).cloned().unwrap_or_default();
        let mut next_port_default: Vec<(usize, &'static str)> = Vec::new();
        // The rows are the rows of the input sockets, on the `StripRows` a Selector's strips
        // stand on, painted and moved the way those are: a strip per port, a handle that
        // drags it, the rows it passes sliding aside (`StripAnim`). They used to be live
        // drop zones in a column of their own pitch, which is why this list stepped the
        // module's height unevenly and was the one list that didn't move under the finger.
        use super::simple_bodies::{StripAnim, STRIP_GAP, STRIP_HANDLE_W, STRIP_PAD, STRIP_REMOVE_H, STRIP_REMOVE_W};
        const COMBO_W: f32 = 58.0;
        let rows_at = super::kit::StripRows::at_cursor("module.automap_combiner", ui);
        let pitch = rows_at.advance;
        let small_font = egui::TextStyle::Small.resolve(ui.style());
        let measure = |s: &str| -> f32 {
            if s.is_empty() {
                return 0.0;
            }
            ui.painter()
                .layout_no_wrap(s.to_string(), small_font.clone(), egui::Color32::PLACEHOLDER)
                .size()
                .x
        };
        // Every row the same width, sized by the longest port name and device label in the
        // list. Measured from the text the rows are going to hold, never from the frame the
        // module has, so it doesn't ratchet; painted at the width the column has, so the
        // strips reach the same right edge whatever they say.
        let widest = (0..n)
            .map(|i| {
                let dev = infos.get(i).map(|info| info.label.as_str()).unwrap_or("");
                let gap = if dev.is_empty() { 0.0 } else { STRIP_GAP };
                measure(&format!("in_{i}")) + gap + measure(dev)
            })
            .fold(0.0f32, f32::max);
        let need_w = 2.0 * STRIP_PAD
            + STRIP_HANDLE_W
            + if n > 2 { STRIP_REMOVE_W + STRIP_GAP } else { 0.0 }
            + STRIP_GAP
            + widest
            + STRIP_GAP
            + COMBO_W;
        let left = ui.cursor().left();
        let strip_w = ui.max_rect().width().max(need_w);
        // Where the strips end: the resolution rows below put their dropdowns against it.
        strip_right = left + strip_w;
        ui.allocate_exact_size(egui::vec2(need_w, rows_at.height(n) - ui.spacing().item_spacing.y), egui::Sense::hover());

        let (fill_active, fill_hovered, fill_idle) = {
            let v = ui.visuals();
            (v.widgets.active.bg_fill, v.widgets.hovered.bg_fill, v.widgets.inactive.weak_bg_fill)
        };
        let weak = ui.visuals().weak_text_color();
        let name_color = ui.visuals().text_color();
        let outline = ui.visuals().widgets.hovered.fg_stroke.color;
        let anim_key = egui::Id::new(("fxi_combiner_anim", node_id.0));
        let mut anim = StripAnim::begin(ui.ctx(), anim_key, n, pitch);
        for i in 0..n {
            // A port "is in conflict" if any pin it offers is offered by ≥2 ports.
            let is_conflict_row = infos.get(i).map_or(false, |info| {
                info.offered.iter().any(|p| is_conflict(p))
            });
            let strip = rows_at
                .band(i, left, strip_w)
                .shrink2(egui::vec2(0.0, 1.0))
                .translate(egui::vec2(0.0, anim.offset(i)));
            super::simple_bodies::trace_rect(ui, node_id, &format!("combiner_row_{i}"), strip);
            super::simple_bodies::trace_rect(ui, node_id, &format!("strip_{i}"), strip);
            let dragged = anim.dragged(i);
            let hovered = ui.rect_contains_pointer(strip);
            let fill = if dragged {
                fill_active
            } else if hovered {
                fill_hovered
            } else {
                fill_idle
            };
            ui.painter().rect(strip, 4.0, fill, egui::Stroke::NONE, egui::StrokeKind::Inside);
            if dragged || hovered {
                ui.painter().rect(
                    strip,
                    4.0,
                    egui::Color32::TRANSPARENT,
                    egui::Stroke::new(1.0_f32, outline),
                    egui::StrokeKind::Inside,
                );
            }

            let mut cx = strip.left() + STRIP_PAD;
            let handle = egui::Rect::from_min_size(
                egui::pos2(cx, strip.center().y - pitch * 0.5),
                egui::vec2(STRIP_HANDLE_W, pitch),
            );
            let hresp = ui
                .interact(handle, egui::Id::new(("automap_combiner_handle", node_id.0, i)), egui::Sense::drag())
                .on_hover_cursor(egui::CursorIcon::Grab);
            ui.painter().text(handle.center(), egui::Align2::CENTER_CENTER, "⠿", small_font.clone(), weak);
            if let Some(mv) = anim.handle(&hresp, i) {
                if mv.0 != mv.1 {
                    to_move = Some(mv);
                }
            }
            hresp.on_hover_text("Drag to move this port up or down the list. Ports higher up win a Sort.");
            cx += STRIP_HANDLE_W + STRIP_GAP * 0.5;

            if n > 2 {
                let xrect = egui::Rect::from_min_size(
                    egui::pos2(cx, strip.center().y - STRIP_REMOVE_H * 0.5),
                    egui::vec2(STRIP_REMOVE_W, STRIP_REMOVE_H),
                );
                let r = ui.put(
                    xrect,
                    egui::Button::new(egui::RichText::new("×").size(crate::settings::ui_size(14.0)))
                        .min_size(egui::vec2(STRIP_REMOVE_W, STRIP_REMOVE_H)),
                );
                if r.on_hover_text("Remove this port").clicked() {
                    to_remove = Some(i);
                }
                cx += STRIP_REMOVE_W + STRIP_GAP * 0.5;
            }
            cx += STRIP_GAP * 0.5;

            // The per-port default policy, against the strip's right edge.
            let combo = egui::Rect::from_min_size(
                egui::pos2(strip.right() - STRIP_PAD * 0.5 - COMBO_W, strip.top() + 1.0),
                egui::vec2(COMBO_W, strip.height() - 2.0),
            );

            // The port, and after it the device it is fed by - cut short if the two don't
            // fit before the dropdown; the whole label is in the hover text.
            let port = format!("in_{i}");
            let port_col = if is_conflict_row { egui::Color32::from_rgb(255, 170, 80) } else { name_color };
            let pg = ui.painter().layout_no_wrap(port, small_font.clone(), port_col);
            let port_w = pg.size().x;
            ui.painter().galley(egui::pos2(cx, strip.center().y - pg.size().y * 0.5), pg, port_col);
            let dev_label = infos.get(i).map(|info| info.label.as_str()).unwrap_or("");
            if !dev_label.is_empty() {
                let dev_left = cx + port_w + STRIP_GAP;
                let room = combo.left() - STRIP_GAP - dev_left;
                if room > 8.0 {
                    let shown = super::simple_bodies::elide_to_width(ui, dev_label, small_font.clone(), weak, room);
                    ui.painter().text(
                        egui::pos2(dev_left, strip.center().y),
                        egui::Align2::LEFT_CENTER,
                        shown,
                        small_font.clone(),
                        weak,
                    );
                }
            }
            let words = egui::Rect::from_min_max(egui::pos2(cx, strip.top()), egui::pos2(combo.left() - STRIP_GAP, strip.bottom()));
            if words.width() > 0.0 {
                let tip = if dev_label.is_empty() {
                    format!("in_{i}")
                } else {
                    format!("in_{i} - {dev_label}")
                };
                let tip = if is_conflict_row {
                    format!("{tip}\nOffers pins another port offers too: see Resolution below.")
                } else {
                    tip
                };
                ui.interact(words, egui::Id::new(("automap_combiner_words", node_id.0, i)), egui::Sense::hover())
                    .on_hover_text(tip);
            }

            let cur_default = port_default_map.get(&i.to_string())
                .and_then(|v| v.as_str()).unwrap_or("SORT");
            let mut cell = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(combo)
                    .layout(egui::Layout::left_to_right(egui::Align::Center))
                    .id_salt(("automap_combiner_policy_cell", node_id, i)),
            );
            let pd_id = egui::Id::new(("automap_combiner_port_default", node_id, i));
            let cr = egui::ComboBox::from_id_salt(pd_id)
                .selected_text(super::simple_bodies::combo_text(ui, cur_default, COMBO_W))
                .width(COMBO_W)
                .show_ui(&mut cell, |ui| {
                    for &mode in &["SORT", "OR", "AND", "XOR", "ADD", "MULT"] {
                        if ui.selectable_label(cur_default == mode,
                            egui::RichText::new(mode).small()).clicked()
                        {
                            next_port_default.push((i, mode));
                        }
                    }
                });
            super::simple_bodies::trace_rect(ui, node_id, &format!("combiner_policy_{i}"), cr.response.rect);
            cr.response.on_hover_text(
                "How this port's pins are merged with the same pins from other ports, \
                 unless a pin is given its own rule under Resolution.",
            );
        }
        anim.end(ui.ctx(), anim_key);
        if let Some(rm) = to_remove {
            remove_input_pin(node_id, rm, inputs, snarl);
        }
        if let Some((from, to)) = to_move {
            combiner_move_input(node_id, inputs, snarl, from, to);
        }
        if !next_port_default.is_empty() {
            if let Some(node) = snarl.get_node_mut(node_id) {
                let mut map = node.params.get("combiner_port_default")
                    .and_then(|v| v.as_object()).cloned().unwrap_or_default();
                for (port, mode) in next_port_default {
                    if mode == "SORT" {
                        map.remove(&port.to_string()); // SORT is the implicit default.
                    } else {
                        map.insert(port.to_string(), Value::String(mode.to_string()));
                    }
                }
                if map.is_empty() {
                    node.params.remove("combiner_port_default");
                } else {
                    node.params.insert("combiner_port_default".to_string(), Value::Object(map));
                }
            }
        }

        // The `+` for another port lives in the header, like the Selector's and Split's.
        // A whole-module pin (a Combiner pinned inside a Controller3D layout) is drawn
        // without the node's header, so that one keeps the button in the body - otherwise
        // there is no way at all to add a port to it.
        if pinned {
            if ui
                .small_button("+ input")
                .on_hover_text("Add another input port")
                .clicked()
            {
                super::simple_bodies::add_port(node_id, "module.automap_combiner", snarl);
            }
        }

        // ── Resolution (collapsible, grouped by category) ─────────────────────
        // Lists pins offered by ≥2 connected inputs (structural overlap — does
        // NOT depend on which buttons are physically held). Also includes any
        // pin that has a stored policy / port override so users can find their
        // overrides regardless of overlap. Each row exposes:
        //   - Policy dropdown (SORT default + OR/AND/XOR/ADD/MULT)
        //   - Port dropdown (Auto default; or pin a specific input port 0..N-1)
        let current_policy: serde_json::Map<String, Value> = snarl.get_node(node_id)
            .and_then(|n| n.params.get("combiner_pin_policy"))
            .and_then(|v| v.as_object()).cloned().unwrap_or_default();
        let current_port: serde_json::Map<String, Value> = snarl.get_node(node_id)
            .and_then(|n| n.params.get("combiner_pin_port"))
            .and_then(|v| v.as_object()).cloned().unwrap_or_default();
        // The Motion section's own mode (`combiner_motion`): gyro, accel and
        // orientation resolved as one unit — see the engine's combiner_motion.
        let current_motion = snarl.get_node(node_id)
            .and_then(|n| n.params.get("combiner_motion"))
            .and_then(|v| v.as_str()).unwrap_or("SORT").to_string();
        let mut next_motion: Option<&'static str> = None;
        let current_gate = snarl.get_node(node_id)
            .and_then(|n| n.params.get("combiner_motion_gate"))
            .and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
        let mut next_gate: Option<f32> = None;

        // Union of structurally-overlapping pins and explicitly-overridden pins.
        let mut tracked: std::collections::BTreeMap<&'static str, ()> = std::collections::BTreeMap::new();
        for (pin, who) in overlap.iter() {
            if who.len() >= 2 { tracked.insert(*pin, ()); }
        }
        for (pin, pol) in current_policy.iter() {
            if pol.as_str().unwrap_or("SORT") != "SORT" {
                if let Some(ap) = am_canon::ALL_PINS.iter().find(|p| p.id == pin.as_str()) {
                    tracked.insert(ap.id, ());
                }
            }
        }
        for pin in current_port.keys() {
            if let Some(ap) = am_canon::ALL_PINS.iter().find(|p| p.id == pin.as_str()) {
                tracked.insert(ap.id, ());
            }
        }

        // Pin → category.
        fn pin_category(id: &str) -> &'static str {
            match id {
                "left_stick" | "right_stick"
                | "left_stick_x" | "left_stick_y"
                | "right_stick_x" | "right_stick_y" => "Sticks",
                "left_trigger" | "right_trigger" => "Triggers",
                "dpad" | "dpad_x" | "dpad_y"
                | "dpad_up" | "dpad_down" | "dpad_left" | "dpad_right" => "D-Pad",
                "gyro_x" | "gyro_y" | "gyro_z"
                | "accel_x" | "accel_y" | "accel_z" | "orientation" => "Motion",
                id if id.starts_with("btn_") => "Buttons",
                id if id.starts_with("key_")
                    || id.starts_with("media_")
                    || id.starts_with("mouse")
                    || id.starts_with("scroll_") => "KB/M",
                _ => "Other",
            }
        }
        // Bucket pins by category, preserving ALL_PINS order within each bucket.
        let categories: &[&str] = &["Sticks", "Triggers", "D-Pad", "Buttons", "Motion", "KB/M", "Other"];
        let mut by_cat: std::collections::HashMap<&'static str, Vec<&'static str>> = std::collections::HashMap::new();
        let mut tracked_ordered: Vec<&'static str> = tracked.keys().copied().collect();
        tracked_ordered.sort_by_key(|p| am_canon::ALL_PINS.iter()
            .position(|ap| ap.id == *p).unwrap_or(usize::MAX));
        for pin_id in tracked_ordered {
            by_cat.entry(pin_category(pin_id)).or_default().push(pin_id);
        }

        let header_id = ui.make_persistent_id(("automap_combiner_resolution", node_id));
        let n_conf = overlap.values().filter(|w| w.len() >= 2).count();
        let n_pol = current_policy.values()
            .filter(|v| v.as_str().map_or(false, |s| s != "SORT")).count();
        let n_port = current_port.len();
        let n_over = n_pol + n_port;
        // The header of the tree is one more row of the list: a strip's height on the
        // strips' pitch, so a closed Combiner is its ports plus one row whatever the count -
        // egui's own collapsing header is a height of its own, and with the gap above it the
        // module's height stepped by a block at some port counts and two at others.
        let mut tree = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), header_id, false);
        let band_h = super::kit::StripRows::at_cursor("module.automap_combiner", ui).row_h;
        let (band, head) = ui.allocate_exact_size(
            egui::vec2((strip_right - ui.cursor().left()).max(140.0), band_h),
            egui::Sense::click(),
        );
        if head.clicked() {
            tree.toggle(ui);
        }
        {
            let weak = ui.visuals().weak_text_color();
            let font = egui::TextStyle::Small.resolve(ui.style());
            let icon = egui::Rect::from_center_size(
                egui::pos2(band.left() + 10.0, band.center().y),
                egui::Vec2::splat(10.0),
            );
            let openness = tree.openness(ui.ctx());
            egui::collapsing_header::paint_default_icon(ui, openness, &head.clone().with_new_rect(icon));
            let text_left = icon.right() + 6.0;
            let text = if n_over == 0 && n_conf == 0 {
                "Resolution".to_string()
            } else {
                format!("Resolution  ·  {n_conf} overlap, {n_over} set")
            };
            let shown = super::simple_bodies::elide_to_width(ui, &text, font.clone(), weak, band.right() - text_left);
            ui.painter().text(egui::pos2(text_left, band.center().y), egui::Align2::LEFT_CENTER, shown, font, weak);
            super::simple_bodies::trace_rect(ui, node_id, "combiner_resolution", band);
        }
        let head = head.on_hover_text(
            "Pins that more than one port offers, and the rule each is merged by. \
             A pin can be given its own rule, or be taken from one port only.",
        );
        tree.show_body_indented(&head, ui, |ui| {
                if tracked.is_empty() {
                    ui.label(egui::RichText::new("No overlapping inputs or overrides.").small().weak());
                    return;
                }
                let mut next_policy: Vec<(&'static str, &'static str)> = Vec::new();
                let mut next_port: Vec<(&'static str, Option<usize>)> = Vec::new();

                for &cat in categories {
                    let Some(pins) = by_cat.get(cat) else { continue; };
                    if pins.is_empty() { continue; }
                    let cat_id = egui::Id::new(("automap_combiner_cat", node_id, cat));
                    egui::CollapsingHeader::new(egui::RichText::new(format!("{cat} ({})", pins.len()))
                            .small().weak())
                        .id_salt(cat_id)
                        .default_open(false)
                        .show(ui, |ui| {
                            if cat == "Motion" {
                                motion_section_mode(ui, node_id, &current_motion, &mut next_motion,
                                    current_gate, &mut next_gate, strip_right);
                            }
                            for &pin_id in pins {
                                let neutral = am_canon::ALL_PINS.iter()
                                    .find(|p| p.id == pin_id)
                                    .map(|p| p.display_name)
                                    .unwrap_or(pin_id);
                                let has_override = current_policy.get(pin_id)
                                    .and_then(|v| v.as_str()).map_or(false, |s| s != "SORT")
                                    || current_port.contains_key(pin_id);
                                let current_pol = current_policy.get(pin_id)
                                    .and_then(|v| v.as_str()).unwrap_or("SORT");
                                let current_port_raw = current_port.get(pin_id).and_then(|v| v.as_u64());
                                let current_port_clamped: Option<usize> = current_port_raw
                                    .map(|p| (p as usize).min(n.saturating_sub(1)));
                                let port_label = current_port_clamped
                                    .map(|p| format!("in_{p}"))
                                    .unwrap_or_else(|| "Auto".to_string());
                                ui.horizontal(|ui| {
                                    let mut name = egui::RichText::new(neutral).small();
                                    if has_override {
                                        name = name.color(egui::Color32::from_rgb(255, 170, 80));
                                    }
                                    // Which ports offer this pin goes in the hover text: at
                                    // the end of the row it pushed every row to a different
                                    // length for something the port dropdown already lists.
                                    let who = overlap.get(pin_id).filter(|w| w.len() >= 2).map(|w| {
                                        w.iter().map(|i| format!("in_{i}")).collect::<Vec<_>>().join(", ")
                                    });
                                    let label = ui.label(name);
                                    if let Some(who) = who {
                                        label.on_hover_text(format!("Offered by {who}"));
                                    }
                                    // The two dropdowns end where the port strips end, on
                                    // every row, whatever the pin is called - in a cell of
                                    // their own (`right_cell`).
                                    let combos_w = 58.0 + 54.0 + ui.spacing().item_spacing.x;
                                    right_cell(ui, strip_right, combos_w, |ui| {

                                    // Stick and motion pins only expose
                                    // SORT/ADD/MULT — bitwise OR/AND/XOR don't
                                    // have meaningful semantics for analog axes.
                                    let modes: &[&str] = if matches!(cat, "Sticks" | "Motion") {
                                        &["SORT", "ADD", "MULT"]
                                    } else {
                                        &["SORT", "OR", "AND", "XOR", "ADD", "MULT"]
                                    };
                                    let pol_id = egui::Id::new(("automap_combiner_policy", node_id, pin_id));
                                    egui::ComboBox::from_id_salt(pol_id)
                                        .selected_text(super::simple_bodies::combo_text(ui, current_pol, 58.0))
                                        .width(58.0)
                                        .show_ui(ui, |ui| {
                                            for &mode in modes {
                                                if ui.selectable_label(current_pol == mode,
                                                    egui::RichText::new(mode).small()).clicked()
                                                {
                                                    next_policy.push((pin_id, mode));
                                                }
                                            }
                                        });

                                    let port_combo_id = egui::Id::new(("automap_combiner_port", node_id, pin_id));
                                    egui::ComboBox::from_id_salt(port_combo_id)
                                        .selected_text(super::simple_bodies::combo_text(ui, &port_label, 54.0))
                                        .width(54.0)
                                        .show_ui(ui, |ui| {
                                            let auto_selected = current_port_clamped.is_none();
                                            if ui.selectable_label(auto_selected,
                                                egui::RichText::new("Auto").small()).clicked()
                                            {
                                                next_port.push((pin_id, None));
                                            }
                                            for p in 0..n {
                                                let label = format!("in_{p}");
                                                let sel = current_port_clamped == Some(p);
                                                if ui.selectable_label(sel,
                                                    egui::RichText::new(&label).small()).clicked()
                                                {
                                                    next_port.push((pin_id, Some(p)));
                                                }
                                            }
                                        });
                                    });
                                });
                            }
                        });
                }
                // Apply policy changes after iteration (avoids mutable borrow conflict).
                if !next_policy.is_empty() || !next_port.is_empty() {
                    if let Some(node) = snarl.get_node_mut(node_id) {
                        if !next_policy.is_empty() {
                            let mut map = node.params.get("combiner_pin_policy")
                                .and_then(|v| v.as_object()).cloned().unwrap_or_default();
                            for (pin_id, mode) in next_policy {
                                if mode == "SORT" {
                                    map.remove(pin_id);
                                } else {
                                    map.insert(pin_id.to_string(), Value::String(mode.to_string()));
                                }
                            }
                            if map.is_empty() {
                                node.params.remove("combiner_pin_policy");
                            } else {
                                node.params.insert("combiner_pin_policy".to_string(),
                                    Value::Object(map));
                            }
                        }
                        if !next_port.is_empty() {
                            let mut map = node.params.get("combiner_pin_port")
                                .and_then(|v| v.as_object()).cloned().unwrap_or_default();
                            for (pin_id, port_opt) in next_port {
                                match port_opt {
                                    None => { map.remove(pin_id); }
                                    Some(p) => {
                                        map.insert(pin_id.to_string(),
                                            Value::Number(serde_json::Number::from(p)));
                                    }
                                }
                            }
                            if map.is_empty() {
                                node.params.remove("combiner_pin_port");
                            } else {
                                node.params.insert("combiner_pin_port".to_string(),
                                    Value::Object(map));
                            }
                        }
                    }
                }
            });
        if let Some(g) = next_gate {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("combiner_motion_gate".to_string(), Value::from(g as f64));
            }
        }
        if let Some(mode) = next_motion {
            if let Some(node) = snarl.get_node_mut(node_id) {
                if mode == "SORT" {
                    node.params.remove("combiner_motion"); // SORT is the implicit default.
                } else {
                    node.params.insert("combiner_motion".to_string(), Value::String(mode.to_string()));
                }
            }
        }
    });
    // Register the whole body as the pinnable element so Layout mode's "Pin
    // element ▶ Whole module" works for the combiner (matches remapper /
    // map_action, which were registered but the combiner was missed).
    register_exposable_element(ui, node_id, "whole_module", body_resp.response.rect);
}

#[cfg(test)]
mod combiner_trace_tests {
    use super::*;
    use flexinput_core::PinDescriptor;
    use serde_json::json;
    use std::collections::HashMap;

    fn node(module: &str, inputs: usize, outputs: usize, params: HashMap<String, Value>) -> NodeData {
        NodeData {
            module_id: module.to_string(),
            display_name: module.to_string(),
            category: String::new(),
            inputs: (0..inputs).map(|i| PinDescriptor::new(format!("in_{i}"), SignalType::AutoMap)).collect(),
            outputs: (0..outputs).map(|i| PinDescriptor::new(format!("out_{i}"), SignalType::AutoMap)).collect(),
            params,
            subpatch: None,
            extra: Default::default(),
        }
    }

    /// Two Joy-Con halves wired into a sub-patch, merged by a Combiner inside
    /// it. Each port's trace has to leave through its inlet into the outer
    /// canvas; it used to stop there, so the ports offered nothing and the
    /// Combiner listed no overlap — and with it no Motion section.
    #[test]
    fn a_combiner_inside_a_sub_patch_sees_its_devices_through_the_inlets() {
        let mut outer = Snarl::new();
        let src = |dev: &str| {
            let mut p = HashMap::new();
            p.insert("device_id".to_string(), json!(dev));
            p.insert("output_pin_ids".to_string(), json!(["gyro_x", "gyro_y", "gyro_z", "orientation", "btn_lb"]));
            node("device.source", 0, 1, p)
        };
        let l = outer.insert_node(egui::pos2(0.0, 0.0), src("jc2:joycon2_l:aa"));
        let r = outer.insert_node(egui::pos2(0.0, 0.0), src("jc2:joycon2_r:bb"));
        let sp = outer.insert_node(egui::pos2(0.0, 0.0), node("subpatch", 2, 0, HashMap::new()));
        outer.connect(OutPinId { node: l, output: 0 }, InPinId { node: sp, input: 0 });
        outer.connect(OutPinId { node: r, output: 0 }, InPinId { node: sp, input: 1 });

        let mut inner = Snarl::new();
        let inlet = |i: u64| {
            let mut p = HashMap::new();
            p.insert("pin_index".to_string(), json!(i));
            node("subpatch.inlet", 0, 1, p)
        };
        let i0 = inner.insert_node(egui::pos2(0.0, 0.0), inlet(0));
        let i1 = inner.insert_node(egui::pos2(0.0, 0.0), inlet(1));
        let comb = inner.insert_node(egui::pos2(0.0, 0.0), node("module.automap_combiner", 2, 1, HashMap::new()));
        inner.connect(OutPinId { node: i0, output: 0 }, InPinId { node: comb, input: 0 });
        inner.connect(OutPinId { node: i1, output: 0 }, InPinId { node: comb, input: 1 });

        let parent = AutomapGlowParent { snarl: &outer, subpatch_node_id: sp, prev: None };
        let infos = combiner_inputs_info(&inner, comb, Some(&parent));
        assert_eq!(infos.len(), 2);
        for info in &infos {
            assert!(info.offered.contains("gyro_x") && info.offered.contains("orientation"), "{:?}", info.offered);
        }
        assert!(infos[0].label.contains("(L)") && infos[1].label.contains("(R)"),
            "{} / {}", infos[0].label, infos[1].label);
        // Without the enclosing canvas the inlets lead nowhere — the old blindness.
        assert!(combiner_inputs_info(&inner, comb, None).iter().all(|i| i.offered.is_empty()));
    }
}
