//! MIDI In/Out node bodies, the shared "add a MIDI pin" picker, and MIDI pin
//! add/remove helpers.
//!
//! A MIDI node's pins are all user-added except its AutoMap port (drawn in the
//! header). Pins are addressed by their REAL index everywhere here, because
//! the AutoMap pin sits wherever it was created or migrated in.

use super::*;

use flexinput_core::midi::{self as fmidi, Channel, MidiPin, Transport};

/// Display label for a MIDI node pin id in either grammar (bus or legacy).
pub(crate) fn midi_pin_label(pin_id: &str) -> String {
    if let Some(p) = fmidi::parse_pin(pin_id) {
        return p.display_name();
    }
    if pin_id == fmidi::LEGACY_PITCH_BEND {
        return "Pitch Bend · any ch".to_string();
    }
    if let Some(cc) = pin_id.strip_prefix("cc_").and_then(|s| s.parse::<u8>().ok()) {
        return cc_display_name(cc);
    }
    pin_id.to_string()
}

// ── Pin picker ────────────────────────────────────────────────────────────────

/// The kinds of MIDI pin the picker can build, in menu order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PickKind {
    Note,
    Velocity,
    PolyAftertouch,
    Cc,
    Cc14,
    Nrpn,
    Rpn,
    PitchBend,
    ChannelPressure,
    ProgramChange,
    Start,
    Stop,
    Continue,
    Playing,
    Bpm,
    SysEx,
}

impl PickKind {
    const ALL: [PickKind; 16] = [
        PickKind::Note, PickKind::Velocity, PickKind::PolyAftertouch, PickKind::Cc,
        PickKind::Cc14, PickKind::Nrpn, PickKind::Rpn, PickKind::PitchBend,
        PickKind::ChannelPressure, PickKind::ProgramChange, PickKind::Start,
        PickKind::Stop, PickKind::Continue, PickKind::Playing, PickKind::Bpm,
        PickKind::SysEx,
    ];

    fn label(self) -> &'static str {
        match self {
            PickKind::Note => "Note",
            PickKind::Velocity => "Note velocity",
            PickKind::PolyAftertouch => "Poly aftertouch",
            PickKind::Cc => "CC",
            PickKind::Cc14 => "CC (14-bit)",
            PickKind::Nrpn => "NRPN",
            PickKind::Rpn => "RPN",
            PickKind::PitchBend => "Pitch bend",
            PickKind::ChannelPressure => "Channel pressure",
            PickKind::ProgramChange => "Program change",
            PickKind::Start => "Start",
            PickKind::Stop => "Stop",
            PickKind::Continue => "Continue",
            PickKind::Playing => "Playing (state)",
            PickKind::Bpm => "Clock BPM",
            PickKind::SysEx => "SysEx",
        }
    }

    /// Whether this kind can be written to a MIDI output.
    fn writable(self) -> bool {
        !matches!(self, PickKind::Playing | PickKind::Bpm)
    }

    fn has_channel(self) -> bool {
        !matches!(
            self,
            PickKind::Start | PickKind::Stop | PickKind::Continue | PickKind::Playing | PickKind::Bpm | PickKind::SysEx
        )
    }

    /// Inclusive number range, when the kind has a number.
    fn number_range(self) -> Option<u32> {
        match self {
            PickKind::Note | PickKind::Velocity | PickKind::PolyAftertouch | PickKind::Cc | PickKind::ProgramChange => Some(127),
            PickKind::Cc14 => Some(31),
            PickKind::Nrpn | PickKind::Rpn => Some(16383),
            _ => None,
        }
    }

    fn is_note(self) -> bool {
        matches!(self, PickKind::Note | PickKind::Velocity | PickKind::PolyAftertouch)
    }

    fn build(self, ch: Channel, n: u32, sysex_hex: &str) -> Option<MidiPin> {
        let n7 = n.min(127) as u8;
        Some(match self {
            PickKind::Note => MidiPin::Note { ch, note: n7 },
            PickKind::Velocity => MidiPin::Velocity { ch, note: n7 },
            PickKind::PolyAftertouch => MidiPin::PolyAftertouch { ch, note: n7 },
            PickKind::Cc => MidiPin::Cc { ch, cc: n7 },
            PickKind::Cc14 => MidiPin::Cc14 { ch, cc: n.min(31) as u8 },
            PickKind::Nrpn => MidiPin::Nrpn { ch, param: n.min(16383) as u16 },
            PickKind::Rpn => MidiPin::Rpn { ch, param: n.min(16383) as u16 },
            PickKind::PitchBend => MidiPin::PitchBend { ch },
            PickKind::ChannelPressure => MidiPin::ChannelPressure { ch },
            PickKind::ProgramChange => MidiPin::ProgramChange { ch, program: n7 },
            PickKind::Start => MidiPin::Transport(Transport::Start),
            PickKind::Stop => MidiPin::Transport(Transport::Stop),
            PickKind::Continue => MidiPin::Transport(Transport::Continue),
            PickKind::Playing => MidiPin::Playing,
            PickKind::Bpm => MidiPin::Bpm,
            PickKind::SysEx => {
                let cleaned: String = sysex_hex.chars().filter(|c| c.is_ascii_hexdigit()).collect();
                return fmidi::parse_pin(&format!("midi:sx:{}", cleaned.to_ascii_uppercase()));
            }
        })
    }
}

/// Picker state, kept in egui temp memory per picker instance.
#[derive(Clone)]
struct PickerState {
    kind: PickKind,
    /// 0 = any channel, 1..=16.
    channel: u8,
    number: u32,
    sysex: String,
}

impl Default for PickerState {
    fn default() -> Self {
        Self { kind: PickKind::Cc, channel: 1, number: 1, sysex: "F0 F7".to_string() }
    }
}

/// Compact "type · channel · number · Add" row that builds one MIDI pin.
/// `for_output` hides kinds (and the "any channel" choice) a MIDI output can't
/// write. Returns the pin when Add is clicked with a valid selection.
pub(crate) fn midi_pin_picker(ui: &mut egui::Ui, id_salt: impl std::hash::Hash, for_output: bool) -> Option<MidiPin> {
    let id = egui::Id::new(("midi_pin_picker", id_salt));
    let mut st: PickerState = ui.ctx().data(|d| d.get_temp(id)).unwrap_or_default();
    if for_output && !st.kind.writable() {
        st.kind = PickKind::Cc;
    }
    if for_output && st.channel == 0 {
        st.channel = 1;
    }

    let mut added = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        egui::ComboBox::from_id_salt(id.with("kind"))
            .selected_text(egui::RichText::new(st.kind.label()).small())
            .width(110.0)
            .show_ui(ui, |ui| {
                for k in PickKind::ALL {
                    if for_output && !k.writable() {
                        continue;
                    }
                    if ui.selectable_label(st.kind == k, egui::RichText::new(k.label()).small()).clicked() {
                        st.kind = k;
                        if let Some(max) = k.number_range() {
                            st.number = st.number.min(max);
                        }
                    }
                }
            });

        if st.kind.has_channel() {
            let ch_text = if st.channel == 0 { "any ch".to_string() } else { format!("ch {}", st.channel) };
            egui::ComboBox::from_id_salt(id.with("ch"))
                .selected_text(egui::RichText::new(ch_text).small())
                .width(56.0)
                .show_ui(ui, |ui| {
                    if !for_output && ui.selectable_label(st.channel == 0, egui::RichText::new("any ch").small()).clicked() {
                        st.channel = 0;
                    }
                    for c in 1..=16u8 {
                        if ui.selectable_label(st.channel == c, egui::RichText::new(format!("ch {c}")).small()).clicked() {
                            st.channel = c;
                        }
                    }
                });
        }

        if let Some(max) = st.kind.number_range() {
            let is_note = st.kind.is_note();
            ui.add(
                egui::DragValue::new(&mut st.number)
                    .range(0..=max)
                    .speed(if max > 127 { 4.0 } else { 0.25 })
                    .custom_formatter(move |v, _| {
                        if is_note { format!("{} {}", v as u32, fmidi::note_name(v as u8)) } else { format!("{}", v as u32) }
                    }),
            );
        }

        if st.kind == PickKind::SysEx {
            ui.add(egui::TextEdit::singleline(&mut st.sysex).desired_width(110.0).hint_text("F0 … F7"));
        }

        let ch = if st.channel == 0 { Channel::Any } else { Channel::Ch(st.channel - 1) };
        let built = st.kind.build(ch, st.number, &st.sysex);
        let resp = ui.add_enabled(built.is_some(), egui::Button::new(egui::RichText::new("Add").small()));
        let resp = if built.is_none() && st.kind == PickKind::SysEx {
            resp.on_disabled_hover_text(format!(
                "A complete SysEx message: starts F0, ends F7, at most {} bytes.",
                fmidi::SYSEX_MAX_BYTES
            ))
        } else {
            resp
        };
        if resp.clicked() {
            added = built;
        }
    });

    ui.ctx().data_mut(|d| d.insert_temp(id, st));
    added
}

/// Move the MIDI pin at `idx` of a mapping's pin list onto another channel.
///
/// A note's velocity twin moves with it: an analog card reads the velocity of
/// the note it fires on, and leaving it behind would have it listening on a
/// channel the note no longer uses. Returns false when the slot isn't a MIDI
/// pin with a channel (transport, BPM, SysEx), which nothing should offer.
pub(crate) fn midi_set_pin_channel(pins: &mut [Value], idx: usize, new_ch: Channel) -> bool {
    let Some(pin) = pins.get(idx).and_then(|v| v.as_str()).and_then(fmidi::parse_pin) else {
        return false;
    };
    let Some(old_ch) = pin.channel() else { return false };
    if old_ch == new_ch {
        return false;
    }
    let velocity_twin = match pin {
        MidiPin::Note { note, .. } => {
            let from = MidiPin::Velocity { ch: old_ch, note };
            Some((from.to_id(), from.with_channel(new_ch).to_id()))
        }
        _ => None,
    };
    pins[idx] = Value::String(pin.with_channel(new_ch).to_id());
    if let Some((from, to)) = velocity_twin {
        for slot in pins.iter_mut() {
            if slot.as_str() == Some(from.as_str()) {
                *slot = Value::String(to.clone());
            }
        }
    }
    true
}

// ── Node bodies ───────────────────────────────────────────────────────────────

/// `(real pin index, pin id)` for every non-AutoMap pin, in order.
fn midi_pin_rows(pins: &[PinDescriptor], ids: Option<&Vec<Value>>) -> Vec<(usize, String)> {
    pins.iter().enumerate()
        .filter(|(_, p)| p.signal_type != SignalType::AutoMap)
        .map(|(i, _)| (i, ids.and_then(|a| a.get(i)).and_then(|v| v.as_str()).unwrap_or("").to_string()))
        .collect()
}

/// Append a MIDI pin to a MIDI node side (skipped if the node already has it).
fn push_midi_pin(node: &mut NodeData, pin: &MidiPin, outputs: bool) {
    let id = pin.to_id();
    let key = if outputs { "output_pin_ids" } else { "input_pin_ids" };
    let has = node.params.get(key).and_then(|v| v.as_array())
        .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(id.as_str())));
    if has {
        return;
    }
    let desc = PinDescriptor::new(&pin.display_name(), pin.signal_type());
    if outputs { node.outputs.push(desc) } else { node.inputs.push(desc) }
    match node.params.get_mut(key) {
        Some(Value::Array(ids)) => ids.push(Value::String(id)),
        _ => { node.params.insert(key.to_string(), Value::Array(vec![Value::String(id)])); }
    }
}

/// Add a learned/picked pin to a MIDI In node's outputs (no-op if present).
pub(crate) fn add_midi_output_pin(node: &mut NodeData, pin: &MidiPin) {
    push_midi_pin(node, pin, true);
}

pub(crate) fn show_midi_in_body(node_id: NodeId, outputs: &[OutPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let Some(node) = snarl.get_node(node_id) else { return };
    let is_learning = node.params.get("learning").and_then(|v| v.as_bool()).unwrap_or(false);
    let rows = midi_pin_rows(&node.outputs, node.params.get("output_pin_ids").and_then(|v| v.as_array()));

    ui.vertical(|ui| {
        ui.set_min_width(180.0);

        let mut to_remove: Option<usize> = None;
        for (idx, id) in &rows {
            ui.horizontal(|ui| {
                if ui.small_button("×").clicked() {
                    to_remove = Some(*idx);
                }
                ui.label(egui::RichText::new(midi_pin_label(id)).small());
            });
        }
        if let Some(rm_idx) = to_remove {
            remove_midi_output(node_id, rm_idx, outputs, snarl);
        }

        ui.add_space(4.0);
        if let Some(pin) = midi_pin_picker(ui, (node_id, "midi_in_add"), false) {
            if let Some(node) = snarl.get_node_mut(node_id) {
                add_midi_output_pin(node, &pin);
            }
        }

        ui.horizontal(|ui| {
            let learn_label = if is_learning {
                egui::RichText::new("● Stop").small().color(Color32::from_rgb(220, 80, 80))
            } else {
                egui::RichText::new("Learn").small()
            };
            let resp = ui.button(learn_label).on_hover_text(
                "Add a pin for each note, controller, bend, program change or \
                 transport message this port receives while Learn is on.",
            );
            if resp.clicked() {
                if let Some(node) = snarl.get_node_mut(node_id) {
                    node.params.insert("learning".to_string(), Value::Bool(!is_learning));
                }
            }
            let has_unused = outputs.iter().enumerate()
                .any(|(i, o)| o.remotes.is_empty() && rows.iter().any(|(r, _)| *r == i));
            if has_unused && ui.small_button("Clear unused").clicked() {
                clear_unused_midi_outputs(node_id, outputs, snarl);
            }
        });
    });
}

pub(crate) fn show_midi_out_body(node_id: NodeId, inputs: &[InPin], ui: &mut egui::Ui, snarl: &mut Snarl<NodeData>) {
    let Some(node) = snarl.get_node(node_id) else { return };
    let rows = midi_pin_rows(&node.inputs, node.params.get("input_pin_ids").and_then(|v| v.as_array()));
    let mut thru = node.params.get("midi_thru").and_then(|v| v.as_bool()).unwrap_or(false);

    ui.vertical(|ui| {
        ui.set_min_width(180.0);

        let resp = ui.checkbox(&mut thru, egui::RichText::new("MIDI Thru").small()).on_hover_text(
            "Off (default): only MIDI that a mapping or a Collector produces is sent \
             from the Auto-Map input.\n\
             On: raw MIDI arriving on the Auto-Map bus is forwarded too.\n\n\
             Leave it off when this port's MIDI In also feeds the patch: \
             forwarding it back out is a feedback loop.",
        );
        if resp.changed() {
            if let Some(node) = snarl.get_node_mut(node_id) {
                node.params.insert("midi_thru".to_string(), Value::Bool(thru));
            }
        }

        let mut to_remove: Option<usize> = None;
        for (idx, id) in &rows {
            ui.horizontal(|ui| {
                if ui.small_button("×").clicked() {
                    to_remove = Some(*idx);
                }
                ui.label(egui::RichText::new(midi_pin_label(id)).small());
            });
        }
        if let Some(rm_idx) = to_remove {
            remove_midi_input(node_id, rm_idx, inputs, snarl);
        }

        ui.add_space(4.0);
        if let Some(pin) = midi_pin_picker(ui, (node_id, "midi_out_add"), true) {
            if let Some(node) = snarl.get_node_mut(node_id) {
                push_midi_pin(node, &pin, false);
            }
        }

        let has_unused = inputs.iter().enumerate()
            .any(|(i, p)| p.remotes.is_empty() && rows.iter().any(|(r, _)| *r == i));
        if has_unused && ui.small_button("Clear unused").clicked() {
            clear_unused_midi_inputs(node_id, inputs, snarl);
        }
    });
}

// ── MIDI pin removal helpers ──────────────────────────────────────────────────
//
// Direction-bound wrappers over the shared dynamic-pin editing in `pins.rs`.
// The id arrays cover every pin (AutoMap port included), hence `id_offset` 0;
// clearing always keeps the AutoMap port.

pub(crate) fn remove_midi_output(node_id: NodeId, rm_idx: usize, outputs: &[OutPin], snarl: &mut Snarl<NodeData>) {
    remove_dynamic_pin::<Outputs>(node_id, rm_idx, outputs, snarl, "output_pin_ids", 0);
}

pub(crate) fn remove_midi_input(node_id: NodeId, rm_idx: usize, inputs: &[InPin], snarl: &mut Snarl<NodeData>) {
    remove_dynamic_pin::<Inputs>(node_id, rm_idx, inputs, snarl, "input_pin_ids", 0);
}

pub(crate) fn clear_unused_midi_outputs(node_id: NodeId, outputs: &[OutPin], snarl: &mut Snarl<NodeData>) {
    clear_unused_dynamic_pins::<Outputs>(node_id, outputs, snarl, "output_pin_ids");
}

pub(crate) fn clear_unused_midi_inputs(node_id: NodeId, inputs: &[InPin], snarl: &mut Snarl<NodeData>) {
    clear_unused_dynamic_pins::<Inputs>(node_id, inputs, snarl, "input_pin_ids");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_picker_kind_builds_a_pin_that_round_trips() {
        for k in PickKind::ALL {
            let ch = if k.has_channel() { Channel::Ch(3) } else { Channel::Any };
            let pin = k.build(ch, 5, "F0 7E 7F F7").unwrap_or_else(|| panic!("{k:?} built nothing"));
            assert_eq!(fmidi::parse_pin(&pin.to_id()), Some(pin.clone()), "{k:?}");
            assert_eq!(k.writable(), pin.is_output_capable(), "{k:?} writability disagrees with core");
        }
    }

    #[test]
    fn a_malformed_sysex_builds_nothing() {
        assert_eq!(PickKind::SysEx.build(Channel::Any, 0, "F0 7E"), None);
        assert_eq!(PickKind::SysEx.build(Channel::Any, 0, "zz"), None);
    }

    #[test]
    fn changing_a_notes_channel_takes_its_velocity_with_it() {
        let mut pins = vec![
            Value::from("midi:note:1:60"),
            Value::from("midi:vel:1:60"),
            Value::from("btn_south"),
        ];
        assert!(midi_set_pin_channel(&mut pins, 0, Channel::Ch(9)));
        assert_eq!(pins[0], Value::from("midi:note:10:60"));
        assert_eq!(pins[1], Value::from("midi:vel:10:60"), "the velocity twin follows");
        assert_eq!(pins[2], Value::from("btn_south"), "other pins untouched");
    }

    #[test]
    fn a_channel_change_round_trips_to_any_and_back() {
        let mut pins = vec![Value::from("midi:cc:3:74")];
        assert!(midi_set_pin_channel(&mut pins, 0, Channel::Any));
        assert_eq!(pins[0], Value::from("midi:cc:*:74"));
        assert!(midi_set_pin_channel(&mut pins, 0, Channel::Ch(2)));
        assert_eq!(pins[0], Value::from("midi:cc:3:74"));
    }

    #[test]
    fn a_pin_with_no_channel_or_no_change_is_refused() {
        let mut pins = vec![Value::from("midi:rt:start"), Value::from("key_a")];
        assert!(!midi_set_pin_channel(&mut pins, 0, Channel::Ch(1)));
        assert!(!midi_set_pin_channel(&mut pins, 1, Channel::Ch(1)));
        let mut same = vec![Value::from("midi:cc:1:7")];
        assert!(!midi_set_pin_channel(&mut same, 0, Channel::Ch(0)), "no change is not a change");
    }

    #[test]
    fn labels_cover_both_grammars() {
        assert_eq!(midi_pin_label("cc_7"), "CC 7 – Volume");
        assert_eq!(midi_pin_label("pitch_bend"), "Pitch Bend · any ch");
        assert_eq!(midi_pin_label("midi:note:1:60"), "C4 · ch 1");
    }
}
