//! Remapper cards that decide AFTER an input goes down, and the hold-back that
//! keeps that input from leaking — or being lost — while they do.
//!
//! Deciding cards:
//!   - an "in order" CHORD: every input held together, but they must have gone
//!     down in the listed order — A then B is a different card from B then A;
//!   - SEQUENCE mode: the inputs are pressed in order, each within the time gap
//!     of the previous one, and earlier steps may already be released;
//!   - the timed press modes SHORT, LONG and DOUBLE, which only know whether a
//!     press counts once it's released, held long enough, or tapped again.
//!
//! While such a card is still deciding, it HOLDS BACK the input it has so far
//! (always for "in order" and the timed modes; Sequence only with its toggle
//! on): the rest of the node doesn't see it. If the card fires, the input is
//! consumed. If it gives up, the input comes back late — still held → live, a
//! press that already ended → replayed at its original length. That's what
//! lets a Short card leave long presses working, a Long card leave taps
//! working, and a Double card leave single taps working.
//!
//! A card with a single analog input (stick direction / trigger) in Short or
//! Sequence mode times the whole MOVE, from where the input leaves zero: Short
//! fires on a flick that crosses the threshold and drops back under it inside
//! the time gap; Sequence fires once the threshold is reached inside the gap
//! and stays on while the input stays past it. A push that reaches the
//! threshold too late is just a slow push — nothing is held back.
//!
//! A held-back stick direction is hidden fully by an order-aware card (the
//! user asked for it). A timed card only CAPS it at the deflection where the
//! holding back began — about the card's threshold — so pushing a stick past a
//! Short/Long card's threshold doesn't stall it at zero while the card decides.
//!
//! Deciding cards evaluate against the node's RAW inputs. Everything else on
//! the node — other cards, analog cards, pass-through — reads the VIEW built
//! here.

use super::*;
use crate::state::{HoldBackPin, MovePhase, MoveTrack, OrderTrack};

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum OrderKind {
    Chord,
    Sequence,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct OrderSpec {
    pub(crate) kind: OrderKind,
    pub(crate) hold_back: bool,
}

/// Whether a card's `in_order` toggle does anything, so the card renderer and
/// gamepad nav only offer it then. Analog and touchpad-output cards ignore
/// order (analog pairs inputs with outputs by position; touch cards gate on
/// buttons and drive by deflection). A one-input card has no order to keep —
/// except a Sequence over one analog input, whose steps are the move toward
/// the threshold and the threshold itself.
pub fn in_order_applies(m: &Value) -> bool {
    let press = PressParams::from_card(m);
    if press.is_analog() || mapping_targets_touch(m) {
        return false;
    }
    let pins: Vec<&str> = m.get("in").and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    pins.len() >= 2 || (matches!(press.mode(), PressMode::Sequence) && single_analog_input(&pins).is_some())
}

/// The input of a card with exactly one analog input (stick direction or
/// trigger).
pub(crate) fn single_analog_input<'a>(pins: &[&'a str]) -> Option<&'a str> {
    match pins {
        [p] if pin_is_analog_input(p) => Some(p),
        _ => None,
    }
}

/// Advance a one-input analog card's move by a tick. `engaged`: the input is
/// off zero; `past`: it's past the card's threshold. Returns, when a fast move
/// drops back under the threshold still inside the time gap (a Short flick),
/// how long it spent past the threshold.
pub(crate) fn move_tick(track: &mut MoveTrack, engaged: bool, past: bool, window_s: f32, dt: f32) -> Option<f32> {
    track.replay_s = (track.replay_s - dt).max(0.0);
    if track.phase == MovePhase::Idle {
        if !engaged && !past {
            return None;
        }
        track.phase = MovePhase::Moving;
        track.since_start = 0.0;
        track.above_s = 0.0;
    }
    track.since_start += dt;
    let in_time = track.since_start <= window_s;
    let mut flick = None;
    if track.phase == MovePhase::Moving {
        track.phase = match (past, in_time) {
            (true, true) => MovePhase::Fast,
            (true, false) | (false, false) => MovePhase::Spent,
            (false, true) => MovePhase::Moving,
        };
    }
    if track.phase == MovePhase::Fast {
        if past {
            track.above_s += dt;
        } else {
            if in_time {
                flick = Some(track.above_s);
            }
            track.phase = MovePhase::Spent;
        }
    }
    if !engaged && !past {
        // Back at rest: the next move starts fresh.
        track.phase = MovePhase::Idle;
    }
    flick
}

/// How a card treats input order, or `None` for an order-agnostic card.
///
/// The card's `in_order` flag makes a chord ordered and holds back its start;
/// Sequence mode is always ordered, so there the flag only adds the hold-back.
pub(crate) fn order_spec(m: &Value) -> Option<OrderSpec> {
    let press = PressParams::from_card(m);
    if press.is_analog() || mapping_targets_touch(m) {
        return None;
    }
    let in_order = in_order_applies(m)
        && m.get("in_order").and_then(|v| v.as_bool()).unwrap_or(false);
    if matches!(press.mode(), PressMode::Sequence) {
        Some(OrderSpec { kind: OrderKind::Sequence, hold_back: in_order })
    } else if in_order {
        Some(OrderSpec { kind: OrderKind::Chord, hold_back: true })
    } else {
        None
    }
}

/// A card that decides after the press: order-aware, timed, or both.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DecidingSpec {
    pub(crate) order: Option<OrderSpec>,
    /// Short / Long / Double.
    pub(crate) timed: bool,
}

impl DecidingSpec {
    fn hold_back(&self) -> bool {
        self.timed || self.order.is_some_and(|o| o.hold_back)
    }
}

pub(crate) fn deciding_spec(m: &Value) -> Option<DecidingSpec> {
    let press = PressParams::from_card(m);
    if press.is_analog() || mapping_targets_touch(m) {
        return None;
    }
    let order = order_spec(m);
    let timed = matches!(press.mode(), PressMode::Short | PressMode::Long | PressMode::Double);
    (order.is_some() || timed).then_some(DecidingSpec { order, timed })
}

/// Advance one order-aware card by a tick and return whether it's matched.
///
/// `pins` are the card's `in` pins, `held` their held state this tick. A step
/// only advances on its input's RISING edge, so an input that went down before
/// its turn can't count until it's released and pressed again.
///
/// Chord: releasing a matched input falls back to that step, so with the
/// earlier inputs still held, pressing the last one again matches again.
///
/// Sequence: overrunning the time gap between steps starts over, and so does
/// pressing one of the card's own inputs out of turn — though that press may
/// itself restart the sequence (a repeated first step). Once matched, the card
/// stays matched while the last step is held.
pub(crate) fn order_tick(
    kind: OrderKind,
    track: &mut OrderTrack,
    pins: &[&str],
    held: &[bool],
    window_s: f32,
    dt: f32,
) -> bool {
    let n = pins.len();
    if n == 0 {
        return false;
    }
    if track.prev_held.len() != n {
        // First tick or the card was edited: start clean, treating inputs that
        // are already down as old presses so they can't count as steps.
        *track = OrderTrack { prev_held: held.to_vec(), progress: 0, since_step: 0.0 };
        return false;
    }
    let rising: Vec<bool> = (0..n).map(|i| held[i] && !track.prev_held[i]).collect();
    track.prev_held.copy_from_slice(held);
    track.since_step += dt;
    match kind {
        OrderKind::Chord => {
            if let Some(j) = (0..track.progress).find(|&j| !held[j]) {
                track.progress = j;
            }
            while track.progress < n && rising[track.progress] {
                track.progress += 1;
                track.since_step = 0.0;
            }
            track.progress == n
        }
        OrderKind::Sequence => {
            if track.progress == n {
                if held[n - 1] {
                    return true;
                }
                track.progress = 0;
            }
            if track.progress > 0 && track.since_step > window_s {
                track.progress = 0;
            }
            // One edge advances at most one step: a sequence may repeat an
            // input (down, down), and those need two separate presses.
            let mut used: Vec<&str> = Vec::new();
            while track.progress < n {
                let p = pins[track.progress];
                if !rising[track.progress] || used.contains(&p) {
                    break;
                }
                used.push(p);
                track.progress += 1;
                track.since_step = 0.0;
            }
            if track.progress == n {
                return held[n - 1];
            }
            for i in 0..n {
                let p = pins[i];
                if !rising[i] || used.contains(&p) {
                    continue;
                }
                used.push(p);
                track.progress = sequence_restart(pins, track.progress, p);
                track.since_step = 0.0;
            }
            false
        }
    }
}

/// Where a sequence stands after `x` is pressed out of turn at `progress`: the
/// longest run of opening steps that ends with this press, given that the steps
/// just matched are the sequence's own first `progress` steps.
fn sequence_restart(pins: &[&str], progress: usize, x: &str) -> usize {
    (1..=progress)
        .rev()
        .find(|&k| pins[k - 1] == x && pins[..k - 1] == pins[progress - (k - 1)..progress])
        .unwrap_or(0)
}

/// Whether a hold-back card is still waiting on the rest of its input: partway
/// through and, for a chord, inside the time gap with none of the remaining
/// inputs already down. An input that went down early can't count until it's
/// pressed again, so holding B then pressing A is B-then-A — an A-then-B card
/// must not hide A from whatever does match. (A sequence that overruns the gap
/// has already started over.)
pub(crate) fn order_pending(spec: OrderSpec, track: &OrderTrack, held: &[bool], window_s: f32) -> bool {
    let n = held.len();
    if !spec.hold_back || track.progress == 0 || track.progress >= n {
        return false;
    }
    match spec.kind {
        OrderKind::Sequence => true,
        OrderKind::Chord => track.since_step <= window_s && !held[track.progress..].contains(&true),
    }
}

/// Where a press mode stands on the current press, read from its slots after
/// this tick's `apply_press_mode`: (still deciding, fired). The timed modes
/// decide over time; every other mode fires exactly while the input is held.
fn press_decision(mode: PressMode, raw: bool, slots: &[f32], window_s: f32) -> (bool, bool) {
    match mode {
        // slots[1] = press length so far (∞ once too long), slots[3] = replay
        // time left after a qualifying tap.
        PressMode::Short => (raw && slots[3] <= 0.0 && slots[1].is_finite(), !raw && slots[3] > 0.0),
        // slots[1] = press length so far; fires once it reaches the window.
        PressMode::Long => {
            let fired = raw && window_s > 0.0 && slots[1] >= window_s;
            (raw && !fired, fired)
        }
        // slots[3]: 1 = first press, 2 = released and waiting, 3 = second press.
        PressMode::Double => {
            let s = slots[3] as i32;
            (s == 1 || s == 2, s == 3)
        }
        _ => (false, raw),
    }
}

/// A deciding card's outcome this tick, used in place of the Remapper's own
/// per-card evaluation.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct CardVerdict {
    /// The press-mode gate: what fires the card's outputs.
    pub(crate) effective: bool,
    /// Whether the card owns (suppresses) its inputs right now.
    pub(crate) held_now: bool,
    /// Order-aware: outranks an order-agnostic card over the same inputs.
    pub(crate) ordered: bool,
}

/// Inputs the hold-back hides (with each one's stick cap) or replays this tick.
#[derive(Default)]
pub(crate) struct HoldBackView {
    pub(crate) hidden: HashMap<String, f32>,
    pub(crate) replayed: HashSet<String>,
}

/// Advance every input's hold-back state by a tick.
///
/// `pending`: inputs a deciding hold-back card holds so far, each flagged true
///   when an order-aware card holds it (hide fully) rather than only timed ones
///   (cap a stick direction where it is).
/// `fired_hold`: every input of a hold-back card that's fired — consumed, and
///   kept hidden until released even once the card lets go.
/// `fired_any`: every input of any fired deciding card; a withheld press one of
///   them used is consumed instead of replayed.
/// `down`: inputs a deciding card counts as held (e.g. past its threshold), on
///   top of each input's own on/off state.
pub(crate) fn hold_back_tick(
    states: &mut HashMap<String, HoldBackPin>,
    pending: &HashMap<&str, bool>,
    fired_hold: &HashSet<&str>,
    fired_any: &HashSet<&str>,
    down: &HashSet<&str>,
    upstream: &HashMap<String, Signal>,
    dt: f32,
) -> HoldBackView {
    let mut pins: HashSet<String> = states.keys().cloned().collect();
    pins.extend(pending.keys().chain(fired_hold.iter()).map(|p| p.to_string()));

    let mut view = HoldBackView::default();
    for p in pins {
        let held = down.contains(p.as_str())
            || upstream.get(&p).map(|s| s.as_bool()).unwrap_or(false);
        let waiting = pending.get(p.as_str()).copied();
        // How far a stick direction still shows once hidden: not at all for an
        // order-aware card, else as far as it's pushed right now.
        let cap_now = |full: bool| if full { 0.0 } else { analog_cardinal_input_value(upstream, &p) };
        let next = match states.get(&p).copied().unwrap_or_default() {
            HoldBackPin::Idle => {
                if held && fired_hold.contains(p.as_str()) {
                    HoldBackPin::Consumed { cap: 0.0 }
                } else if let (true, Some(full)) = (held, waiting) {
                    HoldBackPin::Withheld { held_s: dt, released: false, cap: cap_now(full) }
                } else {
                    HoldBackPin::Idle
                }
            }
            HoldBackPin::Withheld { held_s, released, cap } => {
                if fired_any.contains(p.as_str()) {
                    if held { HoldBackPin::Consumed { cap } } else { HoldBackPin::Idle }
                } else if let Some(full) = waiting {
                    let cap = if full { 0.0 } else { cap };
                    match (held, released) {
                        (true, false) => HoldBackPin::Withheld { held_s: held_s + dt, released: false, cap },
                        // Pressed again while the card still waits: a new press.
                        (true, true) => HoldBackPin::Withheld { held_s: dt, released: false, cap: cap_now(full) },
                        (false, _) => HoldBackPin::Withheld { held_s, released: true, cap },
                    }
                } else if held {
                    // The card gave up while the input is still down: live now.
                    HoldBackPin::Idle
                } else if replayable(upstream, &p) {
                    HoldBackPin::Replaying { remaining_s: held_s.max(PRESS_TRIGGER_PULSE_S) }
                } else {
                    HoldBackPin::Idle
                }
            }
            HoldBackPin::Consumed { cap } => {
                if held { HoldBackPin::Consumed { cap } } else { HoldBackPin::Idle }
            }
            HoldBackPin::Replaying { remaining_s } => {
                if held {
                    // A fresh press cuts the replay short and counts on its own.
                    match waiting {
                        Some(full) => HoldBackPin::Withheld { held_s: dt, released: false, cap: cap_now(full) },
                        None => HoldBackPin::Idle,
                    }
                } else if remaining_s > dt {
                    HoldBackPin::Replaying { remaining_s: remaining_s - dt }
                } else {
                    HoldBackPin::Idle
                }
            }
        };
        match next {
            HoldBackPin::Withheld { cap, .. } | HoldBackPin::Consumed { cap } => {
                view.hidden.insert(p.clone(), cap);
            }
            HoldBackPin::Replaying { .. } => {
                view.replayed.insert(p.clone());
            }
            HoldBackPin::Idle => {}
        }
        if next == HoldBackPin::Idle {
            states.remove(&p);
        } else {
            states.insert(p, next);
        }
    }
    view
}

/// A press that can be replayed late: a plain button. A stick direction or
/// trigger can't be — there's no deflection to play back — so it just stays
/// hidden until the card gives up, then goes live if still pushed.
fn replayable(upstream: &HashMap<String, Signal>, pin: &str) -> bool {
    analog_axis_for_cardinal(pin).is_none() && matches!(upstream.get(pin), Some(Signal::Bool(_)))
}

/// Mark `pin` as held back by a deciding card; `full` (an order-aware card)
/// wins over a timed card's cap.
fn hold_pin<'a>(pending: &mut HashMap<&'a str, bool>, pin: &'a str, full: bool) {
    *pending.entry(pin).or_insert(full) |= full;
}

/// What [`remapper_hold_back_pass`] hands the rest of the Remapper.
pub(crate) struct HoldBackPass {
    /// Each deciding card's outcome; `None` for every other card.
    pub(crate) verdicts: Vec<Option<CardVerdict>>,
    /// The inputs everything else on the node reads.
    pub(crate) view: HashMap<String, Signal>,
    /// Inputs held back this tick. The caller marks them consumed, so a
    /// downstream Combiner with a raw-device port doesn't pick them up there.
    pub(crate) hidden: HashSet<String>,
}

/// Run every deciding card for a tick, then build the view the rest of the node
/// reads: `upstream` itself unless a hold-back is hiding or replaying input.
pub(crate) fn remapper_hold_back_pass(
    mappings: &[Value],
    upstream: HashMap<String, Signal>,
    ns: &mut NodeState,
    dt: f32,
) -> HoldBackPass {
    let specs: Vec<Option<DecidingSpec>> = mappings.iter().map(deciding_spec).collect();
    if specs.iter().all(Option::is_none) {
        ns.order_state.clear();
        ns.move_state.clear();
        ns.hold_back.clear();
        return HoldBackPass { verdicts: vec![None; mappings.len()], view: upstream, hidden: HashSet::new() };
    }
    if ns.order_state.len() < mappings.len() {
        ns.order_state.resize(mappings.len(), OrderTrack::default());
    }
    if ns.move_state.len() < mappings.len() {
        ns.move_state.resize(mappings.len(), MoveTrack::default());
    }

    let mut verdicts: Vec<Option<CardVerdict>> = vec![None; mappings.len()];
    let mut pending: HashMap<&str, bool> = HashMap::new();
    let mut fired_hold: HashSet<&str> = HashSet::new();
    let mut fired_any: HashSet<&str> = HashSet::new();
    let mut down: HashSet<&str> = HashSet::new();
    for (i, m) in mappings.iter().enumerate() {
        let Some(spec) = specs[i] else { continue; };
        let ordered = spec.order.is_some();
        let pins: Vec<&str> = m.get("in").and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        if pins.is_empty() {
            verdicts[i] = Some(CardVerdict { ordered, ..Default::default() });
            continue;
        }
        let press = PressParams::from_card(m);
        let window_s = press.window_ms.max(0.0) / 1000.0;

        // One analog input in Short / Sequence mode: time the whole move.
        let is_sequence = matches!(press.mode(), PressMode::Sequence);
        if let (Some(pin), true) = (
            single_analog_input(&pins),
            is_sequence || matches!(press.mode(), PressMode::Short),
        ) {
            let shape = MappingShape::from_card(m);
            let value = analog_in_value(&upstream, pin).unwrap_or(0.0);
            let engaged = value > 0.0 && shape.shaped(value) > 0.0;
            let past = shape.analog_gate(&upstream, pin)
                .unwrap_or_else(|| upstream.get(pin).map(|s| s.as_bool()).unwrap_or(false));
            // The press is the whole move, so it's down from where it leaves zero.
            if engaged || past {
                down.insert(pin);
            }
            let track = &mut ns.move_state[i];
            let flick = move_tick(track, engaged, past, window_s, dt);
            let (effective, deciding, fired, held_now) = if is_sequence {
                // Reached the threshold in time: on while it stays past it.
                // Held back (with the toggle) while still on its way there.
                let fired = track.phase == MovePhase::Fast && past;
                let deciding = spec.hold_back() && track.phase == MovePhase::Moving;
                (press.gate(fired, press_state_get(ns, i), dt), deciding, fired, fired)
            } else {
                // A flick in and back out inside the gap plays a tap as long
                // as it stayed past the threshold; held back while it's past
                // and could still make it.
                if let Some(above_s) = flick {
                    track.replay_s = above_s.max(PRESS_TRIGGER_PULSE_S);
                }
                let fired = track.replay_s > 0.0;
                let deciding = track.phase == MovePhase::Fast && past && track.since_start <= window_s;
                (fired, deciding, fired, false)
            };
            if deciding {
                hold_pin(&mut pending, pin, ordered);
            }
            if fired {
                fired_any.insert(pin);
                if spec.hold_back() {
                    fired_hold.insert(pin);
                }
            }
            verdicts[i] = Some(CardVerdict { effective, held_now, ordered });
            continue;
        }

        // Is the input there: the order match for an order-aware card, the
        // chord (or stick gesture) for any other.
        let raw = match spec.order {
            Some(order) => {
                let shape = MappingShape::from_card(m);
                let held: Vec<bool> = pins.iter().map(|p| {
                    shape.analog_gate(&upstream, p)
                        .unwrap_or_else(|| upstream.get(*p).map(|s| s.as_bool()).unwrap_or(false))
                }).collect();
                down.extend(pins.iter().zip(&held).filter(|(_, h)| **h).map(|(p, _)| *p));
                let track = &mut ns.order_state[i];
                let matched = order_tick(order.kind, track, &pins, &held, window_s, dt);
                if !matched && order_pending(order, track, &held, window_s) {
                    for p in &pins[..track.progress] {
                        hold_pin(&mut pending, p, true);
                    }
                }
                matched
            }
            None => {
                let raw = chord_raw_held(m, i, &pins, &upstream, ns);
                if raw {
                    down.extend(pins.iter().copied());
                }
                raw
            }
        };

        // The press mode on top.
        let slots = press_state_get(ns, i);
        let effective = press.gate(raw, slots, dt);
        let (deciding, fired) = press_decision(press.mode(), raw, slots, window_s);
        if deciding {
            for p in &pins {
                hold_pin(&mut pending, p, ordered);
            }
        }
        if fired {
            fired_any.extend(pins.iter().copied());
            if spec.hold_back() {
                fired_hold.extend(pins.iter().copied());
            }
        }
        verdicts[i] = Some(CardVerdict { effective, held_now: fired && raw, ordered });
    }

    let hb = hold_back_tick(&mut ns.hold_back, &pending, &fired_hold, &fired_any, &down, &upstream, dt);
    let hidden: HashSet<String> = hb.hidden.keys().cloned().collect();
    if hb.hidden.is_empty() && hb.replayed.is_empty() {
        return HoldBackPass { verdicts, view: upstream, hidden };
    }

    // Per stick / D-pad axis: the (negative, positive) side limits its hidden
    // directions set. A D-pad direction isn't analog, so its cap is always 0.
    let mut axis_caps: HashMap<&'static str, (Option<f32>, Option<f32>)> = HashMap::new();
    for (pin, cap) in &hb.hidden {
        let Some((axis, sign)) = cardinal_axis_for_suppression(pin) else { continue; };
        let sides = axis_caps.entry(axis).or_insert((None, None));
        let side = if sign > 0.0 { &mut sides.1 } else { &mut sides.0 };
        *side = Some(side.map_or(*cap, |c| c.min(*cap)));
    }
    let limit = |v: f32, (neg, pos): (Option<f32>, Option<f32>)| -> f32 {
        if v > 0.0 { pos.map_or(v, |c| v.min(c)) } else { neg.map_or(v, |c| v.max(-c)) }
    };
    let mut view = upstream;
    for (pin, sig) in view.iter_mut() {
        if hb.hidden.contains_key(pin) {
            *sig = sig.zeroed();
            continue;
        }
        match *sig {
            Signal::Float(v) => {
                if let Some(&caps) = axis_caps.get(pin.as_str()) {
                    *sig = Signal::Float(limit(v, caps));
                }
            }
            Signal::Vec2(v) => {
                let axes = match pin.as_str() {
                    "left_stick"  => ("left_stick_x",  "left_stick_y"),
                    "right_stick" => ("right_stick_x", "right_stick_y"),
                    "dpad"        => ("dpad_x",        "dpad_y"),
                    _ => continue,
                };
                let (cx, cy) = (axis_caps.get(axes.0), axis_caps.get(axes.1));
                if cx.is_some() || cy.is_some() {
                    *sig = Signal::Vec2(Vec2::new(
                        cx.map_or(v.x, |c| limit(v.x, *c)),
                        cy.map_or(v.y, |c| limit(v.y, *c)),
                    ));
                }
            }
            _ => {}
        }
    }
    for p in &hb.replayed {
        view.insert(p.clone(), Signal::Bool(true));
    }
    HoldBackPass { verdicts, view, hidden }
}
