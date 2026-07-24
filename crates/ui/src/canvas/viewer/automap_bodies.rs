//! AutoMap routing node bodies: Splitter, Collector, Fork, Selector,
//! Combiner + device-family capability helpers.

use super::*;

// ── AutoMap Splitter body ─────────────────────────────────────────────────────

/// Parse the family slug from a gilrs source ID (`"gilrs:<slug>:<inst>"`) or
/// Per-device-source capability flags for the header control rows.
/// Returns `(has_deadzone, has_gyro_mult, has_sticks_cal)`.
/// - MIDI ports: nothing
/// - XInput (Xbox): deadzone + sticks calibration, no gyro
/// - DualShock4 / DualSense / Switch Pro: deadzone + gyro + sticks
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
            "ds4" | "dualsense" | "switch_pro" => (true, true,  true),
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
            let neutral = am_canon::ALL_PINS.iter()
                .find(|p| p.id == pin_id.as_str())
                .map(|p| p.display_name)
                .unwrap_or(pin_id.as_str());
            let display = splitter_pin_label(pin_id, neutral, family_ref);
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
            // Resolve display name: canonical list first, otherwise show the raw key name
            // (learned keys use their egui Key Debug name as both id and display).
            let neutral = am_canon::ALL_PINS.iter()
                .find(|p| p.id == pin_id.as_str())
                .map(|p| p.display_name)
                .unwrap_or(pin_id.as_str());
            let display = splitter_pin_label(pin_id, neutral, family_ref);
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

pub(crate) fn combiner_inputs_info(
    snarl: &Snarl<NodeData>,
    node_id: NodeId,
) -> Vec<CombinerInputInfo> {
    /// Walks the AutoMap chain to find the originating `device.source` (returns
    /// its device_id), OR detects a collector-shaped node (Remapper/Collector/
    /// Combiner/Splitter+Fork+Selector) and returns a synthetic tag.
    fn trace(snarl: &Snarl<NodeData>, src: OutPinId, depth: u32)
        -> Option<(Option<String>, &'static str)>
    {
        if depth > 16 { return None; }
        let node = snarl.get_node(src.node)?;
        match node.module_id.as_str() {
            "device.source" => {
                let dev_id = node.params.get("device_id").and_then(|v| v.as_str())?.to_string();
                Some((Some(dev_id), "device"))
            }
            // Splitter is a transparent passthrough on its AutoMap input.
            "module.automap_split" => {
                let in_idx = node.inputs.iter().position(|p| p.signal_type == SignalType::AutoMap)?;
                let pin = snarl.in_pin(InPinId { node: src.node, input: in_idx });
                let upstream = *pin.remotes.first()?;
                trace(snarl, upstream, depth + 1)
            }
            "module.automap_collect"  => Some((None, "Collector")),
            "module.automap_fork"     => Some((None, "Fork")),
            "module.automap_selector" => Some((None, "Selector")),
            "module.automap_combiner" => Some((None, "Combiner")),
            "module.remapper"         => Some((None, "Remapper")),
            _ => None,
        }
    }

    let n = snarl.get_node(node_id).map(|n| n.inputs.len()).unwrap_or(0);
    let mut out: Vec<CombinerInputInfo> = Vec::with_capacity(n);
    for i in 0..n {
        let in_pin = snarl.in_pin(InPinId { node: node_id, input: i });
        let traced = in_pin.remotes.first().and_then(|&src| trace(snarl, src, 0));
        let (offered, label) = match traced {
            // Physical device.source: pin set is whatever its output_pin_ids declares.
            Some((Some(dev_id), "device")) => {
                let pins: std::collections::HashSet<&'static str> = snarl
                    .nodes_ids_data()
                    .find_map(|(_, n)| {
                        let nd = &n.value;
                        if nd.module_id != "device.source" { return None; }
                        if nd.params.get("device_id").and_then(|v| v.as_str()) != Some(dev_id.as_str()) {
                            return None;
                        }
                        nd.params.get("output_pin_ids")
                            .and_then(|v| v.as_array())
                            .map(|arr| arr.iter()
                                .filter_map(|v| v.as_str())
                                .filter_map(|s| am_canon::ALL_PINS.iter()
                                    .find(|ap| ap.id == s).map(|ap| ap.id))
                                .collect())
                    })
                    .unwrap_or_default();
                let label = match family_slug_from_device_id(&dev_id) {
                    Some("dualsense")  => "DualSense".to_string(),
                    Some("ds4")        => "DualShock 4".to_string(),
                    Some("xinput")     => "Xbox / XInput".to_string(),
                    Some("switch_pro") => "Switch Pro".to_string(),
                    _ => dev_id,
                };
                (pins, label)
            }
            // Collector-shaped node: offers the full canonical pin set (any
            // pin could be overridden into the bus).
            Some((None, tag)) => {
                let pins: std::collections::HashSet<&'static str> =
                    am_canon::ALL_PINS.iter().map(|p| p.id).collect();
                (pins, tag.to_string())
            }
            _ => (std::collections::HashSet::new(), String::new()),
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
) {
    let _ = live_signals; // no longer used: structural overlap is connection-based
    let n = snarl.get_node(node_id).map(|n| n.inputs.len()).unwrap_or(0);
    let infos = combiner_inputs_info(snarl, node_id);

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
                | "accel_x" | "accel_y" | "accel_z" => "Motion",
                id if id.starts_with("btn_") => "Buttons",
                id if id.starts_with("key_")
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
    });
    // Register the whole body as the pinnable element so Layout mode's "Pin
    // element ▶ Whole module" works for the combiner (matches remapper /
    // map_action, which were registered but the combiner was missed).
    register_exposable_element(ui, node_id, "whole_module", body_resp.response.rect);
}
