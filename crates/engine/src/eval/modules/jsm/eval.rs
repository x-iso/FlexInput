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
use super::pad::Pad as PadOut;
use super::names::{Btn, BtnSource, Out, StickId};
use super::parse::Compiled;

/// The bus carries a rotation rate as a fraction of this many degrees per second
/// (`flexinput_devices::gyro::GYRO_REF_DPS`, as the RWS module also mirrors it).
const GYRO_REF_DPS: f32 = 2000.0;
/// …and a force as a fraction of this many g (`flexinput_devices::gyro::ACCEL_REF_G`).
const ACCEL_REF_G: f32 = 8.0;

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
    /// The sticks and rates pointed at a virtual pad instead.
    pad: PadOut,
    /// Which way is down, and the motion stick built from it.
    motion: super::motion::Motion,
    /// The touchpad: its grid, its two relative sticks, its mouse mode.
    touch: super::touch::Touch,
    /// The tab actually running. A binding can switch it (JSM loads another config
    /// file), so it is not always the tab the editor has open — `RESET_MAPPINGS`,
    /// or an edit, brings it back to that one.
    layer: String,
    /// The calibration sweep's running total. Deliberately NOT reset when the
    /// config recompiles: a Finish rewrites the config text, which recompiles
    /// this node, and clearing here would throw the measurement away on the very
    /// tick it is being read.
    cal: super::cal::Measure,
    /// The stick being circled, when a 360 sweep is turned with a stick.
    cal_flick: super::cal::Flick,
    /// Every pin any layer of this node has ever claimed.
    ///
    /// A sink latches what it was last told, so a pin has to keep being told
    /// "off" once nothing drives it any more. Switching layers is exactly that
    /// situation: the tab that was holding `key_a` is gone, the new one has never
    /// heard of it, and without this the key would stay down for good — the
    /// original stuck-key bug in a different coat. Keeping the union costs a few
    /// pin writes a tick and makes it impossible.
    ever_claimed: HashSet<String>,
    /// The touchpad's last MIDI position, kept while no finger is down.
    midi_touch: (f32, f32),
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
    let tabs = all_tabs(snap);
    let selected = selected_tab(snap, &tabs);

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
    // MIDI pins are dynamic — never in ALL_PINS — so take whatever the bus
    // carries. They are what a `MIDI_*` button reads, and what the pass-through
    // carries on (or strict mode silences).
    crate::eval::midi_bus::fill_upstream_midi(collector_id, dev_id, collector_sigs, dev_sigs, &mut upstream);

    // How far on each Macro Output port is — what an `@Name` button reads, as
    // an amount 0..1 under the port's own pin. A port can carry anything: a
    // bool is 0 or 1, a pull is itself, a stick or a touch deflection is its
    // length. This tick's value where whatever drives the port has already run,
    // else last tick's carry-over, so the order two JSM nodes evaluate in
    // doesn't matter: one reading the other is at most a tick behind, as the
    // Virtual Menu is. Taken here, before this node's own state is borrowed.
    //
    // Nothing that carries `upstream` on reads these pins: the pass-through
    // walks the pad's pins and the MIDI ones by name.
    {
        use flexinput_core::macros::{SIGS_NS, SIGS_NS_VEC2};
        let amounts = |sigs: &mut dyn Iterator<Item = (&(String, String), &Signal)>| {
            let mut out: HashMap<u32, f32> = HashMap::new();
            for ((ns, pin), sig) in sigs {
                if ns != SIGS_NS && ns != SIGS_NS_VEC2 {
                    continue;
                }
                let Some(id) = super::names::macro_btn_id(pin) else { continue };
                let v = macro_amount(*sig);
                let e = out.entry(id).or_insert(0.0);
                *e = e.max(v);
            }
            out
        };
        let mut now = amounts(&mut collector_sigs.iter());
        if let Some(prev) = state.get(&crate::eval::MACRO_CARRY_UID).map(|s| &s.macro_prev) {
            for (id, v) in amounts(&mut prev.iter()) {
                now.entry(id).or_insert(v);
            }
        }
        for (id, v) in now {
            upstream.insert(super::names::macro_btn_pin(id), Signal::Float(v));
        }
    }

    // Compile on the first tick and after every edit; a fresh config starts from
    // a clean slate rather than inheriting half-finished presses. The generation
    // covers every tab's text, not just the live one, so editing a layer a binding
    // switches to recompiles as readily as editing the one on screen.
    let ns = state.entry(uid).or_default();
    let gen = text_gen_of(&tabs, &selected);
    let text_for = |layer: &str| -> String {
        tabs.iter()
            .find(|(n, _)| n == layer)
            .map(|(_, t)| t.clone())
            .unwrap_or_default()
    };
    let text = text_for(&selected);
    // This patch's Macro Output ports and Virtual Menu entries, as the graph
    // builder stamped them, so a line can bind one by name with `@`.
    let ports = macro_ports(snap);
    let st = ns.jsm.get_or_insert_with(|| {
        Box::new(JsmState {
            gen,
            layer: selected.clone(),
            cal: super::cal::Measure::default(),
            cal_flick: super::cal::Flick::default(),
            ever_claimed: HashSet::new(),
            midi_touch: (0.0, 0.0),
            cfg: super::parse::compile_full(&text, &tabs, &ports),
            rt: Runtime::default(),
            analog: Analog::default(),
            aim: Aim::default(),
            pad: PadOut::default(),
            motion: super::motion::Motion::default(),
            touch: super::touch::Touch::default(),
        })
    });
    if st.gen != gen {
        // An edit anywhere puts the module back on the tab the editor has open: a
        // half-edited layer chain is not something to keep running.
        st.gen = gen;
        st.layer = selected.clone();
        st.cfg = super::parse::compile_full(&text, &tabs, &ports);
        st.rt = Runtime::default();
        st.analog = Analog::default();
        st.aim = Aim::default();
        st.pad = PadOut::default();
        st.motion = super::motion::Motion::default();
        st.touch = super::touch::Touch::default();
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

    // Which way is down, and where the fingers are. Both come before the sticks,
    // because each produces one: the motion stick from gravity, a touch stick per
    // finger from how far it has been dragged.
    let gravity = st.motion.tick(read_accel(&upstream), dt);
    let touch_out = st.touch.tick(&res.touch, touch_fingers(&upstream), res.settings.touch);
    let motion_stick = super::motion::motion_stick(gravity, res.settings.motion);

    // Triggers and sticks: they turn into buttons the bindings can use.
    st.analog
        .tick(&res.settings, dt, &read_pad(&upstream, motion_stick, &touch_out));
    // `SET_MOTION_STICK_NEUTRAL`: once as the config starts if it is on a line of
    // its own, and whenever a binding fires it (below, once the bindings have run).
    if st.cfg.neutral_at_load && !st.motion.has_neutral() && gravity.known {
        st.motion.set_neutral(gravity);
    }

    let (lean_l, lean_r) = super::motion::lean(gravity, &res.motion, res.settings.orientation);
    let ext = Extra {
        lean: (lean_l, lean_r),
        cells: touch_out.cells,
        midi: res.midi,
        analog_fed: st.cfg.analog_fed.clone(),
    };
    let analog = &st.analog;
    let held = |btn: Btn| -> bool { button_down(btn, &upstream, analog, &ext) };
    // A trigger handed straight to the virtual pad can still chord, but its own
    // bindings stop running — JSM's rule, and what the editor says on the line.
    let (zl_pad, zr_pad) = (
        res.settings.zl.pad_side().is_some(),
        res.settings.zr.pad_side().is_some(),
    );
    let chord_only = |b: Btn| match b {
        Btn::Zl | Btn::Zlf => zl_pad,
        Btn::Zr | Btn::Zrf => zr_pad,
        _ => false,
    };
    // Cloned so the runtime can be asked, below, which button holds which pin.
    let outputs = st.rt.tick(&st.cfg, &res.timings, dt, &held, &chord_only).clone();
    let held_by = st.rt.held_by();
    let driven: Vec<String> = outputs.pins.iter().cloned().collect();
    let gyro_actions = outputs.gyro.clone();
    let recentre = outputs
        .commands
        .iter()
        .any(|c| c.trim().eq_ignore_ascii_case("SET_MOTION_STICK_NEUTRAL"));
    let switch_to = outputs.layer.clone();
    let reset = outputs.reset;
    if recentre {
        st.motion.set_neutral(gravity);
    }

    // Then aiming: the gyro and whichever sticks are pointed at the mouse. What
    // is pointed at a virtual pad's stick instead comes back as a camera rate for
    // the pad side to convert.
    let f = |pin: &str| upstream.get(pin).map(|s| s.as_float()).unwrap_or(0.0) * GYRO_REF_DPS;
    let gyro = Gyro {
        roll: f("gyro_x"),
        pitch: f("gyro_y"),
        yaw: f("gyro_z"),
    };
    let aimed: super::aim::Aimed = {
        let analog = &st.analog;
        let held = |btn: Btn| -> bool { button_down(btn, &upstream, analog, &ext) };
        st.aim
            .tick(&res.aim, &res.pad, &res.motion, &res.cc, gravity, dt, gyro, &st.analog, &gyro_actions, &held)
    };
    let mouse = aimed.mouse;

    // Then the virtual pad: sticks pointed at one, and the rates just computed.
    let pad_out = st.pad.tick(
        &res.pad,
        dt,
        &st.analog,
        res.aim.stick_power,
        aimed.gyro_dps,
        aimed.flick_dps,
        super::motion::steer(
            gravity,
            res.settings.orientation,
            res.settings.motion.inner_dz * 180.0,
            (1.0 - res.settings.motion.outer_dz) * 180.0,
            res.aim.stick_power,
        ),
    );

    // What the config takes over from the pad.
    let (claimed_digital, claimed_analog) = claimed_pins(&st.cfg, &res);

    if strict {
        for ap in automap::ALL_PINS {
            if let Some(off) = off_value(ap.signal_type) {
                collector_sigs.insert((key.clone(), ap.id.to_string()), off);
            }
        }
        // MIDI too. Without this a MIDI pin was simply absent from this key and a
        // reader downstream fell back to the raw device — so raw MIDI slipped
        // past a strict config.
        for pin in upstream.keys().filter(|p| flexinput_core::midi::is_midi_pin(p)) {
            if let Some(rest) = crate::eval::midi_bus::midi_rest(pin) {
                collector_sigs.insert((key.clone(), pin.clone()), rest);
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
    // Everything the current config can drive, plus everything any layer before it
    // could — see `ever_claimed`.
    let drivable = claimed_outputs(&st.cfg);
    st.ever_claimed.extend(drivable.iter().cloned());
    let drivable: Vec<String> = st.ever_claimed.iter().cloned().collect();
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
        // A Macro Output port or Virtual Menu entry is NOT a bus pin: it routes
        // into the reserved macro namespaces, where only asserted values are
        // written and several writers merge by magnitude. Publishing "off" for
        // one every tick would be this module claiming the port whether or not
        // anything here binds it — which is the opposite of what the other
        // mapping modules do, and would fight them.
        if crate::eval::activation::is_macro_style_target(&pin) {
            if on {
                // A Macro Output port carries an amount, so one held by
                // something with a reading — a trigger's pull, a MIDI knob,
                // another port — passes that on (the strongest holder's), and
                // a node reading it gets the pull rather than a switch. Held by
                // a plain button, or by nobody (a toggle, a tap), it is full on.
                // A Virtual Menu entry is a press either way.
                let amount = flexinput_core::macros::parse_macro_pin(&pin)
                    .and_then(|_| held_by.get(&pin))
                    .into_iter()
                    .flatten()
                    .filter_map(|(b, _)| analog_reading(*b, &upstream, &res.midi))
                    .map(|(r, _)| r.abs().clamp(0.0, 1.0))
                    .reduce(f32::max);
                let sig = match amount {
                    Some(a) => Signal::Float(a),
                    None => on_value(&pin),
                };
                crate::eval::activation::merge_macro_scalar(collector_sigs, &pin, sig);
            }
            continue;
        }
        // A MIDI message this config plays: a note is a gate with its velocity
        // riding a twin pin, a controller goes to its top, and all of it is
        // marked PRODUCED so a MIDI Out sends it with its Thru toggle off — the
        // setting that stops a shared In/Out port feeding itself.
        if let Some(mp) = flexinput_core::midi::parse_pin(&pin) {
            // Held by a trigger or a MIDI input, a value takes that input's
            // reading (`ZL = MIDI_CC7` — the pull IS the value) and a note its
            // velocity from it; otherwise full, and `MIDI_VELOCITY`.
            let holders: &[(Btn, usize)] = held_by.get(&pin).map(Vec::as_slice).unwrap_or(&[]);
            let fed = holders.first().and_then(|(b, _)| analog_reading(*b, &upstream, &res.midi));
            let sig = match (on, mp.is_continuous()) {
                (true, true) => Signal::Float(held_value(&st.cfg, &pin, &mp, holders, &upstream, &res.midi)),
                (true, false) => Signal::Bool(true),
                (false, _) => mp.rest_value(),
            };
            if on {
                if let flexinput_core::midi::MidiPin::Note { ch, note } = mp {
                    let vel = flexinput_core::midi::MidiPin::Velocity { ch, note }.to_id();
                    // Never 0: a note-on at velocity 0 is a note-off.
                    let v = match fed {
                        Some((r, _)) => r.abs().clamp(1.0 / 127.0, 1.0),
                        None => res.midi.velocity as f32 / 127.0,
                    };
                    collector_sigs.insert((key.clone(), vel.clone()), Signal::Float(v));
                    crate::eval::midi_bus::mark_produced(&key, &vel, collector_sigs);
                }
            }
            // Played by a value (a trigger, a knob, another note), the value
            // keeps going after the strike as the note's aftertouch; otherwise
            // it sits at 0. Written every tick like the gate — the MIDI Out
            // encoder sends a change, not a repeat.
            if let flexinput_core::midi::MidiPin::Note { ch, note } = mp {
                let at = flexinput_core::midi::MidiPin::PolyAftertouch { ch, note }.to_id();
                let v = match (on, fed) {
                    (true, Some((r, _))) => r.abs().clamp(0.0, 1.0),
                    _ => 0.0,
                };
                collector_sigs.insert((key.clone(), at.clone()), Signal::Float(v));
                crate::eval::midi_bus::mark_produced(&key, &at, collector_sigs);
            }
            collector_sigs.insert((key.clone(), pin.clone()), sig);
            crate::eval::midi_bus::mark_produced(&key, &pin, collector_sigs);
            continue;
        }
        // A virtual trigger held by a MIDI input takes that input's value
        // (`MIDI_CC7 = X_LT`). Held by the pad's own trigger it stays JSM's
        // full press — `ZL = X_LT` means that in JSM, and `ZL_MODE = X_LT` is
        // the way it passes the pull through.
        let fed = held_by
            .get(&pin)
            .and_then(|h| h.first())
            .filter(|(b, _)| {
                matches!(b, Btn::Midi(_) | Btn::Macro { .. }) && super::parse::takes_a_value(&pin)
            })
            .and_then(|(b, _)| analog_reading(*b, &upstream, &res.midi));
        let sig = match (on, fed) {
            (true, Some((r, _))) => Signal::Float(r.abs().clamp(0.0, 1.0)),
            (true, None) => on_value(&pin),
            (false, _) => pin_off_value(&pin),
        };
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

    // ── the virtual pad's sticks and triggers ────────────────────────────────
    //
    // Same three-forms rule as the D-pad above: a sink reads a stick as a Vec2
    // and as two floats, and prefers the Vec2, so all three have to agree or the
    // one that lands last wins. A stick nothing here drives isn't written at all,
    // so the pad's own passes through.
    for side in 0..2 {
        let name = if side == 0 { "left_stick" } else { "right_stick" };
        if let Some(v) = pad_out.sticks[side] {
            collector_sigs.insert((key.clone(), name.to_string()), Signal::Vec2(v));
            collector_sigs.insert((key.clone(), format!("{name}_x")), Signal::Float(v.x));
            collector_sigs.insert((key.clone(), format!("{name}_y")), Signal::Float(v.y));
        }
        // `ZL_MODE = X_LT`: the trigger's pull goes straight out to the pad.
        if let Some(v) = st.analog.pad_trigger(side) {
            let name = if side == 0 { "left_trigger" } else { "right_trigger" };
            collector_sigs.insert((key.clone(), name.to_string()), Signal::Float(v));
        }
    }

    // Aiming, as one displacement for this tick. `mouse_move` is applied as it
    // stands rather than integrated, which is what JSM computes; the axis pins
    // are left alone so a sink wired to both doesn't move twice. It is published
    // every tick the config aims, zero included — a latched displacement would
    // otherwise keep nudging the cursor for ever.
    if aims_anything(&st.cfg, &res) || res.touch.mode == super::touch::Mode::Mouse {
        let m = if typing { Vec2::ZERO } else { mouse + touch_out.mouse };
        collector_sigs.insert((key.clone(), "mouse_move".to_string()), Signal::Vec2(m));
    }

    // ── continuous sources, as MIDI ──────────────────────────────────────────
    //
    // A stick, the touchpad and the gyro send once their mode says MIDI; the
    // accelerometer whenever it has a target. Written every tick — a value, not
    // a gate — and marked produced so a MIDI Out sends it with Thru off.
    {
        use super::midi::Source as Src;
        let m = res.midi;
        let mut send: Vec<(Src, f32)> = Vec::new();
        for (side, cfg, x, y) in [
            (0, res.settings.left, Src::LeftX, Src::LeftY),
            (1, res.settings.right, Src::RightX, Src::RightY),
            (super::analog::MOTION, res.settings.motion, Src::MotionX, Src::MotionY),
        ] {
            if cfg.mode == super::analog::StickMode::Midi {
                let o = st.analog.stick_out(side);
                send.push((x, o.x));
                send.push((y, o.y));
            }
        }
        if res.touch.mode == super::touch::Mode::Midi {
            // Where the first finger is, 0..1 across and 0..1 up. Lifting it
            // leaves the value where it was, as an XY pad does.
            let f = touch_fingers(&upstream)[0];
            if f.active {
                st.midi_touch = (
                    ((f.x + 1.0) / 2.0).clamp(0.0, 1.0),
                    // The bus counts the pad's y from the top.
                    (1.0 - (f.y + 1.0) / 2.0).clamp(0.0, 1.0),
                );
            }
            send.push((Src::TouchX, st.midi_touch.0));
            send.push((Src::TouchY, st.midi_touch.1));
        }
        if res.pad.gyro_dest == super::pad::Dest::Midi {
            let g = aimed.midi_dps / m.gyro_scale;
            send.extend([(Src::GyroX, g.x), (Src::GyroY, g.y), (Src::GyroZ, g.z)]);
        }
        if let Some(a) = read_accel(&upstream) {
            let a = a * ACCEL_REF_G / m.accel_scale;
            send.extend([(Src::AccelX, a.x), (Src::AccelY, a.y), (Src::AccelZ, a.z)]);
        }
        for (src, reading) in send {
            let Some(target) = m.target(src) else { continue };
            let pin = target.pin(Some(m.channel));
            let v = super::midi::value_for(&pin, target.bend_dir(), reading, src.two_sided());
            let id = pin.to_id();
            collector_sigs.insert((key.clone(), id.clone()), Signal::Float(v));
            crate::eval::midi_bus::mark_produced(&key, &id, collector_sigs);
        }
    }

    // ── a calibration sweep, when one is running ─────────────────────────────
    //
    // Replaces what the config just aimed with, rather than adding to it: the
    // whole point is to drive the game from the raw pad rotation at the current
    // calibration and nothing else. Published last so it lands over the config's
    // own `mouse_move` and stick. See `cal.rs` for why the config's aiming is set
    // aside for the duration.
    let sweep = super::cal::Sweep::read(&snap.params);
    {
        let counts_per_deg = res.aim.real_world_calibration / res.aim.in_game_sens.max(1e-6);
        let (rate, defl) = match sweep {
            Some(sw) => {
                // The 360° can be turned with the pad, with a stick circled round
                // its edge, or both at once (see `Axis::takes_flick`): the rate
                // the stick is circled at adds to the gyro's yaw. The sticks are
                // held still on the bus so neither can turn the camera a second
                // time through the pass-through or whatever the config maps it
                // to.
                let yaw = if sw.axis.takes_flick() {
                    let sticks = [bus_stick(&upstream, "left_stick"), bus_stick(&upstream, "right_stick")];
                    for name in ["left_stick", "right_stick"] {
                        collector_sigs.insert((key.clone(), name.to_string()), Signal::Vec2(Vec2::ZERO));
                        collector_sigs.insert((key.clone(), format!("{name}_x")), Signal::Float(0.0));
                        collector_sigs.insert((key.clone(), format!("{name}_y")), Signal::Float(0.0));
                    }
                    gyro.yaw + st.cal_flick.rate(sticks, dt)
                } else {
                    gyro.yaw
                };
                let (d, rate, defl) = super::cal::drive(
                    sw, yaw, gyro.pitch, counts_per_deg, res.pad.calibration);
                if let Some(m) = d.mouse {
                    collector_sigs.insert((key.clone(), "mouse_move".to_string()), Signal::Vec2(m * dt));
                }
                if let Some(v) = d.stick {
                    // The stick the gyro is pointed at, or the right one when it
                    // is pointed at the mouse and the user is calibrating a stick
                    // anyway (they may be about to switch `GYRO_OUTPUT` over).
                    let side = res.pad.gyro_dest.side().unwrap_or(1);
                    let name = if side == 0 { "left_stick" } else { "right_stick" };
                    collector_sigs.insert((key.clone(), name.to_string()), Signal::Vec2(v));
                    collector_sigs.insert((key.clone(), format!("{name}_x")), Signal::Float(v.x));
                    collector_sigs.insert((key.clone(), format!("{name}_y")), Signal::Float(v.y));
                } else if let Some(side) = res.pad.gyro_dest.side() {
                    // Calibrating the mouse while the gyro drives a stick: hold
                    // that stick still, or it turns the camera too and the
                    // measured rotation answers for both.
                    let name = if side == 0 { "left_stick" } else { "right_stick" };
                    collector_sigs.insert((key.clone(), name.to_string()), Signal::Vec2(Vec2::ZERO));
                    collector_sigs.insert((key.clone(), format!("{name}_x")), Signal::Float(0.0));
                    collector_sigs.insert((key.clone(), format!("{name}_y")), Signal::Float(0.0));
                }
                (rate, defl)
            }
            None => {
                st.cal_flick = super::cal::Flick::default();
                (0.0, 0.0)
            }
        };
        st.cal.tick(sweep, rate, defl, dt);
    }

    // ── what goes back to the pad ────────────────────────────────────────────
    //
    // Rumble, the light bar and the adaptive triggers are the only things this
    // module sends BACKWARDS. They go on the override layer, which drops the game's
    // own feedback for whichever group they touch — the same thing JSM does while
    // it owns the pad. `RUMBLE = ON` (the default) leaves the rumble group alone so
    // the game keeps it; a binding rumbling, or `RUMBLE = OFF`, takes it over.
    //
    // The destination is the physical pad upstream of this node, stamped by the
    // graph builder the way Audio Stream Haptics' is. With nothing resolved there
    // is nowhere to send it, and the editor says as much on the line.
    let dest_dev = snap
        .params
        .get("_jsm_dest_dev")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !dest_dev.is_empty() {
        let fb_key = format!("{}{dest_dev}", crate::eval::feedback::FEEDBACK_OVERRIDE);
        for (pin, v) in super::feedback::pins(&res.fb, outputs.rumble) {
            collector_sigs.insert((fb_key.clone(), pin.to_string()), Signal::Float(v));
        }
    }

    // ── action layers ────────────────────────────────────────────────────────
    //
    // Last, after everything this tick has been published, so the layer that was
    // running gets to finish its tick — including releasing what it drove. Switching
    // first would leave whatever it was holding latched on the bus, which is the
    // stuck-key bug in a different coat.
    //
    // A switch recompiles from the new tab and starts it clean: JSM loading a config
    // replaces the mappings outright, so a press held across the switch does not
    // carry over. `RESET_MAPPINGS` goes back to the tab the editor has open.
    let want = if reset {
        Some(selected.clone())
    } else {
        switch_to.filter(|t| *t != st.layer)
    };
    if let Some(layer) = want {
        if reset || tabs.iter().any(|(n, _)| *n == layer) {
            st.layer = layer.clone();
            st.cfg = super::parse::compile_full(&text_for(&layer), &tabs, &ports);
            st.rt = Runtime::default();
            st.analog = Analog::default();
            st.aim = Aim::default();
            st.pad = PadOut::default();
            st.touch = super::touch::Touch::default();
            // The gravity estimate and its neutral are the PAD's state, not the
            // config's — how the pad is being held doesn't change because a layer
            // did, and re-settling the filter would blank the motion stick for half
            // a second on every switch.
        }
    }

    // output[0] is the AutoMap pass-through, which carries no scalar. The two
    // after it are not pins at all — they are how the calibration widget reads
    // the sweep this node is integrating, the same trailing-output channel RWS
    // publishes its own measurement on. See `CAL_DEG_OUT` / `CAL_PEAK_OUT`.
    let mut out = vec![None; snap.n_outputs.max(1)];
    out.push(Some(Signal::Float(st.cal.deg)));
    out.push(Some(Signal::Float(st.cal.peak)));
    out
}

/// Index of the measured-rotation trailing output in a JSM node's `last_out`,
/// and of the peak stick deflection beside it. The module declares exactly one
/// output pin (the bus), so the display-only pair sits at 1 and 2.
pub const CAL_DEG_OUT: usize = 1;
pub const CAL_PEAK_OUT: usize = 2;

/// Every tab the node holds, as (name, text) — what a layer switch can reach.
fn all_tabs(snap: &NodeSnap) -> Vec<(String, String)> {
    snap.params
        .get("jsm_tabs")
        .and_then(|v| v.as_array())
        .map(|tabs| {
            tabs.iter()
                .map(|t| {
                    let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let text = t.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    (name, text)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The tab the editor has open — the one a config runs from until a binding
/// switches layers.
fn selected_tab(snap: &NodeSnap, tabs: &[(String, String)]) -> String {
    let active = snap
        .params
        .get("jsm_active_tab")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    tabs.get(active)
        .or_else(|| tabs.first())
        .map(|(n, _)| n.clone())
        .unwrap_or_default()
}

/// A generation for the whole tab set plus which tab is selected, so an edit to any
/// of them — or a different tab being opened — recompiles.
fn text_gen_of(tabs: &[(String, String)], selected: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    selected.hash(&mut h);
    for (n, t) in tabs {
        n.hash(&mut h);
        t.hash(&mut h);
    }
    h.finish()
}


/// A stick off the bus: its Vec2 when there is one, else its two axis floats.
fn bus_stick(upstream: &HashMap<String, Signal>, name: &str) -> Vec2 {
    if let Some(Signal::Vec2(v)) = upstream.get(name) {
        return *v;
    }
    let axis = |a: &str| upstream.get(&format!("{name}_{a}")).map(|s| s.as_float()).unwrap_or(0.0);
    Vec2::new(axis("x"), axis("y"))
}

/// What the analog side needs off the bus this tick.
fn read_pad(
    upstream: &HashMap<String, Signal>,
    motion: (f32, f32),
    touch: &super::touch::Out,
) -> Pad {
    let analog = |pin: &str| upstream.get(pin).map(|s| s.as_float());
    let b = |pin: &str| upstream.get(pin).map(|s| s.as_bool()).unwrap_or(false);
    let stick = |name: &str| -> (f32, f32) {
        let v = bus_stick(upstream, name);
        (v.x, v.y)
    };
    let fingers = touch_fingers(upstream);
    Pad {
        triggers: [analog("left_trigger"), analog("right_trigger")],
        trigger_digital: [b("btn_lt_dig"), b("btn_rt_dig")],
        sticks: [
            stick("left_stick"),
            stick("right_stick"),
            motion,
            touch.sticks[0],
            touch.sticks[1],
        ],
        touching: fingers[0].active || fingers[1].active,
        touch_click: b("btn_touchpad"),
    }
}

/// The touchpad's two fingers, as the touch side wants them.
fn touch_fingers(upstream: &HashMap<String, Signal>) -> [super::touch::Finger; 2] {
    let f = |n: usize| {
        let g = |suffix: &str| {
            upstream
                .get(&format!("touch{n}_{suffix}"))
                .map(|s| s.as_float())
                .unwrap_or(0.0)
        };
        super::touch::Finger {
            active: upstream
                .get(&format!("touch{n}_active"))
                .map(|s| s.as_bool())
                .unwrap_or(false),
            x: g("x"),
            y: g("y"),
        }
    };
    [f(1), f(2)]
}

/// The accelerometer, if the pad reports one. A pad that reports nothing here has
/// no idea which way is down, which is not the same as being held flat.
fn read_accel(upstream: &HashMap<String, Signal>) -> Option<glam::Vec3> {
    let any = ["accel_x", "accel_y", "accel_z"]
        .iter()
        .any(|p| upstream.contains_key(*p));
    any.then(|| {
        let f = |pin: &str| upstream.get(pin).map(|s| s.as_float()).unwrap_or(0.0);
        glam::Vec3::new(f("accel_x"), f("accel_y"), f("accel_z"))
    })
}

/// The buttons that come from neither a pin nor the analog side.
struct Extra {
    /// `LEAN_LEFT`, `LEAN_RIGHT`.
    lean: (bool, bool),
    /// Which grid cell each finger is in, 1-based.
    cells: [Option<u8>; 2],
    /// The channel a `MIDI_*` button listens on when it doesn't say, and where a
    /// continuous one counts as pressed.
    midi: super::midi::Settings,
    /// MIDI inputs and ports that only feed a value, and so press from any
    /// movement.
    analog_fed: HashSet<Btn>,
}

/// How far on a value written to a Macro Output port is, 0..1: a bool is 0 or
/// 1, a number is its size, a Vec2 its length.
fn macro_amount(sig: Signal) -> f32 {
    match sig {
        Signal::Bool(b) => b as u8 as f32,
        Signal::Vec2(v) => v.length(),
        other => other.as_float().abs(),
    }
    .clamp(0.0, 1.0)
}

/// The amount a Macro Output port carries this tick, as `jsm_publish` put it
/// into `upstream`.
fn port_amount(upstream: &HashMap<String, Signal>, id: u32) -> f32 {
    upstream.get(&super::names::macro_btn_pin(id)).map(|s| s.as_float()).unwrap_or(0.0)
}

/// Is this JSM button pressed right now? Buttons read straight off a pin; the
/// trigger, stick, motion-stick and touch-stick ones come from the analog side,
/// which runs all five of JSM's sticks; lean and the touchpad grid come from the
/// gravity and touch passes.
fn button_down(
    btn: Btn,
    upstream: &HashMap<String, Signal>,
    analog: &Analog,
    ext: &Extra,
) -> bool {
    let b = |pin: &str| upstream.get(pin).map(|s| s.as_bool()).unwrap_or(false);
    match btn.source() {
        BtnSource::Pin(pin) => b(pin),
        // Whichever of the two this pad has; a pad never has both asserted.
        BtnSource::Either(p, q) => b(p) || b(q),
        BtnSource::Trigger { .. }
        | BtnSource::TriggerFull { .. }
        | BtnSource::Stick { .. }
        | BtnSource::Motion { .. } => analog.down(btn),
        BtnSource::Lean { right } => if right { ext.lean.1 } else { ext.lean.0 },
        // Nothing reports it, so it is never pressed. Its line says as much.
        BtnSource::Absent(_) => false,
        // `TOUCH` is the touchpad soft pull, which the analog side runs.
        BtnSource::Touch => analog.down(btn),
        BtnSource::TouchZone { cell, dir } => match (cell, dir) {
            // A grid cell is down while either finger is in it.
            (Some(c), _) => ext.cells.iter().any(|f| *f == Some(c)),
            // A touch-stick direction, which the analog side runs for both fingers.
            (None, Some(_)) => analog.down(btn),
            (None, None) => false,
        },
        // A MIDI message: a note while it sounds, a knob or bend past the
        // threshold. An idle pin is absent from the bus, which reads released.
        BtnSource::Midi(m) => {
            let threshold = if ext.analog_fed.contains(&btn) { 0.0 } else { ext.midi.in_threshold };
            upstream
                .get(&m.pin(ext.midi.in_channel).to_id())
                .is_some_and(|v| super::midi::value_pressed(m, *v, threshold))
        }
        // A Macro Output port: on from its threshold — the line's own (`>30`),
        // any movement where it only feeds a value, else halfway.
        BtnSource::Macro { id, at } => {
            let amount = port_amount(upstream, id);
            match at {
                Some(0) => amount > 0.0,
                Some(pct) => amount >= pct as f32 / 100.0,
                None if ext.analog_fed.contains(&btn) => amount > 0.0,
                None => amount >= 0.5,
            }
        }
    }
}

/// What a held MIDI value pin reads this tick.
///
/// Held by one binding it takes that binding's source (`ZL = MIDI_CC7` sends the
/// pull). A bend can be held by several pushing different ways (`ZL =
/// MIDI_PB_DOWN`, `ZR = MIDI_PB_UP`), so each holder pushes its binding's way and
/// they add up — both pulled fully cancel, as two hands on one wheel would.
/// Anything else takes the strongest holder. Held with nobody behind it — a
/// toggle, a tap — it reads full, the way any key does.
fn held_value(
    cfg: &Compiled,
    pin: &str,
    mp: &flexinput_core::midi::MidiPin,
    holders: &[(Btn, usize)],
    upstream: &HashMap<String, Signal>,
    midi: &super::midi::Settings,
) -> f32 {
    use flexinput_core::midi::MidiPin;
    // The way a binding pushes this bend, if it names one.
    let dir_in = |binding: &super::parse::Binding| {
        binding.steps.iter().find_map(|s| match &s.out {
            Out::Bend { pin: p, dir } if p == pin => Some(*dir),
            _ => None,
        })
    };
    let bend = matches!(mp, MidiPin::PitchBend { .. });
    if holders.is_empty() {
        // A toggle or a tap: full, whichever way the config pushes this bend.
        let dir = if bend { cfg.bindings.iter().find_map(|b| dir_in(b)) } else { None };
        return super::midi::value_for(mp, dir, 1.0, false);
    }
    let mut total = 0.0f32;
    let mut strongest = 0.0f32;
    for (owner, binding) in holders {
        let dir = cfg.bindings.get(*binding).and_then(|b| dir_in(b));
        let (r, two_sided) = analog_reading(*owner, upstream, midi).unwrap_or((1.0, false));
        let v = super::midi::value_for(mp, dir, r, two_sided);
        total += v;
        if v.abs() > strongest.abs() {
            strongest = v;
        }
    }
    if bend { total.clamp(-1.0, 1.0) } else { strongest }
}

/// A button's reading, where it has one worth handing to a value: a trigger's
/// pull, a MIDI knob, bend or pressure, or the velocity a note arrived with —
/// with whether it is two-sided (a bend). `None` for a plain button.
fn analog_reading(
    btn: Btn,
    upstream: &HashMap<String, Signal>,
    midi: &super::midi::Settings,
) -> Option<(f32, bool)> {
    let f = |pin: &str| upstream.get(pin).map(|s| s.as_float()).unwrap_or(0.0);
    match btn.source() {
        BtnSource::Trigger { analog, digital } => Some((
            if upstream.contains_key(analog) {
                f(analog).clamp(0.0, 1.0)
            } else if upstream.get(digital).is_some_and(|s| s.as_bool()) {
                1.0
            } else {
                0.0
            },
            false,
        )),
        BtnSource::TriggerFull { analog } => Some((f(analog).clamp(0.0, 1.0), false)),
        BtnSource::Midi(m) => {
            use flexinput_core::midi::MidiPin;
            match m.pin(midi.in_channel) {
                // A note's live value: its aftertouch, else its channel's
                // pressure, else its velocity — the same reading every mapping
                // module takes (`note_value`).
                p @ MidiPin::Note { .. } => {
                    Some((crate::eval::modules::note_value(upstream, &p.to_id()).unwrap_or(0.0), false))
                }
                // The whole wheel is two-sided; a named half reads its own side.
                p @ MidiPin::PitchBend { .. } => Some(match m.bend_dir() {
                    Some(_) => (m.reading(f(&p.to_id())), false),
                    None => (f(&p.to_id()), true),
                }),
                p if p.is_continuous() && p != MidiPin::Bpm => Some((f(&p.to_id()), false)),
                _ => None,
            }
        }
        // A port's amount — so `@Throttle = MIDI_CC7` hands on the pull, not
        // just on and off.
        BtnSource::Macro { id, .. } => Some((port_amount(upstream, id), false)),
        _ => None,
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
    // A stick pointed at the mouse — or at a virtual pad — is the config's
    // whether or not a binding ever names one of its directions.
    for (stick, s) in [
        (StickId::Left, res.settings.left),
        (StickId::Right, res.settings.right),
    ] {
        if s.mode.aims() || s.mode.pads() || s.mode == super::analog::StickMode::Midi {
            analog.extend(stick_pins(stick));
        }
        // And the virtual stick it drives is the config's to write, which may not
        // be the one it reads: `RIGHT_STICK_MODE = LEFT_STICK` crosses them over.
        if let Some(side) = s.mode.pad_side() {
            analog.extend(stick_pins(if side == 0 { StickId::Left } else { StickId::Right }));
        }
    }
    // A gyro or flick stick aimed at a virtual stick owns that stick too, even
    // with no physical stick in a pad mode at all.
    for dest in [res.pad.gyro_dest, res.pad.flick_dest] {
        if let Some(side) = dest.side() {
            analog.extend(stick_pins(if side == 0 { StickId::Left } else { StickId::Right }));
        }
    }
    // `ZL_MODE = X_LT` takes over a virtual trigger, and the physical trigger it
    // reads (which the `mentioned` sweep below only claims if a binding names it).
    for (right, mode) in [(false, res.settings.zl), (true, res.settings.zr)] {
        if let Some(side) = mode.pad_side() {
            analog.insert(if side == 0 { "left_trigger" } else { "right_trigger" }.to_string());
            analog.insert(if right { "right_trigger" } else { "left_trigger" }.to_string());
        }
    }
    for &btn in &cfg.mentioned {
        match btn.source() {
            // A Macro Output port isn't on this pad's bus, so reading one claims
            // nothing of it.
            BtnSource::Macro { .. } => {}
            BtnSource::Pin(pin) => {
                digital.insert(pin.to_string());
            }
            // Both, because either could be the one this pad has — and claiming
            // a pin the pad hasn't got costs nothing.
            BtnSource::Either(p, q) => {
                digital.insert(p.to_string());
                digital.insert(q.to_string());
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
            // A name nothing on our bus reports claims nothing — there is no pin to
            // claim, and the line already says it can never fire.
            BtnSource::Absent(_) => {}
            // A MIDI message the config reads is the config's, on every channel
            // an any-channel name could hear it on.
            BtnSource::Midi(m) => digital.extend(m.input_pins(res.midi.in_channel)),
            // Sources later phases read: nothing to claim while they can't fire.
            BtnSource::Motion { .. }
            | BtnSource::Lean { .. }
            | BtnSource::Touch
            | BtnSource::TouchZone { .. } => {}
        }
    }
    // A touchpad this config reads is the config's: its grid buttons, its sticks
    // and its mouse mode all consume the finger positions, so a Touch Zones module
    // downstream shouldn't see them as well. `PS_TOUCHPAD` is the exception —
    // that mode exists precisely to pass them on.
    if res.touch.mode != super::touch::Mode::PsTouchpad
        && (cfg.mentioned.iter().any(|b| {
            matches!(b.source(), BtnSource::Touch | BtnSource::TouchZone { .. })
        }) || res.touch.mode == super::touch::Mode::Mouse
            || res.touch.mode == super::touch::Mode::Midi
            || res.settings.touch.mode.runs_here())
    {
        for pin in ["touch1_x", "touch1_y", "touch1_active", "touch2_x", "touch2_y", "touch2_active"] {
            analog.insert(pin.to_string());
        }
    }
    // Anything measured against gravity reads the accelerometer, and the motion
    // stick and lean buttons read nothing else — so a config using them owns it.
    let reads_gravity = res.motion.space.needs_gravity()
        || res.settings.motion.mode.runs_here()
        || cfg.mentioned.iter().any(|b| {
            matches!(b.source(), BtnSource::Motion { .. } | BtnSource::Lean { .. })
        });
    if reads_gravity {
        for pin in ["accel_x", "accel_y", "accel_z"] {
            analog.insert(pin.to_string());
        }
    }
    // A config that aims with the gyro owns it, so it can't also drive a Gyro
    // 3DOF node downstream. One left at JSM's default sensitivity of zero aims
    // with nothing, and passes the gyro on untouched — and so does `GYRO_OUTPUT =
    // PS_MOTION`, which is the whole of what that setting means here: the pad's
    // own motion goes downstream untouched, for a DualSense or DS4 sink to use.
    // Sent as MIDI, it is the config's whatever the sensitivity says.
    if res.pad.gyro_dest == super::pad::Dest::Midi
        || (res.pad.gyro_dest != super::pad::Dest::PsMotion
            && (res.aim.min_sens != (0.0, 0.0) || res.aim.max_sens != (0.0, 0.0)))
    {
        for pin in ["gyro_x", "gyro_y", "gyro_z"] {
            analog.insert(pin.to_string());
        }
    }
    (digital, analog)
}

/// This patch's FlexInput targets, as `(name, pin)`.
///
/// Stamped onto the node by the graph builder the way `_automap_device_id` is,
/// because the names live in the patch rather than in the config text — and a
/// config that binds `@Reload` has to be told which port that is.
fn macro_ports(snap: &NodeSnap) -> Vec<(String, String)> {
    snap.params
        .get("_macro_ports")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let name = e.get("name")?.as_str()?.to_string();
                    let pin = e.get("pin")?.as_str()?.to_string();
                    Some((name, pin))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Every pin the compiled config could drive, whatever the event that would do
/// it. These are the pins the module answers for each tick.
fn claimed_outputs(cfg: &Compiled) -> Vec<String> {
    let mut pins: HashSet<String> = HashSet::new();
    for b in &cfg.bindings {
        for step in &b.steps {
            match &step.out {
                Out::Pin(p) | Out::Pulse(p) | Out::Bend { pin: p, .. } => { pins.insert(p.clone()); }
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
        // The motion stick and the touch sticks can aim too.
        || res.settings.motion.mode.aims()
        || res.settings.touch.mode.aims()
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
