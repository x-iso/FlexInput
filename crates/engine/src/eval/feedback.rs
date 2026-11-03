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

use flexinput_core::automap::{self, ClickActuator, FeedbackGroup};
use flexinput_core::{BusMap, PinMapExt, Signal};

use crate::graph::{NodeSnap, SinkTarget};

use super::{
    collect_sink_sources, combine_signals, namespaced_uid, shape_hd_feedback,
    AUDIO_STREAM_HAPTICS_ID, NET_SEND_ID,
};

pub(crate) const FEEDBACK_GAME: &str = "feedback_game:";
pub(crate) const FEEDBACK_NET: &str = "feedback_net:";
pub(crate) const FEEDBACK_OVERRIDE: &str = "feedback_override:";
pub(crate) const FEEDBACK_INJECT: &str = "feedback_inject:";
/// The shape of a click going out on that device (`automap::CLICK_TONE_PINS`). No
/// channel of its own to a pad that hasn't got those click pins.
pub(crate) const FEEDBACK_CLICK_TONE: &str = "feedback_click_tone:";

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

fn put_max(collector_sigs: &mut BusMap, key: &str, pin: &str, v: f32) {
    collector_sigs.merge_pin(key, pin, Signal::Float(v), |s| *s = Signal::Float(s.as_float().max(v)));
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
    dev_sigs: &BusMap,
    collector_sigs: &mut BusMap,
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
    dev_sigs: &BusMap,
    virtual_dev: &str,
    key: &str,
    collector_sigs: &mut BusMap,
) {
    for pin in automap::FEEDBACK_INLET_PINS {
        if let Some(sig) = dev_sigs.get_pin(&virtual_dev, &pin.id) {
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
    collector_sigs: &mut BusMap,
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
                            collector_sigs.merge_pin(&net_key, pin, Signal::Float(v),
                                |e| *e = combine_signals(*e, Signal::Float(v)));
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
    collector_sigs: &BusMap,
    targets: &[&str],
    pins: &[&str],
) -> f32 {
    let mut best = 0.0f32;
    for target in targets.iter().filter(|t| !t.is_empty()) {
        let key = format!("{FEEDBACK_GAME}{target}");
        for pin in pins {
            if let Some(sig) = collector_sigs.get_pin(&key, &pin) {
                best = best.max(sig.as_float());
            }
        }
    }
    best
}

/// An AutoMap Selector's output id reads the game feedback of the input it is
/// gating from, so a module placed after the Selector still sees the game.
pub(crate) fn forward_game_feedback(
    collector_sigs: &mut BusMap,
    from: &str,
    to: &str,
) {
    let from_key = format!("{FEEDBACK_GAME}{from}");
    let to_key = format!("{FEEDBACK_GAME}{to}");
    for pin in automap::FEEDBACK_INLET_PINS {
        if let Some(&sig) = collector_sigs.get_pin(&from_key, &pin.id) {
            put_max(collector_sigs, &to_key, pin.id, sig.as_float());
        }
    }
}

/// The feedback groups a target's overrides take over this tick.
pub(crate) fn overridden_groups(
    collector_sigs: &BusMap,
    target: &str,
) -> HashSet<FeedbackGroup> {
    let key = format!("{FEEDBACK_OVERRIDE}{target}");
    automap::FEEDBACK_INLET_PINS.iter()
        .filter(|p| collector_sigs.has_pin(&key, &p.id))
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
    sink_outputs: &mut BusMap,
    dev: &str,
    dst: &str,
    sig: Signal,
) {
    sink_outputs.merge_pin(dev, dst, sig, |cur| {
        *cur = clamp_feedback_signal(dst, combine_signals(*cur, sig));
    });
}

/// True if any module or peer wrote a feedback layer this tick.
pub(crate) fn any_feedback_layer(collector_sigs: &BusMap) -> bool {
    collector_sigs.keys().any(|(k, _)| FEEDBACK_WRITE_CHANNELS.iter().any(|c| k.starts_with(c)))
}

/// Post-pass for one physical pad, on top of the main loop's local game
/// feedback: network game feedback, then overrides, then additive injections.
/// Direct wires are never touched.
pub(crate) fn apply_pad_feedback(
    st: &SinkTarget,
    collector_sigs: &BusMap,
    sink_outputs: &mut BusMap,
) {
    let dev = st.device_id.as_str();
    let layer = |prefix: &str| -> Vec<(&'static str, Signal)> {
        let key = format!("{prefix}{dev}");
        automap::FEEDBACK_INLET_PINS.iter()
            .filter_map(|p| collector_sigs.get_pin(&key, &p.id).map(|s| (p.id, *s)))
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
            if let Some(v) = sink_outputs.get_pin_mut(&dev, &p) {
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

    // What shapes those clicks, for a pad that plays them itself.
    let tone_key = format!("{FEEDBACK_CLICK_TONE}{dev}");
    for (click, tone) in automap::CLICK_TONE_PINS {
        if !dst_pins.contains(&click) { continue; }
        for pin in tone {
            if let Some(&sig) = collector_sigs.get_pin(&tone_key, &pin) {
                sink_outputs.set_pin(&dev, &pin, sig);
            }
        }
    }
}

/// What one edge of a trigger plays (Calibration window): strength in dB, where
/// 0 is the actuator's full, pitch in Hz, length in ms, softness 0..1.
#[derive(Clone, Copy, Debug, PartialEq)]
struct EdgeClick {
    gain_db: f32,
    hz: f32,
    ms: f32,
    soft: f32,
}

impl EdgeClick {
    /// One edge from its four params (`[gain_db, hz, ms, soft]`), with the
    /// defaults for that edge and what plays it.
    fn read(params: &HashMap<String, serde_json::Value>, keys: [&str; 4], release: bool, actuator: ClickActuator) -> Self {
        let num = |key: &str, default: f32| params.get(key).and_then(|v| v.as_f64()).map_or(default, |v| v as f32);
        let gain = if release { automap::TRIGGER_RELEASE_DB } else { 0.0 };
        Self {
            gain_db: num(keys[0], gain),
            hz: num(keys[1], automap::TRIGGER_CLICK_HZ),
            ms: num(keys[2], actuator.default_ms()),
            soft: num(keys[3], 0.0),
        }
    }

    fn values(self) -> [f32; 4] {
        [self.gain_db, self.hz, self.ms, self.soft]
    }

    /// How long it plays on `actuator`, which may not take it as short as set.
    fn secs(self, actuator: ClickActuator) -> f32 {
        self.ms.max(actuator.shortest_ms()).clamp(1.0, 200.0) / 1000.0
    }
}

/// One trigger's click params on the device node, and the pins a pad that plays
/// its own takes them on. Each `[gain_db, hz, ms, soft]` set is press, then
/// release; the edge pins are the point and its band as raw travel
/// (`automap::TriggerEdges`: press, again, release, rest) and the debounce.
struct TriggerClickKeys {
    threshold: &'static str,
    offset: &'static str,
    range: [&'static str; 2],
    press_on: &'static str,
    release_on: &'static str,
    params: [[&'static str; 4]; 2],
    edge_pins: [&'static str; 5],
    press_pin: &'static str,
    release_pin: &'static str,
    pins: [[&'static str; 4]; 2],
}

const TRIGGER_CLICK_KEYS: [TriggerClickKeys; 2] = [
    TriggerClickKeys {
        threshold: "ltrig_digital_threshold",
        offset: "ltrig_click_offset",
        range: ["ltrig_min", "ltrig_max"],
        press_on: "ltrig_press_on",
        release_on: "ltrig_release_on",
        params: [
            ["ltrig_click_gain_db", "ltrig_click_hz", "ltrig_click_ms", "ltrig_click_soft"],
            ["ltrig_release_gain_db", "ltrig_release_hz", "ltrig_release_ms", "ltrig_release_soft"],
        ],
        edge_pins: ["trig_click_l", "trig_top_l", "trig_rearm_l", "trig_rest_l", "trig_hold_l"],
        press_pin: "trig_press_l",
        release_pin: "trig_release_l",
        pins: [
            ["trig_click_gain_l", "trig_click_hz_l", "trig_click_ms_l", "trig_click_soft_l"],
            ["trig_release_gain_l", "trig_release_hz_l", "trig_release_ms_l", "trig_release_soft_l"],
        ],
    },
    TriggerClickKeys {
        threshold: "rtrig_digital_threshold",
        offset: "rtrig_click_offset",
        range: ["rtrig_min", "rtrig_max"],
        press_on: "rtrig_press_on",
        release_on: "rtrig_release_on",
        params: [
            ["rtrig_click_gain_db", "rtrig_click_hz", "rtrig_click_ms", "rtrig_click_soft"],
            ["rtrig_release_gain_db", "rtrig_release_hz", "rtrig_release_ms", "rtrig_release_soft"],
        ],
        edge_pins: ["trig_click_r", "trig_top_r", "trig_rearm_r", "trig_rest_r", "trig_hold_r"],
        press_pin: "trig_press_r",
        release_pin: "trig_release_r",
        pins: [
            ["trig_click_gain_r", "trig_click_hz_r", "trig_click_ms_r", "trig_click_soft_r"],
            ["trig_release_gain_r", "trig_release_hz_r", "trig_release_ms_r", "trig_release_soft_r"],
        ],
    },
];

/// A pad's trigger clicks between ticks, where the engine plays them: where each
/// trigger stands against its point, and the click each is playing and how far in.
#[derive(Default)]
pub struct TriggerClicks {
    crossing: [automap::TriggerCrossing; 2],
    playing: [Option<(EdgeClick, f32)>; 2],
}

/// A trigger click, set in the Calibration window: as each trigger passes the
/// pad's digital threshold, moved by a per-trigger offset, and — where asked — as
/// it comes back past it, each side's press and release on or off of their own.
/// A digital trigger passes it as it is pressed.
///
/// A Steam Controller plays its own: the point goes out as raw trigger travel and
/// what plays as numbers, on pins no layout declares, every tick while it is on.
/// Its backend compares its own poll against them, and when they stop,
/// zero-on-drop ends the click. On any other pad the engine watches the trigger
/// and plays the click on the side's HD actuator, else its rumble motor, on top of
/// whatever else drives it.
pub(crate) fn apply_trigger_click<S: std::hash::BuildHasher>(
    st: &SinkTarget,
    params: &HashMap<String, serde_json::Value>,
    dev_sigs: &HashMap<(String, String), Signal, S>,
    clicks: &mut Option<Box<TriggerClicks>>,
    dt: f32,
    sink_outputs: &mut BusMap,
) {
    let num = |key: &str| params.get(key).and_then(|v| v.as_f64()).map(|v| v as f32);
    let flag = |key: &str| params.get(key).and_then(|v| v.as_bool()).unwrap_or(false);
    let actuator = flag("trig_click_on")
        .then(|| automap::click_actuator(st.pin_ids.iter().map(String::as_str)))
        .flatten();
    let Some(actuator) = actuator else {
        *clicks = None;
        return;
    };
    let dev = st.device_id.as_str();
    let hysteresis = num("trig_hysteresis").unwrap_or(automap::TRIGGER_HYSTERESIS);
    let hold = automap::trigger_debounce_secs(num("trig_debounce_ms"));
    for (side, keys) in TRIGGER_CLICK_KEYS.iter().enumerate() {
        let press_on = params.get(keys.press_on).and_then(|v| v.as_bool()).unwrap_or(true);
        let release = flag(keys.release_on).then(|| EdgeClick::read(params, keys.params[1], true, actuator));
        if !press_on && release.is_none() {
            if let Some(state) = clicks.as_mut() {
                state.crossing[side] = Default::default();
                state.playing[side] = None;
            }
            continue;
        }
        let point = num(keys.threshold).unwrap_or(super::device_cal::TRIG_DIGITAL_THRESHOLD_DEFAULT)
            + num(keys.offset).unwrap_or(0.0);
        let range = (num(keys.range[0]).unwrap_or(0.0), num(keys.range[1]).unwrap_or(1.0));
        let edges = automap::trigger_edges(point, range, hysteresis);
        let press = EdgeClick::read(params, keys.params[0], false, actuator);

        if actuator == ClickActuator::Own {
            let values = [edges.press, edges.again, edges.release, edges.rest, hold];
            for (pin, v) in keys.edge_pins.iter().zip(values) {
                sink_outputs.set_pin(&dev, pin, Signal::Float(v));
            }
            sink_outputs.set_pin(&dev, &keys.press_pin, Signal::Float(if press_on { 1.0 } else { 0.0 }));
            for (pin, v) in keys.pins[0].iter().zip(press.values()) {
                sink_outputs.set_pin(&dev, pin, Signal::Float(v));
            }
            if let Some(release) = release {
                sink_outputs.set_pin(&dev, &keys.release_pin, Signal::Float(1.0));
                for (pin, v) in keys.pins[1].iter().zip(release.values()) {
                    sink_outputs.set_pin(&dev, pin, Signal::Float(v));
                }
            }
            continue;
        }

        let state = clicks.get_or_insert_with(Default::default);
        if let Some(travel) = trigger_travel(dev_sigs, dev, side) {
            match state.crossing[side].step(&edges, travel, dt, hold) {
                Some(true) => if press_on { state.playing[side] = Some((press, 0.0)) },
                Some(false) => if let Some(release) = release { state.playing[side] = Some((release, 0.0)) },
                None => {}
            }
        }
        let Some((click, t)) = state.playing[side].as_mut() else { continue };
        let secs = click.secs(actuator);
        // Within a tenth of a millisecond is the end: ticks don't add up exactly.
        if *t + 1e-4 >= secs {
            state.playing[side] = None;
            continue;
        }
        let level = 10f32.powf(click.gain_db.min(0.0) / 20.0)
            * automap::click_envelope(*t / secs, click.soft);
        // A Switch Pro's pin is felt strength on its own table, not a level.
        let amp = if actuator == ClickActuator::Switch { automap::switch_hd_amp(level) } else { level };
        play_trigger_click(st, side, amp, hd_freq_norm(actuator, click.hz), sink_outputs);
        *t += dt;
    }
}

/// A trigger's raw travel: the analog value, else the digital button as 0 or 1.
/// `None` for a pad without that trigger.
fn trigger_travel<S: std::hash::BuildHasher>(
    dev_sigs: &HashMap<(String, String), Signal, S>,
    dev: &str,
    side: usize,
) -> Option<f32> {
    let [analog, digital] = [["left_trigger", "btn_lt_dig"], ["right_trigger", "btn_rt_dig"]][side];
    dev_sigs.get_pin(&dev, &analog)
        .or_else(|| dev_sigs.get_pin(&dev, &digital))
        .map(|s| s.as_float())
}

/// One tick of a click at `level`, on the side's HD actuator at pitch `freq` (its
/// `hd_*_freq` value; a pad with one actuator, a Joy-Con half, takes both sides
/// on it), else on its rumble motor: left strong, right weak, the way the HD and
/// rumble pins pair. It wins only where it is the stronger, so a heavy rumble
/// isn't cut short by a click.
fn play_trigger_click(st: &SinkTarget, side: usize, level: f32, freq_value: f32, sink_outputs: &mut BusMap) {
    let has = |pin: &str| st.pin_ids.iter().any(|p| p == pin);
    let dev = st.device_id.as_str();
    let hd = [("hd_l_amp", "hd_l_freq"), ("hd_r_amp", "hd_r_freq")];
    let actuator = if has(hd[side].0) {
        Some(hd[side])
    } else if has(hd[0].0) {
        Some(hd[0])
    } else {
        None
    };
    let (amp, freq) = match actuator {
        Some((amp, freq)) => (amp, Some(freq)),
        None => (["rumble_strong", "rumble_weak"][side], None),
    };
    if !has(amp) {
        return;
    }
    if sink_outputs.get_pin(&dev, &amp).is_some_and(|s| s.as_float() > level) {
        return;
    }
    sink_outputs.set_pin(&dev, &amp, Signal::Float(level));
    if let Some(freq) = freq {
        sink_outputs.set_pin(&dev, &freq, Signal::Float(freq_value));
    }
}

/// A pitch as the pad's `hd_*_freq` value: a Switch Pro's on its own frequency
/// table ([`automap::switch_hd_freq`]); every other HD pad reads it linearly over
/// `haptic_pcm`'s range.
fn hd_freq_norm(actuator: ClickActuator, hz: f32) -> f32 {
    use flexinput_core::haptic_pcm::{FREQ_MAX_HZ, FREQ_MIN_HZ};
    if actuator == ClickActuator::Switch {
        return automap::switch_hd_freq(hz);
    }
    ((hz - FREQ_MIN_HZ) / (FREQ_MAX_HZ - FREQ_MIN_HZ)).clamp(0.0, 1.0)
}

/// A Steam Controller's click feedback on its trackpads (Calibration window), on
/// pins no layout declares, every tick while it is on; zero-on-drop ends it.
pub(crate) fn apply_pad_click(
    st: &SinkTarget,
    params: &HashMap<String, serde_json::Value>,
    sink_outputs: &mut BusMap,
) {
    use flexinput_core::automap::{
        PAD_CLICK_HZ, PAD_CLICK_MS, PAD_CLICK_ON, PAD_CLICK_ON_RELEASE, PAD_CLICK_STRONG, PAD_CLICK_TONE,
    };
    let flag = |key: &str| params.get(key).and_then(|v| v.as_bool()).unwrap_or(false);
    if !flag("pad_click_on") {
        return;
    }
    let mode = PAD_CLICK_ON
        | if flag("pad_click_release") { PAD_CLICK_ON_RELEASE } else { 0 }
        | if flag("pad_click_strong") { PAD_CLICK_STRONG } else { 0 }
        | if flag("pad_click_tone") { PAD_CLICK_TONE } else { 0 };
    let num = |key: &str, default: f32| params.get(key).and_then(|v| v.as_f64()).map_or(default, |v| v as f32);
    let dev = st.device_id.as_str();
    sink_outputs.set_pin(&dev, &"pad_click_mode", Signal::Float(mode as f32));
    sink_outputs.set_pin(&dev, &"pad_click_gain", Signal::Float(num("pad_click_gain_db", 0.0)));
    sink_outputs.set_pin(&dev, &"pad_click_hz", Signal::Float(num("pad_click_hz", PAD_CLICK_HZ)));
    sink_outputs.set_pin(&dev, &"pad_click_ms", Signal::Float(num("pad_click_ms", PAD_CLICK_MS)));
    sink_outputs.set_pin(&dev, &"pad_click_soft", Signal::Float(num("pad_click_soft", 0.0)));
}

/// A Steam Controller's grip sensor settings (Calibration window), on pins no
/// layout declares. Absent until the user sets them, which leaves the pad as Steam
/// or its defaults set it; the backend writes only what changed, and ignores the 0
/// zero-on-drop sends.
pub(crate) fn apply_grip_sensor(
    st: &SinkTarget,
    params: &HashMap<String, serde_json::Value>,
    sink_outputs: &mut BusMap,
) {
    for pin in ["grip_touch", "grip_release"] {
        if let Some(v) = params.get(pin).and_then(|v| v.as_f64()) {
            sink_outputs.set_pin(&st.device_id.as_str(), &pin, Signal::Float(v as f32));
        }
    }
}

/// Device-node params holding a pad's default adaptive-trigger effect, left then
/// right: a JSM effect word and its numbers (Calibration window). Absent = no
/// default; the trigger is left exactly as before.
pub const TRIGGER_DEFAULT_MODE_PARAMS: [&str; 2] = ["ltrig_effect_mode", "rtrig_effect_mode"];
pub const TRIGGER_DEFAULT_VALUES_PARAMS: [&str; 2] = ["ltrig_effect_values", "rtrig_effect_values"];

/// Last pass for one physical pad: its default trigger effect, the lowest layer
/// of all — what a trigger does while NOTHING gives it instructions. Any value
/// on the trigger's group, Off included, is an instruction: the game's (a
/// virtual DualSense publishes its trigger pins only once the game has set that
/// trigger's effect — a game without adaptive-trigger support, or any other
/// virtual pad, publishes none), a network peer's, a module's override or
/// injection, or a direct wire.
///
/// A default of `OFF` is meaningful: it is how a cleared default lets go of the
/// effect the pad would otherwise hold (the pad keeps its last report).
pub(crate) fn apply_default_trigger_effects(
    st: &SinkTarget,
    params: &HashMap<String, serde_json::Value>,
    collector_sigs: &BusMap,
    sink_outputs: &mut BusMap,
) {
    for side in 0..2 {
        let Some(mode) = params.get(TRIGGER_DEFAULT_MODE_PARAMS[side]).and_then(|v| v.as_str()) else {
            continue;
        };
        let pins = super::JSM_TRIGGER_PINS[side];
        let dev = st.device_id.as_str();
        // Only a pad with adaptive triggers, and only where no wire owns them.
        if !st.pin_ids.iter().any(|p| p == pins[0]) { continue; }
        let wired = st.pin_ids.iter().enumerate().any(|(i, pid)| {
            pins.contains(&pid.as_str()) && st.multi_sources.get(i).is_some_and(|s| !s.is_empty())
        });
        if wired { continue; }
        let override_key = format!("{FEEDBACK_OVERRIDE}{dev}");
        if pins.iter().any(|p| collector_sigs.has_pin(&override_key, &p)) {
            continue;
        }
        if pins.iter().any(|p| sink_outputs.has_pin(&dev, &p)) {
            continue;
        }
        let values: Vec<u8> = params.get(TRIGGER_DEFAULT_VALUES_PARAMS[side])
            .and_then(|v| v.as_array())
            .map(|a| a.iter().map(|x| x.as_u64().unwrap_or(0).min(255) as u8).collect())
            .unwrap_or_default();
        let Some(effect) = super::jsm_effect_pins(mode, &values) else { continue };
        for (pin, v) in pins.iter().zip(effect) {
            sink_outputs.set_pin(&dev, &pin, Signal::Float(v));
        }
    }
}
