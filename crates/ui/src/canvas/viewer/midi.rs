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
/// The message types a MIDI picker offers. No velocity or poly-aftertouch
/// type: those are part of a note — its strike and its live value — carried on
/// the note's own card, never picked on their own.
enum PickKind {
    Note,
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
    const ALL: [PickKind; 14] = [
        PickKind::Note, PickKind::Cc,
        PickKind::Cc14, PickKind::Nrpn, PickKind::Rpn, PickKind::PitchBend,
        PickKind::ChannelPressure, PickKind::ProgramChange, PickKind::Start,
        PickKind::Stop, PickKind::Continue, PickKind::Playing, PickKind::Bpm,
        PickKind::SysEx,
    ];

    fn label(self) -> &'static str {
        match self {
            PickKind::Note => "Note",
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
            PickKind::Note | PickKind::Cc | PickKind::ProgramChange => Some(127),
            PickKind::Cc14 => Some(31),
            PickKind::Nrpn | PickKind::Rpn => Some(16383),
            _ => None,
        }
    }

    fn is_note(self) -> bool {
        matches!(self, PickKind::Note)
    }

    fn build(self, ch: Channel, n: u32, sysex_hex: &str) -> Option<MidiPin> {
        let n7 = n.min(127) as u8;
        Some(match self {
            PickKind::Note => MidiPin::Note { ch, note: n7 },
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

// ── Inline picker: the names the body and the gamepad-nav driver share ───────

/// Param an earlier build kept open-state of the Remapper's inline MIDI row in.
/// The row is gone (the MIDI editor is a window now); loading still strips the
/// key from patches saved with it — see `clear_stuck_nav_arms`.
pub const MIDI_PICK_OPEN: &str = "_midi_pick_open";
/// The Remapper action-row item for the MIDI… button (the nav driver opens the
/// MIDI editor window on it).
pub const NAV_ACT_MIDI: &str = "_nav_act_midi";

/// Where a node's inline picker keeps its selection. The body and the nav
/// driver both address it through here, so they can never drift onto two
/// different ids for one picker.
fn midi_pick_state_id(node: NodeId) -> egui::Id {
    egui::Id::new(("midi_pin_picker", (node, "midi_pick")))
}

/// Step one of the inline picker's fields. The type and channel wrap (a list
/// with ends you can fall off is worse on a stick than one that comes round);
/// the number clamps to its type's range, because 0 and 127 are meaningful
/// places to sit.
pub fn midi_pick_nav_adjust(ctx: &egui::Context, node: NodeId, field: usize, delta: i32) {
    let id = midi_pick_state_id(node);
    let mut st: PickerState = ctx.data(|d| d.get_temp(id)).unwrap_or_default();
    pick_state_adjust(&mut st, field, delta);
    ctx.data_mut(|d| d.insert_temp(id, st));
}

/// The state transition behind [`midi_pick_nav_adjust`], split out so the
/// stepping rules can be tested without an egui context.
fn pick_state_adjust(st: &mut PickerState, field: usize, delta: i32) {
    match field {
        0 => {
            let n = PickKind::ALL.len() as i32;
            let cur = PickKind::ALL.iter().position(|k| *k == st.kind).unwrap_or(0) as i32;
            st.kind = PickKind::ALL[(cur + delta).rem_euclid(n) as usize];
            if let Some(max) = st.kind.number_range() {
                st.number = st.number.min(max);
            }
        }
        // 0 is "any channel", then 1..=16. A type with no channel ignores it,
        // so walking the field there changes nothing rather than editing a
        // value the pin will never carry.
        1 if st.kind.has_channel() => {
            st.channel = (st.channel as i32 + delta).rem_euclid(17) as u8
        }
        2 => {
            if let Some(max) = st.kind.number_range() {
                st.number = (st.number as i32 + delta).clamp(0, max as i32) as u32;
            }
        }
        _ => {}
    }
}

/// Compact "type · channel · number · Add" row that builds one MIDI pin.
/// `for_output` hides kinds (and the "any channel" choice) a MIDI output can't
/// write. Returns the pin when Add is clicked with a valid selection.
pub(crate) fn midi_pin_picker(ui: &mut egui::Ui, id_salt: impl std::hash::Hash, for_output: bool) -> Option<MidiPin> {
    let mut added = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let built = midi_pin_picker_fields(ui, id_salt, for_output);
        if midi_pin_add_button(ui, "Add", built.clone(), true).clicked() {
            added = built;
        }
    });
    added
}

/// An Add-style button for a picked pin: disabled (with the reason) when the
/// selection can't be used. Shared by every place the picker is offered, so
/// "why is this greyed out" is answered the same way each time.
pub(crate) fn midi_pin_add_button(
    ui: &mut egui::Ui,
    label: &str,
    built: Option<MidiPin>,
    allowed: bool,
) -> egui::Response {
    let usable = allowed && built.is_some();
    let resp = ui.add_enabled(usable, egui::Button::new(egui::RichText::new(label).small()));
    match (&built, allowed) {
        (None, _) => resp.on_disabled_hover_text(format!(
            "A complete SysEx message: starts F0, ends F7, at most {} bytes.",
            fmidi::SYSEX_MAX_BYTES
        )),
        (Some(_), false) => resp.on_disabled_hover_text(
            "An output has to name one channel, and has to be a message that can be sent.",
        ),
        _ => resp,
    }
}

/// The picker's controls WITHOUT an Add button, for callers that offer their
/// own (the Remapper adds a picked message as an input or as an output, and
/// which one is not the picker's business). Returns the pin currently built,
/// or `None` while the selection is incomplete.
pub(crate) fn midi_pin_picker_fields(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash,
    for_output: bool,
) -> Option<MidiPin> {
    let id = egui::Id::new(("midi_pin_picker", id_salt));
    let mut st: PickerState = ui.ctx().data(|d| d.get_temp(id)).unwrap_or_default();
    if for_output && !st.kind.writable() {
        st.kind = PickKind::Cc;
    }
    if for_output && st.channel == 0 {
        st.channel = 1;
    }

    let built;
    {
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

        // A greyed "—" says plainly that this type has no channel / number,
        // rather than the field silently vanishing.
        if !st.kind.has_channel() {
            ui.add_enabled(false, egui::Button::new(egui::RichText::new("—").small()))
                .on_disabled_hover_text("This message has no channel.");
        }
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

        if st.kind.number_range().is_none() && st.kind != PickKind::SysEx {
            ui.add_enabled(false, egui::Button::new(egui::RichText::new("—").small()))
                .on_disabled_hover_text("This message carries no number.");
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
        built = st.kind.build(ch, st.number, &st.sysex);
    }

    ui.ctx().data_mut(|d| d.insert_temp(id, st));
    built
}

/// Key for the per-frame list of MIDI In ports a mapping card can learn from.
const MIDI_IN_REGISTRY: &str = "fxi_midi_in_ports";

/// Publish the MIDI In ports currently on the machine, as `(device id, name)`.
///
/// A card's Learn needs the ports themselves, not the pins they happen to be
/// publishing: a port sitting idle carries no pins at all (the backend only
/// emits what is away from rest), so live signals can't be asked which ports
/// exist. Called once a frame from the app, read by every card.
pub fn set_midi_in_registry(ctx: &egui::Context, ports: Vec<(String, String)>) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(MIDI_IN_REGISTRY), ports));
}

/// The loop guard as the UI shows it, published by the app each frame (the
/// backend sits behind a lock the UI must not wait on).
#[derive(Clone, Debug, Default)]
pub struct MidiGuardView {
    /// `(out port, in port, status)` for every pair the guard knows about.
    pub pairs: Vec<(String, String, flexinput_devices::midi::PairStatus)>,
    /// Out ports the loop breaker has muted.
    pub muted: Vec<String>,
}

const MIDI_GUARD_VIEW: &str = "fxi_midi_guard_view";
const MIDI_UNMUTE_REQ: &str = "fxi_midi_unmute_req";

pub fn set_midi_guard_view(ctx: &egui::Context, v: MidiGuardView) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(MIDI_GUARD_VIEW), v));
}

pub(crate) fn midi_guard_view(ctx: &egui::Context) -> MidiGuardView {
    ctx.data(|d| d.get_temp::<MidiGuardView>(egui::Id::new(MIDI_GUARD_VIEW))).unwrap_or_default()
}

/// Ask the app to unmute an Out port the loop breaker muted.
pub(crate) fn request_midi_unmute(ctx: &egui::Context, out_id: &str) {
    ctx.data_mut(|d| {
        let v: &mut Vec<String> = d.get_temp_mut_or_default(egui::Id::new(MIDI_UNMUTE_REQ));
        if !v.iter().any(|x| x == out_id) {
            v.push(out_id.to_string());
        }
    });
}

pub fn take_midi_unmutes(ctx: &egui::Context) -> Vec<String> {
    ctx.data_mut(|d| d.remove_temp::<Vec<String>>(egui::Id::new(MIDI_UNMUTE_REQ))).unwrap_or_default()
}

const MIDI_FLUSH_REQ: &str = "fxi_midi_flush_req";

/// Ask the app to release every note an In port still holds.
pub(crate) fn request_midi_flush(ctx: &egui::Context, in_id: &str) {
    ctx.data_mut(|d| {
        let v: &mut Vec<String> = d.get_temp_mut_or_default(egui::Id::new(MIDI_FLUSH_REQ));
        if !v.iter().any(|x| x == in_id) {
            v.push(in_id.to_string());
        }
    });
}

pub fn take_midi_flushes(ctx: &egui::Context) -> Vec<String> {
    ctx.data_mut(|d| d.remove_temp::<Vec<String>>(egui::Id::new(MIDI_FLUSH_REQ))).unwrap_or_default()
}

/// Why a MIDI Out node risks feeding itself, if it does: its Thru is on, and a
/// MIDI In placed in the same patch is paired with its port — the same port
/// name, or echoes seen coming back — so raw MIDI can go round and round. With
/// Thru off only what a mapping PRODUCES is sent, which can't loop by itself.
pub(crate) fn midi_out_loop_risk(
    snarl: &Snarl<NodeData>,
    out_id: &str,
    thru: bool,
    guard: &MidiGuardView,
) -> Option<String> {
    use flexinput_devices::midi::PairStatus;
    if !thru {
        return None;
    }
    let placed_ins: Vec<&str> = snarl
        .nodes_ids_data()
        .filter(|(_, n)| n.value.module_id == "device.source")
        .filter_map(|(_, n)| n.value.params.get("device_id").and_then(|v| v.as_str()))
        .filter(|id| id.starts_with("midi_in:"))
        .collect();
    let (_, in_id, status) = guard.pairs.iter().find(|(o, i, s)| {
        o == out_id && placed_ins.contains(&i.as_str()) && *s != PairStatus::Demoted
    })?;
    let cancelling = matches!(status, PairStatus::Seeded | PairStatus::Confirmed);
    Some(format!(
        "MIDI Thru is on, and {in_id} in this patch is the same port coming back in — \
         raw MIDI can go round in a loop.{}\n\nTurn Thru off unless you need raw MIDI \
         forwarded; mappings still send what they produce.",
        if cancelling {
            " Echo cancelling is catching the echoes for now, and the loop breaker mutes the \
             port if it runs away."
        } else {
            ""
        }
    ))
}

/// The MIDI In ports published this frame, `(device id, name)`.
pub(crate) fn midi_in_registry(ctx: &egui::Context) -> Vec<(String, String)> {
    ctx.data(|d| d.get_temp::<Vec<(String, String)>>(egui::Id::new(MIDI_IN_REGISTRY)))
        .unwrap_or_default()
}

/// Which MIDI In ports a card should listen to while learning an output.
///
/// All of them by default — a controller is usually the only one, and asking
/// which port a note came from before you have played it is backwards. A card
/// that picked one in `midi_learn_ref` hears only that one.
pub(crate) fn midi_learn_ref_ports(ctx: &egui::Context, picked: &str) -> Vec<String> {
    midi_in_registry(ctx)
        .into_iter()
        .filter(|(id, _)| picked.is_empty() || id == picked)
        .map(|(id, _)| id)
        .collect()
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

// ── MIDI editor window ────────────────────────────────────────────────────────
//
// One editor for every place a MIDI message is chosen or tuned: the Remapper's
// MIDI… button and a mapping card's MIDI chips. It is a WINDOW owned by the
// app, driven by gamepad nav while it is open — never a popup, for three
// reasons that each broke something: a dropdown inside a popup closes the
// popup; a popup cannot be walked by a pad without threading its fields through
// a row that was never laid out for them; and a popup opened from the config
// overlay renders in the main window, behind the game.

/// What the editor is for.
#[derive(Clone, Debug, PartialEq)]
pub enum MidiModalPurpose {
    /// Build a message and add it to the card being drafted.
    Add,
    /// Tune a MIDI chip already on mapping card `card` of the node's
    /// `cards_key` list (`mappings` for a Remapper): the out row when
    /// `side_out`, else the in row; `pin_idx` is the chip's index in that row.
    Chip { card: usize, cards_key: String, side_out: bool, pin_idx: usize },
    /// Build a message and write its `MIDI_*` name into a JSM Config editor:
    /// right of an `=` when `output` (something to play), left of one otherwise
    /// (something that presses). `cursor` is the pad's cursor in the editor, or
    /// `None` for a mouse, whose pick goes on a line of its own.
    JsmInsert { output: bool, cursor: Option<flexinput_engine::eval::JsmCursor> },
    /// Build a message and add it to another mapping module's OUTPUT draft — a
    /// Lean section's (`_lean_<side>_draft`, with its phase key) or a Touch Zones
    /// / Virtual Menu zone's (`_tz_draft_out`). The same draft the Special picker
    /// writes, by the same rules.
    AddTo { draft_key: String, phase_key: Option<String> },
}

/// The command-list row that opens the MIDI editor for a JSM Config editor.
pub(crate) const JSM_MIDI_ROW: &str = "MIDI…";

/// A `MIDI_*` name the editor window built for a JSM Config editor, waiting for
/// that editor's body to put it in place — the body owns the text, and is the
/// one place a pick from the command list is already applied.
#[derive(Clone, Debug, Default)]
pub(crate) struct JsmMidiInsert {
    pub tag: String,
    pub output: bool,
    pub cursor: Option<flexinput_engine::eval::JsmCursor>,
}

fn jsm_midi_insert_id(path: &[usize], node: NodeId) -> egui::Id {
    egui::Id::new(("jsm_midi_insert", path.to_vec(), node.0))
}

pub(crate) fn set_jsm_midi_insert(ctx: &egui::Context, path: &[usize], node: NodeId, v: JsmMidiInsert) {
    ctx.data_mut(|d| d.insert_temp(jsm_midi_insert_id(path, node), v));
}

pub(crate) fn take_jsm_midi_insert(ctx: &egui::Context, path: &[usize], node: NodeId) -> Option<JsmMidiInsert> {
    ctx.data_mut(|d| d.remove_temp::<JsmMidiInsert>(jsm_midi_insert_id(path, node)))
}

/// A request to open the editor, from a node body (a mouse click) to the app,
/// which owns the window. Nav opens it directly.
#[derive(Clone, Debug, PartialEq)]
pub struct MidiModalRequest {
    /// The Remapper node, within its own snarl.
    pub inner: NodeId,
    /// Sub-patch path from the tab canvas down to that snarl (empty = the tab
    /// canvas itself), as the Special picker addresses its target.
    pub path: Vec<usize>,
    pub purpose: MidiModalPurpose,
}

impl Default for MidiModalRequest {
    fn default() -> Self {
        Self { inner: NodeId(0), path: Vec::new(), purpose: MidiModalPurpose::Add }
    }
}

const MIDI_MODAL_REQ_KEY: &str = "fxi_midi_modal_request";

pub fn request_midi_modal(ctx: &egui::Context, req: MidiModalRequest) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(MIDI_MODAL_REQ_KEY), req));
}

pub fn take_midi_modal_request(ctx: &egui::Context) -> Option<MidiModalRequest> {
    ctx.data_mut(|d| d.remove_temp::<MidiModalRequest>(egui::Id::new(MIDI_MODAL_REQ_KEY)))
}

/// A row of the Add editor, in the order a pad walks them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MidiAddRow {
    Type,
    Channel,
    Number,
    AddInput,
    AddOutput,
}

impl MidiAddRow {
    pub const ALL: [MidiAddRow; 5] = [
        MidiAddRow::Type, MidiAddRow::Channel, MidiAddRow::Number,
        MidiAddRow::AddInput, MidiAddRow::AddOutput,
    ];

    /// The picker field a value row steps (see [`midi_pick_nav_adjust`]).
    pub fn field(self) -> Option<usize> {
        match self {
            MidiAddRow::Type => Some(0),
            MidiAddRow::Channel => Some(1),
            MidiAddRow::Number => Some(2),
            _ => None,
        }
    }
}

/// The message the Add editor currently builds for `node`, if it is complete.
pub fn midi_pick_built(ctx: &egui::Context, node: NodeId) -> Option<MidiPin> {
    let st: PickerState = ctx.data(|d| d.get_temp(midi_pick_state_id(node))).unwrap_or_default();
    let ch = if st.channel == 0 { Channel::Any } else { Channel::Ch(st.channel - 1) };
    st.kind.build(ch, st.number, &st.sysex)
}

/// Draw the Add editor's value rows for `node` — type, channel, number, and a
/// SysEx text box when that is the type — ringing `focused`. Mouse edits go
/// through the ordinary widgets (a dropdown works in a window); the pad steps
/// the very same state through [`midi_pick_nav_adjust`]. Returns each value
/// row's rect, in `MidiAddRow` order, for the caller's own focus handling.
pub(crate) fn midi_add_rows_ui(
    ui: &mut egui::Ui,
    node: NodeId,
    focused: Option<MidiAddRow>,
) -> [egui::Rect; 3] {
    let id = midi_pick_state_id(node);
    let mut st: PickerState = ui.ctx().data(|d| d.get_temp(id)).unwrap_or_default();
    let mut rects = [egui::Rect::NOTHING; 3];
    let label = |ui: &mut egui::Ui, text: &str| {
        ui.add_sized([70.0, 18.0], egui::Label::new(egui::RichText::new(text).small().weak()));
    };

    // Type
    rects[0] = ui.horizontal(|ui| {
        label(ui, "Type");
        egui::ComboBox::from_id_salt(id.with("kind"))
            .selected_text(egui::RichText::new(st.kind.label()).small())
            .width(150.0)
            .show_ui(ui, |ui| {
                for k in PickKind::ALL {
                    if ui.selectable_label(st.kind == k, egui::RichText::new(k.label()).small()).clicked() {
                        st.kind = k;
                        if let Some(max) = k.number_range() {
                            st.number = st.number.min(max);
                        }
                    }
                }
            });
    }).response.rect;

    // Channel
    rects[1] = ui.horizontal(|ui| {
        label(ui, "Channel");
        if st.kind.has_channel() {
            let ch_text = if st.channel == 0 { "any channel".to_string() } else { format!("ch {}", st.channel) };
            egui::ComboBox::from_id_salt(id.with("ch"))
                .selected_text(egui::RichText::new(ch_text).small())
                .width(150.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(st.channel == 0, egui::RichText::new("any channel").small()).clicked() {
                        st.channel = 0;
                    }
                    for c in 1..=16u8 {
                        if ui.selectable_label(st.channel == c, egui::RichText::new(format!("ch {c}")).small()).clicked() {
                            st.channel = c;
                        }
                    }
                });
        } else {
            ui.label(egui::RichText::new("— this message has no channel").small().weak());
        }
    }).response.rect;

    // Number (or the SysEx bytes, which have no number)
    rects[2] = ui.horizontal(|ui| {
        match (st.kind.number_range(), st.kind) {
            (Some(max), _) => {
                label(ui, if st.kind.is_note() { "Note" } else { "Number" });
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
            (None, PickKind::SysEx) => {
                label(ui, "Bytes");
                ui.add(egui::TextEdit::singleline(&mut st.sysex).desired_width(150.0).hint_text("F0 … F7"));
            }
            (None, _) => {
                label(ui, "Number");
                ui.label(egui::RichText::new("— this message carries no number").small().weak());
            }
        }
    }).response.rect;

    ui.ctx().data_mut(|d| d.insert_temp(id, st));

    if let Some(row) = focused {
        if let Some(r) = row.field().and_then(|f| rects.get(f)) {
            ring_row(ui, *r);
        }
    }
    rects
}

/// Paint the pad-focus ring around a row. The editor is drawn at top level by
/// the app (never inside a node body's child layer), so painting here is safe.
pub(crate) fn ring_row(ui: &egui::Ui, rect: egui::Rect) {
    let accent = ui.visuals().selection.stroke.color;
    let [r, g, b, _] = accent.to_array();
    ui.painter().rect_filled(rect.expand(2.0), 4.0, Color32::from_rgba_unmultiplied(r, g, b, 40));
    ui.painter().rect_stroke(rect.expand(2.0), 4.0, egui::Stroke::new(1.5, accent), egui::StrokeKind::Outside);
}

/// One editable row of a MIDI chip already on a card.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MidiChipRow {
    Channel,
    /// A note output's NoteOn velocity (the card's `midi_vel`).
    Velocity,
    /// The value a button sends on a value pin while pressed (`midi_on`).
    On,
    /// … and when released (`midi_off`).
    Off,
}

impl MidiChipRow {
    pub fn label(self) -> &'static str {
        match self {
            MidiChipRow::Channel => "Channel",
            MidiChipRow::Velocity => "Velocity",
            MidiChipRow::On => "On value",
            MidiChipRow::Off => "Off value",
        }
    }

    /// The card key, default and range of a level row.
    fn level(self) -> Option<(&'static str, f64, f64, f64)> {
        match self {
            MidiChipRow::Velocity => Some(("midi_vel", 100.0, 1.0, 127.0)),
            MidiChipRow::On => Some(("midi_on", 127.0, 0.0, 127.0)),
            MidiChipRow::Off => Some(("midi_off", 0.0, 0.0, 127.0)),
            MidiChipRow::Channel => None,
        }
    }
}

/// What a chip lets you tune. An input chip has only its channel (the message
/// it matches); an output chip also says what it SENDS: a note its velocity, a
/// value pin the levels a button drives it between. A transport or SysEx chip
/// has nothing, and is not offered.
pub fn midi_chip_rows(pin_id: &str, side_out: bool) -> Vec<MidiChipRow> {
    let Some(p) = fmidi::parse_pin(pin_id) else { return Vec::new() };
    let mut rows = Vec::new();
    if p.channel().is_some() {
        rows.push(MidiChipRow::Channel);
    }
    if side_out {
        if matches!(p, MidiPin::Note { .. }) {
            rows.push(MidiChipRow::Velocity);
        } else if p.is_continuous() {
            rows.push(MidiChipRow::On);
            rows.push(MidiChipRow::Off);
        }
    }
    rows
}

fn chip_pin(card: &serde_json::Map<String, Value>, side_out: bool, pin_idx: usize) -> Option<MidiPin> {
    let key = if side_out { "out" } else { "in" };
    card.get(key)?.as_array()?.get(pin_idx)?.as_str().and_then(fmidi::parse_pin)
}

/// A chip row's current value, as the editor shows it.
pub fn midi_chip_value(
    card: &serde_json::Map<String, Value>,
    side_out: bool,
    pin_idx: usize,
    row: MidiChipRow,
) -> String {
    match row.level() {
        Some((key, default, _, _)) => {
            let v = card.get(key).and_then(|v| v.as_f64()).unwrap_or(default);
            format!("{}", v.round() as i64)
        }
        None => match chip_pin(card, side_out, pin_idx).and_then(|p| p.channel()) {
            Some(Channel::Any) => "any channel".to_string(),
            Some(Channel::Ch(c)) => format!("ch {}", c + 1),
            None => "—".to_string(),
        },
    }
}

/// Step a chip row by `delta` on its card. The channel comes round — an input
/// passes through "any channel" on the way, an output has no such stop — and a
/// level clamps to its range. Returns whether the card changed.
pub fn midi_chip_step(
    card: &mut serde_json::Map<String, Value>,
    side_out: bool,
    pin_idx: usize,
    row: MidiChipRow,
    delta: i32,
) -> bool {
    if delta == 0 {
        return false;
    }
    if let Some((key, default, lo, hi)) = row.level() {
        let cur = card.get(key).and_then(|v| v.as_f64()).unwrap_or(default);
        let next = (cur + delta as f64).clamp(lo, hi);
        if next == cur {
            return false;
        }
        card.insert(key.to_string(), Value::from(next));
        return true;
    }
    let Some(cur) = chip_pin(card, side_out, pin_idx).and_then(|p| p.channel()) else { return false };
    let stops: Vec<Channel> = if side_out {
        (0..16).map(Channel::Ch).collect()
    } else {
        std::iter::once(Channel::Any).chain((0..16).map(Channel::Ch)).collect()
    };
    let pos = stops.iter().position(|c| *c == cur).unwrap_or(0) as i32;
    let next = stops[(pos + delta).rem_euclid(stops.len() as i32) as usize];
    let key = if side_out { "out" } else { "in" };
    match card.get_mut(key) {
        Some(Value::Array(arr)) => midi_set_pin_channel(arr, pin_idx, next),
        _ => false,
    }
}

/// Set a level row outright (a mouse drag). Clamped like a step.
pub fn midi_chip_set_level(card: &mut serde_json::Map<String, Value>, row: MidiChipRow, value: f64) -> bool {
    let Some((key, default, lo, hi)) = row.level() else { return false };
    let cur = card.get(key).and_then(|v| v.as_f64()).unwrap_or(default);
    let next = value.round().clamp(lo, hi);
    if next == cur {
        return false;
    }
    card.insert(key.to_string(), Value::from(next));
    true
}

/// Read a level row's value, for a mouse DragValue.
pub fn midi_chip_level(card: &serde_json::Map<String, Value>, row: MidiChipRow) -> Option<f64> {
    let (key, default, _, _) = row.level()?;
    Some(card.get(key).and_then(|v| v.as_f64()).unwrap_or(default))
}

// ── Node bodies ───────────────────────────────────────────────────────────────

/// `(real pin index, pin id)` for every non-AutoMap pin, in order.
fn midi_pin_rows(pins: &[PinDescriptor], ids: Option<&Vec<Value>>) -> Vec<(usize, String)> {
    pins.iter().enumerate()
        .filter(|(_, p)| p.signal_type != SignalType::AutoMap)
        .map(|(i, _)| (i, ids.and_then(|a| a.get(i)).and_then(|v| v.as_str()).unwrap_or("").to_string()))
        .collect()
}

/// Append a MIDI pin to a MIDI node side — a note with its velocity and
/// aftertouch (see [`MidiPin::node_pins`]). Pins the node already has are
/// skipped.
fn push_midi_pin(node: &mut NodeData, pin: &MidiPin, outputs: bool) {
    for p in pin.node_pins() {
        push_one_midi_pin(node, &p, outputs);
    }
}

fn push_one_midi_pin(node: &mut NodeData, pin: &MidiPin, outputs: bool) {
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
    let out_id = node.params.get("device_id").and_then(|v| v.as_str()).unwrap_or("").to_string();

    ui.vertical(|ui| {
        ui.set_min_width(180.0);

        let guard = midi_guard_view(ui.ctx());
        let risk = midi_out_loop_risk(snarl, &out_id, thru, &guard);
        let muted = guard.muted.iter().any(|m| *m == out_id);
        ui.horizontal(|ui| {
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
            if let Some(why) = &risk {
                ui.label(egui::RichText::new("⚠").color(Color32::from_rgb(230, 170, 60)))
                    .on_hover_text(why);
            }
        });
        // The breaker muted this port: say so, loudly, with the way back.
        if muted {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("⛔ Muted — a feedback loop was caught")
                    .small().color(Color32::from_rgb(226, 104, 92)))
                    .on_hover_text(
                        "This port was sending in a runaway loop with a MIDI In, so the loop \
                         breaker stopped it. Break the loop (Thru off, or unwire the mapping \
                         that answers itself), then unmute.",
                    );
                if ui.small_button("Unmute").clicked() {
                    request_midi_unmute(ui.ctx(), &out_id);
                }
            });
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

    fn card(json: serde_json::Value) -> serde_json::Map<String, Value> {
        json.as_object().unwrap().clone()
    }

    #[test]
    fn a_chip_offers_what_it_can_actually_change() {
        use MidiChipRow::*;
        assert_eq!(midi_chip_rows("midi:note:1:60", false), vec![Channel]);
        assert_eq!(midi_chip_rows("midi:note:1:60", true), vec![Channel, Velocity]);
        assert_eq!(midi_chip_rows("midi:cc:1:7", true), vec![Channel, On, Off]);
        assert_eq!(midi_chip_rows("midi:pc:1:5", true), vec![Channel], "a pulse has no level");
        assert!(midi_chip_rows("midi:rt:start", true).is_empty(), "nothing to tune");
        assert!(midi_chip_rows("btn_south", false).is_empty());
    }

    #[test]
    fn an_input_channel_passes_through_any_and_an_output_never_does() {
        let mut c = card(serde_json::json!({ "in": ["midi:cc:1:7"], "out": ["midi:cc:1:8"] }));
        assert!(midi_chip_step(&mut c, false, 0, MidiChipRow::Channel, -1));
        assert_eq!(c["in"][0], "midi:cc:*:7", "one below ch 1 on an input is any channel");
        assert!(midi_chip_step(&mut c, true, 0, MidiChipRow::Channel, -1));
        assert_eq!(c["out"][0], "midi:cc:16:8", "an output comes round to ch 16 instead");
        assert_eq!(midi_chip_value(&c, false, 0, MidiChipRow::Channel), "any channel");
        assert_eq!(midi_chip_value(&c, true, 0, MidiChipRow::Channel), "ch 16");
    }

    #[test]
    fn levels_start_at_their_defaults_and_clamp() {
        let mut c = card(serde_json::json!({ "out": ["midi:note:1:60"] }));
        assert_eq!(midi_chip_value(&c, true, 0, MidiChipRow::Velocity), "100");
        assert!(midi_chip_step(&mut c, true, 0, MidiChipRow::Velocity, 5));
        assert_eq!(midi_chip_level(&c, MidiChipRow::Velocity), Some(105.0));
        assert!(midi_chip_set_level(&mut c, MidiChipRow::Velocity, 500.0));
        assert_eq!(midi_chip_level(&c, MidiChipRow::Velocity), Some(127.0));
        assert!(!midi_chip_step(&mut c, true, 0, MidiChipRow::Velocity, 1), "already at the top");
        assert!(midi_chip_set_level(&mut c, MidiChipRow::Velocity, 0.0));
        assert_eq!(midi_chip_level(&c, MidiChipRow::Velocity), Some(1.0), "velocity 0 is a note-off");
        assert_eq!(midi_chip_value(&c, true, 0, MidiChipRow::Off), "0");
    }

    #[test]
    fn a_zero_step_changes_nothing() {
        let mut c = card(serde_json::json!({ "in": ["midi:cc:1:7"] }));
        assert!(!midi_chip_step(&mut c, false, 0, MidiChipRow::Channel, 0));
        assert_eq!(c["in"][0], "midi:cc:1:7");
    }

    /// The editor's value rows ALWAYS lay out — a type with no channel or no
    /// number still gets its row, saying so. A row that vanished for one type
    /// is exactly how the pad's focus ring once came to sit on one control
    /// while South fired the next.
    #[test]
    fn every_add_row_lays_out_whatever_the_type() {
        let ctx = egui::Context::default();
        let node = NodeId(7);
        for kind in PickKind::ALL {
            let id = midi_pick_state_id(node);
            ctx.data_mut(|d| d.insert_temp(id, PickerState { kind, channel: 1, number: 1, sysex: "F0 F7".into() }));
            let mut rects = [egui::Rect::NOTHING; 3];
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    rects = midi_add_rows_ui(ui, node, Some(MidiAddRow::Number));
                });
            });
            for (i, r) in rects.iter().enumerate() {
                assert!(r.is_finite() && r.width() > 0.5, "{kind:?}: row {i} has no rect");
            }
        }
    }

    /// A note reaches the bus three ways — its channel's pin, the any-channel
    /// twin, and its velocity — and Learn must capture it once, as sent. It
    /// used to learn the twin too, so every note became a two-message chord.
    #[test]
    fn learn_captures_a_midi_message_once_as_it_was_sent() {
        let ctx = egui::Context::default();
        let mut live = std::collections::HashMap::new();
        let dev = "midi_in:0".to_string();
        live.insert((dev.clone(), "midi:note:3:60".to_string()), Signal::Bool(true));
        live.insert((dev.clone(), "midi:note:*:60".to_string()), Signal::Bool(true));
        live.insert((dev.clone(), "midi:vel:3:60".to_string()), Signal::Float(0.9));
        live.insert((dev.clone(), "midi:vel:*:60".to_string()), Signal::Float(0.9));
        let mut got = Vec::new();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                got = crate::canvas::viewer::remapper_midi_pressed_now(ui, &live, &dev);
            });
        });
        assert_eq!(got, vec!["midi:note:3:60".to_string()]);
    }

    #[test]
    fn a_stick_step_walks_the_type_and_channel_round_and_clamps_the_number() {
        let mut st = PickerState { kind: PickKind::Cc, channel: 1, number: 7, sysex: String::new() };

        // Channel: 1 → any (0) → wraps to 16.
        pick_state_adjust(&mut st, 1, -1);
        assert_eq!(st.channel, 0, "one below ch 1 is any channel");
        pick_state_adjust(&mut st, 1, -1);
        assert_eq!(st.channel, 16, "and below that it comes round");
        pick_state_adjust(&mut st, 1, 1);
        assert_eq!(st.channel, 0);

        // Number clamps at its type's ends rather than wrapping: 0 and 127 are
        // places you want to sit.
        st.number = 0;
        pick_state_adjust(&mut st, 2, -1);
        assert_eq!(st.number, 0);
        st.number = 127;
        pick_state_adjust(&mut st, 2, 1);
        assert_eq!(st.number, 127);

        // Type walks the whole list and comes round.
        let first = PickKind::ALL[0];
        st.kind = first;
        pick_state_adjust(&mut st, 0, -1);
        assert_eq!(st.kind, *PickKind::ALL.last().unwrap());
        pick_state_adjust(&mut st, 0, 1);
        assert_eq!(st.kind, first);
    }

    /// Stepping onto a narrower type pulls the number into its range — a CC 100
    /// left behind on a 14-bit CC would build a pin that doesn't exist.
    #[test]
    fn narrowing_the_type_pulls_the_number_into_range() {
        let mut st = PickerState { kind: PickKind::Cc, channel: 1, number: 100, sysex: String::new() };
        let cc14 = PickKind::ALL.iter().position(|k| *k == PickKind::Cc14).unwrap() as i32;
        let cur = PickKind::ALL.iter().position(|k| *k == PickKind::Cc).unwrap() as i32;
        pick_state_adjust(&mut st, 0, cc14 - cur);
        assert_eq!(st.kind, PickKind::Cc14);
        assert_eq!(st.number, 31, "clamped to the 14-bit MSB range");
        assert!(st.kind.build(Channel::Ch(0), st.number, "").is_some());
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

    /// The ⚠ on a MIDI Out: only with Thru on, and only when a MIDI In placed
    /// in the same patch is paired with its port — a pair shown to be two
    /// separate ports (demoted) is no risk.
    #[test]
    fn a_midi_out_warns_only_when_thru_can_loop() {
        use flexinput_devices::midi::PairStatus;
        let mut snarl: Snarl<NodeData> = Snarl::new();
        let mut params = std::collections::HashMap::new();
        params.insert("device_id".to_string(), Value::from("midi_in:loop"));
        snarl.insert_node(egui::Pos2::ZERO, NodeData {
            module_id: "device.source".into(),
            display_name: "loopMIDI".into(),
            category: "Device".into(),
            inputs: vec![],
            outputs: vec![],
            params,
            subpatch: None,
            extra: Default::default(),
        });
        let guard = |s: PairStatus| MidiGuardView {
            pairs: vec![("midi_out:loop".into(), "midi_in:loop".into(), s)],
            muted: vec![],
        };
        assert!(midi_out_loop_risk(&snarl, "midi_out:loop", true, &guard(PairStatus::Seeded)).is_some());
        assert!(midi_out_loop_risk(&snarl, "midi_out:loop", false, &guard(PairStatus::Seeded)).is_none(),
            "Thru off sends only what mappings produce");
        assert!(midi_out_loop_risk(&snarl, "midi_out:loop", true, &guard(PairStatus::Demoted)).is_none());
        assert!(midi_out_loop_risk(&snarl, "midi_out:other", true, &guard(PairStatus::Confirmed)).is_none());
        assert!(midi_out_loop_risk(&Snarl::new(), "midi_out:loop", true, &guard(PairStatus::Confirmed)).is_none(),
            "the paired In isn't in this patch");
    }
}
