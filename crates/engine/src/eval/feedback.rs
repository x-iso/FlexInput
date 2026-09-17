//! Feedback layers: what reaches a physical pad's haptic inputs (rumble, light
//! bar, LEDs, adaptive triggers) — or a Network Receive's return frame — when
//! the game, modules and direct wires all ask for something at once.
//!
//! Precedence per haptic pin, highest first:
//!   1. a direct wire into the pad's haptic input;
//!   2. OVERRIDE (`feedback_override:{target}`) — a module taking over a kind of
//!      feedback from the game. The game's feedback for that whole
//!      [`FeedbackGroup`] is dropped and the module's values replace it
//!      (Audio Stream Haptics always; Feedback Control in Override mode);
//!   3. the GAME — local virtual pads (the main-loop auto-feedback) and network
//!      peers (`feedback_net:{target}`, gathered before the main loop).
//!
//! ADDITIVE injections (`feedback_inject:{target}`) combine on top of whatever 2
//! and 3 left (Feedback Control in Add mode).
//!
//! `feedback_game:{id}` is the read side, gathered before the main loop so a
//! module sees this tick's request: what the game asks of a pad or of a bus, for
//! modules that shape their own output by it.

use std::collections::{HashMap, HashSet};

use flexinput_core::automap::{self, FeedbackGroup};
use flexinput_core::Signal;

use crate::graph::{NodeSnap, SinkTarget};

use super::{
    collect_sink_sources, combine_signals, namespaced_uid, shape_hd_feedback,
    AUDIO_STREAM_HAPTICS_ID, NET_SEND_ID,
};

pub(crate) const FEEDBACK_GAME: &str = "feedback_game:";
pub(crate) const FEEDBACK_NET: &str = "feedback_net:";
pub(crate) const FEEDBACK_OVERRIDE: &str = "feedback_override:";
pub(crate) const FEEDBACK_INJECT: &str = "feedback_inject:";

/// The channels a feedback-producing module or a Selector route can carry.
pub(crate) const FEEDBACK_WRITE_CHANNELS: [&str; 3] = [FEEDBACK_NET, FEEDBACK_OVERRIDE, FEEDBACK_INJECT];

/// True if a node that reads the game's feedback, or brings in a network peer's,
/// exists anywhere in the graph. Gates [`gather_game_feedback`] so patches
/// without them pay nothing.
fn needs_game_feedback(nodes: &[NodeSnap]) -> bool {
    nodes.iter().any(|n| {
        n.module_id == AUDIO_STREAM_HAPTICS_ID
            || n.module_id == NET_SEND_ID
            || n.inline_subgraph.as_ref().is_some_and(|sg| needs_game_feedback(&sg.graph.nodes))
    })
}

fn put_max(collector_sigs: &mut HashMap<(String, String), Signal>, key: &str, pin: &str, v: f32) {
    collector_sigs
        .entry((key.to_string(), pin.to_string()))
        .and_modify(|s| *s = Signal::Float(s.as_float().max(v)))
        .or_insert(Signal::Float(v));
}

/// Pre-pass, before any node runs:
///   * `feedback_game:{pad}` — what a physical pad's virtual feedback sources
///     request (the same virtual pads the main loop forwards to it), plus what
///     its network peers sent back;
///   * `feedback_game:{bus}` — what the virtual pads fed from a bus id request
///     (`collector:{uid}`, `remap:{uid}`, a raw device …), so a module can read
///     the game behind it even when its target isn't a pad (e.g. a Network
///     Receive);
///   * `feedback_net:{target}` — a Network Send's received peer feedback, applied
///     as game feedback by [`apply_pad_feedback`] / the receiver frame fold.
pub(crate) fn gather_game_feedback(
    nodes: &[NodeSnap],
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
) {
    if !needs_game_feedback(nodes) { return; }
    // Main-loop auto-feedback only runs for top-level pads, so only those.
    for snap in nodes {
        let Some(ref st) = snap.sink_target else { continue; };
        if st.device_id.starts_with("virtual.") || st.feedback_sources.is_empty() { continue; }
        let key = format!("{FEEDBACK_GAME}{}", st.device_id);
        for src in &st.feedback_sources {
            read_virtual_feedback(dev_sigs, &src.device_id, &key, collector_sigs);
        }
    }
    let mut sink_sources: HashMap<String, Vec<String>> = HashMap::new();
    collect_sink_sources(nodes, &mut sink_sources);
    for (bus, sinks) in &sink_sources {
        let key = format!("{FEEDBACK_GAME}{bus}");
        for sink in sinks {
            read_virtual_feedback(dev_sigs, sink, &key, collector_sigs);
        }
    }
    gather_net_send_feedback(nodes, 0, false, collector_sigs);
}

fn read_virtual_feedback(
    dev_sigs: &HashMap<(String, String), Signal>,
    virtual_dev: &str,
    key: &str,
    collector_sigs: &mut HashMap<(String, String), Signal>,
) {
    for pin in automap::FEEDBACK_INLET_PINS {
        if let Some(sig) = dev_sigs.get(&(virtual_dev.to_string(), pin.id.to_string())) {
            put_max(collector_sigs, key, pin.id, sig.as_float());
        }
    }
}

/// Network Send nodes at any sub-patch depth, keyed by effective uid (raw at the
/// top level, namespaced inside a sub-patch — matching the send worker): the
/// feedback the peer's game sent back for the pad upstream of the node. Older
/// than ~1 s is ignored, matching the send worker's status window, so a dead
/// peer can't leave the pad buzzing forever.
fn gather_net_send_feedback(
    nodes: &[NodeSnap],
    outer_uid: usize,
    nested: bool,
    collector_sigs: &mut HashMap<(String, String), Signal>,
) {
    for node in nodes {
        let uid = if nested { namespaced_uid(outer_uid, node.node_uid) } else { node.node_uid };
        if node.module_id == NET_SEND_ID {
            let target = node.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("");
            if !target.is_empty() {
                if let Some((fb, age)) = flexinput_net::latest_feedback(uid) {
                    if age.as_millis() < 1000 {
                        let net_key = format!("{FEEDBACK_NET}{target}");
                        let game_key = format!("{FEEDBACK_GAME}{target}");
                        for (pin, v) in fb.iter_present() {
                            collector_sigs
                                .entry((net_key.clone(), pin.to_string()))
                                .and_modify(|e| *e = combine_signals(*e, Signal::Float(v)))
                                .or_insert(Signal::Float(v));
                            put_max(collector_sigs, &game_key, pin, v);
                        }
                    }
                }
            }
        }
        if let Some(ref sg) = node.inline_subgraph {
            gather_net_send_feedback(&sg.graph.nodes, uid, true, collector_sigs);
        }
    }
}

/// The strongest value the game requests on any of `pins` across `targets`
/// (pad or bus ids; empty ids are skipped). 0 when the game asks for nothing.
pub(crate) fn game_feedback(
    collector_sigs: &HashMap<(String, String), Signal>,
    targets: &[&str],
    pins: &[&str],
) -> f32 {
    let mut best = 0.0f32;
    for target in targets.iter().filter(|t| !t.is_empty()) {
        let key = format!("{FEEDBACK_GAME}{target}");
        for pin in pins {
            if let Some(sig) = collector_sigs.get(&(key.clone(), pin.to_string())) {
                best = best.max(sig.as_float());
            }
        }
    }
    best
}

/// An AutoMap Selector's output id reads the game feedback of the input it is
/// gating from, so a module placed after the Selector still sees the game.
pub(crate) fn forward_game_feedback(
    collector_sigs: &mut HashMap<(String, String), Signal>,
    from: &str,
    to: &str,
) {
    let from_key = format!("{FEEDBACK_GAME}{from}");
    let to_key = format!("{FEEDBACK_GAME}{to}");
    for pin in automap::FEEDBACK_INLET_PINS {
        if let Some(&sig) = collector_sigs.get(&(from_key.clone(), pin.id.to_string())) {
            put_max(collector_sigs, &to_key, pin.id, sig.as_float());
        }
    }
}

/// The feedback groups a target's overrides take over this tick.
pub(crate) fn overridden_groups(
    collector_sigs: &HashMap<(String, String), Signal>,
    target: &str,
) -> HashSet<FeedbackGroup> {
    let key = format!("{FEEDBACK_OVERRIDE}{target}");
    automap::FEEDBACK_INLET_PINS.iter()
        .filter(|p| collector_sigs.contains_key(&(key.clone(), p.id.to_string())))
        .filter_map(|p| automap::feedback_group(p.id))
        .collect()
}

/// Clamp a combined feedback value to the valid range for its haptic pin so
/// merging (game rumble + injected effect) can't overflow. Amplitudes and most
/// haptic pins are 0–1; everything falls back to 0–1, which is correct for the
/// rumble / light bar / amp pins modules inject.
fn clamp_feedback_signal(_pin: &str, sig: Signal) -> Signal {
    match sig {
        Signal::Float(f) => Signal::Float(f.clamp(0.0, 1.0)),
        other => other,
    }
}

/// Route a feedback-vocabulary pin onto one of the pad's haptic inputs: the same
/// id if the pad has it, else the rumble/light aliasing (`resolve_feedback_pin`).
/// `None` when the pad lacks it or a direct wire owns the destination.
fn route_pin<'a>(
    pin: &'static str,
    sig: Signal,
    dst_pins: &[&'a str],
    directly_wired: &HashSet<&str>,
) -> Option<(&'a str, Signal)> {
    let dst = if dst_pins.contains(&pin) { pin } else { automap::resolve_feedback_pin(pin, dst_pins)? };
    if directly_wired.contains(dst) { return None; }
    // Perceptual HD shaping for a CLASSIC rumble that remapped onto an HD
    // voice-coil amp pin (e.g. a networked Switch Pro: rumble_strong → hd_l_amp,
    // since the pad exposes no rumble_strong inlet). Mirrors the main-loop
    // auto-feedback pass (`shape_hd_feedback`) so a weak game rumble (0.1–0.3)
    // run through the encoder's power-law curve is still perceptible. Only when
    // the pin actually REMAPPED: a direct hd_l_amp value (Audio Stream Haptics /
    // Feedback Control) already carries an intended amplitude and must NOT be
    // reshaped. Uses the standard default floor/max/exp — a networked source's
    // per-device shaping isn't available on this end.
    let sig = if pin != dst && matches!(dst, "hd_l_amp" | "hd_r_amp") {
        shape_hd_feedback(sig, 0.35, 1.0, 0.6)
    } else {
        sig
    };
    Some((dst, sig))
}

fn combine_into(
    sink_outputs: &mut HashMap<(String, String), Signal>,
    dev: &str,
    dst: &str,
    sig: Signal,
) {
    use std::collections::hash_map::Entry;
    match sink_outputs.entry((dev.to_string(), dst.to_string())) {
        Entry::Occupied(mut o) => {
            let merged = combine_signals(*o.get(), sig);
            *o.get_mut() = clamp_feedback_signal(dst, merged);
        }
        Entry::Vacant(v) => { v.insert(sig); }
    }
}

/// True if any module or peer wrote a feedback layer this tick.
pub(crate) fn any_feedback_layer(collector_sigs: &HashMap<(String, String), Signal>) -> bool {
    collector_sigs.keys().any(|(k, _)| FEEDBACK_WRITE_CHANNELS.iter().any(|c| k.starts_with(c)))
}

/// Post-pass for one physical pad, on top of the main loop's local game
/// feedback: network game feedback, then overrides, then additive injections.
/// Direct wires are never touched.
pub(crate) fn apply_pad_feedback(
    st: &SinkTarget,
    collector_sigs: &HashMap<(String, String), Signal>,
    sink_outputs: &mut HashMap<(String, String), Signal>,
) {
    let dev = st.device_id.as_str();
    let layer = |prefix: &str| -> Vec<(&'static str, Signal)> {
        let key = format!("{prefix}{dev}");
        automap::FEEDBACK_INLET_PINS.iter()
            .filter_map(|p| collector_sigs.get(&(key.clone(), p.id.to_string())).map(|s| (p.id, *s)))
            .collect()
    };
    let game_net = layer(FEEDBACK_NET);
    let overrides = layer(FEEDBACK_OVERRIDE);
    let additive = layer(FEEDBACK_INJECT);
    if game_net.is_empty() && overrides.is_empty() && additive.is_empty() { return; }

    let dst_pins: Vec<&str> = st.pin_ids.iter()
        .filter(|p| !p.is_empty())
        .map(|p| p.as_str())
        .collect();
    let directly_wired: HashSet<&str> = st.pin_ids.iter().enumerate()
        .filter(|(i, pid)| !pid.is_empty() && st.multi_sources.get(*i).is_some_and(|s| !s.is_empty()))
        .map(|(_, pid)| pid.as_str())
        .collect();

    // The game, from a network peer (local virtual pads landed in the main loop).
    for (pin, sig) in game_net {
        if let Some((dst, sig)) = route_pin(pin, sig, &dst_pins, &directly_wired) {
            combine_into(sink_outputs, dev, dst, sig);
        }
    }

    // Overrides: silence the game's feedback for every group taken over — zeroed
    // rather than dropped, so a pad holding the game's last value lets go at once —
    // then the modules' values replace it.
    let groups: HashSet<FeedbackGroup> = overrides.iter()
        .filter_map(|(p, _)| automap::feedback_group(p))
        .collect();
    if !groups.is_empty() {
        for &p in &dst_pins {
            if directly_wired.contains(p) { continue; }
            if !automap::feedback_group(p).is_some_and(|g| groups.contains(&g)) { continue; }
            if let Some(v) = sink_outputs.get_mut(&(dev.to_string(), p.to_string())) {
                *v = Signal::Float(0.0);
            }
        }
    }
    for (pin, sig) in overrides {
        if let Some((dst, sig)) = route_pin(pin, sig, &dst_pins, &directly_wired) {
            combine_into(sink_outputs, dev, dst, sig);
        }
    }

    // Additive injections on top.
    for (pin, sig) in additive {
        if let Some((dst, sig)) = route_pin(pin, sig, &dst_pins, &directly_wired) {
            combine_into(sink_outputs, dev, dst, sig);
        }
    }
}
