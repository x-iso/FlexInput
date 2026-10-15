//! Raw-joystick device nodes (`sdl:raw:`): Learn, motion setup, and the list
//! of what has been learned.
//!
//! A controller no mapping database knows arrives as numbered axes, buttons
//! and hats — plus, where its HID descriptor could be read, the fields SDL
//! skips and the opaque bytes of its vendor data (`raw_hid`). Learn listens
//! for whichever element the user moves next, says what it looks like (a
//! button, a centred axis, an axis resting at one end — a trigger — or a
//! hat) and offers the AutoMap pins it could be, or a port of its own. Motion
//! sensors are set up whole (`raw_motion`). Every choice goes into the pad's
//! layout in settings (`raw_learn`), which the backend applies, and its port
//! is added to this node. See `flexinput_core::raw_pad`.

use super::*;

use std::collections::HashSet;

use flexinput_core::raw_pad::{
    self, all_at_rest, detect, detect_blob_byte, Detected, ElemKind, RawBind, RawPadProfile, RawSrc,
    RawValue, HAT_DPAD_PIN,
};
use flexinput_devices::raw_hid;

use super::raw_motion::{analyze, motion_setup_active, show_motion_section, Verdicts};
use crate::raw_learn::{self, RawEdit};

/// How long a pressed button waits for an axis to move with it before Learn
/// takes the button. Pads that report a trigger as both (a digital click and
/// an analog travel) click first; the analog one is the one worth having.
const SETTLE_SECS: f64 = 0.25;

/// The buttons Learn offers, in menu order.
const BUTTON_PINS: &[&str] = &[
    "btn_south", "btn_east", "btn_west", "btn_north",
    "btn_lb", "btn_rb", "btn_lt_dig", "btn_rt_dig",
    "btn_ls", "btn_rs", "btn_start", "btn_back", "btn_guide", "btn_capture",
    "dpad_up", "dpad_down", "dpad_left", "dpad_right",
    "btn_paddle_l1", "btn_paddle_r1", "btn_paddle_l2", "btn_paddle_r2",
    "btn_touchpad",
    "btn_misc1", "btn_misc2", "btn_misc3", "btn_misc4", "btn_misc5", "btn_misc6",
];

/// One entry of the "Assign to…" list.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Choice {
    pub pin: &'static str,
    pub label: String,
    /// Axis only: whether the way the user moved it is the pin's positive
    /// direction (stick right/up, a trigger pulled, a button pressed).
    pub moved_is_positive: bool,
}

fn pin_label(pin: &str) -> String {
    match pin {
        // Positional names read better with the letters most generic pads
        // print on them.
        "btn_south" => "South (A)".into(),
        "btn_east" => "East (B)".into(),
        "btn_west" => "West (X)".into(),
        "btn_north" => "North (Y)".into(),
        HAT_DPAD_PIN => "D-Pad".into(),
        _ => am_canon::ALL_PINS.iter().find(|p| p.id == pin)
            .map(|p| p.display_name.to_string())
            .unwrap_or_else(|| pin.to_string()),
    }
}

/// An element's name: its descriptor usage where the HID tier knows one.
pub(crate) fn elem_label(dev_id: &str, src: RawSrc) -> String {
    raw_hid::hid_info(dev_id)
        .and_then(|h| h.label_of(src).map(str::to_string))
        .unwrap_or_else(|| src.label())
}

fn plain(pin: &'static str) -> Choice {
    Choice { pin, label: pin_label(pin), moved_is_positive: true }
}

/// What Learn offers for what it saw, grouped: `(heading, choices)`.
pub(crate) fn choices_for(det: &Detected) -> Vec<(&'static str, Vec<Choice>)> {
    let buttons = || BUTTON_PINS.iter().map(|p| plain(p)).collect::<Vec<_>>();
    let triggers = || vec![plain("left_trigger"), plain("right_trigger")];
    match det.src.kind() {
        ElemKind::Button => vec![("Button", buttons()), ("Trigger (fully pressed)", triggers())],
        ElemKind::Hat => vec![("D-Pad", vec![plain(HAT_DPAD_PIN)])],
        ElemKind::Axis => {
            let triggers = ("Trigger", triggers());
            let as_button = ("Button (past halfway)", buttons());
            // An axis that rests at one end has no centre to be a stick about:
            // it is a trigger (or a pedal, a throttle) — or a button.
            if det.rests_at_end() {
                return vec![triggers, as_button];
            }
            let mut sticks = Vec::new();
            for (stick, x, y) in [("Left Stick", "left_stick_x", "left_stick_y"),
                                  ("Right Stick", "right_stick_x", "right_stick_y")] {
                for (dir, pin, positive) in [("right", x, true), ("left", x, false),
                                             ("up", y, true), ("down", y, false)] {
                    sticks.push(Choice {
                        pin,
                        label: format!("{stick} — you pushed it {dir}"),
                        moved_is_positive: positive,
                    });
                }
            }
            vec![("Stick", sticks), triggers, as_button]
        }
    }
}

/// The layout entry a choice makes of what Learn saw.
pub(crate) fn bind_for(det: &Detected, choice: &Choice) -> RawBind {
    let (rest, full) = match det.src.kind() {
        ElemKind::Axis => {
            let d = det.direction();
            (det.rest, if choice.moved_is_positive { d } else { -d })
        }
        _ => (0.0, 1.0),
    };
    RawBind { src: det.src, pin: choice.pin.to_string(), rest, full, label: String::new() }
}

/// The layout entry that keeps what Learn saw as a port of its own.
pub(crate) fn own_port_bind(det: &Detected, name: &str, fallback: &str) -> RawBind {
    let name = name.trim();
    RawBind {
        src: det.src,
        pin: det.src.extra_pin_id(),
        rest: 0.0,
        full: 1.0,
        label: if name.is_empty() { fallback.to_string() } else { name.to_string() },
    }
}

fn describe(det: &Detected, name: &str) -> String {
    match det.src.kind() {
        ElemKind::Button => format!("{name} pressed"),
        ElemKind::Hat => format!("{name} moved — a D-pad"),
        ElemKind::Axis if det.rests_at_end() => format!("{name} moved from one end — a trigger?"),
        ElemKind::Axis => format!(
            "{name} moved {} from centre",
            if det.direction() > 0.0 { "+" } else { "−" },
        ),
    }
}

/// Learn's progress on one node. Lives in egui's temp data, not the node's
/// params: none of it is worth saving with the patch.
#[derive(Clone, Default)]
struct LearnState {
    /// Every element's reading when Learn started — "at rest".
    baseline: Vec<(RawSrc, RawValue)>,
    /// Vendor-blob bytes when Learn started: `((report, byte), value)`.
    blob_base: Vec<((u8, u16), u8)>,
    started: bool,
    phase: Phase,
    /// The name typed for "Add as its own port".
    own_name: String,
}

#[derive(Clone, Default)]
enum Phase {
    /// Waiting for everything to return to rest (Learn just started, or the
    /// last element is still held).
    #[default]
    Release,
    /// Listening for the next element.
    Listen,
    /// A button went down; giving an analog twin [`SETTLE_SECS`] to show.
    Settle { det: Detected, since: f64 },
    /// Showing what was seen, waiting for the user's choice.
    Captured(Detected),
}

fn state_id(node: NodeId, dev_id: &str) -> egui::Id {
    egui::Id::new(("raw_pad_learn", node.0, dev_id))
}

/// This device's raw readings Learn may listen to: everything published,
/// minus what moves on its own (a sensor, a counter) and what a motion
/// sensor already uses.
fn raw_readings(
    live: &std::collections::HashMap<(String, String), Signal>,
    dev_id: &str,
    ignore: &HashSet<RawSrc>,
) -> Vec<(RawSrc, RawValue)> {
    let mut out: Vec<(RawSrc, RawValue)> = live.iter()
        .filter(|((d, _), _)| d == dev_id)
        .filter_map(|((_, pin), sig)| {
            let src = RawSrc::from_pin_id(pin)?;
            (!ignore.contains(&src)).then(|| (src, RawValue::from_signal(src, *sig)))
        })
        .collect();
    // Map order is arbitrary; detection takes the first hit, so fix it.
    out.sort_by_key(|(src, _)| match *src {
        RawSrc::Button(i) => (0u8, i as u32),
        RawSrc::Hat(i) => (1, i as u32),
        RawSrc::Axis(i) => (2, i as u32),
        RawSrc::Field(f) => (3, (f.report as u32) << 16 | f.bit as u32),
    });
    out
}

/// The vendor-blob bytes Learn may diff: every blob byte that doesn't change
/// on its own.
fn blob_bytes(dev_id: &str, verdicts: &Verdicts) -> Vec<((u8, u16), u8)> {
    let (Some(hid), Some(reports)) = (raw_hid::hid_info(dev_id), raw_hid::latest_reports(dev_id)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for &(report, start, len) in &hid.blobs {
        let Some(payload) = reports.get(&report) else { continue };
        for b in start..start + len {
            if verdicts.noisy_bytes.contains(&(report, b)) {
                continue;
            }
            if let Some(v) = payload.get(b as usize) {
                out.push(((report, b), *v));
            }
        }
    }
    out
}

/// Append the ports a layout implies that the node doesn't have yet.
fn add_missing_ports(node: &mut NodeData, profile: &RawPadProfile) {
    for (id, name, ty) in profile.ports() {
        let has = node.params.get("output_pin_ids").and_then(|v| v.as_array())
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(id.as_str())));
        if has {
            continue;
        }
        node.outputs.push(PinDescriptor::new(&name, ty));
        match node.params.get_mut("output_pin_ids") {
            Some(Value::Array(ids)) => ids.push(Value::String(id)),
            _ => {
                node.params.insert("output_pin_ids".into(), Value::Array(vec![Value::String(id)]));
            }
        }
    }
}

/// Learn, motion setup and Clear unused under a raw pad node's title, and —
/// while Learn is on — what it saw and the choices for it.
pub(crate) fn show_raw_pad_header_controls(
    node_id: NodeId,
    outputs: &[OutPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
    live: &std::collections::HashMap<(String, String), Signal>,
) {
    let Some(node) = snarl.get_node(node_id) else { return };
    let dev_id = node.params.get("device_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut learning = node.params.get("learning").and_then(|v| v.as_bool()).unwrap_or(false);
    let (Some(key), Some(profile)) = (raw_learn::key_of(&dev_id), raw_learn::profile_of(&dev_id)) else {
        ui.label(egui::RichText::new("Connect the controller to Learn its layout.").small().weak());
        return;
    };
    let dev_name = node.display_name.clone();
    let sid = state_id(node_id, &dev_id);
    let verdicts = analyze(ui.ctx(), node_id, &dev_id, live);
    let in_motion_setup = motion_setup_active(ui.ctx(), node_id, &dev_id);
    if in_motion_setup && learning {
        learning = false;
        if let Some(n) = snarl.get_node_mut(node_id) {
            n.params.insert("learning".into(), Value::Bool(false));
        }
    }

    ui.horizontal(|ui| {
        let label = if learning {
            egui::RichText::new("● Stop").color(Color32::from_rgb(220, 80, 80))
        } else {
            egui::RichText::new("Learn")
        };
        let resp = ui.add_enabled(!in_motion_setup, egui::Button::new(label).small()).on_hover_text(
            "Teach FlexInput this controller: with Learn on, press each button and move \
             each stick, trigger and D-pad in turn, and say what it is. The layout is \
             saved for this model and works everywhere it is plugged in.",
        );
        if resp.clicked() {
            if let Some(n) = snarl.get_node_mut(node_id) {
                n.params.insert("learning".into(), Value::Bool(!learning));
            }
            // A fresh start every time: the baseline is wherever things rest NOW.
            ui.ctx().data_mut(|d| d.remove::<LearnState>(sid));
        }
        let has_unused = outputs.iter().any(|o| o.remotes.is_empty())
            && snarl.get_node(node_id).is_some_and(|n| n.outputs.iter()
                .any(|p| p.signal_type != SignalType::AutoMap));
        if has_unused && ui.small_button("Clear unused").clicked() {
            clear_unused_dynamic_pins::<Outputs>(node_id, outputs, snarl, "output_pin_ids");
        }
    });

    if !learning {
        if profile.is_empty() && !in_motion_setup {
            ui.label(egui::RichText::new(
                "Unknown controller — press Learn to map its buttons and sticks.",
            ).small().color(Color32::from_rgb(232, 180, 65)));
        }
        if let Some(found) = show_motion_section(ui, node_id, &dev_id, live, &verdicts, &profile) {
            let mut next = profile.clone();
            next.set_motion(found.gyro.clone(), found.accel.clone());
            raw_learn::request(&key, &dev_name, RawEdit::SetMotion { gyro: found.gyro, accel: found.accel });
            if let Some(n) = snarl.get_node_mut(node_id) {
                add_missing_ports(n, &next);
            }
        }
        return;
    }
    request_repaint_throttled(ui.ctx());

    let ignore: HashSet<RawSrc> = verdicts.restless().chain(profile.motion_srcs()).collect();
    let now = raw_readings(live, &dev_id, &ignore);
    let blobs_now = blob_bytes(&dev_id, &verdicts);
    let t = ui.input(|i| i.time);
    let mut st: LearnState = ui.ctx().data(|d| d.get_temp(sid)).unwrap_or_default();
    if !st.started {
        st.baseline = now.clone();
        st.blob_base = blobs_now.clone();
        st.started = true;
    }
    // Blob bytes: a byte that drifted off its baseline without anyone touching
    // it (one turned out to be noisy late) just moves the baseline along.
    let blob_detect = || -> Option<Detected> {
        blobs_now.iter().find_map(|&((report, byte), v)| {
            let base = st.blob_base.iter().find(|(k, _)| *k == (report, byte)).map(|(_, b)| *b)?;
            detect_blob_byte(report, byte, base, v)
        })
    };
    let blobs_at_rest = blobs_now.iter().all(|&(k, v)| {
        st.blob_base.iter().find(|(b, _)| *b == k).is_none_or(|(_, b)| *b == v)
    });
    st.phase = match std::mem::take(&mut st.phase) {
        Phase::Release if all_at_rest(&st.baseline, &now) && blobs_at_rest => Phase::Listen,
        Phase::Listen => match detect(&st.baseline, &now).or_else(blob_detect) {
            Some(det) if det.src.kind() == ElemKind::Button => Phase::Settle { det, since: t },
            Some(det) => Phase::Captured(det),
            None => Phase::Listen,
        },
        Phase::Settle { det, since } => {
            let axes: Vec<_> = now.iter().copied()
                .filter(|(s, _)| s.kind() == ElemKind::Axis).collect();
            match detect(&st.baseline, &axes) {
                Some(axis) => Phase::Captured(axis),
                None if t - since >= SETTLE_SECS => Phase::Captured(det),
                None => Phase::Settle { det, since },
            }
        }
        p => p,
    };

    let mut chosen: Option<RawBind> = None;
    let mut skip = false;
    match &st.phase {
        Phase::Release => {
            ui.label(egui::RichText::new("Let go of everything…").small().italics());
        }
        Phase::Listen | Phase::Settle { .. } => {
            ui.label(egui::RichText::new(
                "Press a button, or push a stick, trigger or D-pad all the way.",
            ).small().italics().color(Color32::from_rgb(106, 167, 255)));
        }
        Phase::Captured(det) => {
            let det = *det;
            let name = elem_label(&dev_id, det.src);
            ui.label(egui::RichText::new(describe(&det, &name)).small().strong()
                .color(Color32::from_rgb(127, 201, 127)));
            let owner_of = |pin: &str| profile.binds.iter()
                .find(|b| b.pin == pin).map(|b| elem_label(&dev_id, b.src));
            egui::ComboBox::from_id_salt((node_id, "raw_pad_assign"))
                .selected_text(egui::RichText::new("Assign to…").small())
                .width(230.0)
                .height(420.0)
                .show_ui(ui, |ui| {
                    for (heading, choices) in choices_for(&det) {
                        ui.label(egui::RichText::new(heading).small().weak());
                        for c in choices {
                            let text = match owner_of(c.pin) {
                                Some(owner) => format!("{}   · now {owner}", c.label),
                                None => c.label.clone(),
                            };
                            if ui.selectable_label(false, egui::RichText::new(text).small()).clicked() {
                                chosen = Some(bind_for(&det, &c));
                            }
                        }
                        ui.separator();
                    }
                });
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut st.own_name)
                    .hint_text(name.clone())
                    .desired_width(110.0)
                    .font(egui::TextStyle::Small));
                if ui.small_button("Add as its own port")
                    .on_hover_text("Not a gamepad control: keep it as a port of its own on this \
                                    node, under this name. It rides the Auto-Map bus too, so a \
                                    Remapper downstream can Learn it.")
                    .clicked()
                {
                    chosen = Some(own_port_bind(&det, &st.own_name, &name));
                }
                if ui.small_button("Skip").clicked() {
                    skip = true;
                }
            });
        }
    }

    if let Some(bind) = chosen {
        let mut next = profile.clone();
        next.set(bind.clone());
        raw_learn::request(&key, &dev_name, RawEdit::Bind(bind));
        if let Some(n) = snarl.get_node_mut(node_id) {
            add_missing_ports(n, &next);
        }
        st.phase = Phase::Release;
        st.own_name.clear();
    } else if skip {
        st.phase = Phase::Release;
        st.own_name.clear();
    }
    ui.ctx().data_mut(|d| d.insert_temp(sid, st));
}

/// What a raw pad has learned, one row per element (and one per motion
/// sensor), each with × to forget it and drop its port from this node.
pub(crate) fn show_raw_pad_body(
    node_id: NodeId,
    _outputs: &[OutPin],
    ui: &mut egui::Ui,
    snarl: &mut Snarl<NodeData>,
) {
    let Some(node) = snarl.get_node(node_id) else { return };
    let dev_id = node.params.get("device_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let (Some(key), Some(profile)) = (raw_learn::key_of(&dev_id), raw_learn::profile_of(&dev_id)) else {
        return;
    };
    if profile.is_empty() {
        return;
    }
    let mut forget: Option<(String, Vec<&'static str>)> = None;
    ui.vertical(|ui| {
        ui.set_min_width(180.0);
        for b in &profile.binds {
            ui.horizontal(|ui| {
                if ui.small_button("×").on_hover_text("Forget this").clicked() {
                    forget = Some((b.pin.clone(), Vec::new()));
                }
                let target = if b.is_own_port() {
                    format!("“{}” (own port)", if b.label.is_empty() { elem_label(&dev_id, b.src) } else { b.label.clone() })
                } else {
                    pin_label(&b.pin)
                };
                ui.label(egui::RichText::new(format!("{} → {target}", elem_label(&dev_id, b.src))).small());
            });
        }
        for (map, what, pins) in [
            (&profile.gyro, "Gyro", ["gyro_x", "gyro_y", "gyro_z"]),
            (&profile.accel, "Accelerometer", ["accel_x", "accel_y", "accel_z"]),
        ] {
            let Some(map) = map else { continue };
            ui.horizontal(|ui| {
                if ui.small_button("×").on_hover_text("Forget this sensor").clicked() {
                    forget = Some((pins[0].to_string(), pins.to_vec()));
                }
                let from: Vec<String> = map.src.iter().map(|s| elem_label(&dev_id, *s)).collect();
                ui.label(egui::RichText::new(format!("{} → {what}", from.join(", "))).small());
            });
        }
    });
    let Some((pin, ports)) = forget else { return };
    raw_learn::request(&key, &profile.name, RawEdit::RemovePin(pin.clone()));
    // A sensor takes its three ports with it; a bind, its one.
    let ports: Vec<String> = if ports.is_empty() { vec![pin] } else { ports.iter().map(|p| p.to_string()).collect() };
    for port in ports.iter().rev() {
        let fresh: Vec<OutPin> = (0..snarl.get_node(node_id).map_or(0, |n| n.outputs.len()))
            .map(|i| snarl.out_pin(egui_snarl::OutPinId { node: node_id, output: i }))
            .collect();
        let idx = snarl.get_node(node_id)
            .and_then(|n| n.params.get("output_pin_ids").and_then(|v| v.as_array()))
            .and_then(|ids| ids.iter().position(|v| v.as_str() == Some(port.as_str())));
        if let Some(idx) = idx.filter(|&i| i < fresh.len()) {
            remove_dynamic_pin::<Outputs>(node_id, idx, &fresh, snarl, "output_pin_ids", 0);
        }
    }
}

/// Whether a device node is a raw pad's.
pub(crate) fn is_raw_pad_node(module_id: &str, dev_id: &str) -> bool {
    module_id == "device.source" && raw_pad::is_raw_device_id(dev_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn axis(rest: f32, moved_to: f32) -> Detected {
        Detected { src: RawSrc::Axis(1), rest, moved_to }
    }

    fn pick(det: &Detected, pin: &str, positive: bool) -> RawBind {
        let c = choices_for(det).into_iter().flat_map(|(_, cs)| cs)
            .find(|c| c.pin == pin && c.moved_is_positive == positive)
            .unwrap_or_else(|| panic!("{pin} not offered"));
        bind_for(det, &c)
    }

    fn reads(b: &RawBind, v: f32) -> f32 {
        let mut p = RawPadProfile::default();
        p.set(b.clone());
        let mut out = 0.0;
        let axes = [0.0, v];
        p.compile().apply(
            &raw_pad::RawReading { axes: &axes, buttons: &[], hats: &[], reports: &() },
            |id, s| if id == b.pin { out = s.as_float() },
        );
        out
    }

    #[test]
    fn pushing_up_on_a_down_positive_axis_reads_up() {
        // DirectInput Y grows downward: pushing up reads −1.
        let det = axis(0.0, -0.9);
        let b = pick(&det, "left_stick_y", true);
        assert_eq!(reads(&b, -1.0), 1.0);
        assert_eq!(reads(&b, 1.0), -1.0);
    }

    #[test]
    fn pushing_left_binds_x_inverted_from_the_move() {
        let det = axis(0.0, -0.8);
        let b = pick(&det, "right_stick_x", false);
        assert_eq!(reads(&b, -1.0), -1.0, "left reads left");
        assert_eq!(reads(&b, 1.0), 1.0);
    }

    #[test]
    fn an_axis_resting_at_an_end_is_offered_as_a_trigger_and_never_a_stick() {
        let det = axis(-1.0, 0.3);
        let groups = choices_for(&det);
        assert_eq!(groups[0].0, "Trigger");
        assert!(groups.iter().all(|(h, _)| *h != "Stick"));
        let b = pick(&det, "right_trigger", true);
        assert_eq!(reads(&b, -1.0), 0.0);
        assert_eq!(reads(&b, 1.0), 1.0);
    }

    #[test]
    fn every_offered_pin_is_one_the_layout_can_drive() {
        let field_flag = RawSrc::Field(raw_pad::HidField::flag(1, 9, 3));
        let field_axis = RawSrc::Field(raw_pad::HidField::byte(1, 10));
        for det in [axis(0.0, 0.9), axis(-1.0, 0.9),
                    Detected { src: RawSrc::Button(0), rest: 0.0, moved_to: 0.0 },
                    Detected { src: RawSrc::Hat(0), rest: 0.0, moved_to: 0.0 },
                    Detected { src: field_flag, rest: 0.0, moved_to: 0.0 },
                    Detected { src: field_axis, rest: -1.0, moved_to: 0.6 }] {
            for (_, cs) in choices_for(&det) {
                for c in cs {
                    let mut p = RawPadProfile::default();
                    p.set(bind_for(&det, &c));
                    assert!(!p.compile().is_empty(), "{:?} → {} compiles to nothing", det.src, c.pin);
                }
            }
        }
    }

    #[test]
    fn an_own_port_is_an_extra_pin_named_by_the_user_or_the_element() {
        let det = Detected { src: RawSrc::Button(4), rest: 0.0, moved_to: 0.0 };
        assert_eq!(own_port_bind(&det, "  ", "Button 5").label, "Button 5");
        let b = own_port_bind(&det, "Turbo", "Button 5");
        assert_eq!((b.pin.as_str(), b.label.as_str()), ("extra:b4", "Turbo"));
    }
}
