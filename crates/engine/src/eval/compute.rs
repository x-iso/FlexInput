//! The per-node computation: `compute_node` dispatch, the `eval_pure`
//! entry point, and the stateful generators and filters behind them
//! (oscillator, envelope, delay, average, DC filter, response curve,
//! change detector, logic delay, counter).

use super::*;

// ── Per-node dispatch ─────────────────────────────────────────────────────────
pub(crate) fn compute_node(
    snap: &NodeSnap,
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &HashMap<(String, String), Signal>,
    dt: f32,
) -> Vec<Option<Signal>> {
    puffin::profile_function!();
    match snap.module_id.as_str() {
        "device.source" => {
            // Deadzone + gyro multiplier already applied in `preprocess_dev_sigs`
            // at the top of `eval_graph_tick` so AutoMap/splitter/collector see
            // the same processed values via raw dev_sigs reads.
            let dev_id = snap.device_id.as_deref().unwrap_or("");
            (0..snap.n_outputs).map(|i| {
                let pin_id = snap.output_pin_ids.get(i).map(|s| s.as_str()).unwrap_or("");
                if pin_id.is_empty() { return None; }
                dev_sigs.get(&(dev_id.to_string(), pin_id.to_string())).copied()
            }).collect()
        }
        "module.automap_split" => {
            let dev_id = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("");
            // The collector_id (set by build_processing_graph) is the closest
            // upstream collector in the AutoMap wire chain. Splitter prefers its
            // injected/overridden signals over the raw device samples so the
            // probe reflects the most recent state along the chain.
            let collector_id = snap.params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("");
            (0..snap.n_outputs).map(|i| {
                let pin_id = snap.output_pin_ids.get(i).map(|s| s.as_str()).unwrap_or("");
                // "automap_pass" or empty = the AutoMap passthrough slot — no signal value.
                if pin_id.is_empty() || pin_id == "automap_pass" { return None; }
                if !collector_id.is_empty() {
                    if let Some(&sig) = collector_sigs.get(&(collector_id.to_string(), pin_id.to_string())) {
                        return Some(sig);
                    }
                }
                dev_sigs.get(&(dev_id.to_string(), pin_id.to_string())).copied()
            }).collect()
        }
        "module.input_viewer" => {
            // Pure display: the board renders from live signals on the UI
            // thread; output 0 is the AutoMap passthrough (resolved by the
            // device walk, carries no Signal).
            (0..snap.n_outputs).map(|_| None).collect()
        }
        "module.menu" => {
            // Fallback only — both eval loops intercept module.menu with
            // eval_menu_node before compute_node runs. Idle-typed outputs:
            // closed, nothing hovered, all zone pins off. Slot 0 stays the
            // AutoMap passthrough sentinel (no Signal). Zone pins share the
            // Touch Zones id vocabulary (f0z{id}_x/y/act + f0c = Select).
            use flexinput_core::menu as fm;
            use flexinput_core::touchzones as tz;
            (0..snap.n_outputs).map(|i| {
                let pin_id = snap.output_pin_ids.get(i).map(|s| s.as_str()).unwrap_or("");
                match fm::parse_pin(pin_id) {
                    Some(fm::Pin::Open) => return Some(Signal::Bool(false)),
                    Some(fm::Pin::Hover) => return Some(Signal::Float(-1.0)),
                    Some(fm::Pin::Zone { .. }) | None => {}
                }
                match tz::parse_pin(pin_id)? {
                    tz::Pin::Zone { comp: tz::ZoneComp::Active, .. } => Some(Signal::Bool(false)),
                    tz::Pin::Zone { .. } => Some(Signal::Float(0.0)),
                    tz::Pin::Click { .. } => Some(Signal::Bool(false)),
                }
            }).collect()
        }
        "module.touch_zones" => {
            use flexinput_core::touchzones as tz;
            // Same upstream resolution as the Splitter: prefer the closest
            // collector's injected signals, else the raw device samples.
            let dev_id = snap.params.get("_automap_device_id").and_then(|v| v.as_str()).unwrap_or("");
            let collector_id = snap.params.get("_automap_collector_id").and_then(|v| v.as_str()).unwrap_or("");
            let read = |pin: &str| -> Option<Signal> {
                if !collector_id.is_empty() {
                    if let Some(&s) = collector_sigs.get(&(collector_id.to_string(), pin.to_string())) {
                        return Some(s);
                    }
                }
                dev_sigs.get(&(dev_id.to_string(), pin.to_string())).copied()
            };
            let read_edges = |field: usize, which: &str| -> Vec<f32> {
                let key = if field == 0 { which.to_string() } else { format!("{which}{field}") };
                snap.params.get(&key).and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect())
                    .unwrap_or_default()
            };

            // Split mode: field 0 tracks touch1, field 1 tracks touch2 — each on
            // its own grid (a Steam-Controller-style pair, or two fingers tracked
            // separately). Single mode: one field, both fingers.
            let split = snap.params.get("field_mode").and_then(|v| v.as_str()) == Some("split");
            let n_fields = if split { 2 } else { 1 };

            // Resolve which zone each active finger occupies, per field, keeping
            // per-zone local coords. In single mode touch1 is processed last so it
            // wins when both fingers land in the same zone.
            let mut zone_hit: HashMap<(usize, usize), (f32, f32)> = HashMap::new();
            for field in 0..n_fields {
                let col_edges = read_edges(field, "col_edges");
                let row_edges = read_edges(field, "row_edges");
                let fingers: &[(&str, &str, &str)] = if split {
                    if field == 0 { &[("touch1_x", "touch1_y", "touch1_active")] }
                    else          { &[("touch2_x", "touch2_y", "touch2_active")] }
                } else {
                    &[("touch2_x", "touch2_y", "touch2_active"),
                      ("touch1_x", "touch1_y", "touch1_active")]
                };
                for &(px, py, pa) in fingers {
                    if !read(pa).map(|s| s.as_bool()).unwrap_or(false) { continue; }
                    let (x, y) = tz::pad_point_to_unit(
                        read(px).map(|s| s.as_float()).unwrap_or(0.0),
                        read(py).map(|s| s.as_float()).unwrap_or(0.0),
                    );
                    let (idx, lx, ly) = tz::locate_unit(x, y, &col_edges, &row_edges);
                    zone_hit.insert((field, idx), (lx, ly));
                }
            }

            (0..snap.n_outputs).map(|i| {
                let pin_id = snap.output_pin_ids.get(i).map(|s| s.as_str()).unwrap_or("");
                match tz::parse_pin(pin_id)? {
                    tz::Pin::Zone { field, idx, comp } => Some(match (zone_hit.get(&(field, idx)), comp) {
                        (Some(&(lx, _)), tz::ZoneComp::X) => Signal::Float(lx),
                        (Some(&(_, ly)), tz::ZoneComp::Y) => Signal::Float(ly),
                        (Some(_), tz::ZoneComp::Active)   => Signal::Bool(true),
                        (None, tz::ZoneComp::Active)      => Signal::Bool(false),
                        (None, _)                         => Signal::Float(0.0),
                    }),
                    // Field 0 click = the touchpad button. Field 1 reads the
                    // reserved `btn_touchpad2` pin (populated only once a device
                    // with two clickable pads — e.g. Steam Controller — exposes it).
                    tz::Pin::Click { field } => {
                        let pin = if field == 0 { "btn_touchpad" } else { "btn_touchpad2" };
                        Some(Signal::Bool(read(pin).map(|s| s.as_bool()).unwrap_or(false)))
                    }
                }
            }).collect()
        }
        "module.macro" => {
            // Macro Output: no wired inputs — each output pin reads back the
            // per-tick macro namespace that mapping evaluators (Remapper /
            // Touch Zones cards / 3DOF-Lean) published into via
            // `merge_macro_scalar` / `merge_macro_vec2`, then coerces to the
            // port's declared type. Absent entry = mapping released → the
            // type's off value, so downstream logic always sees a defined
            // signal (Any ports emit None when unset, like an unwired pin).
            use flexinput_core::macros as mac;
            let port_types: HashMap<String, SignalType> = mac::ports_from_params(&snap.params)
                .into_iter()
                .map(|p| (mac::macro_pin_id(&p.id), p.signal_type))
                .collect();
            (0..snap.n_outputs).map(|i| {
                let pin_id = snap.output_pin_ids.get(i).map(|s| s.as_str()).unwrap_or("");
                if pin_id.is_empty() { return None; }
                let ty = port_types.get(pin_id).copied().unwrap_or(SignalType::Bool);
                let scalar = collector_sigs.get(&(mac::SIGS_NS.to_string(), pin_id.to_string())).copied();
                let vec2 = collector_sigs.get(&(mac::SIGS_NS_VEC2.to_string(), pin_id.to_string())).copied();
                match ty {
                    SignalType::Vec2 => Some(vec2.unwrap_or(Signal::Vec2(Vec2::ZERO))),
                    // Float / Any prefer the deflection aspect when present:
                    // a Touch Zones card writes BOTH the (binary) gate and the
                    // deflection, and an analog-typed port wants the position,
                    // not a gate pinned at 1.0. Remapper/Lean write only the
                    // scalar, so they're unaffected.
                    SignalType::Float => Some(match (scalar, vec2) {
                        (_, Some(Signal::Vec2(v))) => Signal::Float(v.length().min(1.0)),
                        (Some(s), _) => Signal::Float(s.as_float().clamp(0.0, 1.0)),
                        _ => Signal::Float(0.0),
                    }),
                    SignalType::Any => vec2.or(scalar),
                    _ => Some(match (scalar, vec2) {
                        (Some(s), _) => Signal::Bool(s.as_bool()),
                        (None, Some(Signal::Vec2(v))) => Signal::Bool(v.length() >= 0.5),
                        _ => Signal::Bool(false),
                    }),
                }
            }).collect()
        }
        "module.constant" | "module.knob" => {
            let v = snap.params.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            vec![Some(Signal::Float(v))]
        }
        "module.switch" => {
            // The engine is the sole authority on `active`. The UI signals its
            // intent through a monotonically-increasing `ui_toggle_seq` counter
            // (bumped on each button click); the engine compares against its
            // last-seen value and toggles. This avoids the two-writer race that
            // happens when both UI and engine modify the same `active` param.
            //
            //   aux_f32[0] = current `active`         (0/1)
            //   aux_f32[1] = previous `latch` level   (0/1)
            //   aux_f32[2] = last-seen ui_toggle_seq  (truncated to f32; suitable
            //                for counters well past patch lifetime — wraparound
            //                isn't a concern in practice and a mismatch just
            //                toggles once).
            //   aux_f32[3] = init flag                (0 until first tick)
            while state.aux_f32.len() < 4 { state.aux_f32.push(0.0); }
            let initialised = state.aux_f32[3] > 0.5;
            let prev_active = state.aux_f32[0] > 0.5;
            let prev_latch  = state.aux_f32[1] > 0.5;
            let prev_seq    = state.aux_f32[2];

            let saved_active = snap.params.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
            let cur_seq = snap.params.get("ui_toggle_seq")
                .and_then(|v| v.as_u64()).unwrap_or(0) as f32;
            let direct = inputs.get(0).copied().flatten()
                .and_then(|s| s.coerce_to(SignalType::Bool))
                .map(|s| matches!(s, Signal::Bool(true))).unwrap_or(false);
            let latch  = inputs.get(1).copied().flatten()
                .and_then(|s| s.coerce_to(SignalType::Bool))
                .map(|s| matches!(s, Signal::Bool(true))).unwrap_or(false);

            // First tick after load: adopt the persisted `active` so saved
            // patches reopen in their stored state.
            let mut active = if initialised { prev_active } else { saved_active };

            // UI clicks since last tick: toggle once per increment of the
            // sequence counter. We can't replay individual clicks if many
            // happened between ticks, so collapse to "differs → toggle once".
            if initialised && cur_seq != prev_seq {
                active = !active;
            }
            // Latch rising edge → toggle.
            if latch && !prev_latch {
                active = !active;
            }
            // Direct HIGH → force ON; falling edge does not force OFF.
            if direct {
                active = true;
            }

            state.aux_f32[0] = if active { 1.0 } else { 0.0 };
            state.aux_f32[1] = if latch  { 1.0 } else { 0.0 };
            state.aux_f32[2] = cur_seq;
            state.aux_f32[3] = 1.0;

            let out = vec![Some(Signal::Bool(active))];
            state.last_signals = out.clone();
            out
        }
        "module.dropdown" => {
            let n = snap.params.get("options")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            let idx = snap.params.get("selected_index")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            if n == 0 {
                vec![Some(Signal::Float(0.0)), Some(Signal::Int(0))]
            } else {
                let idx = idx.min(n - 1);
                // Centre-of-bucket quantisation: matches the inverse mapping
                // used by `Selector`/`Split` ((sel*N).floor()), so wiring the
                // float output into a Selector with N inputs selects bucket
                // `idx` exactly.
                let f = (idx as f32 + 0.5) / n as f32;
                vec![Some(Signal::Float(f)), Some(Signal::Int(idx as i32))]
            }
        }
        "generator.oscillator" => {
            let out = compute_oscillator(inputs, state, &snap.params, dt);
            state.last_signals = out.clone();
            out
        }
        "generator.envelope" => {
            // last_signals set inside compute_envelope: [output, phase]
            compute_envelope(inputs, state, &snap.params, dt)
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
        "module.twoway_response_curve" => {
            let out = compute_twoway_response_curve(inputs, state, &snap.params, dt);
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
            let out = compute_gyro_3dof(inputs, state, &snap.params, dev_sigs, collector_sigs, dt);
            state.last_signals = out.clone();
            out
        }
        "module.response_curve" | "module.vec_response_curve" | "module.vec_reshape" => {
            state.last_signals = inputs.to_vec();
            (0..snap.n_outputs).map(|out_idx| {
                eval_pure(&snap.module_id, out_idx, inputs, &snap.params, snap.n_outputs)
            }).collect()
        }
        "display.oscilloscope" | "display.vectorscope" | "display.readout"
        | "display.controller3d" => vec![],
        "device.sink" => {
            if snap.n_outputs == 0 { return vec![]; }
            let dev_id = snap.device_id.as_deref().unwrap_or("");
            let dz = snap.params.get("deadzone").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            (0..snap.n_outputs).map(|i| {
                let pin_id = snap.output_pin_ids.get(i).map(|s| s.as_str()).unwrap_or("");
                if pin_id.is_empty() { return None; }
                let sig = dev_sigs.get(&(dev_id.to_string(), pin_id.to_string())).copied()?;
                Some(if dz > 0.0 && is_stick_pin(pin_id) { apply_deadzone(sig, dz) } else { sig })
            }).collect()
        }
        "subpatch.inlet" => vec![],
        "subpatch.outlet" => vec![inputs.first().copied().flatten()],
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
        "math.add" => {
            if inputs.iter().any(|s| matches!(s, Some(Signal::Vec2(_)))) {
                let sum = (0..inputs.len())
                    .map(|i| get_v2(inputs, i, 0.0))
                    .fold(Vec2::ZERO, |acc, v| acc + v);
                Some(Signal::Vec2(sum))
            } else {
                Some(Signal::Float((0..inputs.len()).map(|i| get_f(inputs, i, 0.0)).sum()))
            }
        }
        "math.subtract" => {
            if inputs.iter().any(|s| matches!(s, Some(Signal::Vec2(_)))) {
                let first = get_v2(inputs, 0, 0.0);
                let rest = (1..inputs.len()).map(|i| get_v2(inputs, i, 0.0)).fold(Vec2::ZERO, |acc, v| acc + v);
                Some(Signal::Vec2(first - rest))
            } else {
                let first = get_f(inputs, 0, 0.0);
                let rest: f32 = (1..inputs.len()).map(|i| get_f(inputs, i, 0.0)).sum();
                Some(Signal::Float(first - rest))
            }
        }
        "math.multiply" => {
            if inputs.iter().any(|s| matches!(s, Some(Signal::Vec2(_)))) {
                let first = get_v2(inputs, 0, 0.0);
                let scale = (1..inputs.len()).map(|i| get_v2(inputs, i, 1.0)).fold(Vec2::ONE, |acc, v| acc * v);
                Some(Signal::Vec2(first * scale))
            } else {
                let first = get_f(inputs, 0, 0.0);
                let rest: f32 = (1..inputs.len()).map(|i| get_f(inputs, i, 1.0)).product();
                Some(Signal::Float(first * rest))
            }
        }
        "math.divide" => {
            if inputs.iter().any(|s| matches!(s, Some(Signal::Vec2(_)))) {
                let mut v = get_v2(inputs, 0, 0.0);
                for i in 1..inputs.len() {
                    let d = get_v2(inputs, i, 1.0);
                    v = Vec2::new(
                        if d.x == 0.0 { 0.0 } else { v.x / d.x },
                        if d.y == 0.0 { 0.0 } else { v.y / d.y },
                    );
                }
                Some(Signal::Vec2(v))
            } else {
                let mut v = get_f(inputs, 0, 0.0);
                for i in 1..inputs.len() {
                    let d = get_f(inputs, i, 1.0);
                    v = if d == 0.0 { 0.0 } else { v / d };
                }
                Some(Signal::Float(v))
            }
        }
        "math.abs" => match inputs.get(0).and_then(|s| *s) {
            Some(Signal::Vec2(v)) => Some(Signal::Vec2(v.abs())),
            other => Some(Signal::Float(other.map(|s| s.as_float()).unwrap_or(0.0).abs())),
        },
        "math.negate" => match inputs.get(0).and_then(|s| *s) {
            Some(Signal::Vec2(v)) => Some(Signal::Vec2(-v)),
            other => Some(Signal::Float(-other.map(|s| s.as_float()).unwrap_or(0.0))),
        },
        "math.clamp"  => {
            let min = if inputs.get(1).and_then(|s| *s).is_some() { get_f(inputs, 1, -1.0) } else { param_f("min", -1.0) };
            let max = if inputs.get(2).and_then(|s| *s).is_some() { get_f(inputs, 2,  1.0) } else { param_f("max",  1.0) };
            match inputs.get(0).and_then(|s| *s) {
                Some(Signal::Vec2(v)) => Some(Signal::Vec2(v.clamp(Vec2::splat(min), Vec2::splat(max)))),
                other => Some(Signal::Float(other.map(|s| s.as_float()).unwrap_or(0.0).clamp(min, max))),
            }
        }
        "math.map_range" => {
            let in_min  = if inputs.get(1).and_then(|s| *s).is_some() { get_f(inputs, 1, -1.0) } else { param_f("in_min",  -1.0) };
            let in_max  = if inputs.get(2).and_then(|s| *s).is_some() { get_f(inputs, 2,  1.0) } else { param_f("in_max",   1.0) };
            let out_min = if inputs.get(3).and_then(|s| *s).is_some() { get_f(inputs, 3, -1.0) } else { param_f("out_min", -1.0) };
            let out_max = if inputs.get(4).and_then(|s| *s).is_some() { get_f(inputs, 4,  1.0) } else { param_f("out_max",  1.0) };
            let map = |v: f32| -> f32 {
                let t = if (in_max - in_min).abs() < f32::EPSILON { 0.0 }
                        else { (v - in_min) / (in_max - in_min) };
                out_min + t * (out_max - out_min)
            };
            match inputs.get(0).and_then(|s| *s) {
                Some(Signal::Vec2(v)) => Some(Signal::Vec2(Vec2::new(map(v.x), map(v.y)))),
                other => Some(Signal::Float(map(other.map(|s| s.as_float()).unwrap_or(0.0)))),
            }
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
            let sel = get_f(inputs, 0, 0.0);
            let raw = inputs.get(1).and_then(|s| *s);
            let n   = n_outputs;
            let interp = params.get("interpolate").and_then(|v| v.as_bool()).unwrap_or(false);
            let zero_like = |sig: Option<Signal>| -> Signal {
                match sig {
                    Some(Signal::Vec2(_)) => Signal::Vec2(glam::Vec2::ZERO),
                    Some(Signal::Bool(_)) => Signal::Bool(false),
                    Some(Signal::Int(_))  => Signal::Int(0),
                    _                     => Signal::Float(0.0),
                }
            };
            if interp && n >= 2 {
                let pos = sel.clamp(0.0, 1.0) * (n - 1) as f32;
                let lo  = pos.floor() as usize;
                let hi  = (lo + 1).min(n - 1);
                let t   = pos.fract();
                match raw {
                    Some(Signal::Vec2(v)) => {
                        if out_idx == lo && lo == hi { Some(Signal::Vec2(v)) }
                        else if out_idx == lo        { Some(Signal::Vec2(v * (1.0 - t))) }
                        else if out_idx == hi        { Some(Signal::Vec2(v * t)) }
                        else                         { Some(Signal::Vec2(glam::Vec2::ZERO)) }
                    }
                    _ => {
                        let val = raw.map(|s| s.as_float()).unwrap_or(0.0);
                        if out_idx == lo && lo == hi { Some(Signal::Float(val)) }
                        else if out_idx == lo        { Some(Signal::Float(val * (1.0 - t))) }
                        else if out_idx == hi        { Some(Signal::Float(val * t)) }
                        else                         { Some(Signal::Float(0.0)) }
                    }
                }
            } else {
                let idx = (sel.clamp(0.0, 1.0) * n as f32).floor() as usize;
                let idx = idx.min(n.saturating_sub(1));
                if out_idx == idx { Some(raw.unwrap_or(Signal::Float(0.0))) } else { Some(zero_like(raw)) }
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
        "module.vec_reshape" => {
            if out_idx >= n_outputs { return None; }
            let vec = match inputs.get(out_idx).and_then(|s| *s) {
                Some(Signal::Vec2(v)) => v,
                _ => return Some(Signal::Vec2(glam::Vec2::ZERO)),
            };
            let boundary = reshape_pts(params, "boundary_pts", VEC_RESHAPE_BOUNDARY_DEFAULT);
            let gain     = reshape_pts(params, "gain_pts",     VEC_RESHAPE_GAIN_DEFAULT);
            let gbiases: Vec<f32> = params.get("gain_biases").and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|b| b.as_f64().map(|f| f as f32)).collect())
                .unwrap_or_default();
            let sym     = params.get("symmetry").and_then(|v| v.as_str()).unwrap_or("quad4");
            let renorm  = params.get("renorm").and_then(|v| v.as_bool()).unwrap_or(true);
            let in_max  = params.get("in_max") .and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let out_max = params.get("out_max").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            Some(Signal::Vec2(vec_reshape_apply(vec, &boundary, &gain, &gbiases, sym, renorm, in_max, out_max)))
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

pub(crate) fn compute_oscillator(
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

    let base_freq = params.get("freq_param").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
    // When freq is wired the input is a normalized multiplier [0,1] (or bipolar) applied
    // to the base frequency set in the node. This lets you sweep 0→base_freq with a
    // unipolar source or modulate depth with another oscillator.
    let freq_val  = if freq_wired  { get_f(inputs, 0, 1.0).max(0.0) * base_freq } else { base_freq };
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

// ── Envelope Generator ────────────────────────────────────────────────────────
//
// Behavior is set by three combinable flags — Hold, Bounce, Loop — rather than a
// single mode. `envelope_flags` resolves them (with a fallback that maps the old
// `mode` string for patches saved before the switch). The eight combinations:
//
//   (none)        one-shot: a single 0→1 pass on trigger.
//   Hold          attack→sustain, hold while held, release →1.
//   Loop          sawtooth 0↔1 while held; returns to 0 on release.
//   Bounce        forward while held (sustains at 1), reverses to 0 on release.
//   Hold+Bounce   bounce, value held flat at the sustain level through the
//                 post-sustain time buffer (the "B+Hold" buffer mode).
//   Hold+Loop     attack→sustain, then sawtooth loop between sustain and 1.
//   Bounce+Loop   ping-pong 0↔1 while held; recedes to 0 on release.
//   Hold+Bounce+Loop  attack→sustain, then ping-pong between sustain and 1.
//
// State layout in aux_f32:
//   [0] = current phase (0..1 along the curve X axis)
//   [1] = previous trigger value (0/1)
//   [2] = stage: 0=idle/done, 1=attack, 2=sustain-active, 3=release
//   [3] = discontinuity epoch (bumped on teleports; UI breaks the trail on change)
//   [4] = bounce ping-pong direction (+1 forward, -1 backward)
//
// last_signals = [output, phase, epoch, applied_time] for the UI.

/// Resolve the (hold, bounce, loop) envelope flags, falling back to the legacy
/// `mode` string for patches saved before flags existed.
pub fn envelope_flags(params: &HashMap<String, Value>) -> (bool, bool, bool) {
    let has_new = params.contains_key("hold")
        || params.contains_key("bounce")
        || params.contains_key("loop");
    if has_new {
        let g = |k: &str| params.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
        (g("hold"), g("bounce"), g("loop"))
    } else {
        match params.get("mode").and_then(|v| v.as_str()).unwrap_or("oneshot") {
            "hold"        => (true,  false, false),
            "loop"        => (false, false, true),
            "bounce"      => (false, true,  false),
            "bounce_hold" => (true,  true,  false),
            _             => (false, false, false),
        }
    }
}

pub(crate) fn compute_envelope(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
    dt: f32,
) -> Vec<Option<Signal>> {
    let (hold, bounce, loopf) = envelope_flags(params);
    let timebase   = params.get("timebase").and_then(|v| v.as_str()).unwrap_or("ms");
    let time_param = params.get("time_mul").and_then(|v| v.as_f64()).unwrap_or(500.0) as f32;
    let sustain_x  = params.get("sustain") .and_then(|v| v.as_f64()).unwrap_or(0.5)   as f32;
    let sustain_c  = sustain_x.clamp(0.0, 1.0);
    let pts    = curve_points_from_params(params);
    let biases = biases_from_params(params);

    let trig_wired = inputs.get(0).and_then(|s| *s).is_some();
    let time_wired = inputs.get(1).and_then(|s| *s).is_some();
    let time_val   = if time_wired { get_f(inputs, 1, time_param).max(0.0) } else { time_param };

    let period_s = match timebase {
        "s"  => time_val.max(0.0001),
        "hz" => if time_val > 0.0 { 1.0 / time_val } else { 1.0 },
        _    => (time_val / 1000.0).max(0.0001),
    };
    let dt_phase = (dt / period_s).min(1.0);

    while state.aux_f32.len() < 5 { state.aux_f32.push(0.0); }
    let mut phase = state.aux_f32[0];
    let prev_trig = state.aux_f32[1];
    let mut stage = state.aux_f32[2];
    // Discontinuity epoch: bumped every time `phase` is set non-continuously
    // (retrigger, loop wrap, loop release-reset, hold early-release jump). The
    // UI reads this from last_signals[2] and breaks the trail across an epoch
    // change so the dot teleports (old trail fades in place) rather than drawing
    // a bridging streak across the jump.
    let mut epoch = state.aux_f32[3];
    // Bounce ping-pong direction (+1 forward, -1 backward). Default forward.
    let mut dir = if state.aux_f32[4] == 0.0 { 1.0 } else { state.aux_f32[4] };

    let trigger   = get_b(inputs, 0, false);
    let trig_edge = trigger && prev_trig < 0.5;

    if bounce {
        // ── Bounce family ─────────────────────────────────────────────────────
        // Continuous motion (no teleports), so the trail follows the dot. With
        // Hold the active region is [sustain, 1] (climb to sustain first); with
        // Loop the dot ping-pongs in that region instead of sustaining at 1.
        let lo = if hold { sustain_c } else { 0.0 };
        if trigger {
            if loopf {
                if phase < lo {
                    phase = (phase + dt_phase).min(lo);
                    dir = 1.0;
                } else {
                    phase += dir * dt_phase;
                    if phase >= 1.0 { phase = 1.0; dir = -1.0; }
                    if phase <= lo  { phase = lo;  dir =  1.0; }
                }
            } else {
                // Forward, sustaining at the end (value frozen at sustain when
                // Hold is set — the post-sustain time buffer, see sample below).
                phase = (phase + dt_phase).min(1.0);
                dir = 1.0;
            }
        } else {
            phase = (phase - dt_phase).max(0.0);
            dir = 1.0;
        }
    } else if hold {
        // ── Hold family (no bounce) ───────────────────────────────────────────
        if trig_edge { phase = 0.0; stage = 1.0; epoch += 1.0; }
        if stage == 1.0 {
            // Attack: climb to the sustain point.
            phase += dt_phase;
            if phase >= sustain_c {
                phase = sustain_c;
                stage = 2.0;
            } else if !trigger {
                // Released before reaching sustain. Jump onto the release side
                // (X >= sustain) at the point whose curve value best matches the
                // current output — similar level or higher, never a downward jump.
                let current_y = sample_curve(&pts, phase, &biases);
                let steps = 200u32;
                let mut best_x = sustain_c;
                let mut best_d = f32::INFINITY;
                for i in 0..=steps {
                    let x = sustain_c + (1.0 - sustain_c) * i as f32 / steps as f32;
                    let d = (sample_curve(&pts, x, &biases) - current_y).abs();
                    if d < best_d { best_d = d; best_x = x; }
                }
                phase = best_x;
                stage = 3.0;
                epoch += 1.0; // teleport across the sustain point
            }
        }
        if stage == 2.0 {
            if !trigger {
                stage = 3.0; // begin release
            } else if loopf {
                // Hold+Loop: sawtooth loop between sustain and 1.
                let span = (1.0 - sustain_c).max(1e-4);
                let advanced = phase + dt_phase;
                if advanced >= 1.0 {
                    epoch += 1.0;
                    phase = sustain_c + (advanced - 1.0).rem_euclid(span);
                } else {
                    phase = advanced;
                }
            }
            // Plain Hold: phase stays parked at sustain.
        }
        if stage == 3.0 {
            // Release: run forward to the end.
            phase += dt_phase;
            if phase >= 1.0 { phase = 1.0; stage = 0.0; }
        }
    } else if loopf {
        // ── Loop (no hold, no bounce) ─────────────────────────────────────────
        if trig_wired && !trigger {
            if phase != 0.0 { epoch += 1.0; }
            phase = 0.0;
        } else {
            if trig_edge { phase = 0.0; epoch += 1.0; }
            let advanced = phase + dt_phase;
            if advanced >= 1.0 { epoch += 1.0; } // wrapped around
            phase = advanced % 1.0;
        }
    } else {
        // ── One-shot ──────────────────────────────────────────────────────────
        if trig_edge { phase = 0.0; stage = 1.0; epoch += 1.0; }
        if stage == 1.0 {
            phase += dt_phase;
            if phase >= 1.0 { phase = 1.0; stage = 0.0; }
        }
    }

    state.aux_f32[0] = phase;
    state.aux_f32[1] = if trigger { 1.0 } else { 0.0 };
    state.aux_f32[2] = stage;
    state.aux_f32[3] = epoch;
    state.aux_f32[4] = dir;

    // Hold+Bounce (no loop) freezes the value at the sustain level through the
    // post-sustain buffer; every other combination samples the live phase.
    let buffer_mode = hold && bounce && !loopf;
    let sample_phase = if buffer_mode { phase.min(sustain_c) } else { phase };
    let output = sample_curve(&pts, sample_phase, &biases).clamp(0.0, 1.0);
    state.last_signals = vec![
        Some(Signal::Float(output)),
        Some(Signal::Float(phase)),
        Some(Signal::Float(epoch)),
        // Applied time value in the current unit — the UI shows this in the
        // grayed-out time box when the Time input is wired.
        Some(Signal::Float(time_val)),
    ];
    vec![Some(Signal::Float(output))]
}

pub(crate) fn compute_delay(
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

pub(crate) fn compute_average(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
) -> Vec<Option<Signal>> {
    let buf_size = params.get("buf_size").and_then(|v| v.as_f64())
        .map(|f| f as u64).unwrap_or(10).clamp(1, 10_000) as usize;
    let spike_mad = params.get("spike_mad").and_then(|v| v.as_f64()).unwrap_or(0.0).max(0.0);

    while state.avg_bufs.len()    < inputs.len() { state.avg_bufs.push(VecDeque::new()); }
    while state.avg_bufs_v2.len() < inputs.len() { state.avg_bufs_v2.push(VecDeque::new()); }

    let mut results = Vec::with_capacity(inputs.len());
    for (ch, inp) in inputs.iter().enumerate() {
        match inp {
            Some(Signal::Vec2(v)) => {
                let buf = &mut state.avg_bufs_v2[ch];
                buf.push_back(*v);
                while buf.len() > buf_size { buf.pop_front(); }

                let avg = if spike_mad > 0.0 && buf.len() >= 3 {
                    Vec2::new(
                        mad_average(buf.iter().map(|v| v.x), spike_mad as f32),
                        mad_average(buf.iter().map(|v| v.y), spike_mad as f32),
                    )
                } else {
                    buf.iter().copied().sum::<Vec2>() / buf.len() as f32
                };
                results.push(Some(Signal::Vec2(avg)));
            }
            inp => {
                let Some(v) = sig_to_f32(*inp) else { results.push(None); continue; };
                let buf = &mut state.avg_bufs[ch];
                buf.push_back(v);
                while buf.len() > buf_size { buf.pop_front(); }

                let avg = if spike_mad > 0.0 && buf.len() >= 3 {
                    mad_average(buf.iter().copied(), spike_mad as f32)
                } else {
                    buf.iter().sum::<f32>() / buf.len() as f32
                };
                results.push(Some(Signal::Float(avg)));
            }
        }
    }
    results
}

pub(crate) fn mad_average(values: impl Iterator<Item = f32> + Clone, spike_mad: f32) -> f32 {
    let mut sorted: Vec<f32> = values.collect();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let median = sorted_median(&sorted);
    let mut devs: Vec<f32> = sorted.iter().map(|&x| (x - median).abs()).collect();
    devs.sort_by(|a, b| a.total_cmp(b));
    let mad = sorted_median(&devs);
    if mad < 1e-9 {
        sorted.iter().sum::<f32>() / sorted.len() as f32
    } else {
        let thresh = spike_mad * mad;
        let kept: Vec<f32> = sorted.iter().cloned().filter(|&x| (x - median).abs() <= thresh).collect();
        if kept.is_empty() { sorted.iter().sum::<f32>() / sorted.len() as f32 }
        else { kept.iter().sum::<f32>() / kept.len() as f32 }
    }
}

pub(crate) fn sorted_median(sorted: &[f32]) -> f32 {
    let n = sorted.len();
    if n == 0 { return 0.0; }
    if n % 2 == 0 { (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0 } else { sorted[n / 2] }
}

pub(crate) const DC_THRESHOLD: f64    = 0.005;
pub(crate) const DC_STABILITY: f64    = 0.02;
pub(crate) const DC_FAST_TC_SECS: f64 = 0.05;

pub(crate) fn compute_dc_filter(
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

pub(crate) fn compute_twoway_response_curve(
    inputs: &[Option<Signal>],
    state: &mut NodeState,
    params: &HashMap<String, Value>,
    dt: f32,
) -> Vec<Option<Signal>> {
    let n_ch = inputs.len();

    // Grow per-channel state vectors lazily.
    while state.twoway_lane.len()       < n_ch { state.twoway_lane.push(1); }
    while state.twoway_dir_buf.len()    < n_ch { state.twoway_dir_buf.push(VecDeque::new()); }
    while state.twoway_blend.len()      < n_ch { state.twoway_blend.push(1.0); }
    while state.twoway_prev_input.len() < n_ch { state.twoway_prev_input.push(0.0); }
    while state.twoway_old_output.len() < n_ch { state.twoway_old_output.push(0.0); }

    // Shared params (applied to both curves).
    let abs     = params.get("absolute").and_then(|v| v.as_bool()).unwrap_or(true);
    let in_max  = params.get("in_max") .and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32;
    let in_min  = params.get("in_min") .and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
    let out_max = params.get("out_max").and_then(|v| v.as_f64()).unwrap_or(1.0)  as f32;
    let out_min = params.get("out_min").and_then(|v| v.as_f64()).unwrap_or(-1.0) as f32;
    let scale_t = read_scale_t(params);

    let vec_mode = params.get("vec_mode").and_then(|v| v.as_bool()).unwrap_or(false);

    // Up-lane (rising) curve params.
    let pts_up   = curve_points_from_params(params);
    let biases_up = biases_from_params(params);

    // Down-lane (falling) curve uses "_dn"-suffixed params, falling back to up-lane.
    let pts_dn: Vec<[f32; 2]> = params.get("points_dn")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|p| {
            let a = p.as_array()?;
            Some([a.get(0)?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
        }).collect())
        .unwrap_or_else(|| pts_up.clone());
    let biases_dn: Vec<f32> = params.get("biases_dn")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|b| b.as_f64().map(|f| f as f32)).collect())
        .unwrap_or_else(|| biases_up.clone());

    // Hysteresis params.
    let hyst_pct  = params.get("hysteresis_pct").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
    let hyst_ms   = params.get("hysteresis_ms") .and_then(|v| v.as_f64()).unwrap_or(20.0) as f32;
    let interp_ms = params.get("interp_ms")     .and_then(|v| v.as_f64()).unwrap_or(50.0) as f32;

    let hyst_ticks = ((hyst_ms / 1000.0) / dt).ceil() as usize;
    let hyst_ticks = hyst_ticks.max(1);

    let abs_max   = in_max.abs().max(in_min.abs()).max(f32::EPSILON);
    let threshold = hyst_pct / 100.0 * abs_max;

    let interp_step = if interp_ms > 0.0 { dt / (interp_ms / 1000.0) } else { 1.0 };

    let mut results = Vec::with_capacity(n_ch);

    for ch in 0..n_ch {
        let raw_input = if vec_mode {
            match inputs.get(ch).and_then(|s| *s) {
                Some(Signal::Vec2(v)) => v.length(),
                Some(Signal::Float(f)) => f,
                _ => { results.push(None); continue; }
            }
        } else {
            match inputs.get(ch).and_then(|s| *s) {
                Some(Signal::Float(f)) => f,
                _ => { results.push(None); continue; }
            }
        };

        // Use magnitude in abs/vec mode; signed value in bipolar mode.
        let hyst_input = if abs || vec_mode { raw_input.abs() } else { raw_input };

        // Hysteresis: sliding-window peak/trough detector.
        // twoway_dir_buf stores the last hyst_ticks samples of hyst_input.
        // running_max = highest value in window → if current falls threshold below it → Down.
        // running_min = lowest  value in window → if current rises threshold above it → Up.
        // Works at any speed: a fast release immediately shows a large gap from the window max.
        let win = &mut state.twoway_dir_buf[ch];
        win.push_back(hyst_input);
        while win.len() > hyst_ticks { win.pop_front(); }

        let running_max = win.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let running_min = win.iter().copied().fold(f32::INFINITY,     f32::min);

        // Use a minimum window of 1 tick so single-tick reversals are detected immediately.
        let fell_from_peak = hyst_input < running_max - threshold;
        let rose_from_trough = hyst_input > running_min + threshold;

        let prev_lane = state.twoway_lane[ch];
        if rose_from_trough && prev_lane != 1 {
            state.twoway_old_output[ch] = apply_curve(raw_input, &pts_dn, &biases_dn, abs, in_min, in_max, out_min, out_max, scale_t);
            state.twoway_lane[ch]  =  1;
            state.twoway_blend[ch] = 0.0;
            state.twoway_dir_buf[ch].clear();
            state.twoway_dir_buf[ch].push_back(hyst_input);
        } else if fell_from_peak && prev_lane != -1 {
            state.twoway_old_output[ch] = apply_curve(raw_input, &pts_up, &biases_up, abs, in_min, in_max, out_min, out_max, scale_t);
            state.twoway_lane[ch]  = -1;
            state.twoway_blend[ch] = 0.0;
            state.twoway_dir_buf[ch].clear();
            state.twoway_dir_buf[ch].push_back(hyst_input);
        }

        // Advance blend.
        state.twoway_blend[ch] = (state.twoway_blend[ch] + interp_step).min(1.0);
        let blend = state.twoway_blend[ch];

        // Evaluate active-lane curve at current input.
        let new_output = if state.twoway_lane[ch] >= 0 {
            apply_curve(raw_input, &pts_up, &biases_up, abs, in_min, in_max, out_min, out_max, scale_t)
        } else {
            apply_curve(raw_input, &pts_dn, &biases_dn, abs, in_min, in_max, out_min, out_max, scale_t)
        };

        // Blend from old-lane-output-at-switch-point toward new-lane-output-at-current-input.
        // When both curves are identical, old_output == new_output so blend has no effect.
        let output = blend * new_output + (1.0 - blend) * state.twoway_old_output[ch];

        let sig = if vec_mode {
            match inputs.get(ch).and_then(|s| *s) {
                Some(Signal::Vec2(v)) => {
                    let mag = v.length();
                    if mag < f32::EPSILON { Signal::Vec2(glam::Vec2::ZERO) }
                    else { Signal::Vec2(v / mag * output) }
                }
                _ => Signal::Float(output),
            }
        } else {
            Signal::Float(output)
        };

        results.push(Some(sig));
    }

    results
}

pub(crate) fn compute_has_changed(
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

pub(crate) fn compute_logic_delay(
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

pub(crate) fn compute_counter(
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
