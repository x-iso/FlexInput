//! The Area Mapper evaluator: one XY pair of an AutoMap bus laid onto the
//! cells of one or more LAYERS — each an [`AreaLayout`] with its own cards —
//! every cell driving its mapping cards. All layers act together on the same
//! point (an outer-ring layer can hold Shift whichever way the stick points).
//!
//! The node hands its bus on under `collector:{uid}` like the AutoMap curves,
//! with the picked pair consumed (zeroed) unless `area_pass_source` is on, and
//! the cards' outputs written over it.
//!
//! A cell's WEIGHT is its share of the point: 1 inside it, 0 outside, and in
//! between across a gradient border, where the border's curve shapes the fade.
//! Analog outputs (stick directions, triggers) take the weight. Digital outputs
//! follow the gradient that governs the cell (see [`CellWeight::governor`]):
//! PWM with the cells taking turns (Alternate) or on their own clocks
//! (Independent), a tap train, or a threshold. An ANALOG layer ignores its
//! borders and turns the stick's deflection into key pulses on its direction
//! cells. It mixes by DIRECTION (`AreaLayout::analog_moves`: steer between a
//! key and the diagonal, the push is how much of the time either holds) or
//! PER KEY (`AreaLayout::analog_shares`: each key its own axis). It pulses
//! SMOOTH (error diffusion: every press and gap at least the minimum, as many
//! as the share allows, debt paid back at once) or at a FIXED PWM period.
//!
//! [`AreaLayout`]: flexinput_core::area::AreaLayout
//! [`CellWeight::governor`]: flexinput_core::area::CellWeight

use super::*;
use flexinput_core::area::{AnalogTune, AreaLayout, CellWeight, DigitalMode, Gradient, Phase, Shape};

/// Stable module id.
pub const AREA_MAPPER_ID: &str = "module.area_mapper";
/// The XY pair the node reads: `left_stick`, `right_stick`, `touch1`, `touch2`.
pub const AREA_INPUT_PARAM: &str = "area_input";
/// The loaded layer's layout, as [`AreaLayout::to_value`].
pub const AREA_LAYOUT_PARAM: &str = "area_layout";
/// Keep the picked pair on the passthrough bus instead of consuming it.
pub const AREA_PASS_SOURCE_PARAM: &str = "area_pass_source";
/// The loaded layer's cell ids that only count while the input is touched.
pub const AREA_TOUCH_CELLS_PARAM: &str = "area_touch_cells";
/// Every layer, each an object holding [`AREA_LAYER_KEYS`]. Absent on a node
/// with one layer, which lives in the loaded keys alone.
pub const AREA_LAYERS_PARAM: &str = "area_layers";
/// Which layer's data sits in the loaded keys (the one being edited); the
/// other layers are read from [`AREA_LAYERS_PARAM`].
pub const AREA_LAYER_LOADED_PARAM: &str = "_area_layer_loaded";
/// The loaded layer's colour, `[r, g, b, a]`.
pub const AREA_LAYER_COLOR_PARAM: &str = "area_layer_color";
/// The loaded layer is an analog layer (see the module docs).
pub const AREA_LAYER_ANALOG_PARAM: &str = "area_layer_analog";
/// The loaded analog layer's PWM period, ms (Fixed pulses).
pub const AREA_LAYER_MS_PARAM: &str = "area_layer_ms";
/// The loaded analog layer's mix: `direction` or `keys`.
pub const AREA_LAYER_MIX_PARAM: &str = "area_layer_mix";
/// The loaded analog layer's pulses: `smooth` or `pwm` (fixed period).
pub const AREA_LAYER_PULSE_PARAM: &str = "area_layer_pulse";
/// The loaded analog layer's shortest press or gap, ms (Smooth pulses).
pub const AREA_LAYER_HOLD_PARAM: &str = "area_layer_hold_ms";
/// The loaded analog layer's model of the game's acceleration: ms from
/// standing to full speed (Smooth pulses; 0 = the game moves at once).
pub const AREA_LAYER_RAMP_PARAM: &str = "area_layer_ramp_ms";
/// The loaded analog layer's full-push radius (0..1): beyond it the push is
/// full, a stable zone for holding the keys.
pub const AREA_LAYER_FULL_PARAM: &str = "area_layer_full";
/// The loaded analog layer's midpoint: the push (0..1) that holds half the
/// time (0.5 = linear).
pub const AREA_LAYER_MID_PARAM: &str = "area_layer_mid";
/// The keys a layer carries: the loaded layer under these params, every
/// layer under the same names inside its [`AREA_LAYERS_PARAM`] object.
pub const AREA_LAYER_KEYS: &[&str] = &[
    AREA_LAYOUT_PARAM, "zone_meta", AREA_TOUCH_CELLS_PARAM,
    AREA_LAYER_COLOR_PARAM, AREA_LAYER_ANALOG_PARAM, AREA_LAYER_MS_PARAM,
    AREA_LAYER_MIX_PARAM, AREA_LAYER_PULSE_PARAM, AREA_LAYER_HOLD_PARAM,
    AREA_LAYER_RAMP_PARAM, AREA_LAYER_FULL_PARAM, AREA_LAYER_MID_PARAM,
];
/// One press mode for every card (`area_press_*` then rule them all).
pub const AREA_PRESS_LOCK_PARAM: &str = "area_press_lock";
pub const AREA_PRESS_MODE_PARAM: &str = "area_press_mode";
pub const AREA_PRESS_MS_PARAM: &str = "area_press_ms";
pub const AREA_PRESS_HOLD_PARAM: &str = "area_press_hold";
pub const AREA_PRESS_TURBO_PARAM: &str = "area_press_turbo";
/// The press modes the global setting offers, in its menu's order.
pub const AREA_PRESS_MODES: &[&str] = &["down", "short", "long", "double", "on_press", "on_release"];
/// Default analog-layer PWM period.
pub const AREA_ANALOG_MS_DEFAULT: f32 = 50.0;
/// Default analog-layer shortest press: two frames at 60 fps, so a game that
/// reads keys once a frame sees every press.
pub const AREA_ANALOG_HOLD_DEFAULT: f32 = 34.0;
/// Default analog-layer full-push radius.
pub const AREA_ANALOG_FULL_DEFAULT: f32 = 0.95;
/// The live mirror tags each cell with its layer as `layer * AREA_LAYER_STRIDE + id`.
pub const AREA_LAYER_STRIDE: u32 = 4096;

/// Card trigger tokens.
pub const AREA_TRIG_IN: &str = "area_in";
pub const AREA_TRIG_ENTER: &str = "area_enter";
pub const AREA_TRIG_LEAVE: &str = "area_leave";

/// A full-share cell at full deflection drives the mouse at this velocity
/// (times the card's `mouse_speed`) — the Touch Zones gain, so the two feel
/// alike against the same sink sensitivity.
const AREA_MOUSE_BASE: f32 = 0.03;

/// The XY pairs an Area Mapper can read, in the header's order.
pub const AREA_INPUTS: &[&str] = &["left_stick", "right_stick", "touch1", "touch2"];

/// The loaded layer's layout: the stored one, else the default circle.
pub fn area_layout_of(params: &HashMap<String, Value>) -> AreaLayout {
    params.get(AREA_LAYOUT_PARAM).and_then(AreaLayout::from_value)
        .unwrap_or_else(|| AreaLayout::default_for(Shape::Circle))
}

/// One layer as the evaluator needs it.
pub struct AreaLayerSpec {
    pub layout: AreaLayout,
    pub touch_cells: HashSet<u32>,
    pub analog: bool,
    pub analog_ms: f32,
    /// Mix by direction (else per key).
    pub by_direction: bool,
    /// Smooth pulses (else a fixed PWM period).
    pub smooth: bool,
    pub hold_ms: f32,
    pub ramp_ms: f32,
    pub tune: AnalogTune,
}

impl AreaLayerSpec {
    fn read(get: impl Fn(&str) -> Option<Value>) -> AreaLayerSpec {
        AreaLayerSpec {
            layout: get(AREA_LAYOUT_PARAM).as_ref().and_then(AreaLayout::from_value)
                .unwrap_or_else(|| AreaLayout::default_for(Shape::Circle)),
            touch_cells: get(AREA_TOUCH_CELLS_PARAM).and_then(|v| v.as_array().cloned())
                .map(|a| a.iter().filter_map(|v| v.as_u64().map(|i| i as u32)).collect())
                .unwrap_or_default(),
            analog: get(AREA_LAYER_ANALOG_PARAM).and_then(|v| v.as_bool()).unwrap_or(false),
            analog_ms: get(AREA_LAYER_MS_PARAM).and_then(|v| v.as_f64()).map(|v| v as f32)
                .unwrap_or(AREA_ANALOG_MS_DEFAULT).max(1.0),
            by_direction: get(AREA_LAYER_MIX_PARAM).and_then(|v| v.as_str().map(|s| s != "keys")).unwrap_or(true),
            smooth: get(AREA_LAYER_PULSE_PARAM).and_then(|v| v.as_str().map(|s| s != "pwm")).unwrap_or(true),
            hold_ms: get(AREA_LAYER_HOLD_PARAM).and_then(|v| v.as_f64()).map(|v| v as f32)
                .unwrap_or(AREA_ANALOG_HOLD_DEFAULT).max(1.0),
            ramp_ms: get(AREA_LAYER_RAMP_PARAM).and_then(|v| v.as_f64()).map(|v| v as f32)
                .unwrap_or(0.0).max(0.0),
            tune: AnalogTune {
                full: get(AREA_LAYER_FULL_PARAM).and_then(|v| v.as_f64()).map(|v| v as f32)
                    .unwrap_or(AREA_ANALOG_FULL_DEFAULT).clamp(0.05, 1.0),
                mid: get(AREA_LAYER_MID_PARAM).and_then(|v| v.as_f64()).map(|v| v as f32)
                    .unwrap_or(0.5).clamp(0.05, 0.95),
            },
        }
    }
}

/// Every layer, in order: the loaded one from the loaded keys, the rest from
/// their stored copies.
pub fn area_layers_of(params: &HashMap<String, Value>) -> Vec<AreaLayerSpec> {
    let from_params = || AreaLayerSpec::read(|k| params.get(k).cloned());
    let Some(stored) = params.get(AREA_LAYERS_PARAM).and_then(|v| v.as_array()).filter(|a| !a.is_empty()) else {
        return vec![from_params()];
    };
    let loaded = params.get(AREA_LAYER_LOADED_PARAM).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    stored.iter().enumerate().map(|(k, l)| {
        if k == loaded { from_params() } else { AreaLayerSpec::read(|key| l.get(key).cloned()) }
    }).collect()
}

/// The touch flag that gates a picked input: a touchpad point's `_active`, a
/// stick's capacitive `_touch` (a pin no backend publishes yet).
pub fn area_touch_pin(input: &str) -> String {
    if input.starts_with("touch") { format!("{input}_active") } else { format!("{input}_touch") }
}

/// A gradient's crossfade through its curve (linear without one).
fn shape_fade(g: &Gradient, t: f32) -> f32 {
    shape_mag(&g.curve, t)
}

/// Error-diffusion pulsing: of several states, each owed its share of the
/// time (`targets`, summing to 1), hold one; once it has been held `min_hold`
/// seconds, switch to the state most owed. Every press and gap lasts at least
/// `min_hold` (a game reading keys once a frame sees each one), they come as
/// often as that allows, and time owed is paid back as soon as it can be, so a
/// change of push shows at once instead of at the next period. A state whose
/// target drops to nothing lets go at once. `st` is `[current, lock left,
/// owed per state…]`, zeroed to start; returns the state held.
pub(crate) fn smooth_pick(targets: &[f32], st: &mut [f32], min_hold: f32, dt: f32) -> usize {
    let k = targets.len();
    let mut cur = st[0] as usize;
    if cur >= k {
        cur = 0;
    }
    let lim = min_hold.max(dt) * 2.0;
    let owed = &mut st[2..2 + k];
    for (o, t) in owed.iter_mut().zip(targets) {
        *o += t * dt;
        // Nothing is owed a state nobody wants any more.
        if *t <= 0.0 {
            *o = o.min(0.0);
        }
    }
    owed[cur] -= dt;
    for o in owed.iter_mut() {
        *o = o.clamp(-lim, lim);
    }
    st[1] -= dt;
    if st[1] <= 0.0 || targets[cur] <= 0.0 {
        let owed = &st[2..2 + k];
        let mut best = cur;
        for i in 0..k {
            if targets[i] > 0.0 && (targets[best] <= 0.0 || owed[i] > owed[best]) {
                best = i;
            }
        }
        if best != cur {
            cur = best;
            st[1] = min_hold;
        }
    }
    st[0] = cur as f32;
    cur
}

/// One key pulsed against a model of the game's acceleration: the game's
/// speed `v` climbs while the key is down and falls while it is up, taking
/// `ramp` seconds end to end; the key goes down while the model runs short of
/// `target` (plus what it has run short so far) and up while it runs over —
/// each press and gap at least `min_hold`. So the game's own smoothing does
/// the averaging and the presses hold its speed at the push, instead of
/// stopping and starting. `st` is `[down, lock left, owed, v]`, zeroed to
/// start; returns whether the key is down.
pub(crate) fn ramp_pick(target: f32, st: &mut [f32], min_hold: f32, ramp: f32, dt: f32) -> bool {
    let down = st[0] > 0.5;
    let step = dt / ramp.max(1e-4);
    st[3] = if down { (st[3] + step).min(1.0) } else { (st[3] - step).max(0.0) };
    let lim = ramp * 0.25;
    st[2] = (st[2] + (target - st[3]) * dt).clamp(-lim, lim);
    st[1] -= dt;
    let want = if target <= 0.0 {
        false
    } else if target >= 1.0 {
        true
    } else {
        (target - st[3]) + st[2] / ramp.max(1e-4) > 0.0
    };
    if want != down && (st[1] <= 0.0 || target <= 0.0 || target >= 1.0) {
        st[0] = if want { 1.0 } else { 0.0 };
        st[1] = min_hold;
    }
    st[0] > 0.5
}

/// An analog layer's shares and key gates this tick. `slots` is the layer's
/// state block (see [`analog_block`]).
fn analog_layer(
    spec: &AreaLayerSpec,
    point: Option<Vec2>,
    gated: &dyn Fn(u32) -> bool,
    slots: &mut [f32],
    dt: f32,
) -> (Vec<CellWeight>, HashMap<u32, bool>) {
    let layout = &spec.layout;
    let ids = analog_ids(layout);
    let mut digital = HashMap::new();
    let Some(p) = point else {
        slots.fill(0.0);
        return (Vec::new(), digital);
    };
    let (phase, rest) = slots.split_at_mut(1);
    let (per_cell, dir_st) = rest.split_at_mut(4 * ids);
    let hold = spec.hold_ms / 1000.0;
    let ramp = spec.ramp_ms / 1000.0;
    let period = (spec.analog_ms / 1000.0).max(0.001);
    let modelled = spec.smooth && ramp > 0.0;

    // The shares, and for a direction mix its moves.
    let (weights, band_moves): (Vec<CellWeight>, Option<(usize, Vec<flexinput_core::area::AnalogMove>)>) =
        if spec.by_direction {
            let (band, mut moves) = layout.analog_moves(p.x, p.y, &spec.tune);
            for m in &mut moves {
                m.ids.retain(|id| !gated(*id));
            }
            // Each cell's share is the time it is held, across the moves it is in.
            let mut weights: Vec<CellWeight> = Vec::new();
            for m in &moves {
                for &id in &m.ids {
                    match weights.iter_mut().find(|c| c.id == id) {
                        Some(c) => c.weight += m.share,
                        None => {
                            let index = layout.bands[band].cells.iter().position(|c| *c == id).unwrap_or(0);
                            weights.push(CellWeight { band, index, id, weight: m.share, governor: None });
                        }
                    }
                }
            }
            for c in &mut weights {
                c.weight = c.weight.min(1.0);
            }
            (weights, Some((band, moves)))
        } else {
            let w = layout.analog_shares(p.x, p.y, &spec.tune).into_iter().filter(|c| !gated(c.id)).collect();
            (w, None)
        };

    match band_moves.filter(|_| !modelled) {
        // Per key: each cell on its own (fixed PWM, smooth, or against the
        // game's ramp — a ramp also takes a direction mix's per-key shares).
        None => {
            for c in &weights {
                let st = &mut per_cell[4 * c.id as usize..4 * c.id as usize + 4];
                let on = if modelled {
                    ramp_pick(c.weight, st, hold, ramp, dt)
                } else {
                    c.weight >= 1.0 - 1e-6 || if spec.smooth {
                        smooth_pick(&[1.0 - c.weight, c.weight], st, hold, dt) == 1
                    } else {
                        analog_digital_pulse(c.weight, spec.analog_ms, true, false, &mut st[..1], dt)
                    }
                };
                digital.insert(c.id, on);
            }
            // A cell that lost its share starts over next time (a modelled
            // one keeps its speed running down).
            for id in 0..ids {
                if !digital.contains_key(&(id as u32)) {
                    let st = &mut per_cell[4 * id..4 * id + 4];
                    if modelled && st[3] > 0.0 {
                        ramp_pick(0.0, st, hold, ramp, dt);
                    } else {
                        st.fill(0.0);
                    }
                }
            }
            phase[0] = 0.0;
            dir_st.fill(0.0);
        }
        Some((band, moves)) => {
            per_cell.fill(0.0);
            // The state starts over in another band (its slots mean other cells).
            let k = layout.analog_slot_count();
            let (tag, st) = dir_st.split_first_mut().expect("analog block");
            if *tag != (band + 1) as f32 {
                st.fill(0.0);
                *tag = (band + 1) as f32;
            }
            let held: Vec<u32> = if spec.smooth {
                let mut targets = vec![0.0f32; k];
                for m in &moves {
                    if m.slot < k {
                        targets[m.slot] += m.share;
                    }
                }
                let s = smooth_pick(&targets, &mut st[..2 + k], hold, dt);
                moves.iter().find(|m| m.slot == s).map(|m| m.ids.clone()).unwrap_or_default()
            } else {
                // One period: each move in turn for its share, nothing last.
                let mut t = phase[0] + dt;
                if t >= period { t -= period * (t / period).floor(); }
                phase[0] = t;
                let at = t / period;
                let mut cum = 0.0f32;
                let mut held = Vec::new();
                for m in &moves {
                    if at >= cum && at < cum + m.share {
                        held = m.ids.clone();
                        break;
                    }
                    cum += m.share;
                }
                held
            };
            for c in &weights {
                digital.insert(c.id, held.contains(&c.id));
            }
        }
    }
    (weights, digital)
}

/// Cell slots an analog layer keeps state for (its highest id + 1).
fn analog_ids(layout: &AreaLayout) -> usize {
    layout.cell_ids().into_iter().max().unwrap_or(0) as usize + 1
}

/// An analog layer's state block: `[PWM phase][4 per cell id: per-key
/// pulses][band tag, current, lock, owed per move slot…]`.
fn analog_block(layout: &AreaLayout) -> usize {
    1 + 4 * analog_ids(layout) + 1 + 2 + layout.analog_slot_count()
}

/// One layer's live state this tick.
struct LayerLive {
    weights: Vec<CellWeight>,
    hard: Option<u32>,
    digital: HashMap<u32, bool>,
}

/// Evaluate an Area Mapper node — shared by the top-level and sub-patch loops.
///
/// Returns the live mirror the body draws from (beyond the one real AutoMap
/// port, so the UI reads it by position): `[0]` None (the AutoMap port),
/// `[1]` the point as a centred Vec2 (+Y up) or None when there is none (a
/// lifted finger), `[2..]` one `Vec2(layer * AREA_LAYER_STRIDE + cell id,
/// weight)` per cell with a share.
pub(crate) fn eval_area_mapper_node(
    snap: &NodeSnap,
    uid: usize,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
    state: &mut HashMap<usize, NodeState>,
    dt: f32,
) -> Vec<Option<Signal>> {
    republish_bus_as_collector(snap, uid, dev_sigs, collector_sigs);
    let key = format!("collector:{uid}");
    let dev_id = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let bus = |pin: &str, cs: &HashMap<(String, String), Signal>| -> Option<Signal> {
        cs.get(&(key.clone(), pin.to_string())).copied()
            .or_else(|| dev_sigs.get(&(dev_id.clone(), pin.to_string())).copied())
    };

    let input = snap.params.get(AREA_INPUT_PARAM).and_then(|v| v.as_str()).unwrap_or("left_stick");
    let layers = area_layers_of(&snap.params);
    let pass_source = snap.params.get(AREA_PASS_SOURCE_PARAM).and_then(|v| v.as_bool()).unwrap_or(false);
    let touchpad = input.starts_with("touch");
    let touched = bus(&area_touch_pin(input), collector_sigs).is_some_and(|s| s.as_bool());

    // ── The point, centred, +Y up. A touchpad has none while no finger is down.
    let point: Option<Vec2> = if touchpad {
        touched.then(|| {
            let (ux, uy) = flexinput_core::touchzones::pad_point_to_unit(
                bus(&format!("{input}_x"), collector_sigs).map(|s| s.as_float()).unwrap_or(0.0),
                bus(&format!("{input}_y"), collector_sigs).map(|s| s.as_float()).unwrap_or(0.0),
            );
            Vec2::new(ux * 2.0 - 1.0, 1.0 - uy * 2.0)
        })
    } else {
        Some(match bus(input, collector_sigs) {
            Some(Signal::Vec2(v)) => v,
            _ => Vec2::new(
                bus(&format!("{input}_x"), collector_sigs).map(|s| s.as_float()).unwrap_or(0.0),
                bus(&format!("{input}_y"), collector_sigs).map(|s| s.as_float()).unwrap_or(0.0),
            ),
        })
    };

    // ── Per layer: weights, the hard cell, and each cell's digital gate. State
    // per layer in NodeState.aux_f32: [base] the Alternate PWM phase, then one
    // phase slot per cell id (Independent PWM, tap trains, analog layers).
    let ns = state.entry(uid).or_insert_with(NodeState::default);
    let mut base = 0usize;
    let mut live: Vec<LayerLive> = Vec::with_capacity(layers.len());
    for spec in &layers {
        let layout = &spec.layout;
        let block = if spec.analog {
            analog_block(layout)
        } else {
            1 + layout.cell_ids().into_iter().max().unwrap_or(0) as usize + 1
        };
        if ns.aux_f32.len() < base + block {
            ns.aux_f32.resize(base + block, 0.0);
        }
        let slots = &mut ns.aux_f32[base..base + block];
        base += block;

        let gated = |id: u32| spec.touch_cells.contains(&id) && !touched;
        let hard: Option<u32> = point.map(|p| layout.locate_id(p.x, p.y)).filter(|id| !gated(*id));
        if spec.analog {
            let (weights, digital) = analog_layer(spec, point, &gated, slots, dt);
            live.push(LayerLive { weights, hard, digital });
            continue;
        }
        let weights: Vec<CellWeight> = point
            .map(|p| layout.weights(p.x, p.y, shape_fade))
            .unwrap_or_default()
            .into_iter()
            .filter(|c| !gated(c.id))
            .collect();

        let mut digital: HashMap<u32, bool> = HashMap::new();
        {
            let gradient_of = |c: &CellWeight| -> Option<&Gradient> {
                c.governor.and_then(|(r, _)| layout.border(r)).and_then(|b| b.gradient.as_ref())
            };
            // Alternate PWM: every cell governed by an Alternate PWM gradient
            // takes a turn within one shared period, for its weight's share, in
            // a fixed order — exactly one on at a time, so keys emulate a
            // direction.
            let alternating: Vec<&CellWeight> = weights.iter()
                .filter(|c| gradient_of(c).is_some_and(|g| g.digital == DigitalMode::Pwm && g.phase == Phase::Alternate))
                .collect();
            let alt_on: HashSet<u32> = if alternating.is_empty() {
                slots[0] = 0.0;
                HashSet::new()
            } else {
                let period = alternating.iter()
                    .filter_map(|c| gradient_of(c).map(|g| g.period_ms))
                    .fold(f32::INFINITY, f32::min) / 1000.0;
                let period = period.max(0.001);
                let mut phase = slots[0] + dt;
                if phase >= period { phase -= period * (phase / period).floor(); }
                slots[0] = phase;
                let at = phase / period;
                let mut cum = 0.0f32;
                let mut on = HashSet::new();
                for c in &alternating {
                    if at >= cum && at < cum + c.weight { on.insert(c.id); }
                    cum += c.weight;
                }
                on
            };
            for c in &weights {
                let on = match gradient_of(c) {
                    None => c.weight >= 0.5,
                    Some(_) if c.weight >= 1.0 - 1e-6 => true,
                    Some(g) => match g.digital {
                        DigitalMode::Threshold => c.weight >= g.threshold,
                        DigitalMode::Pwm if g.phase == Phase::Alternate => alt_on.contains(&c.id),
                        mode => {
                            let slot = 1 + c.id as usize;
                            analog_digital_pulse(c.weight, g.period_ms, mode == DigitalMode::Pwm, false,
                                &mut slots[slot..slot + 1], dt)
                        }
                    },
                };
                digital.insert(c.id, on);
            }
        }
        // A cell that lost its share restarts its own clock next time.
        for id in layout.cell_ids() {
            if !digital.contains_key(&id) {
                slots[1 + id as usize] = 0.0;
            }
        }
        live.push(LayerLive { weights, hard, digital });
    }

    // One press mode for all cards, when locked.
    let press_lock = snap.params.get(AREA_PRESS_LOCK_PARAM).and_then(|v| v.as_bool()).unwrap_or(false);
    let global_press = serde_json::json!({
        "mode": snap.params.get(AREA_PRESS_MODE_PARAM).and_then(|v| v.as_str()).unwrap_or("down"),
        "window_ms": snap.params.get(AREA_PRESS_MS_PARAM).and_then(|v| v.as_f64()).unwrap_or(200.0),
        "sustain": snap.params.get(AREA_PRESS_HOLD_PARAM).and_then(|v| v.as_bool()).unwrap_or(false),
        "turbo": snap.params.get(AREA_PRESS_TURBO_PARAM).and_then(|v| v.as_bool()).unwrap_or(false),
    });

    // ── Cards ──
    let cards = snap.params.get("zone_maps").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut button_on: HashMap<String, bool> = HashMap::new();
    let mut axis_acc: HashMap<&'static str, f32> = HashMap::new();
    let mut mouse: Option<Vec2> = None;
    let mut scroll: Option<Vec2> = None;
    for (i, card) in cards.iter().enumerate() {
        let z = card.get("z").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        // The card's layer (the card list's "field").
        let Some(lay) = live.get(card.get("f").and_then(|v| v.as_u64()).unwrap_or(0) as usize) else { continue };
        let trigger = card.get("in").and_then(|v| v.as_array())
            .and_then(|a| a.first()).and_then(|v| v.as_str()).unwrap_or(AREA_TRIG_IN);
        let inside = lay.hard == Some(z);
        let press = PressParams::from_card(if press_lock { &global_press } else { card });
        let slots = press_state_get(ns, i);
        // Enter / Leave pulse for the card's window as the point crosses the
        // cell's hard border; In follows the cell's digital gate through the
        // card's press mode.
        let (held, analog) = match trigger {
            AREA_TRIG_ENTER | AREA_TRIG_LEAVE => {
                let mode = if trigger == AREA_TRIG_ENTER { PressMode::OnPress } else { PressMode::OnRelease };
                let held = apply_press_mode(inside, mode, press.window_ms, false, slots, dt);
                (held, if held { 1.0 } else { 0.0 })
            }
            _ => {
                let raw = lay.digital.get(&z).copied().unwrap_or(false);
                let share = lay.weights.iter().find(|c| c.id == z).map(|c| c.weight).unwrap_or(0.0);
                (press.gate(raw, slots, dt), share)
            }
        };
        for p in card.get("out").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|v| v.as_str()) {
            if is_macro_style_target(p) {
                if held { merge_macro_scalar(collector_sigs, p, Signal::Bool(true)); }
                continue;
            }
            if let Some((axis, sign)) = analog_axis_for_cardinal(p) {
                if analog > 0.0 { *axis_acc.entry(axis).or_insert(0.0) += sign * analog; }
                continue;
            }
            if let Some(trigger_pin) = analog_trigger_out(p) {
                if analog > 0.0 { *axis_acc.entry(trigger_pin).or_insert(0.0) += analog; }
                continue;
            }
            // The point itself, scaled by the cell's share: a whole stick gets
            // it as a stick, the mouse as a velocity, scroll as a rate — so a
            // cell can say "in here, the stick aims".
            let pt = point.filter(|_| analog > 0.0).map(|v| v * analog);
            match p {
                "left_stick" | "right_stick" => {
                    if let Some(v) = pt {
                        let (x, y) = if p == "left_stick" {
                            ("left_stick_x", "left_stick_y")
                        } else {
                            ("right_stick_x", "right_stick_y")
                        };
                        *axis_acc.entry(x).or_insert(0.0) += v.x;
                        *axis_acc.entry(y).or_insert(0.0) += v.y;
                    }
                    continue;
                }
                "mouse" | "mouse_x" | "mouse_y" => {
                    if let Some(v) = pt {
                        let speed = card.get("mouse_speed").and_then(|s| s.as_f64()).unwrap_or(1.0) as f32;
                        let v = v * speed.clamp(0.1, 10.0) * AREA_MOUSE_BASE;
                        let m = mouse.get_or_insert(Vec2::ZERO);
                        if p != "mouse_y" { m.x += v.x; }
                        if p != "mouse_x" { m.y += v.y; }
                    }
                    continue;
                }
                "scroll_x" | "scroll_y" => {
                    if let Some(v) = pt {
                        let s = scroll.get_or_insert(Vec2::ZERO);
                        if p == "scroll_x" { s.x += v.x; } else { s.y += v.y; }
                    }
                    continue;
                }
                _ => {}
            }
            let e = button_on.entry(p.to_string()).or_insert(false);
            *e = *e || held;
        }
    }

    // ── Consume the picked pair on our own bus ──
    let mut sticks_touched: HashSet<&'static str> = HashSet::new();
    if !pass_source {
        match input {
            "left_stick" | "right_stick" => {
                let (v, x, y) = if input == "left_stick" {
                    ("left_stick", "left_stick_x", "left_stick_y")
                } else {
                    ("right_stick", "right_stick_x", "right_stick_y")
                };
                collector_sigs.insert((key.clone(), v.to_string()), Signal::Vec2(Vec2::ZERO));
                collector_sigs.insert((key.clone(), x.to_string()), Signal::Float(0.0));
                collector_sigs.insert((key.clone(), y.to_string()), Signal::Float(0.0));
                sticks_touched.insert(v);
            }
            _ => {
                collector_sigs.insert((key.clone(), format!("{input}_active")), Signal::Bool(false));
                collector_sigs.insert((key.clone(), format!("{input}_x")), Signal::Float(0.0));
                collector_sigs.insert((key.clone(), format!("{input}_y")), Signal::Float(0.0));
            }
        }
    }

    // ── Publish card outputs ──
    for (pin, on) in &button_on {
        if flexinput_core::midi::is_midi_pin(pin) {
            let s = if *on { CardMidi::On } else { CardMidi::Off };
            publish_card_midi(&key, pin, s, card_levels_for(&cards, pin), collector_sigs);
            continue;
        }
        let sig_type = automap::ALL_PINS.iter().find(|ap| ap.id == pin.as_str())
            .map(|ap| ap.signal_type).unwrap_or(SignalType::Bool);
        let carried = collector_sigs.contains_key(&(key.clone(), pin.clone()));
        // Released and on the bus anyway → leave the passthrough value (a real
        // press of the same button still comes through). Released and NOT on
        // the bus → write it off, or a virtual sink latches the last press.
        if !*on && carried { continue; }
        let sig = match (sig_type, *on) {
            (SignalType::Vec2, _) => continue,
            (SignalType::Float, on) => Signal::Float(if on { 1.0 } else { 0.0 }),
            (SignalType::Int, on) => Signal::Int(on as i32),
            (_, on) => Signal::Bool(on),
        };
        collector_sigs.insert((key.clone(), pin.clone()), sig);
    }
    for (axis, v) in &axis_acc {
        let clamped = if matches!(*axis, "left_trigger" | "right_trigger") { v.clamp(0.0, 1.0) } else { v.clamp(-1.0, 1.0) };
        collector_sigs.insert((key.clone(), axis.to_string()), Signal::Float(clamped));
        if let Some(vec_pin) = vec2_pin_for_axis(axis) {
            sticks_touched.insert(vec_pin);
        }
    }
    // Mouse and scroll only while a cell drives them; otherwise the bus's own
    // values pass.
    if let Some(m) = mouse {
        collector_sigs.insert((key.clone(), "mouse".to_string()), Signal::Vec2(m));
        collector_sigs.insert((key.clone(), "mouse_x".to_string()), Signal::Float(m.x));
        collector_sigs.insert((key.clone(), "mouse_y".to_string()), Signal::Float(m.y));
    }
    if let Some(s) = scroll {
        collector_sigs.insert((key.clone(), "scroll_x".to_string()), Signal::Float(s.x));
        collector_sigs.insert((key.clone(), "scroll_y".to_string()), Signal::Float(s.y));
    }
    // Keep each written stick's Vec2 and cardinal pins in step with its axes.
    for stick in sticks_touched {
        let (x, y) = (format!("{stick}_x"), format!("{stick}_y"));
        let ax = collector_sigs.get(&(key.clone(), x.clone())).map(|s| s.as_float()).unwrap_or(0.0);
        let ay = collector_sigs.get(&(key.clone(), y.clone())).map(|s| s.as_float()).unwrap_or(0.0);
        if stick != "dpad" {
            collector_sigs.insert((key.clone(), stick.to_string()), Signal::Vec2(Vec2::new(ax, ay)));
        }
        let mut local: HashMap<String, Signal> = HashMap::new();
        local.insert(x, Signal::Float(ax));
        local.insert(y, Signal::Float(ay));
        derive_stick_cardinals(&mut local);
        for (k, v) in local {
            if k.starts_with(stick) && (k.ends_with("_up") || k.ends_with("_down")
                || k.ends_with("_left") || k.ends_with("_right"))
            {
                collector_sigs.insert((key.clone(), k), v);
            }
        }
    }

    // ── Live mirror ──
    let mut out: Vec<Option<Signal>> = vec![None, point.map(Signal::Vec2)];
    for (k, l) in live.iter().enumerate() {
        let tag = k as u32 * AREA_LAYER_STRIDE;
        out.extend(l.weights.iter().map(|c| Some(Signal::Vec2(Vec2::new((tag + c.id) as f32, c.weight)))));
    }
    out
}
