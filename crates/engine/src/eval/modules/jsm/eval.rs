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
use super::names::{Btn, BtnSource, Out, StickId};
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
    let strict = snap
        .params
        .get("jsm_strict")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let text = active_tab_text(snap);

    // Snapshot the upstream bus once, so publishing can't alias the read side.
    let dev_id = snap
        .params
        .get("_automap_device_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let collector_id = snap
        .params
        .get("_automap_collector_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let mut upstream: HashMap<String, Signal> = HashMap::new();
    for ap in automap::ALL_PINS {
        let sig = (!collector_id.is_empty())
            .then(|| {
                collector_sigs
                    .get(&(collector_id.to_string(), ap.id.to_string()))
                    .copied()
            })
            .flatten()
            .or_else(|| {
                (!dev_id.is_empty())
                    .then(|| {
                        dev_sigs
                            .get(&(dev_id.to_string(), ap.id.to_string()))
                            .copied()
                    })
                    .flatten()
            });
        if let Some(s) = sig {
            upstream.insert(ap.id.to_string(), s);
        }
    }

    // Compile on the first tick and after every edit; a fresh config starts from
    // a clean slate rather than inheriting half-finished presses.
    let ns = state.entry(uid).or_default();
    let gen = text_gen(&text);
    let st = ns.jsm.get_or_insert_with(|| {
        Box::new(JsmState {
            gen,
            cfg: compile(&text),
            rt: Runtime::default(),
            analog: Analog::default(),
            aim: Aim::default(),
        })
    });
    if st.gen != gen {
        st.gen = gen;
        st.cfg = compile(&text);
        st.rt = Runtime::default();
        st.analog = Analog::default();
        st.aim = Aim::default();
    }

    // What the settings are this tick, once the held chords have had their say.
    // The chord stack is the one the press machinery left at the end of the last
    // tick, so a modeshift takes hold a tick after its button — imperceptible at
    // any tick rate we run, and it keeps the order here simple: settings, then
    // buttons, then aiming.
    let mut res = super::parse::resolve(&st.cfg, st.rt.chords());
    for side in 0..2 {
        let stick = if side == 0 { &mut res.settings.left } else { &mut res.settings.right };
        if res.stick_mode_chorded[side] {
            // A chord is supplying the mode: remember it, so letting go leaves
            // the stick waiting for centre rather than acting on a full push.
            st.analog.set_stick_recentring(side, true);
        } else if st.analog.stick_recentring(side) {
            stick.mode = super::analog::StickMode::Inert;
        }
        // A flick that hasn't finished paying out keeps the stick in flick mode
        // until it has, whatever the mode underneath now says.
        if st.aim.flick_unfinished(side)
            && !matches!(stick.mode, super::analog::StickMode::Flick | super::analog::StickMode::FlickOnly)
        {
            stick.mode = super::analog::StickMode::FlickOnly;
        }
    }

    // Triggers and sticks first: they turn into buttons the bindings can use.
    st.analog.tick(&res.settings, dt, &read_pad(&upstream));
    let analog = &st.analog;
    let held = |btn: Btn| -> bool { button_down(btn, &upstream, analog) };
    let outputs = st.rt.tick(&st.cfg, &res.timings, dt, &held);
    let driven: Vec<String> = outputs.pins.iter().cloned().collect();
    let gyro_actions = outputs.gyro.clone();

    // Then aiming: the gyro and whichever sticks are pointed at the mouse.
    let mouse = {
        let f = |pin: &str| upstream.get(pin).map(|s| s.as_float()).unwrap_or(0.0) * GYRO_REF_DPS;
        let gyro = Gyro {
            roll: f("gyro_x"),
            pitch: f("gyro_y"),
            yaw: f("gyro_z"),
        };
        let analog = &st.analog;
        let held = |btn: Btn| -> bool { button_down(btn, &upstream, analog) };
        st.aim
            .tick(&res.aim, dt, gyro, &st.analog, &gyro_actions, &held)
    };

    // What the config takes over from the pad.
    let (claimed_digital, claimed_analog) = claimed_pins(&st.cfg, &res);

    if strict {
        for ap in automap::ALL_PINS {
            if let Some(off) = off_value(ap.signal_type) {
                collector_sigs.insert((key.clone(), ap.id.to_string()), off);
            }
        }
    } else {
        remapper_pass_through_and_suppress(
            &key,
            &upstream,
            &claimed_digital,
            &claimed_analog,
            collector_sigs,
        );
    }
    let typing = jsm_editor_focus();
    let held: HashSet<String> = driven.iter().cloned().collect();

    // EVERY pin the config can ever drive gets a definitive value every tick —
    // on while it is held, off the rest of the time. A sink latches what it was
    // last told, and most of these pins (`key_e` and the like) are outside
    // ALL_PINS, so nothing else on the bus ever carries their off value: saying
    // it once on the tick of release is not enough, because a tick where nobody
    // downstream looks leaves the key down for good. This is the rule the
    // Remapper follows for its own output pins.
    let drivable = claimed_outputs(&st.cfg);
    for pin in &drivable {
        let pin = pin.clone();
        // A pin the pad itself drives is the pass-through's to answer for: the
        // config saying "off" here would fight it every tick it isn't pressed.
        if upstream.contains_key(&pin) && !held.contains(&pin) {
            continue;
        }
        // While an editor has focus the keys and mouse pause, and pause means
        // released — a binding under test shouldn't freeze mid-press.
        let on = held.contains(&pin) && !(typing && types_into_the_editor(&pin));
        let sig = if on { on_value(&pin) } else { pin_off_value(&pin) };
        collector_sigs.insert((key.clone(), pin), sig);
    }

    // ── the D-pad's other two forms ──────────────────────────────────────────
    //
    // A sink sees the D-pad three ways: four direction Bools, `dpad_x`/`dpad_y`,
    // and the `dpad` Vec2 — and every sink derives all four hat bits from the
    // Vec2 when it has one. Driving only the Bool left the zeroed Vec2 to land
    // after it and cancel it, so `S = X_UP` lit up on the bus and never reached
    // the pad. The Remapper hit this too (`remapper.rs`, "D-pad analog
    // synthesis"): whenever the config drives any direction, recompute the axis
    // and Vec2 forms from all four, reading each direction's published value so
    // the ones this config doesn't touch keep their pass-through state.
    const DPAD_DIRS: [(&str, usize, f32); 4] = [
        ("dpad_right", 0, 1.0),
        ("dpad_left", 0, -1.0),
        ("dpad_up", 1, 1.0),
        ("dpad_down", 1, -1.0),
    ];
    if DPAD_DIRS.iter().any(|(d, _, _)| drivable.iter().any(|p| p == d)) {
        let mut xy = [0.0f32; 2];
        for (dir, axis, sign) in DPAD_DIRS {
            // Read back what was published, so a direction this config doesn't
            // drive keeps whatever the pass-through gave it. Every D-pad pin is
            // on the bus by now — passed through, or zeroed by strict mode — so
            // there is nothing to fall back to.
            let on = collector_sigs
                .get(&(key.clone(), dir.to_string()))
                .copied()
                .map(|s| s.as_bool())
                .unwrap_or(false);
            if on {
                xy[axis] += sign;
            }
        }
        // Opposite directions cancel — one axis can't hold both.
        let (x, y) = (xy[0].clamp(-1.0, 1.0), xy[1].clamp(-1.0, 1.0));
        collector_sigs.insert((key.clone(), "dpad_x".to_string()), Signal::Float(x));
        collector_sigs.insert((key.clone(), "dpad_y".to_string()), Signal::Float(y));
        collector_sigs.insert((key.clone(), "dpad".to_string()), Signal::Vec2(Vec2::new(x, y)));
    }

    // Aiming, as one displacement for this tick. `mouse_move` is applied as it
    // stands rather than integrated, which is what JSM computes; the axis pins
    // are left alone so a sink wired to both doesn't move twice. It is published
    // every tick the config aims, zero included — a latched displacement would
    // otherwise keep nudging the cursor for ever.
    if aims_anything(&st.cfg, &res) {
        let m = if typing { Vec2::ZERO } else { mouse };
        collector_sigs.insert((key.clone(), "mouse_move".to_string()), Signal::Vec2(m));
    }

    // output[0] is the AutoMap pass-through, which carries no scalar.
    vec![None; snap.n_outputs.max(1)]
}

/// The text of the tab the module is applying.
fn active_tab_text(snap: &NodeSnap) -> String {
    let tabs = snap.params.get("jsm_tabs").and_then(|v| v.as_array());
    let active = snap
        .params
        .get("jsm_active_tab")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
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
            upstream
                .get(&format!("{name}_x"))
                .map(|s| s.as_float())
                .unwrap_or(0.0),
            upstream
                .get(&format!("{name}_y"))
                .map(|s| s.as_float())
                .unwrap_or(0.0),
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
        BtnSource::Trigger { .. } | BtnSource::TriggerFull { .. } | BtnSource::Stick { .. } => {
            analog.down(btn)
        }
        BtnSource::Motion { .. }
        | BtnSource::Lean { .. }
        | BtnSource::Touch
        | BtnSource::TouchZone { .. } => false,
    }
}

/// The pins the config takes over, split the way the suppression pass wants
/// them: buttons on one side, analog axes and triggers on the other.
fn claimed_pins(cfg: &Compiled, res: &super::parse::Resolved) -> (HashSet<String>, HashSet<String>) {
    let mut digital = HashSet::new();
    let mut analog = HashSet::new();
    let stick_pins = |stick: StickId| {
        let name = match stick {
            StickId::Left => "left_stick",
            StickId::Right => "right_stick",
        };
        [name.to_string(), format!("{name}_x"), format!("{name}_y")]
    };
    // A stick pointed at the mouse is the config's whether or not a binding ever
    // names one of its directions.
    for (stick, s) in [
        (StickId::Left, cfg.settings.left),
        (StickId::Right, cfg.settings.right),
    ] {
        if s.mode.aims() {
            analog.extend(stick_pins(stick));
        }
    }
    for &btn in &cfg.mentioned {
        match btn.source() {
            BtnSource::Pin(pin) => {
                digital.insert(pin.to_string());
            }
            BtnSource::Trigger {
                analog: a,
                digital: d,
            } => {
                analog.insert(a.to_string());
                digital.insert(d.to_string());
            }
            // Same rule as the sticks: a full pull the mode never fires (JSM's
            // default NO_FULL) claims nothing, so the trigger isn't silenced for
            // a binding that can't run.
            BtnSource::TriggerFull { analog: a } => {
                let mode = if btn == Btn::Zlf {
                    cfg.settings.zl
                } else {
                    cfg.settings.zr
                };
                if mode.has_full() {
                    analog.insert(a.to_string());
                }
            }
            // A stick the config reads — as directions, or to aim with — is the
            // config's; one left in a mode we don't run yet passes through
            // untouched rather than going quiet for nothing.
            BtnSource::Stick { stick, .. } => {
                let cfg = match stick {
                    StickId::Left => res.settings.left,
                    StickId::Right => res.settings.right,
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
    if res.aim.min_sens != (0.0, 0.0) || res.aim.max_sens != (0.0, 0.0) {
        for pin in ["gyro_x", "gyro_y", "gyro_z"] {
            analog.insert(pin.to_string());
        }
    }
    (digital, analog)
}

/// Every pin the compiled config could drive, whatever the event that would do
/// it. These are the pins the module answers for each tick.
fn claimed_outputs(cfg: &Compiled) -> Vec<String> {
    let mut pins: HashSet<String> = HashSet::new();
    for b in &cfg.bindings {
        for step in &b.steps {
            match &step.out {
                Out::Pin(p) | Out::Pulse(p) => { pins.insert(p.clone()); }
                _ => {}
            }
        }
    }
    pins.into_iter().collect()
}

/// Is any part of this config aiming the mouse?
fn aims_anything(cfg: &Compiled, res: &super::parse::Resolved) -> bool {
    let _ = cfg;
    res.aim.min_sens != (0.0, 0.0)
        || res.aim.max_sens != (0.0, 0.0)
        || res.settings.left.mode.aims()
        || res.settings.right.mode.aims()
}

/// A pin's released value, in the type it is declared with — a virtual pad's
/// trigger goes to 0.0, not to `false`.
fn pin_off_value(pin: &str) -> Signal {
    match automap::ALL_PINS.iter().find(|ap| ap.id == pin).map(|ap| ap.signal_type) {
        Some(SignalType::Float) => Signal::Float(0.0),
        Some(SignalType::Int) => Signal::Int(0),
        Some(SignalType::Vec2) => Signal::Vec2(Vec2::ZERO),
        // Keys and mouse buttons aren't in ALL_PINS at all; they are booleans.
        _ => Signal::Bool(false),
    }
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
    match automap::ALL_PINS
        .iter()
        .find(|ap| ap.id == pin)
        .map(|ap| ap.signal_type)
    {
        Some(SignalType::Float) => Signal::Float(1.0),
        Some(SignalType::Int) => Signal::Int(1),
        _ => Signal::Bool(true),
    }
}
