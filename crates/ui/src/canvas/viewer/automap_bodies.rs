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
    slug != "switch_pro"
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

    let resp = ui.checkbox(&mut checked, egui::RichText::new("Override game feedback").small());
    if resp.changed() {
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
pub(crate) const CAPACITIVE_LABEL: &str = "Suppress capacitive touch";
pub(crate) const CAPACITIVE_HOVER: &str =
    "Mute this pad's capacitive sensors: touch on the stick caps and, on a \
     Steam Controller, the grips.\n\
     They fire just from holding the controller, which steals Learn captures \
     and triggers mappings you didn't press. Turn this on while you build the \
     mapping, then off to use those inputs.\n\
     Trackpads, pad clicks, paddles and every other button stay live.";

/// Advanced-mode device.source body toggle muting the capacitive stick-cap /
/// grip pins while the user builds a mapping. Stored on `suppress_touch_misc`;
/// consumed by the UI signal mask (Learn / previews) and the engine's
/// `preprocess_dev_sigs` (routing) from that one param.
pub(crate) fn touch_misc_header_toggle(
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    node: NodeId,
) {
    let mut checked = snarl.get_node(node)
        .and_then(|n| n.params.get("suppress_touch_misc"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

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
fn motion_section_mode(
    ui: &mut egui::Ui,
    node_id: NodeId,
    current: &str,
    next: &mut Option<&'static str>,
    gate: f32,
    next_gate: &mut Option<f32>,
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
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Section").small());
        let resp = egui::ComboBox::from_id_salt(egui::Id::new(("automap_combiner_motion", node_id)))
            .selected_text(egui::RichText::new(label).small())
            .width(110.0)
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
        if current == "CORRELATE" {
            let mut g = gate;
            ui.label(egui::RichText::new("noise gate ×").small().weak());
            let r = ui.add(egui::DragValue::new(&mut g).speed(0.02).range(0.0..=4.0).fixed_decimals(2))
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
            | "joycon2_l" | "joycon2_r"       => (true, true,  true),
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
            "switch_pro" => Some("switch_pro"),
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

/// Collapsible "MIDI…" section holding the MIDI pin picker. Returns the pin
/// when one was added. Collapsed by default so gamepad-only patches don't
/// carry the extra row.
pub(crate) fn automap_midi_section(
    ui: &mut egui::Ui,
    node_id: NodeId,
    salt: &str,
    for_output: bool,
) -> Option<flexinput_core::midi::MidiPin> {
    let mut picked = None;
    egui::CollapsingHeader::new(egui::RichText::new("MIDI…").small().weak())
        .id_salt((node_id, salt, "midi_section"))
        .default_open(false)
        .show(ui, |ui| {
            picked = midi_pin_picker(ui, (node_id, salt), for_output);
        });
    picked
}

/// Row label for a bus pin: canonical name, MIDI name, or the raw id.
fn bus_pin_neutral_label(pin_id: &str) -> String {
    if let Some(p) = am_canon::ALL_PINS.iter().find(|p| p.id == pin_id) {
        return p.display_name.to_string();
    }
    if flexinput_core::midi::is_midi_pin(pin_id) {
        return midi_pin_label(pin_id);
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
            .selected_text(egui::RichText::new(selected).small())
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

    ui.vertical(|ui| {
        ui.set_min_width(170.0);

        // Existing individual outputs with remove buttons.
        let mut to_remove: Option<usize> = None;
        for (i, pin_id) in current_ids.iter().enumerate() {
            let neutral = bus_pin_neutral_label(pin_id);
            let display = splitter_pin_label(pin_id, &neutral, family_ref);
            ui.horizontal(|ui| {
                if ui.small_button("×").clicked() { to_remove = Some(i + 1); }
                ui.label(egui::RichText::new(&display).small());
            });
        }
        if let Some(rm_idx) = to_remove {
            remove_automap_split_output(node_id, rm_idx, outputs, snarl);
        }

        ui.add_space(4.0);

        egui::ComboBox::from_id_salt((node_id, "add_am_split"))
            .selected_text(egui::RichText::new("+ Add output").small())
            .width(150.0)
            .show_ui(ui, |ui| {
                for ap in am_canon::ALL_PINS {
                    if current_ids.iter().any(|id| id == ap.id) { continue; }
                    let label = splitter_pin_label(ap.id, ap.display_name, family_ref);
                    if ui.selectable_label(false, egui::RichText::new(&label).small()).clicked() {
                        if let Some(node) = snarl.get_node_mut(node_id) {
                            node.outputs.push(PinDescriptor::new(ap.display_name, ap.signal_type));
                            match node.params.get_mut("output_pin_ids") {
                                Some(Value::Array(ids)) => { ids.push(Value::String(ap.id.to_string())); }
                                _ => {
                                    node.params.insert("output_pin_ids".to_string(), Value::Array(vec![
                                        Value::String("automap_pass".to_string()),
                                        Value::String(ap.id.to_string()),
                                    ]));
                                }
                            }
                        }
                    }
                }
            });

        // A note comes with its velocity and aftertouch.
        let picked = automap_midi_section(ui, node_id, "am_split", false);
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

        let has_unused = outputs.iter().skip(1).any(|o| o.remotes.is_empty());
        if has_unused && ui.small_button("Clear unused").clicked() {
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

    ui.vertical(|ui| {
        ui.set_min_width(170.0);

        // Existing individual inputs with remove buttons.
        let mut to_remove: Option<usize> = None;
        for (i, pin_id) in current_ids.iter().enumerate() {
            // Resolve display name: canonical list, then MIDI, otherwise the raw
            // key name (learned keys use their egui Key Debug name as both id
            // and display).
            let neutral = bus_pin_neutral_label(pin_id);
            let display = splitter_pin_label(pin_id, &neutral, family_ref);
            ui.horizontal(|ui| {
                if ui.small_button("×").clicked() { to_remove = Some(i + 1); }
                ui.label(egui::RichText::new(&display).small());
            });
        }
        if let Some(rm_idx) = to_remove {
            remove_automap_collect_input(node_id, rm_idx, inputs, snarl);
        }

        ui.add_space(4.0);

        // ── Add-input dropdown (canonical ALL_PINS) ───────────────────────────
        egui::ComboBox::from_id_salt((node_id, "add_am_collect"))
            .selected_text(egui::RichText::new("+ Add input").small())
            .width(150.0)
            .show_ui(ui, |ui| {
                for ap in am_canon::ALL_PINS {
                    if current_ids.iter().any(|id| id == ap.id) { continue; }
                    let label = splitter_pin_label(ap.id, ap.display_name, family_ref);
                    if ui.selectable_label(false, egui::RichText::new(&label).small()).clicked() {
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
                }
            });

        // A MIDI pin collected here is PRODUCED: a MIDI Out sink downstream
        // sends it even with Thru off.
        // A note comes with its velocity and aftertouch.
        let picked = automap_midi_section(ui, node_id, "am_collect", true);
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
        if is_learning {
            ui.label(egui::RichText::new("Press a key… (Esc cancels)").italics().small());

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
        } else if ui.small_button("+ Learn key").clicked() {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("learning".to_string(), Value::Bool(true));
            }
        }

        let has_unwired = inputs.iter().skip(1).any(|p| p.remotes.is_empty());
        if has_unwired && ui.small_button("Clear unused").clicked() {
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

pub(crate) fn show_automap_fork_body(
    node_id: NodeId,
    outputs: &[OutPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
) {
    let n_out = snarl.get_node(node_id).map(|n| n.outputs.len()).unwrap_or(2);
    ui.vertical(|ui| {
        ui.set_min_width(80.0);
        let mut to_remove: Option<usize> = None;
        for i in 0..n_out {
            ui.horizontal(|ui| {
                if n_out > 2 {
                    if ui.small_button("×").clicked() { to_remove = Some(i); }
                } else {
                    ui.add_space(18.0);
                }
                ui.label(egui::RichText::new(format!("out_{i}")).small());
            });
        }
        if let Some(rm) = to_remove {
            remove_output_pin(node_id, rm, outputs, snarl);
        }
        ui.add_space(2.0);
        if ui.small_button("+ output").clicked() {
            if let Some(node) = snarl.get_node_mut(node_id) {
                let next = node.outputs.len();
                node.outputs.push(PinDescriptor::new(format!("out_{next}"), SignalType::AutoMap));
            }
        }
    });
}

// ── AutoMap Selector body ─────────────────────────────────────────────────────

pub(crate) fn show_automap_selector_body(
    node_id: NodeId,
    inputs: &[InPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
) {
    // inputs[0] = select (fixed); inputs[1..] = in_0, in_1, ... (dynamic AutoMap)
    let n_value = snarl.get_node(node_id).map(|n| n.inputs.len().saturating_sub(1)).unwrap_or(2);
    ui.vertical(|ui| {
        ui.set_min_width(80.0);
        let mut to_remove: Option<usize> = None;
        for i in 0..n_value {
            ui.horizontal(|ui| {
                if n_value > 2 {
                    if ui.small_button("×").clicked() { to_remove = Some(i + 1); }
                } else {
                    ui.add_space(18.0);
                }
                ui.label(egui::RichText::new(format!("in_{i}")).small());
            });
        }
        if let Some(rm) = to_remove {
            remove_input_pin(node_id, rm, inputs, snarl);
        }
        ui.add_space(2.0);
        if ui.small_button("+ input").clicked() {
            if let Some(node) = snarl.get_node_mut(node_id) {
                let next = node.inputs.len() - 1;
                node.inputs.push(PinDescriptor::new(format!("in_{next}"), SignalType::AutoMap));
            }
        }
    });
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
                // Joy-Con halves by their pad slug: they have no glyph family,
                // so `family_slug_from_device_id` (a glyph lookup) knows nothing
                // of them.
                let label = match crate::canvas::remapper_icons::phys_pad_slug(&dev_id) {
                    Some("joycon2_l") => "Joy-Con 2 (L)".to_string(),
                    Some("joycon2_r") => "Joy-Con 2 (R)".to_string(),
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
}

pub(crate) fn show_automap_combiner_body(
    node_id: NodeId,
    inputs: &[InPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    live_signals: &std::collections::HashMap<(String, String), Signal>,
    parent: Option<&AutomapGlowParent<'_>>,
) {
    let _ = live_signals; // no longer used: structural overlap is connection-based
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

        // Per-port default merge policy (SORT default). Applies to every pin a
        // port offers that has no per-pin override. Collected here; written
        // after the loop to avoid a mutable-borrow conflict with `snarl`.
        let port_default_map: serde_json::Map<String, Value> = snarl.get_node(node_id)
            .and_then(|node| node.params.get("combiner_port_default"))
            .and_then(|v| v.as_object()).cloned().unwrap_or_default();
        let mut next_port_default: Vec<(usize, &'static str)> = Vec::new();

        for i in 0..n {
            // A port "is in conflict" if any pin it offers is offered by ≥2 ports.
            let is_conflict_row = infos.get(i).map_or(false, |info| {
                info.offered.iter().any(|p| is_conflict(p))
            });

            // Each row is a drop zone (egui draws a hover-highlight frame
            // automatically while a compatible payload is in flight). Inside it,
            // the drag handle glyph is the actual drag source so × stays clickable.
            let drop_frame = egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(2, 1));
            let (_, dropped) = ui.dnd_drop_zone::<usize, _>(drop_frame, |ui| {
                ui.horizontal(|ui| {
                    let handle_id = egui::Id::new(("automap_combiner_handle", node_id, i));
                    ui.dnd_drag_source(handle_id, i, |ui| {
                        ui.label(egui::RichText::new("⠿").weak().small());
                    });
                    if n > 2 {
                        if ui.small_button("×").clicked() { to_remove = Some(i); }
                    } else {
                        ui.add_space(18.0);
                    }
                    let mut label = egui::RichText::new(format!("in_{i}")).small();
                    if is_conflict_row {
                        label = label.color(egui::Color32::from_rgb(255, 170, 80));
                    }
                    ui.label(label);
                    let dev_label = infos.get(i).map(|info| info.label.as_str()).unwrap_or("");
                    if !dev_label.is_empty() {
                        ui.label(egui::RichText::new(dev_label).small().weak());
                    }
                    // Per-port default policy dropdown (SORT default).
                    let cur_default = port_default_map.get(&i.to_string())
                        .and_then(|v| v.as_str()).unwrap_or("SORT");
                    let pd_id = egui::Id::new(("automap_combiner_port_default", node_id, i));
                    egui::ComboBox::from_id_salt(pd_id)
                        .selected_text(egui::RichText::new(cur_default).small())
                        .width(58.0)
                        .show_ui(ui, |ui| {
                            for &mode in &["SORT", "OR", "AND", "XOR", "ADD", "MULT"] {
                                if ui.selectable_label(cur_default == mode,
                                    egui::RichText::new(mode).small()).clicked()
                                {
                                    next_port_default.push((i, mode));
                                }
                            }
                        });
                });
            });
            if let Some(dragged_idx) = dropped {
                let from = *dragged_idx;
                if from != i { to_move = Some((from, i)); }
            }
        }
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

        ui.add_space(2.0);
        if ui.small_button("+ input").clicked() {
            if let Some(node) = snarl.get_node_mut(node_id) {
                let next = node.inputs.len();
                node.inputs.push(PinDescriptor::new(format!("in_{next}"), SignalType::AutoMap));
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

        ui.add_space(4.0);
        let header_id = egui::Id::new(("automap_combiner_resolution", node_id));
        let n_conf = overlap.values().filter(|w| w.len() >= 2).count();
        let n_pol = current_policy.values()
            .filter(|v| v.as_str().map_or(false, |s| s != "SORT")).count();
        let n_port = current_port.len();
        let n_over = n_pol + n_port;
        let header_text = format!("Resolution ({n_over} overrides, {n_conf} pins overlap)");
        egui::CollapsingHeader::new(egui::RichText::new(header_text).small().weak())
            .id_salt(header_id)
            .default_open(false)
            .show(ui, |ui| {
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
                                    current_gate, &mut next_gate);
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
                                    ui.label(name);

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
                                        .selected_text(egui::RichText::new(current_pol).small())
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
                                        .selected_text(egui::RichText::new(&port_label).small())
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

                                    if let Some(who) = overlap.get(pin_id) {
                                        if who.len() >= 2 {
                                            let parts: Vec<String> = who.iter().map(|i| format!("in_{i}")).collect();
                                            ui.label(egui::RichText::new(format!("  ({})", parts.join(",")))
                                                .small().weak());
                                        }
                                    }
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
