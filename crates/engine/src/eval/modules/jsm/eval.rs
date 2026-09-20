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
    /// The sticks and rates pointed at a virtual pad instead.
    pad: PadOut,
    /// Which way is down, and the motion stick built from it.
    motion: super::motion::Motion,
    /// The touchpad: its grid, its two relative sticks, its mouse mode.
    touch: super::touch::Touch,
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
            pad: PadOut::default(),
            motion: super::motion::Motion::default(),
            touch: super::touch::Touch::default(),
        })
    });
    if st.gen != gen {
        st.gen = gen;
        st.cfg = compile(&text);
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
    let ext = Extra { lean: (lean_l, lean_r), cells: touch_out.cells };
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
    let outputs = st.rt.tick(&st.cfg, &res.timings, dt, &held, &chord_only);
    let driven: Vec<String> = outputs.pins.iter().cloned().collect();
    let gyro_actions = outputs.gyro.clone();
    let recentre = outputs
        .commands
        .iter()
        .any(|c| c.trim().eq_ignore_ascii_case("SET_MOTION_STICK_NEUTRAL"));
    if recentre {
        st.motion.set_neutral(gravity);
    }

    // Then aiming: the gyro and whichever sticks are pointed at the mouse. What
    // is pointed at a virtual pad's stick instead comes back as a camera rate for
    // the pad side to convert.
    let aimed = {
        let f = |pin: &str| upstream.get(pin).map(|s| s.as_float()).unwrap_or(0.0) * GYRO_REF_DPS;
        let gyro = Gyro {
            roll: f("gyro_x"),
            pitch: f("gyro_y"),
            yaw: f("gyro_z"),
        };
        let analog = &st.analog;
        let held = |btn: Btn| -> bool { button_down(btn, &upstream, analog, &ext) };
        st.aim
            .tick(&res.aim, &res.pad, &res.motion, gravity, dt, gyro, &st.analog, &gyro_actions, &held)
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
fn read_pad(
    upstream: &HashMap<String, Signal>,
    motion: (f32, f32),
    touch: &super::touch::Out,
) -> Pad {
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
        BtnSource::Trigger { .. }
        | BtnSource::TriggerFull { .. }
        | BtnSource::Stick { .. }
        | BtnSource::Motion { .. } => analog.down(btn),
        BtnSource::Lean { right } => if right { ext.lean.1 } else { ext.lean.0 },
        // `TOUCH` is the touchpad soft pull, which the analog side runs.
        BtnSource::Touch => analog.down(btn),
        BtnSource::TouchZone { cell, dir } => match (cell, dir) {
            // A grid cell is down while either finger is in it.
            (Some(c), _) => ext.cells.iter().any(|f| *f == Some(c)),
            // A touch-stick direction, which the analog side runs for both fingers.
            (None, Some(_)) => analog.down(btn),
            (None, None) => false,
        },
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
        if s.mode.aims() || s.mode.pads() {
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
    // A touchpad this config reads is the config's: its grid buttons, its sticks
    // and its mouse mode all consume the finger positions, so a Touch Zones module
    // downstream shouldn't see them as well. `PS_TOUCHPAD` is the exception —
    // that mode exists precisely to pass them on.
    if res.touch.mode != super::touch::Mode::PsTouchpad
        && (cfg.mentioned.iter().any(|b| {
            matches!(b.source(), BtnSource::Touch | BtnSource::TouchZone { .. })
        }) || res.touch.mode == super::touch::Mode::Mouse
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
    if res.pad.gyro_dest != super::pad::Dest::PsMotion
        && (res.aim.min_sens != (0.0, 0.0) || res.aim.max_sens != (0.0, 0.0))
    {
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
