//! Running a JSM Config node: read the bus, run the config, publish the result.
//!
//! The node sits inline on an AutoMap wire and republishes the bus under its own
//! `collector:{uid}` key, the same way the AutoMap Collector and Audio Stream
//! Haptics do. What it publishes depends on the header toggle:
//!
//! * **pass through** (default) — every pin the config never mentions goes
//!   straight on, the mentioned ones are suppressed (and marked consumed for a
//!   downstream Combiner), and the config's own output is written on top;
//! * **strict** — nothing passes; only what the config produces is published,
//!   which is how JSM behaves with the pad hidden from the game.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};

use flexinput_core::{automap, Signal, SignalType};
use glam::Vec2;

use crate::eval::remapper_pass_through_and_suppress;
use crate::graph::NodeSnap;
use crate::state::NodeState;

use super::aim::{Aim, Gyro};
use super::analog::{Analog, Pad};
use super::bind::Runtime;
use super::names::{Btn, BtnSource, StickId};
use super::parse::{compile, Compiled};

/// The bus carries a rotation rate as a fraction of this many degrees per second
/// (`flexinput_devices::gyro::GYRO_REF_DPS`, as the RWS module also mirrors it).
const GYRO_REF_DPS: f32 = 2000.0;

/// Set by the UI while a JSM editor has keyboard focus. A binding under test
/// would otherwise type into the very editor it is being written in, so while
/// one is focused the config's keyboard and mouse output pauses (the pad half of
/// its output keeps working, which is what you want to feel while editing).
static EDITOR_FOCUS: AtomicBool = AtomicBool::new(false);

pub fn set_jsm_editor_focus(focused: bool) {
    EDITOR_FOCUS.store(focused, Ordering::Relaxed);
}

pub fn jsm_editor_focus() -> bool {
    EDITOR_FOCUS.load(Ordering::Relaxed)
}

/// Pins that would land in a text editor: keys, mouse buttons, the wheel.
fn types_into_the_editor(pin: &str) -> bool {
    pin.starts_with("key_") || pin.starts_with("mouse_") || pin.starts_with("scroll_")
}

/// A node's compiled config and the button state running it.
pub struct JsmState {
    /// Hash of the text this was compiled from, so it recompiles only on edits.
    gen: u64,
    cfg: Compiled,
    rt: Runtime,
    /// Triggers and sticks turned into JSM's buttons.
    analog: Analog,
    /// The gyro and the aiming sticks, turned into mouse movement.
    aim: Aim,
}

/// Evaluate one JSM Config node.
pub(crate) fn jsm_publish(
    snap: &NodeSnap,
    uid: usize,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
    state: &mut HashMap<usize, NodeState>,
    dt: f32,
) -> Vec<Option<Signal>> {
    let key = format!("collector:{uid}");
    let strict = snap.params.get("jsm_strict").and_then(|v| v.as_bool()).unwrap_or(false);
    let text = active_tab_text(snap);

    // Snapshot the upstream bus once, so publishing can't alias the read side.
    let dev_id = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("");
    let collector_id = snap.params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("");
    let mut upstream: HashMap<String, Signal> = HashMap::new();
    for ap in automap::ALL_PINS {
        let sig = (!collector_id.is_empty())
            .then(|| collector_sigs.get(&(collector_id.to_string(), ap.id.to_string())).copied())
            .flatten()
            .or_else(|| (!dev_id.is_empty())
                .then(|| dev_sigs.get(&(dev_id.to_string(), ap.id.to_string())).copied())
                .flatten());
        if let Some(s) = sig { upstream.insert(ap.id.to_string(), s); }
    }

    // Compile on the first tick and after every edit; a fresh config starts from
    // a clean slate rather than inheriting half-finished presses.
    let ns = state.entry(uid).or_default();
    let gen = text_gen(&text);
    let st = ns.jsm.get_or_insert_with(|| Box::new(JsmState {
        gen,
        cfg: compile(&text),
        rt: Runtime::default(),
        analog: Analog::default(),
        aim: Aim::default(),
    }));
    if st.gen != gen {
        st.gen = gen;
        st.cfg = compile(&text);
        st.rt = Runtime::default();
        st.analog = Analog::default();
        st.aim = Aim::default();
    }

    // Triggers and sticks first: they turn into buttons the bindings can use.
    st.analog.tick(&st.cfg.settings, dt, &read_pad(&upstream));
    let analog = &st.analog;
    let held = |btn: Btn| -> bool { button_down(btn, &upstream, analog) };
    let outputs = st.rt.tick(&st.cfg, dt, &held);
    let driven: Vec<String> = outputs.pins.iter().cloned().collect();
    let gyro_actions = outputs.gyro.clone();

    // Then aiming: the gyro and whichever sticks are pointed at the mouse.
    let mouse = {
        let f = |pin: &str| upstream.get(pin).map(|s| s.as_float()).unwrap_or(0.0) * GYRO_REF_DPS;
        let gyro = Gyro { roll: f("gyro_x"), pitch: f("gyro_y"), yaw: f("gyro_z") };
        let analog = &st.analog;
        let held = |btn: Btn| -> bool { button_down(btn, &upstream, analog) };
        st.aim.tick(&st.cfg.aim, dt, gyro, &st.analog, &gyro_actions, &held)
    };

    // What the config takes over from the pad.
    let (claimed_digital, claimed_analog) = claimed_pins(&st.cfg);

    if strict {
        for ap in automap::ALL_PINS {
            if let Some(off) = off_value(ap.signal_type) {
                collector_sigs.insert((key.clone(), ap.id.to_string()), off);
            }
        }
    } else {
        remapper_pass_through_and_suppress(&key, &upstream, &claimed_digital, &claimed_analog, collector_sigs);
    }
    let typing = jsm_editor_focus();
    for pin in driven {
        if typing && types_into_the_editor(&pin) { continue; }
        let on = on_value(&pin);
        collector_sigs.insert((key.clone(), pin), on);
    }
    // Aiming, as one displacement for this tick. `mouse_move` is applied as it
    // stands rather than integrated, which is what JSM computes; the axis pins
    // are left alone so a sink wired to both doesn't move twice.
    if mouse != Vec2::ZERO && !typing {
        collector_sigs.insert((key.clone(), "mouse_move".to_string()), Signal::Vec2(mouse));
    }

    // output[0] is the AutoMap pass-through, which carries no scalar.
    vec![None; snap.n_outputs.max(1)]
}

/// The text of the tab the module is applying.
fn active_tab_text(snap: &NodeSnap) -> String {
    let tabs = snap.params.get("jsm_tabs").and_then(|v| v.as_array());
    let active = snap.params.get("jsm_active_tab").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    tabs.and_then(|t| t.get(active).or_else(|| t.first()))
        .and_then(|t| t.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn text_gen(text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// What the analog side needs off the bus this tick.
fn read_pad(upstream: &HashMap<String, Signal>) -> Pad {
    let analog = |pin: &str| upstream.get(pin).map(|s| s.as_float());
    let b = |pin: &str| upstream.get(pin).map(|s| s.as_bool()).unwrap_or(false);
    let stick = |name: &str| -> (f32, f32) {
        if let Some(Signal::Vec2(v)) = upstream.get(name) {
            return (v.x, v.y);
        }
        (
            upstream.get(&format!("{name}_x")).map(|s| s.as_float()).unwrap_or(0.0),
            upstream.get(&format!("{name}_y")).map(|s| s.as_float()).unwrap_or(0.0),
        )
    };
    Pad {
        triggers: [analog("left_trigger"), analog("right_trigger")],
        trigger_digital: [b("btn_lt_dig"), b("btn_rt_dig")],
        sticks: [stick("left_stick"), stick("right_stick")],
    }
}

/// Is this JSM button pressed right now? Triggers and sticks come from the
/// analog side; the buttons whose source needs work still to come (the touchpad,
/// the motion sensors) read as off — their lines compile as pending, so nothing
/// claims to work that doesn't.
fn button_down(btn: Btn, upstream: &HashMap<String, Signal>, analog: &Analog) -> bool {
    let b = |pin: &str| upstream.get(pin).map(|s| s.as_bool()).unwrap_or(false);
    match btn.source() {
        BtnSource::Pin(pin) => b(pin),
        BtnSource::Trigger { .. }
        | BtnSource::TriggerFull { .. }
        | BtnSource::Stick { .. } => analog.down(btn),
        BtnSource::Motion { .. }
        | BtnSource::Lean { .. }
        | BtnSource::Touch
        | BtnSource::TouchZone { .. } => false,
    }
}

/// The pins the config takes over, split the way the suppression pass wants
/// them: buttons on one side, analog axes and triggers on the other.
fn claimed_pins(cfg: &Compiled) -> (HashSet<String>, HashSet<String>) {
    let mut digital = HashSet::new();
    let mut analog = HashSet::new();
    let stick_pins = |stick: StickId| {
        let name = match stick { StickId::Left => "left_stick", StickId::Right => "right_stick" };
        [name.to_string(), format!("{name}_x"), format!("{name}_y")]
    };
    // A stick pointed at the mouse is the config's whether or not a binding ever
    // names one of its directions.
    for (stick, s) in [(StickId::Left, cfg.settings.left), (StickId::Right, cfg.settings.right)] {
        if s.mode.aims() {
            analog.extend(stick_pins(stick));
        }
    }
    for &btn in &cfg.mentioned {
        match btn.source() {
            BtnSource::Pin(pin) => { digital.insert(pin.to_string()); }
            BtnSource::Trigger { analog: a, digital: d } => {
                analog.insert(a.to_string());
                digital.insert(d.to_string());
            }
            // Same rule as the sticks: a full pull the mode never fires (JSM's
            // default NO_FULL) claims nothing, so the trigger isn't silenced for
            // a binding that can't run.
            BtnSource::TriggerFull { analog: a } => {
                let mode = if btn == Btn::Zlf { cfg.settings.zl } else { cfg.settings.zr };
                if mode.has_full() { analog.insert(a.to_string()); }
            }
            // A stick the config reads — as directions, or to aim with — is the
            // config's; one left in a mode we don't run yet passes through
            // untouched rather than going quiet for nothing.
            BtnSource::Stick { stick, .. } => {
                let cfg = match stick {
                    StickId::Left => cfg.settings.left,
                    StickId::Right => cfg.settings.right,
                };
                if cfg.mode.runs_here() {
                    analog.extend(stick_pins(stick));
                }
            }
            // Sources later phases read: nothing to claim while they can't fire.
            BtnSource::Motion { .. }
            | BtnSource::Lean { .. }
            | BtnSource::Touch
            | BtnSource::TouchZone { .. } => {}
        }
    }
    // A config that aims with the gyro owns it, so it can't also drive a Gyro
    // 3DOF node downstream. One left at JSM's default sensitivity of zero aims
    // with nothing, and passes the gyro on untouched.
    if cfg.aim.min_sens != (0.0, 0.0) || cfg.aim.max_sens != (0.0, 0.0) {
        for pin in ["gyro_x", "gyro_y", "gyro_z"] {
            analog.insert(pin.to_string());
        }
    }
    (digital, analog)
}

fn off_value(ty: SignalType) -> Option<Signal> {
    Some(match ty {
        SignalType::Bool => Signal::Bool(false),
        SignalType::Float => Signal::Float(0.0),
        SignalType::Vec2 => Signal::Vec2(Vec2::ZERO),
        SignalType::Int => Signal::Int(0),
        _ => return None,
    })
}

/// A driven pin's "on" value: a button is true, an analog destination (a virtual
/// pad's trigger, say) goes to full.
fn on_value(pin: &str) -> Signal {
    match automap::ALL_PINS.iter().find(|ap| ap.id == pin).map(|ap| ap.signal_type) {
        Some(SignalType::Float) => Signal::Float(1.0),
        Some(SignalType::Int) => Signal::Int(1),
        _ => Signal::Bool(true),
    }
}
