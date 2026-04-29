use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use flexinput_core::{Signal, SignalType};
use serde_json::Value;

use crate::graph::{NodeSnap, ProcessingGraph};
use crate::state::NodeState;

// ── Public output type ────────────────────────────────────────────────────────

pub struct TickOutput {
    /// Latest output per (node_uid, output_pin). Excludes device.source (UI evaluates fresh).
    pub outputs: HashMap<(usize, usize), Option<Signal>>,
    /// Per display node: one scope sample for this tick (uid, per-channel values).
    pub scope_samples: Vec<(usize, Vec<Option<f32>>)>,
    /// Latest inputs per display/response_curve node for UI readout rendering.
    pub last_inputs: HashMap<usize, Vec<Option<Signal>>>,
    /// Latest signals destined for each (device_id, pin_id) sink slot.
    pub sink_outputs: HashMap<(String, String), Signal>,
}

fn combine_signals(a: Signal, b: Signal) -> Signal {
    match (a, b) {
        (Signal::Float(x), Signal::Float(y)) => Signal::Float(x + y),
        (Signal::Vec2(x),  Signal::Vec2(y))  => Signal::Vec2(x + y),
        (Signal::Bool(x),  Signal::Bool(y))  => Signal::Bool(x || y),
        (Signal::Int(x),   Signal::Int(y))   => Signal::Int(x + y),
        (_, b) => b,
    }
}

// ── Main graph tick ───────────────────────────────────────────────────────────

pub fn eval_graph_tick(
    graph: &ProcessingGraph,
    state: &mut HashMap<usize, NodeState>,
    dev_sigs: &HashMap<(String, String), Signal>,
    dt: f32,
) -> TickOutput {
    let n = graph.nodes.len();
    let mut computed: Vec<Vec<Option<Signal>>> = vec![vec![]; n];

    let mut outputs: HashMap<(usize, usize), Option<Signal>> = HashMap::new();
    let mut scope_samples: Vec<(usize, Vec<Option<f32>>)> = Vec::new();
    let mut last_inputs: HashMap<usize, Vec<Option<Signal>>> = HashMap::new();
    let mut sink_outputs: HashMap<(String, String), Signal> = HashMap::new();

    for (idx, snap) in graph.nodes.iter().enumerate() {
        // ── device.sink: collect combined inputs, populate sink_outputs ──────
        if let Some(ref st) = snap.sink_target {
            // Direct-wire inputs (possibly multi-source per pin, combined additively).
            for (in_idx, pin_id) in st.pin_ids.iter().enumerate() {
                if pin_id.is_empty() { continue; }
                let mut combined: Option<Signal> = None;
                if let Some(sources) = st.multi_sources.get(in_idx) {
                    for &(src_idx, out_pin) in sources {
                        if let Some(Some(sig)) = computed.get(src_idx).and_then(|v| v.get(out_pin)) {
                            combined = Some(match combined {
                                None => *sig,
                                Some(prev) => combine_signals(prev, *sig),
                            });
                        }
                    }
                }
                if let Some(sig) = combined {
                    sink_outputs.insert((st.device_id.clone(), pin_id.clone()), sig);
                }
            }
            // AutoMap: name-match source device pins → sink device pins.
            if let Some((ref src_dev, ref src_pins)) = st.automap_source {
                let direct: HashSet<&str> = st.pin_ids.iter().map(|s| s.as_str()).collect();
                for src_pin in src_pins {
                    if src_pin == "automap_out" { continue; }
                    // Direct-wire connection already covers this pin — skip.
                    if direct.contains(src_pin.as_str()) { continue; }
                    if let Some(&sig) = dev_sigs.get(&(src_dev.clone(), src_pin.clone())) {
                        sink_outputs
                            .entry((st.device_id.clone(), src_pin.clone()))
                            .or_insert(sig);
                    }
                }
            }
            computed[idx] = vec![];
            continue; // no further processing for sink nodes
        }

        let inputs: Vec<Option<Signal>> = snap.input_sources.iter()
            .map(|src| src.and_then(|(src_idx, out_pin)| {
                computed.get(src_idx).and_then(|v| v.get(out_pin)).copied().flatten()
            }))
            .collect();

        let node_state = state.entry(snap.node_uid).or_insert_with(NodeState::default);

        // Apply any pending state override (e.g. counter reset from UI).
        if let Some(ref vals) = snap.aux_f32_override {
            node_state.aux_f32 = vals.clone();
        }

        let node_outputs = compute_node(snap, &inputs, node_state, dev_sigs, dt);

        match snap.module_id.as_str() {
            "display.oscilloscope" | "display.readout" => {
                let sample = inputs.iter().map(|s| sig_to_f32(*s)).collect();
                scope_samples.push((snap.node_uid, sample));
                last_inputs.insert(snap.node_uid, inputs.clone());
            }
            "display.vectorscope" => {
                let sample = inputs.iter().flat_map(|sig| match sig {
                    Some(Signal::Vec2(v)) => [Some(v.x), Some(v.y)],
                    _ => [None, None],
                }).collect();
                scope_samples.push((snap.node_uid, sample));
                last_inputs.insert(snap.node_uid, inputs.clone());
            }
            "module.response_curve" | "module.vec_response_curve" => {
                last_inputs.insert(snap.node_uid, inputs.clone());
            }
            // Export outputs (not inputs) so the UI body can show a live readout.
            "processing.gyro_3dof" => {
                last_inputs.insert(snap.node_uid, node_outputs.clone());
            }
            _ => {}
        }

        // Exclude device.source from the exported outputs; UI evaluates those fresh.
        if snap.module_id != "device.source" {
            for (out_pin, sig) in node_outputs.iter().enumerate() {
                outputs.insert((snap.node_uid, out_pin), *sig);
            }
        }

        computed[idx] = node_outputs;
    }

    TickOutput { outputs, scope_samples, last_inputs, sink_outputs }
}

// ── Per-node dispatch ─────────────────────────────────────────────────────────

fn compute_node(
    snap: &NodeSnap,
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    dev_sigs: &HashMap<(String, String), Signal>,
    dt: f32,
) -> Vec<Option<Signal>> {
    match snap.module_id.as_str() {
        "device.source" => {
            let dev_id = snap.device_id.as_deref().unwrap_or("");
            (0..snap.n_outputs).map(|i| {
                let pin_id = snap.output_pin_ids.get(i).map(|s| s.as_str()).unwrap_or("");
                if pin_id.is_empty() { return None; }
                dev_sigs.get(&(dev_id.to_string(), pin_id.to_string())).copied()
            }).collect()
        }
        "module.constant" | "module.knob" => {
            let v = snap.params.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            vec![Some(Signal::Float(v))]
        }
        "module.switch" => {
            let a = snap.params.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
            vec![Some(Signal::Bool(a))]
        }
        "generator.oscillator" => {
            let out = compute_oscillator(inputs, state, &snap.params, dt);
            state.last_signals = out.clone();
            out
        }
        "module.delay" => {
            let out = compute_delay(inputs, state, &snap.params);
            state.last_signals = out.clone();
            out
        }
        "module.average" => {
            let out = compute_average(inputs, state, &snap.params);
            state.last_signals = out.clone();
            out
        }
        "module.dc_filter" => {
            let out = compute_dc_filter(inputs, state, &snap.params, dt);
            state.last_signals = out.clone();
            out
        }
        "logic.has_changed" => {
            let out = compute_has_changed(inputs, state);
            state.last_signals = out.clone();
            out
        }
        "logic.delay" => {
            let out = compute_logic_delay(inputs, state, &snap.params, dt);
            state.last_signals = out.clone();
            out
        }
        "logic.counter" => {
            let out = compute_counter(inputs, state, &snap.params);
            state.last_signals = out.clone();
            out
        }
        "processing.gyro_3dof" => {
            let out = compute_gyro_3dof(inputs, state, &snap.params, dev_sigs, dt);
            state.last_signals = out.clone();
            out
        }
        "module.response_curve" | "module.vec_response_curve" => {
            state.last_signals = inputs.to_vec();
            (0..snap.n_outputs).map(|out_idx| {
                eval_pure(&snap.module_id, out_idx, inputs, &snap.params, snap.n_outputs)
            }).collect()
        }
        "display.oscilloscope" | "display.vectorscope" | "display.readout" | "device.sink" => vec![],
        id => {
            (0..snap.n_outputs).map(|out_idx| {
                eval_pure(id, out_idx, inputs, &snap.params, snap.n_outputs)
            }).collect()
        }
    }
}

// ── Pure module evaluation ────────────────────────────────────────────────────

pub fn eval_pure(
    id: &str,
    out_idx: usize,
    inputs: &[Option<Signal>],
    params: &HashMap<String, Value>,
    n_outputs: usize,
) -> Option<Signal> {
    let param_f = |name: &str, default: f32| -> f32 {
        params.get(name).and_then(|v| v.as_f64()).map(|f| f as f32).unwrap_or(default)
    };

    match id {
        "math.add" => Some(Signal::Float((0..inputs.len()).map(|i| get_f(inputs, i, 0.0)).sum())),
        "math.subtract" => {
            let first = get_f(inputs, 0, 0.0);
            let rest: f32 = (1..inputs.len()).map(|i| get_f(inputs, i, 0.0)).sum();
            Some(Signal::Float(first - rest))
        }
        "math.multiply" => {
            let first = get_f(inputs, 0, 0.0);
            let rest: f32 = (1..inputs.len()).map(|i| get_f(inputs, i, 1.0)).product();
            Some(Signal::Float(first * rest))
        }
        "math.divide" => {
            let mut v = get_f(inputs, 0, 0.0);
            for i in 1..inputs.len() {
                let d = get_f(inputs, i, 1.0);
                v = if d == 0.0 { 0.0 } else { v / d };
            }
            Some(Signal::Float(v))
        }
        "math.abs"    => Some(Signal::Float(get_f(inputs, 0, 0.0).abs())),
        "math.negate" => Some(Signal::Float(-get_f(inputs, 0, 0.0))),
        "math.clamp"  => {
            let v   = get_f(inputs, 0, 0.0);
            let min = if inputs.get(1).and_then(|s| *s).is_some() { get_f(inputs, 1, -1.0) } else { param_f("min", -1.0) };
            let max = if inputs.get(2).and_then(|s| *s).is_some() { get_f(inputs, 2,  1.0) } else { param_f("max",  1.0) };
            Some(Signal::Float(v.clamp(min, max)))
        }
        "math.map_range" => {
            let v       = get_f(inputs, 0, 0.0);
            let in_min  = if inputs.get(1).and_then(|s| *s).is_some() { get_f(inputs, 1, -1.0) } else { param_f("in_min",  -1.0) };
            let in_max  = if inputs.get(2).and_then(|s| *s).is_some() { get_f(inputs, 2,  1.0) } else { param_f("in_max",   1.0) };
            let out_min = if inputs.get(3).and_then(|s| *s).is_some() { get_f(inputs, 3, -1.0) } else { param_f("out_min", -1.0) };
            let out_max = if inputs.get(4).and_then(|s| *s).is_some() { get_f(inputs, 4,  1.0) } else { param_f("out_max",  1.0) };
            let t = if (in_max - in_min).abs() < f32::EPSILON { 0.0 }
                    else { (v - in_min) / (in_max - in_min) };
            Some(Signal::Float(out_min + t * (out_max - out_min)))
        }
        "logic.and"       => Some(Signal::Bool(get_b(inputs, 0, false) && get_b(inputs, 1, false))),
        "logic.or"        => Some(Signal::Bool(get_b(inputs, 0, false) || get_b(inputs, 1, false))),
        "logic.not"       => Some(Signal::Bool(!get_b(inputs, 0, false))),
        "logic.xor"       => Some(Signal::Bool(get_b(inputs, 0, false) ^ get_b(inputs, 1, false))),
        "logic.equal"     => Some(Signal::Bool(get_f(inputs, 0, 0.0) == get_f(inputs, 1, 0.0))),
        "logic.not_equal" => Some(Signal::Bool(get_f(inputs, 0, 0.0) != get_f(inputs, 1, 0.0))),
        "logic.greater_than" => {
            let (a, b) = (get_f(inputs, 0, 0.0), get_f(inputs, 1, 0.0));
            let or_eq = params.get("or_equal").and_then(|v| v.as_bool()).unwrap_or(false);
            Some(Signal::Bool(if or_eq { a >= b } else { a > b }))
        }
        "logic.less_than" => {
            let (a, b) = (get_f(inputs, 0, 0.0), get_f(inputs, 1, 0.0));
            let or_eq = params.get("or_equal").and_then(|v| v.as_bool()).unwrap_or(false);
            Some(Signal::Bool(if or_eq { a <= b } else { a < b }))
        }
        "module.selector" => {
            if out_idx != 0 { return None; }
            let n_inputs = inputs.len().saturating_sub(1);
            let sel = get_f(inputs, 0, 0.0);
            let interp = params.get("interpolate").and_then(|v| v.as_bool()).unwrap_or(false);
            if interp && n_inputs >= 2 {
                let pos = sel.clamp(0.0, 1.0) * (n_inputs - 1) as f32;
                let lo = pos.floor() as usize;
                let hi = (lo + 1).min(n_inputs - 1);
                let t = pos.fract();
                let lo_v = inputs.get(lo + 1).and_then(|s| *s).map(|s| s.as_float()).unwrap_or(0.0);
                let hi_v = inputs.get(hi + 1).and_then(|s| *s).map(|s| s.as_float()).unwrap_or(0.0);
                Some(Signal::Float(lo_v * (1.0 - t) + hi_v * t))
            } else {
                let n = n_inputs as f32;
                let idx = (sel.clamp(0.0, 1.0) * n).floor() as usize;
                let idx = idx.min(n_inputs.saturating_sub(1));
                inputs.get(idx + 1).and_then(|s| *s)
            }
        }
        "module.split" => {
            let sel  = get_f(inputs, 0, 0.0);
            let val  = get_f(inputs, 1, 0.0);
            let n    = n_outputs;
            let interp = params.get("interpolate").and_then(|v| v.as_bool()).unwrap_or(false);
            if interp && n >= 2 {
                let pos = sel.clamp(0.0, 1.0) * (n - 1) as f32;
                let lo  = pos.floor() as usize;
                let hi  = (lo + 1).min(n - 1);
                let t   = pos.fract();
                if out_idx == lo && lo == hi { Some(Signal::Float(val)) }
                else if out_idx == lo        { Some(Signal::Float(val * (1.0 - t))) }
                else if out_idx == hi        { Some(Signal::Float(val * t)) }
                else                         { Some(Signal::Float(0.0)) }
            } else {
                let idx = (sel.clamp(0.0, 1.0) * n as f32).floor() as usize;
                let idx = idx.min(n.saturating_sub(1));
                if out_idx == idx { Some(Signal::Float(val)) } else { Some(Signal::Float(0.0)) }
            }
        }
        "module.response_curve" => {
            if out_idx >= n_outputs { return None; }
            let x       = get_f(inputs, out_idx, 0.0);
            let pts     = curve_points_from_params(params);
            let biases  = biases_from_params(params);
            let abs     = params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true);
            let in_max  = params.get("in_max") .and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32;
            let in_min  = params.get("in_min") .and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
            let out_max = params.get("out_max").and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32;
            let out_min = params.get("out_min").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
            Some(Signal::Float(apply_curve(x, &pts, &biases, abs, in_min, in_max, out_min, out_max, read_scale_t(params))))
        }
        "module.vec_response_curve" => {
            if out_idx >= n_outputs { return None; }
            let vec = match inputs.get(out_idx).and_then(|s| *s) {
                Some(Signal::Vec2(v)) => v,
                _ => return Some(Signal::Vec2(glam::Vec2::ZERO)),
            };
            let mag = vec.length();
            if mag < f32::EPSILON { return Some(Signal::Vec2(glam::Vec2::ZERO)); }
            let pts     = curve_points_from_params(params);
            let biases  = biases_from_params(params);
            let in_max  = params.get("in_max") .and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let out_max = params.get("out_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let out_mag = apply_curve(mag, &pts, &biases, true, 0.0, in_max, 0.0, out_max, read_scale_t(params));
            Some(Signal::Vec2(vec / mag * out_mag))
        }
        "module.vec_to_axis" => {
            let vec = match inputs.first().and_then(|s| *s) {
                Some(Signal::Vec2(v)) => v,
                _ => glam::Vec2::ZERO,
            };
            match out_idx { 0 => Some(Signal::Float(vec.x)), 1 => Some(Signal::Float(vec.y)), _ => None }
        }
        "module.axis_to_vec" => {
            if out_idx != 0 { return None; }
            let x = match inputs.first().and_then(|s| *s) { Some(Signal::Float(f)) => f, _ => 0.0 };
            let y = match inputs.get(1).and_then(|s| *s)  { Some(Signal::Float(f)) => f, _ => 0.0 };
            Some(Signal::Vec2(glam::Vec2::new(x, y)))
        }
        _ => None,
    }
}

// ── Stateful compute functions ────────────────────────────────────────────────

fn compute_oscillator(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
    dt: f32,
) -> Vec<Option<Signal>> {
    let shape     = params.get("shape")     .and_then(|v| v.as_str()) .unwrap_or("sine");
    let freq_unit = params.get("freq_unit") .and_then(|v| v.as_str()) .unwrap_or("hz");
    let bipolar   = params.get("bipolar")   .and_then(|v| v.as_bool()).unwrap_or(true);

    let freq_wired  = inputs.get(0).and_then(|s| *s).is_some();
    let phase_wired = inputs.get(1).and_then(|s| *s).is_some();

    let freq_val  = if freq_wired  { get_f(inputs, 0, 1.0) } else { params.get("freq_param") .and_then(|v| v.as_f64()).unwrap_or(1.0) as f32 };
    let phase_off = if phase_wired { get_f(inputs, 1, 0.0) } else { params.get("phase_param").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32 };
    let retrig    = get_b(inputs, 2, false);

    let period_s = match freq_unit {
        "hz" => if freq_val > 0.0 { 1.0 / freq_val } else { 1.0 },
        _    => (freq_val / 1000.0).max(0.0001),
    }.max(0.0001);

    while state.aux_f32.len() < 2 { state.aux_f32.push(0.0); }

    let retrig_edge = retrig && state.aux_f32[1] < 0.5;
    state.aux_f32[1] = if retrig { 1.0 } else { 0.0 };
    if retrig_edge { state.aux_f32[0] = 0.0; }

    state.aux_f32[0] = (state.aux_f32[0] + dt / period_s) % 1.0;
    let phase  = (state.aux_f32[0] + phase_off).rem_euclid(1.0);
    let val    = osc_sample(shape, phase);
    let output = if bipolar { val } else { (val + 1.0) * 0.5 };
    vec![Some(Signal::Float(output))]
}

pub fn osc_sample(shape: &str, phase: f32) -> f32 {
    match shape {
        "sine"     => (phase * std::f32::consts::TAU).sin(),
        "triangle" => if phase < 0.5 { 4.0 * phase - 1.0 } else { 3.0 - 4.0 * phase },
        "saw"      => 2.0 * phase - 1.0,
        "square"   => if phase < 0.5 { 1.0 } else { -1.0 },
        _          => 0.0,
    }
}

fn compute_delay(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
) -> Vec<Option<Signal>> {
    let delay_secs = params.get("delay_ms").and_then(|v| v.as_f64()).unwrap_or(100.0)
        .clamp(0.0, 60_000.0) as f32 / 1000.0;
    let now = Instant::now();

    while state.delay_bufs.len() < inputs.len() {
        state.delay_bufs.push(VecDeque::new());
    }

    let mut results = Vec::with_capacity(inputs.len());
    for (ch, inp) in inputs.iter().enumerate() {
        let Some(v) = sig_to_f32(*inp) else { results.push(None); continue; };
        let buf = &mut state.delay_bufs[ch];
        buf.push_back((now, v));

        let mut output = buf.front().map(|(_, v)| *v);
        for (ts, val) in buf.iter() {
            if now.duration_since(*ts).as_secs_f32() >= delay_secs { output = Some(*val); }
            else { break; }
        }

        let max_age = delay_secs + 1.0;
        while buf.len() > 2 {
            let oldest_age = now.duration_since(buf.front().unwrap().0).as_secs_f32();
            if oldest_age > max_age { buf.pop_front(); } else { break; }
        }

        results.push(output.map(Signal::Float));
    }
    results
}

fn compute_average(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
) -> Vec<Option<Signal>> {
    let buf_size = params.get("buf_size").and_then(|v| v.as_f64())
        .map(|f| f as u64).unwrap_or(10).clamp(1, 10_000) as usize;
    let spike_mad = params.get("spike_mad").and_then(|v| v.as_f64()).unwrap_or(0.0).max(0.0);

    while state.avg_bufs.len() < inputs.len() { state.avg_bufs.push(VecDeque::new()); }

    let mut results = Vec::with_capacity(inputs.len());
    for (ch, inp) in inputs.iter().enumerate() {
        let Some(v) = sig_to_f32(*inp) else { results.push(None); continue; };
        let buf = &mut state.avg_bufs[ch];
        buf.push_back(v);
        while buf.len() > buf_size { buf.pop_front(); }

        let avg = if spike_mad > 0.0 && buf.len() >= 3 {
            let mut sorted: Vec<f32> = buf.iter().cloned().collect();
            sorted.sort_by(|a, b| a.total_cmp(b));
            let median = sorted_median(&sorted);
            let mut devs: Vec<f32> = sorted.iter().map(|&x| (x - median).abs()).collect();
            devs.sort_by(|a, b| a.total_cmp(b));
            let mad = sorted_median(&devs);
            if mad < 1e-9 { buf.iter().sum::<f32>() / buf.len() as f32 }
            else {
                let thresh = spike_mad as f32 * mad;
                let kept: Vec<f32> = buf.iter().cloned().filter(|&x| (x - median).abs() <= thresh).collect();
                if kept.is_empty() { buf.iter().sum::<f32>() / buf.len() as f32 }
                else { kept.iter().sum::<f32>() / kept.len() as f32 }
            }
        } else {
            buf.iter().sum::<f32>() / buf.len() as f32
        };
        results.push(Some(Signal::Float(avg)));
    }
    results
}

fn sorted_median(sorted: &[f32]) -> f32 {
    let n = sorted.len();
    if n == 0 { return 0.0; }
    if n % 2 == 0 { (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0 } else { sorted[n / 2] }
}

const DC_THRESHOLD: f64    = 0.005;
const DC_STABILITY: f64    = 0.02;
const DC_FAST_TC_SECS: f64 = 0.05;

fn compute_dc_filter(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
    dt: f32,
) -> Vec<Option<Signal>> {
    let window_secs = params.get("window_ms").and_then(|v| v.as_f64()).unwrap_or(500.0)
        .clamp(10.0, 60_000.0) as f32 / 1000.0;
    let decay_secs = params.get("decay_ms").and_then(|v| v.as_f64()).unwrap_or(200.0)
        .clamp(10.0, 60_000.0) / 1000.0;

    let dt64       = dt as f64;
    let alpha_fast = 1.0 - (-dt64 / DC_FAST_TC_SECS).exp();
    let alpha_est  = 1.0 - (-dt64 / window_secs as f64).exp();
    let alpha_corr = 1.0 - (-dt64 / decay_secs).exp();
    let blend_step = dt as f64 / decay_secs;

    while state.dc_fast.len()        < inputs.len() { state.dc_fast.push(0.0); }
    while state.dc_estimates.len()   < inputs.len() { state.dc_estimates.push(0.0); }
    while state.dc_corrections.len() < inputs.len() { state.dc_corrections.push(0.0); }
    while state.dc_timers.len()      < inputs.len() { state.dc_timers.push(0.0); }
    while state.dc_frozen.len()      < inputs.len() { state.dc_frozen.push(0.0); }
    while state.dc_blend.len()       < inputs.len() { state.dc_blend.push(0.0); }

    let mut results = Vec::with_capacity(inputs.len());
    for (ch, inp) in inputs.iter().enumerate() {
        let Some(v) = sig_to_f32(*inp) else { results.push(None); continue; };
        let v64 = v as f64;

        state.dc_fast[ch]      += alpha_fast * (v64 - state.dc_fast[ch]);
        state.dc_estimates[ch] += alpha_est  * (v64 - state.dc_estimates[ch]);

        let is_stable  = (state.dc_fast[ch] - state.dc_estimates[ch]).abs() < DC_STABILITY;
        let is_nonzero = state.dc_estimates[ch].abs() > DC_THRESHOLD;

        if is_stable && is_nonzero { state.dc_timers[ch] = (state.dc_timers[ch] + dt).min(window_secs + 1.0); }
        else                       { state.dc_timers[ch] = 0.0; }

        let output = if is_stable {
            if state.dc_timers[ch] >= window_secs {
                state.dc_corrections[ch] += alpha_corr * (state.dc_estimates[ch] - state.dc_corrections[ch]);
            } else {
                state.dc_corrections[ch] += alpha_corr * (0.0 - state.dc_corrections[ch]);
            }
            let out = v64 - state.dc_corrections[ch];
            state.dc_frozen[ch] = out;
            state.dc_blend[ch]  = 0.0;
            out
        } else {
            state.dc_blend[ch] = (state.dc_blend[ch] + blend_step).min(1.0);
            let b   = state.dc_blend[ch];
            let out = state.dc_frozen[ch] * (1.0 - b) + v64 * b;
            state.dc_corrections[ch] = v64 - out;
            out
        };
        results.push(Some(Signal::Float(output as f32)));
    }
    results
}

fn compute_has_changed(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
) -> Vec<Option<Signal>> {
    let cur = inputs.first().copied().flatten();
    while state.prev_signals.len() < 1 { state.prev_signals.push(None); }
    let prev = state.prev_signals[0];
    state.prev_signals[0] = cur;

    let (changed, increased, decreased) = match (prev, cur) {
        (Some(p), Some(c)) => {
            let ch = p != c;
            let (ps, cs) = (sig_scalar(p), sig_scalar(c));
            (ch, cs > ps, cs < ps)
        }
        (None, Some(_)) => (true, false, false),
        _ => (false, false, false),
    };
    vec![Some(Signal::Bool(changed)), Some(Signal::Bool(increased)), Some(Signal::Bool(decreased))]
}

fn compute_logic_delay(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
    dt: f32,
) -> Vec<Option<Signal>> {
    let mode      = params.get("mode").and_then(|v| v.as_str()).unwrap_or("delay_false");
    let time      = params.get("time").and_then(|v| v.as_f64()).unwrap_or(100.0) as f32;
    let use_ms    = params.get("unit").and_then(|v| v.as_str()).unwrap_or("ms") == "ms";
    let threshold = if use_ms { time / 1000.0 } else { time };
    let tick      = if use_ms { dt } else { 1.0 };

    while state.aux_f32.len() < 2 { state.aux_f32.push(0.0); }
    let mode_code = if mode == "delay_true" { 0.0f32 } else { 1.0f32 };
    if state.aux_f32[1] != mode_code {
        state.aux_f32[0] = if mode == "delay_true" { 0.0 } else { threshold };
        state.aux_f32[1] = mode_code;
    }

    let input = inputs.first().copied().flatten()
        .and_then(|s| s.coerce_to(SignalType::Bool))
        .map(|s| matches!(s, Signal::Bool(true)))
        .unwrap_or(false);

    let timer  = &mut state.aux_f32[0];
    let output = match mode {
        "delay_true" => { if input { *timer += tick; *timer >= threshold } else { *timer = 0.0; false } }
        _            => { if input { *timer = 0.0; true } else { *timer += tick; *timer < threshold } }
    };
    vec![Some(Signal::Bool(output))]
}

fn compute_counter(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
) -> Vec<Option<Signal>> {
    let mode       = params.get("mode")      .and_then(|v| v.as_str()) .unwrap_or("loop");
    let normalized = params.get("normalized").and_then(|v| v.as_bool()).unwrap_or(false);

    let step_wired = inputs.get(3).and_then(|s| *s).is_some();
    let min_wired  = inputs.get(4).and_then(|s| *s).is_some();
    let max_wired  = inputs.get(5).and_then(|s| *s).is_some();

    let step = (if step_wired { get_f(inputs, 3, 1.0)  } else { params.get("step_param").and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32 }).max(f32::EPSILON);
    let min  =  if min_wired  { get_f(inputs, 4, 0.0)  } else { params.get("min_param") .and_then(|v| v.as_f64()).unwrap_or(0.0)  as f32 };
    let max  =  if max_wired  { get_f(inputs, 5, 10.0) } else { params.get("max_param") .and_then(|v| v.as_f64()).unwrap_or(10.0) as f32 };

    let max_steps = ((max - min) / step).round().max(0.0) as i32;

    while state.aux_f32.len() < 5 { state.aux_f32.push(0.0); }
    if state.aux_f32[1] == 0.0 { state.aux_f32[1] = 1.0; }

    let inc   = get_b(inputs, 0, false);
    let dec   = get_b(inputs, 1, false);
    let reset = get_b(inputs, 2, false);

    let inc_edge   = inc   && state.aux_f32[2] < 0.5;
    let dec_edge   = dec   && state.aux_f32[3] < 0.5;
    let reset_edge = reset && state.aux_f32[4] < 0.5;

    state.aux_f32[2] = if inc   { 1.0 } else { 0.0 };
    state.aux_f32[3] = if dec   { 1.0 } else { 0.0 };
    state.aux_f32[4] = if reset { 1.0 } else { 0.0 };

    let mut count = state.aux_f32[0] as i32;
    let mut dir   = state.aux_f32[1];

    if reset_edge {
        count = 0; dir = 1.0;
    } else {
        match mode {
            "loop" => {
                if inc_edge { count = (count + 1).rem_euclid(max_steps + 1); }
                if dec_edge { count = (count - 1).rem_euclid(max_steps + 1); }
            }
            "limit" => {
                if inc_edge { count = (count + 1).min(max_steps); }
                if dec_edge { count = (count - 1).max(0); }
            }
            "bounce" => {
                if max_steps > 0 {
                    if inc_edge { count += 1; }
                    if dec_edge { count -= 1; }
                    if count > max_steps { count = 2 * max_steps - count; }
                    if count < 0         { count = -count; }
                }
            }
            _ => {
                if inc_edge { count += 1; }
                if dec_edge { count = (count - 1).max(0); }
            }
        }
    }

    if mode != "unlimited" { count = count.clamp(0, max_steps); }
    state.aux_f32[0] = count as f32;
    state.aux_f32[1] = dir;

    let output = if normalized {
        if max_steps > 0 { count as f32 / max_steps as f32 } else { 0.0 }
    } else {
        min + count as f32 * step
    };
    vec![Some(Signal::Float(output))]
}

fn compute_gyro_3dof(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
    dev_sigs: &HashMap<(String, String), Signal>,
    dt: f32,
) -> Vec<Option<Signal>> {
    let mode = params.get("mode").and_then(|v| v.as_str()).unwrap_or("local");

    let inv = |name: &str| -> f32 {
        if params.get(name).and_then(|v| v.as_bool()).unwrap_or(false) { -1.0 } else { 1.0 }
    };

    // Auto-map path: read all six axes from the connected device.
    let (gx_am, gy_am, gz_am, ax_am, ay_am, az_am) =
        if let Some(dev_id) = params.get("_automap_device_id").and_then(|v| v.as_str()) {
            let get = |pin: &str| -> f32 {
                match dev_sigs.get(&(dev_id.to_string(), pin.to_string())) {
                    Some(Signal::Float(f)) => *f,
                    _ => 0.0,
                }
            };
            let az_raw = match dev_sigs.get(&(dev_id.to_string(), "accel_z".to_string())) {
                Some(Signal::Float(f)) => *f,
                _ => 1.0,
            };
            (get("gyro_x"), get("gyro_y"), get("gyro_z"), get("accel_x"), get("accel_y"), az_raw)
        } else {
            (0.0, 0.0, 0.0, 0.0, 0.0, 1.0)
        };

    // Direct pin overrides (inputs 2–7: Gyro X/Y/Z, Accel X/Y/Z).
    // A wired pin supersedes the auto-map value for that axis only.
    let pin_or = |idx: usize, fallback: f32| -> f32 {
        if inputs.get(idx).and_then(|s| *s).is_some() { get_f(inputs, idx, fallback) } else { fallback }
    };
    let gx = pin_or(2, gx_am) * inv("inv_roll");
    let gy = pin_or(3, gy_am);   // pitch inversion applied to out_y below
    let gz = pin_or(4, gz_am);   // yaw   inversion applied to out_x below
    let ax = pin_or(5, ax_am) * inv("inv_accel_x");
    let ay = pin_or(6, ay_am) * inv("inv_accel_y");
    let az = pin_or(7, az_am) * inv("inv_accel_z");

    // aux_f32: [0]=laser_x, [1]=laser_y, [2]=smooth_gvx, [3]=smooth_gvy, [4]=smooth_gvz, [5]=prev_reset
    while state.aux_f32.len() < 6 { state.aux_f32.push(0.0); }

    // Laser reset: inputs[1] is the optional Reset bool pin.
    let reset_now = get_b(inputs, 1, false);
    let reset_edge = reset_now && state.aux_f32[5] < 0.5;
    state.aux_f32[5] = if reset_now { 1.0 } else { 0.0 };
    if reset_edge { state.aux_f32[0] = 0.0; state.aux_f32[1] = 0.0; }

    let (out_x_raw, out_y_raw) = match mode {
        "player" | "world" => {
            let gyro  = glam::Vec3::new(gx, gy, gz);
            let accel = glam::Vec3::new(ax, ay, az);

            // Low-pass filter the gravity estimate so that fast pitch oscillations do not
            // alias into g_hat, which would cause world_yaw = dot(gyro, g_hat) to oscillate
            // at 2× the pitch frequency — the "figure-8" Lissajous artifact.
            // Player: 1 s time constant tracks slow resting-orientation changes.
            // World:  3 s time constant gives a very stable reference frame.
            let tau = if mode == "world" { 3.0_f32 } else { 1.0_f32 };
            let alpha = 1.0 - (-dt / tau).exp();
            let acc_mag = accel.length();
            if acc_mag > 0.01 {
                let norm = accel / acc_mag;
                state.aux_f32[2] += alpha * (norm.x - state.aux_f32[2]);
                state.aux_f32[3] += alpha * (norm.y - state.aux_f32[3]);
                state.aux_f32[4] += alpha * (norm.z - state.aux_f32[4]);
            }
            let sg = glam::Vec3::new(state.aux_f32[2], state.aux_f32[3], state.aux_f32[4]);
            let sg_len = sg.length();
            let g_hat = if sg_len > 0.01 { sg / sg_len } else { glam::Vec3::new(0.0, 0.0, 1.0) };

            let world_yaw   = gyro.dot(g_hat);
            let gyro_no_yaw = gyro - world_yaw * g_hat;
            (world_yaw, gyro_no_yaw.y)
        }
        "laser" => {
            state.aux_f32[0] += gz * dt;
            state.aux_f32[1] += gy * dt;
            (state.aux_f32[0], state.aux_f32[1])
        }
        _ => (gz, gy), // "local": gz=yaw→X, gy=pitch→Y
    };

    // Apply yaw/pitch inversions to final output only (not inside the dot-product math).
    let out_x = out_x_raw * inv("inv_yaw");
    let out_y = out_y_raw * inv("inv_pitch");

    let out_vec = glam::Vec2::new(out_x, out_y);
    vec![
        Some(Signal::Vec2(out_vec)),
        Some(Signal::Float(out_x)),
        Some(Signal::Float(out_y)),
    ]
}

// ── Curve helpers ─────────────────────────────────────────────────────────────

pub fn sample_curve(pts: &[[f32; 2]], x: f32, biases: &[f32]) -> f32 {
    match pts.len() {
        0 => x,
        1 => pts[0][1],
        _ => {
            if x <= pts[0][0] { return pts[0][1]; }
            let last = pts.len() - 1;
            if x >= pts[last][0] { return pts[last][1]; }
            let seg = pts.windows(2).position(|w| x <= w[1][0]).unwrap_or(last - 1);
            let p1 = pts[seg]; let p2 = pts[seg + 1];
            let t    = (x - p1[0]) / (p2[0] - p1[0]);
            let bias = biases.get(seg).copied().unwrap_or(0.0);
            let base = p1[1] + (p2[1] - p1[1]) * t;
            base + bias * 4.0 * t * (1.0 - t)
        }
    }
}

pub fn apply_curve(
    x: f32, pts: &[[f32; 2]], biases: &[f32],
    absolute: bool, in_min: f32, in_max: f32, out_min: f32, out_max: f32, scale_t: f32,
) -> f32 {
    if absolute {
        let sign     = if x < 0.0 { -1.0f32 } else { 1.0 };
        let abs_max  = in_max.abs().max(in_min.abs()).max(f32::EPSILON);
        let abs_norm = (x.abs() / abs_max).clamp(0.0, 1.0);
        let scaled   = curve_scale(abs_norm, scale_t);
        let curve_y  = sample_curve(pts, scaled, biases).clamp(0.0, 1.0);
        let out_y    = curve_scale_inv(curve_y, scale_t);
        sign * out_y * out_max.abs().max(out_min.abs())
    } else {
        let in_range  = (in_max - in_min).abs().max(f32::EPSILON);
        let out_range = out_max - out_min;
        let norm      = ((x - in_min) / in_range * 2.0 - 1.0).clamp(-1.0, 1.0);
        let sign      = if norm < 0.0 { -1.0f32 } else { 1.0 };
        let scaled    = sign * curve_scale(norm.abs(), scale_t);
        let curve_y   = sample_curve(pts, scaled, biases);
        let sign_out  = if curve_y < 0.0 { -1.0f32 } else { 1.0 };
        let out_y     = sign_out * curve_scale_inv(curve_y.abs(), scale_t);
        out_min + (out_y.clamp(-1.0, 1.0) + 1.0) * 0.5 * out_range
    }
}

pub fn curve_scale(x: f32, t: f32) -> f32 {
    if t.abs() < 1e-4 { return x; }
    x.clamp(0.0, 1.0).powf(2.0f32.powf(t * 3.0))
}

pub fn curve_scale_inv(y: f32, t: f32) -> f32 {
    if t.abs() < 1e-4 { return y; }
    y.clamp(0.0, 1.0).powf(1.0 / 2.0f32.powf(t * 3.0))
}

pub fn curve_points_from_params(params: &HashMap<String, Value>) -> Vec<[f32; 2]> {
    let absolute = params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true);
    params.get("points").and_then(|v| v.as_array()).map(|arr| {
        arr.iter().filter_map(|pt| {
            let a = pt.as_array()?;
            Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
        }).collect()
    }).unwrap_or_else(|| {
        if absolute { vec![[0.0, 0.0], [1.0, 1.0]] } else { vec![[-1.0, -1.0], [1.0, 1.0]] }
    })
}

pub fn biases_from_params(params: &HashMap<String, Value>) -> Vec<f32> {
    params.get("biases").and_then(|v| v.as_array()).map(|arr| {
        arr.iter().map(|v| v.as_f64().unwrap_or(0.0) as f32).collect()
    }).unwrap_or_default()
}

pub fn read_scale_t(params: &HashMap<String, Value>) -> f32 {
    params.get("scale_t").and_then(|v| v.as_f64()).map(|f| f as f32)
        .unwrap_or_else(|| match params.get("in_scale").and_then(|v| v.as_i64()).unwrap_or(0) {
            1 => -0.5,
            2 =>  0.5,
            _ =>  0.0,
        })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn sig_to_f32(s: Option<Signal>) -> Option<f32> {
    match s {
        Some(Signal::Float(f)) => Some(f),
        Some(Signal::Bool(b))  => Some(if b { 1.0 } else { 0.0 }),
        Some(Signal::Vec2(v))  => Some(v.length()),
        Some(Signal::Int(i))   => Some(i as f32),
        None => None,
    }
}

pub fn get_f(inputs: &[Option<Signal>], i: usize, default: f32) -> f32 {
    inputs.get(i).and_then(|s| *s).map(|s| s.as_float()).unwrap_or(default)
}

pub fn get_b(inputs: &[Option<Signal>], i: usize, default: bool) -> bool {
    inputs.get(i).and_then(|s| *s).map(|s| s.as_bool()).unwrap_or(default)
}

fn sig_scalar(s: Signal) -> f32 {
    match s {
        Signal::Float(f) => f,
        Signal::Int(i)   => i as f32,
        Signal::Bool(b)  => if b { 1.0 } else { 0.0 },
        Signal::Vec2(v)  => v.length(),
    }
}
