//! Evaluator tests: press-mode / trigger behaviour, and the Virtual Menu
//! state machine.
//!
//! These reach exactly the names they did when the evaluator was one file,
//! with no import churn — see the glob below for how.

// The two test mods below are children of THIS module, not of `eval` as they
// were when the evaluator was one file — so their own `use super::*` reaches
// here first. This glob chains it on to the facade, restoring the names they
// were written against.
use super::*;

#[cfg(test)]
mod trigger_tests {
    use super::*;
    use crate::graph::SinkTarget;

    // Two virtual sinks feeding back to one physical pad must COMBINE (max), not
    // first-wins — the "only one virtual passes ping after restart" bug.
    #[test]
    fn combine_feedback_takes_max_not_first() {
        // Loud + quiet → loud, regardless of order.
        assert_eq!(
            combine_feedback_max(Signal::Float(0.2), Signal::Float(0.9)),
            Signal::Float(0.9)
        );
        assert_eq!(
            combine_feedback_max(Signal::Float(0.9), Signal::Float(0.2)),
            Signal::Float(0.9)
        );
        // One source idle (0.0), the other active → active wins (the exact bug:
        // an idle virtual must not mask an active one).
        assert_eq!(
            combine_feedback_max(Signal::Float(0.0), Signal::Float(0.7)),
            Signal::Float(0.7)
        );
        // Bool OR.
        assert_eq!(
            combine_feedback_max(Signal::Bool(false), Signal::Bool(true)),
            Signal::Bool(true)
        );
        // Float vs Bool coercion.
        assert_eq!(
            combine_feedback_max(Signal::Float(0.3), Signal::Bool(true)),
            Signal::Float(1.0)
        );
    }

    fn canonical_pins() -> Vec<String> {
        automap::ALL_PINS.iter().map(|p| p.id.to_string()).collect()
    }

    fn empty_node(uid: usize, module_id: &str) -> NodeSnap {
        NodeSnap {
            node_uid: uid,
            module_id: module_id.to_string(),
            params: HashMap::new(),
            n_outputs: 0,
            input_sources: Vec::new(),
            device_id: None,
            output_pin_ids: Vec::new(),
            aux_f32_override: None,
            sink_target: None,
            inline_subgraph: None,
        }
    }

    fn sink_node(uid: usize, device_id: &str, src_dev: &str, bridge: bool) -> NodeSnap {
        let mut n = empty_node(uid, "device.sink");
        n.sink_target = Some(SinkTarget {
            device_id: device_id.to_string(),
            // All canonical pins are valid sink destinations.
            pin_ids: canonical_pins(),
            multi_sources: vec![Vec::new(); canonical_pins().len()],
            automap_source: Some((src_dev.to_string(), canonical_pins())),
            automap_fallback_dev: Some("gilrs:switch_pro:0".to_string()),
            feedback_sources: Vec::new(),
            is_self_sink: false,
            digital_trigger_bridge: bridge,
        });
        n
    }

    // ── Macro Output routing ──────────────────────────────────────────────────

    /// Macro node snap with `ports` as (id, type_str) pairs.
    fn macro_node(uid: usize, ports: &[(&str, &str)]) -> NodeSnap {
        let mut n = empty_node(uid, "module.macro");
        n.n_outputs = ports.len();
        n.output_pin_ids = ports.iter().map(|(id, _)| format!("macro:{id}")).collect();
        n.params.insert("macro_ports".into(), Value::Array(ports.iter().map(|(id, ty)|
            serde_json::json!({ "id": id, "name": id, "icon": "", "type": ty })
        ).collect()));
        n
    }

    // A digital Remapper mapping targeting a macro pin drives the macro node's
    // Bool port (same tick — the macro node evaluates after the remapper), the
    // unmapped port emits its typed off value, and the macro pin never leaks
    // onto the AutoMap bus toward the sink.
    #[test]
    fn remapper_digital_mapping_drives_macro_port() {
        let dev = "gilrs:switch_pro:0";
        let src = source_node(1, dev, 0.0);
        let mut remap = empty_node(2, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["btn_south"], "out": ["macro:aa11bb22"] }
        ]));
        remap.input_sources = vec![Some((0, 0))];
        remap.n_outputs = 1;
        let mac = macro_node(3, &[("aa11bb22", "bool"), ("cc33dd44", "float")]);
        let sink = sink_node(4, "virtual.xinput:0", "remap:2", true);
        let graph = ProcessingGraph { nodes: vec![src, remap, mac, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let press = |on: bool| {
            let mut m = HashMap::new();
            m.insert((dev.to_string(), "btn_south".to_string()), Signal::Bool(on));
            m
        };

        eval_graph_tick(&graph, &mut state, &press(true), 0.016, &mut out);
        assert_eq!(out.outputs.get(&(3, 0)).copied().flatten(), Some(Signal::Bool(true)),
            "mapped macro Bool port must assert while the chord is held");
        assert_eq!(out.outputs.get(&(3, 1)).copied().flatten(), Some(Signal::Float(0.0)),
            "unmapped Float port emits its typed off value");
        assert!(out.sink_outputs.keys().all(|(_, p)| !p.starts_with("macro:")),
            "macro pins must never reach a sink");

        eval_graph_tick(&graph, &mut state, &press(false), 0.016, &mut out);
        assert_eq!(out.outputs.get(&(3, 0)).copied().flatten(), Some(Signal::Bool(false)),
            "released mapping must drop the port back to false");
    }

    // A Virtual Menu placed UPSTREAM of the Remapper that maps a button to its
    // Select target is a feedback cycle: the Remapper is forced to evaluate
    // AFTER the menu, so this tick's `collector_sigs` never carries the Select
    // value when the menu reads it. The cross-tick macro carry-over
    // (`NodeState::macro_prev`) delivers it one tick later, so `select_on =
    // "press"` fires. Also exercises the Show target opening the menu the same
    // way. Node order [src, menu, remap, sink] reproduces the cyclic fallback
    // (menu before its producer).
    #[test]
    fn menu_select_from_downstream_remapper_via_carryover() {
        let dev = "gilrs:switch_pro:0";
        let src = source_node(1, dev, 0.0);

        let mut menu = empty_node(2, "module.menu");
        menu.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        menu.params.insert("menu_id".into(), Value::String("abcd1234".into()));
        menu.params.insert("select_on".into(), Value::String("press".into()));
        menu.params.insert("col_edges".into(), serde_json::json!([0.5]));
        menu.params.insert("row_edges".into(), serde_json::json!([0.5]));
        menu.params.insert("zone_mode".into(), Value::String("mapping".into()));
        menu.params.insert("zone_maps".into(), serde_json::json!([
            { "f": 0, "z": 0, "in": ["menu_sel"], "out": ["btn_north"] }
        ]));
        menu.n_outputs = 3;
        menu.output_pin_ids = vec![
            "automap_pass".into(), "menu_open".into(), "menu_hover".into(),
        ];

        let mut remap = empty_node(3, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["btn_start"], "out": ["menu:abcd1234_show"] },
            { "in": ["btn_south"], "out": ["menu:abcd1234_sel"] },
        ]));
        remap.input_sources = vec![Some((0, 0))];
        remap.n_outputs = 1;

        // Sink pulls the menu's published bus (where the zone card writes btn_north).
        let sink = sink_node(4, "virtual.xinput:0", "menumap:2", false);

        let graph = ProcessingGraph { nodes: vec![src, menu, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // btn_start held (Show), stick points top-left (zone 0); btn_south varies.
        let sigs = |south: bool| {
            let mut m = HashMap::new();
            m.insert((dev.to_string(), "btn_start".to_string()), Signal::Bool(true));
            m.insert((dev.to_string(), "btn_south".to_string()), Signal::Bool(south));
            m.insert((dev.to_string(), "left_stick".to_string()), Signal::Vec2(Vec2::new(-0.8, 0.8)));
            m
        };
        let north = |o: &TickOutput| o.sink_outputs
            .get(&("virtual.xinput:0".to_string(), "btn_north".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false);
        let menu_open = |o: &TickOutput| o.last_outputs.get(&2)
            .and_then(|v| v.get(1)).copied().flatten().map(|s| s.as_bool()).unwrap_or(false);

        // Warm up: the Show macro opens the menu one tick after it's published.
        for _ in 0..4 {
            eval_graph_tick(&graph, &mut state, &sigs(false), 0.016, &mut out);
        }
        assert!(menu_open(&out), "macro Show target must open the menu via the carry-over");
        assert!(!north(&out), "no selection before Select is pressed");
        let hover = out.last_outputs.get(&2).and_then(|v| v.get(2)).copied().flatten();
        assert_eq!(hover, Some(Signal::Float(0.0)), "stick must hover zone 0 after warm-up");

        // Press Select: the menu sees a STALE (false) value this tick — the
        // Remapper only publishes it now, one node later …
        eval_graph_tick(&graph, &mut state, &sigs(true), 0.016, &mut out);
        assert!(!north(&out), "Select is one node downstream — not visible the same tick");
        // … and reads it via the carry-over on the next tick, firing the card
        // whose btn_north reaches the sink through the menu's published bus.
        eval_graph_tick(&graph, &mut state, &sigs(true), 0.016, &mut out);
        assert!(north(&out),
            "press-mode Select from a downstream Remapper must fire the zone card via the carry-over");
    }

    // The Virtual Menu's SOURCE-BLOCK must suppress a navigation input even when a
    // PARALLEL Combiner port carries a RAW copy that bypasses the menu — the exact
    // leak the user hit (SORT picks the raw port over the menu's zero). Blocking at
    // the source (dev_sigs) zeroes the raw port too, so nothing reaches the sink.
    #[test]
    fn menu_blocks_navigation_input_at_sink_despite_parallel_raw_port() {
        let dev = "gilrs:switch_pro:0";
        let src = source_node(1, dev, 0.0);

        // Menu opened by a Remapper's Show macro; suppresses the left stick.
        let mut menu = empty_node(2, "module.menu");
        menu.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        menu.params.insert("menu_id".into(), Value::String("abcd1234".into()));
        menu.n_outputs = 3;
        menu.output_pin_ids = vec![
            "automap_pass".into(), "menu_open".into(), "menu_hover".into(),
        ];

        let mut remap = empty_node(3, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["btn_start"], "out": ["menu:abcd1234_show"] },
        ]));
        remap.input_sources = vec![Some((0, 0))];
        remap.n_outputs = 1;

        // Combiner: port 0 = menu bus (suppressed), port 1 = RAW device (leak path).
        let mut comb = empty_node(4, "module.automap_combiner");
        comb.params.insert("_automap_input_devs".into(), Value::Array(vec![
            Value::String(String::new()), Value::String(dev.into()),
        ]));
        comb.params.insert("_automap_input_collectors".into(), Value::Array(vec![
            Value::String("menumap:2".into()), Value::String(String::new()),
        ]));
        comb.input_sources = vec![Some((0, 0)), Some((1, 0))];

        // sink_node sets automap_fallback_dev = the switch_pro pad — the physical
        // source the menu keys its block by.
        let sink = sink_node(5, "virtual.xinput:0", "combiner:4", false);

        let graph = ProcessingGraph { nodes: vec![src, menu, remap, comb, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // btn_start opens the menu; the left stick is fully deflected.
        let sigs = || {
            let mut m = HashMap::new();
            m.insert((dev.to_string(), "btn_start".to_string()), Signal::Bool(true));
            m.insert((dev.to_string(), "left_stick".to_string()), Signal::Vec2(Vec2::new(0.9, 0.0)));
            m.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(0.9));
            m
        };
        for _ in 0..4 { eval_graph_tick(&graph, &mut state, &sigs(), 0.016, &mut out); }

        assert!(out.last_outputs.get(&2).and_then(|v| v.get(1)).copied().flatten()
            .map(|s| s.as_bool()).unwrap_or(false), "menu should be open after warm-up");

        // Combiner SORT lets the raw port's 0.9 win, but the sink block zeroes it.
        let lx = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "left_stick_x".to_string()))
            .map(|s| s.as_float());
        let lv = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "left_stick".to_string()))
            .and_then(|s| if let Signal::Vec2(v) = s { Some(v.x) } else { None });
        let v = lx.or(lv).unwrap_or(0.0);
        assert!(v.abs() < 1e-4,
            "menu must suppress the navigation stick at the game boundary even via a parallel raw port, got {v}");
    }

    // An analog-mode mapping targeting a Float macro port passes the live
    // stick magnitude through — continuous, not a binary gate — and a Bool
    // port fed by the same analog write thresholds at 0.5.
    #[test]
    fn remapper_analog_mapping_drives_float_macro() {
        let dev = "gilrs:switch_pro:0";
        let src = source_node(1, dev, 0.0);
        let mut remap = empty_node(2, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["macro:f1f1f1f1"], "mode": "analog" }
        ]));
        remap.input_sources = vec![Some((0, 0))];
        remap.n_outputs = 1;
        let mac = macro_node(3, &[("f1f1f1f1", "float")]);
        let graph = ProcessingGraph { nodes: vec![src, remap, mac] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let push = |y: f32| {
            let mut m = HashMap::new();
            m.insert((dev.to_string(), "left_stick_y".to_string()), Signal::Float(y));
            m
        };

        eval_graph_tick(&graph, &mut state, &push(0.5), 0.016, &mut out);
        let v = out.outputs.get(&(3, 0)).copied().flatten().map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((v - 0.5).abs() < 0.05, "half push should give ~0.5 on the Float port, got {v}");

        eval_graph_tick(&graph, &mut state, &push(0.0), 0.016, &mut out);
        let v = out.outputs.get(&(3, 0)).copied().flatten().map(|s| s.as_float()).unwrap_or(-1.0);
        assert!(v.abs() < 0.01, "neutral stick should release the port to 0, got {v}");
    }

    // A Touch Zones card targeting a macro pin publishes BOTH aspects: the
    // shaped gate (Bool) and the zone-local deflection (Vec2). The macro node
    // then coerces per port type: Vec2 passes through, Float takes the
    // magnitude, Bool follows the gate.
    #[test]
    fn touch_zones_card_drives_macro_aspects() {
        let mut tz = empty_node(1, "module.touch_zones");
        tz.params.insert("zone_mode".into(), Value::String("mapping".into()));
        tz.params.insert("_automap_device_id".into(), Value::String("pad".into()));
        tz.params.insert("col_edges".into(), serde_json::json!([]));
        tz.params.insert("row_edges".into(), serde_json::json!([]));
        tz.params.insert("zone_maps".into(), serde_json::json!([
            {"f":0,"z":0,"in":["tz_touch"],"out":["macro:abcd0123"]},
        ]));
        // Finger at pad center then pushed right: unit x 0.5→0.75 within the
        // single full-pad zone → deflection x ≈ +0.5 from the zone center.
        let finger = |px: f32| {
            let mut m: HashMap<(String, String), Signal> = HashMap::new();
            m.insert(("pad".into(), "touch1_active".into()), Signal::Bool(true));
            m.insert(("pad".into(), "touch1_x".into()), Signal::Float(px));
            m.insert(("pad".into(), "touch1_y".into()), Signal::Float(0.0));
            m
        };
        let mut state = HashMap::new();
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        // Land at center (adaptive center latches there), then move right.
        eval_touch_zones_map_node(&tz, 1, &finger(0.0), &mut c, &mut state, 0.016);
        c.clear();
        eval_touch_zones_map_node(&tz, 1, &finger(0.5), &mut c, &mut state, 0.016);

        assert_eq!(c.get(&("macro".to_string(), "macro:abcd0123".to_string())).copied(),
            Some(Signal::Bool(true)), "gate aspect must assert while touched");
        let v2 = c.get(&("macro#v2".to_string(), "macro:abcd0123".to_string())).copied();
        let Some(Signal::Vec2(v)) = v2 else { panic!("deflection aspect missing: {v2:?}") };
        assert!(v.x > 0.4 && v.y.abs() < 0.05, "rightward deflection expected, got {v:?}");
        assert!(c.iter().all(|((k, p), _)| k != "touchmap:1" || !p.starts_with("macro:")),
            "macro pins must not be published on the touchmap bus key");

        // Coercion: read the same namespace back through each port type.
        let mut ns = NodeState::default();
        let dev_sigs = HashMap::new();
        let mac = macro_node(2, &[("abcd0123", "vec2")]);
        let out = compute_node(&mac, &[], &mut ns, &dev_sigs, &c, 0.016);
        assert!(matches!(out[0], Some(Signal::Vec2(v)) if v.x > 0.4),
            "Vec2 port passes the deflection through, got {:?}", out[0]);
        let mac = macro_node(2, &[("abcd0123", "float")]);
        let out = compute_node(&mac, &[], &mut ns, &dev_sigs, &c, 0.016);
        assert!(matches!(out[0], Some(Signal::Float(f)) if (f - 0.5).abs() < 0.05),
            "Float port prefers the deflection magnitude over the binary gate, got {:?}", out[0]);
        let mac = macro_node(2, &[("abcd0123", "bool")]);
        let out = compute_node(&mac, &[], &mut ns, &dev_sigs, &c, 0.016);
        assert_eq!(out[0], Some(Signal::Bool(true)), "Bool port follows the gate");
    }

    // 3DOF-Lean mappings targeting macro pins: analog mode passes the live
    // lean magnitude; digital (down) mode asserts while the side is active.
    #[test]
    fn lean_mapping_drives_macro_port() {
        let mk = |mode: &str| {
            let mut n = empty_node(1, "processing.gyro_3dof");
            n.params.insert("lean_left".into(), serde_json::json!([
                { "out": ["macro:11aa22bb"], "mode": mode }
            ]));
            n
        };
        let outs = |lean: f32| vec![None, None, None, Some(Signal::Float(lean))];
        let get = |c: &HashMap<(String, String), Signal>|
            c.get(&("macro".to_string(), "macro:11aa22bb".to_string())).copied();

        // Analog: leaning left at 0.8 → Float(0.8) on the macro namespace.
        let snap = mk("analog");
        let mut ns = NodeState::default();
        let mut c = HashMap::new();
        lean_dispatch_into_collector_sigs(&snap, 1, &outs(-0.8), &mut ns, &mut c, 0.016);
        let v = get(&c).map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((v - 0.8).abs() < 1e-4, "analog lean should pass magnitude, got {v}");
        // Below threshold → no write (port reads as released).
        c.clear();
        lean_dispatch_into_collector_sigs(&snap, 1, &outs(-0.1), &mut ns, &mut c, 0.016);
        assert_eq!(get(&c), None, "below-threshold lean must not assert the port");

        // Down mode: asserts Bool while the side is active.
        let snap = mk("down");
        let mut ns = NodeState::default();
        let mut c = HashMap::new();
        lean_dispatch_into_collector_sigs(&snap, 1, &outs(-0.8), &mut ns, &mut c, 0.016);
        assert_eq!(get(&c), Some(Signal::Bool(true)));
    }

    // ── Per-card response curve + manual activation threshold ────────────────

    fn curve_remap_graph(mapping: serde_json::Value) -> ProcessingGraph {
        let dev = "gilrs:switch_pro:0";
        let src = source_node(1, dev, 0.0);
        let mut remap = empty_node(2, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([mapping]));
        remap.input_sources = vec![Some((0, 0))];
        remap.n_outputs = 1;
        let sink = sink_node(3, "virtual.xinput:0", "remap:2", true);
        ProcessingGraph { nodes: vec![src, remap, sink] }
    }

    fn stick_y(y: f32) -> HashMap<(String, String), Signal> {
        let mut m = HashMap::new();
        m.insert(("gilrs:switch_pro:0".to_string(), "left_stick_y".to_string()), Signal::Float(y));
        m
    }

    fn sinkv(out: &TickOutput, pin: &str) -> Option<Signal> {
        out.sink_outputs.get(&("virtual.xinput:0".to_string(), pin.to_string())).copied()
    }

    // An analog mapping's per-card curve reshapes the emitted magnitude —
    // a halving curve turns a full stick push into ~0.5 trigger travel.
    #[test]
    fn remapper_analog_curve_shapes_output() {
        let graph = curve_remap_graph(serde_json::json!({
            "in": ["left_stick_up"], "out": ["right_trigger"], "mode": "analog",
            "curve": [[0.0, 0.0], [1.0, 0.5]],
        }));
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        eval_graph_tick(&graph, &mut state, &stick_y(1.0), 0.016, &mut out);
        let v = sinkv(&out, "right_trigger").map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((v - 0.5).abs() < 0.05, "halving curve should give ~0.5 at full push, got {v}");
    }

    // Manual threshold on an analog→digital mapping: PLAIN HOLD above the
    // line (steady across ticks — no tap train), release the moment the
    // shaped value dips below.
    #[test]
    fn remapper_analog_threshold_holds_digital() {
        let graph = curve_remap_graph(serde_json::json!({
            "in": ["left_stick_up"], "out": ["btn_east"], "mode": "analog",
            "threshold": 0.6,
        }));
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let east = |out: &TickOutput| sinkv(out, "btn_east").map(|s| s.as_bool()).unwrap_or(false);

        eval_graph_tick(&graph, &mut state, &stick_y(0.4), 0.016, &mut out);
        assert!(!east(&out), "below threshold must stay released");
        // Above threshold: held EVERY tick — the legacy pulse train would
        // toggle off within this window.
        for tick in 0..20 {
            eval_graph_tick(&graph, &mut state, &stick_y(0.8), 0.016, &mut out);
            assert!(east(&out), "threshold hold must be steady (tick {tick})");
        }
        eval_graph_tick(&graph, &mut state, &stick_y(0.4), 0.016, &mut out);
        assert!(!east(&out), "dipping below the line must release");
    }

    // Manual threshold on a DIGITAL-mode mapping with a cardinal input
    // overrides the built-in cardinal derivation (~0.5): the mapping only
    // fires past the card's own line.
    #[test]
    fn remapper_digital_threshold_overrides_cardinal() {
        let graph = curve_remap_graph(serde_json::json!({
            "in": ["left_stick_up"], "out": ["btn_east"],
            "threshold": 0.8,
        }));
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let east = |out: &TickOutput| sinkv(out, "btn_east").map(|s| s.as_bool()).unwrap_or(false);

        eval_graph_tick(&graph, &mut state, &stick_y(0.6), 0.016, &mut out);
        assert!(!east(&out), "0.6 push is past the built-in derivation but below the card threshold");
        eval_graph_tick(&graph, &mut state, &stick_y(0.9), 0.016, &mut out);
        assert!(east(&out), "0.9 push crosses the card threshold");
        eval_graph_tick(&graph, &mut state, &stick_y(0.6), 0.016, &mut out);
        assert!(!east(&out), "falling back below the threshold releases");
    }

    // Lean cards: a per-card threshold replaces the node lean_threshold for
    // that card, and a curve reshapes the analog magnitude the card emits.
    #[test]
    fn lean_card_threshold_and_curve() {
        // Threshold 0.7 on a down-mode card: node threshold (0.3) alone
        // would fire at 0.5 lean — the card must not.
        let mut n = empty_node(1, "processing.gyro_3dof");
        n.params.insert("lean_left".into(), serde_json::json!([
            { "out": ["btn_south"], "mode": "down", "threshold": 0.7 }
        ]));
        let outs = |lean: f32| vec![None, None, None, Some(Signal::Float(lean))];
        let get = |c: &HashMap<(String, String), Signal>, pin: &str|
            c.get(&("lean:1".to_string(), pin.to_string())).copied();
        let mut ns = NodeState::default();
        let mut c = HashMap::new();
        lean_dispatch_into_collector_sigs(&n, 1, &outs(-0.5), &mut ns, &mut c, 0.016);
        assert_eq!(get(&c, "btn_south"), Some(Signal::Bool(false)),
            "below the card threshold the mapping must not fire");
        c.clear();
        lean_dispatch_into_collector_sigs(&n, 1, &outs(-0.8), &mut ns, &mut c, 0.016);
        assert_eq!(get(&c, "btn_south"), Some(Signal::Bool(true)));

        // Halving curve on an analog card: full lean → ~0.5 on the Float out.
        let mut n = empty_node(1, "processing.gyro_3dof");
        n.params.insert("lean_right".into(), serde_json::json!([
            { "out": ["right_trigger"], "mode": "analog",
              "curve": [[0.0, 0.0], [1.0, 0.5]] }
        ]));
        let mut ns = NodeState::default();
        let mut c = HashMap::new();
        lean_dispatch_into_collector_sigs(&n, 1, &outs(1.0), &mut ns, &mut c, 0.016);
        let v = get(&c, "right_trigger").map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((v - 0.5).abs() < 0.01, "curve must shape the analog lean magnitude, got {v}");
    }

    // Multiple writers to one macro port merge by larger magnitude, in either
    // arrival order — an asserted mapping beats an idle/weaker one.
    #[test]
    fn macro_merge_larger_magnitude_wins() {
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        merge_macro_scalar(&mut c, "macro:x", Signal::Float(0.3));
        merge_macro_scalar(&mut c, "macro:x", Signal::Bool(true)); // mag 1.0
        merge_macro_scalar(&mut c, "macro:x", Signal::Float(0.6));
        assert_eq!(c.get(&("macro".to_string(), "macro:x".to_string())).copied(),
            Some(Signal::Bool(true)), "largest-magnitude write must win");
    }

    // Remapper in analog mode mapping a stick cardinal → right_trigger should
    // produce a CONTINUOUS value tracking how far the stick is pushed, not a
    // binary 0/1. Regression guard for the "stick→trigger outputs binary" bug.
    #[test]
    fn remapper_analog_stick_to_trigger_is_continuous() {
        let remap_uid = 1usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String("gilrs:switch_pro:0".into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["right_trigger"], "mode": "analog" }
        ]));

        // Zero-deadzone source so this test measures continuity, not the
        // deadzone curve (deadzone is covered by the dedicated tests above).
        let src = source_node(3, "gilrs:switch_pro:0", 0.0);
        let sink = sink_node(2, "virtual.xinput:0", &format!("remap:{remap_uid}"), true);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };

        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Stick pushed halfway up (y = +0.5).
        let mut dev = HashMap::new();
        dev.insert(("gilrs:switch_pro:0".to_string(), "left_stick_y".to_string()), Signal::Float(0.5));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let v = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()))
            .map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((v - 0.5).abs() < 0.05, "half stick push should give ~0.5 trigger, got {v}");

        // Full push → full trigger.
        dev.insert(("gilrs:switch_pro:0".to_string(), "left_stick_y".to_string()), Signal::Float(1.0));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let v = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()))
            .map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((v - 1.0).abs() < 0.05, "full stick push should give ~1.0 trigger, got {v}");

        // Neutral stick → trigger releases to 0.
        dev.insert(("gilrs:switch_pro:0".to_string(), "left_stick_y".to_string()), Signal::Float(0.0));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let v = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()))
            .map(|s| s.as_float()).unwrap_or(0.0);
        assert!(v.abs() < 0.05, "neutral stick should release trigger to 0, got {v}");
    }

    // A Remapper captures its output by chord-learning, so on a Switch Pro the
    // user maps to the DIGITAL ZR button (`btn_rt_dig`), not `right_trigger`.
    // In analog mode that digital-trigger target must still produce continuous
    // analog travel on the virtual pad — not a binary press.
    #[test]
    fn remapper_analog_to_digital_trigger_button_is_continuous() {
        let remap_uid = 1usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String("gilrs:switch_pro:0".into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["btn_rt_dig"], "mode": "analog" }
        ]));
        let src = source_node(3, "gilrs:switch_pro:0", 0.0);
        let sink = sink_node(2, "virtual.xinput:0", &format!("remap:{remap_uid}"), true);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        let mut dev = HashMap::new();
        dev.insert(("gilrs:switch_pro:0".to_string(), "left_stick_y".to_string()), Signal::Float(0.5));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let v = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()))
            .map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((v - 0.5).abs() < 0.05, "analog map to digital ZR should give ~0.5 analog RT, got {v}");
    }

    // Two analog mappings sharing an input but with different outputs must BOTH
    // fire: left_stick_up→right_trigger AND left_stick_up→left_stick_up should
    // drive the trigger AND keep the stick output (not replace one another).
    #[test]
    fn analog_same_input_different_outputs_both_fire() {
        let remap_uid = 1usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String("gilrs:switch_pro:0".into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["right_trigger"],  "mode": "analog" },
            { "in": ["left_stick_up"], "out": ["left_stick_up"],  "mode": "analog" }
        ]));
        let sink = sink_node(2, "virtual.xinput:0", &format!("remap:{remap_uid}"), true);
        let graph = ProcessingGraph { nodes: vec![remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        let mut dev = HashMap::new();
        dev.insert(("gilrs:switch_pro:0".to_string(), "left_stick_y".to_string()), Signal::Float(1.0));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);

        let rt = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()))
            .map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((rt - 1.0).abs() < 0.05, "trigger mapping should still fire, got RT={rt}");
        // The stick output must be preserved (left_stick_y stays at +1).
        let ly = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "left_stick_y".to_string()))
            .map(|s| s.as_float());
        let lstick = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "left_stick".to_string()))
            .and_then(|s| if let Signal::Vec2(v) = s { Some(v.y) } else { None });
        let y = ly.or(lstick).unwrap_or(-1.0);
        assert!((y - 1.0).abs() < 0.05, "stick output should be preserved, got left_stick_y={y}");
    }

    // A Remapper's mapped OUTPUT pin must survive a downstream Combiner whose
    // higher-priority port carries the raw device bus. Regression for the
    // "General purpose preset" button→button bug: a real controller reports
    // every button each tick (false when up), so the raw-bus Collector on port 0
    // explicitly carries `btn_rb = false`. With the old SORT (`first port wins`)
    // that false value clobbered the Remapper's `btn_rb = true` on port 1 — so a
    // single mapped button produced nothing, yet pressing both swapped buttons
    // lit both (those pins ARE consumed and take the hierarchy branch).
    //
    // Topology:  device → Collector (port 0) ┐
    //            device → Remapper  (port 1) ├→ Combiner → sink
    #[test]
    fn remapped_output_survives_combiner_raw_bus_priority() {
        let dev = "gilrs:switch_pro:0";
        let src = source_node(1, dev, 0.0);

        let mut collect = empty_node(2, "module.automap_collect");
        collect.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        collect.input_sources = vec![Some((0, 0))];
        collect.n_outputs = 1;

        let mut remap = empty_node(3, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["btn_south"], "out": ["btn_east"] },
            { "in": ["btn_east"],  "out": ["btn_rb"]   },
            { "in": ["btn_rb"],    "out": ["btn_east"] }
        ]));
        remap.input_sources = vec![Some((0, 0))];
        remap.n_outputs = 1;

        let mut combiner = empty_node(4, "module.automap_combiner");
        combiner.params.insert("_automap_input_devs".into(), Value::Array(vec![
            Value::String(String::new()), Value::String(String::new()),
        ]));
        combiner.params.insert("_automap_input_collectors".into(), Value::Array(vec![
            Value::String("collector:2".into()), Value::String("remap:3".into()),
        ]));
        combiner.input_sources = vec![Some((2, 0)), Some((3, 0))];
        combiner.n_outputs = 1;

        let sink = sink_node(20, "virtual.xinput:0", "combiner:4", true);
        let graph = ProcessingGraph { nodes: vec![src, collect, remap, combiner, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let getb = |out: &TickOutput, pin: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), pin.to_string())).map(|s| s.as_bool()).unwrap_or(false);
        // A real controller reports EVERY button each tick (false when up).
        let press = |pins: &[&str]| {
            let mut m = HashMap::new();
            for p in ["btn_south", "btn_east", "btn_rb", "btn_west", "btn_north", "btn_lb"] {
                m.insert((dev.to_string(), p.to_string()), Signal::Bool(pins.contains(&p)));
            }
            m
        };
        let tick = |graph: &ProcessingGraph, state: &mut HashMap<usize, NodeState>,
                    out: &mut TickOutput, pins: &[&str]| {
            // Settle (release) between presses so press-mode edges are clean.
            eval_graph_tick(graph, state, &press(&[]), 0.016, out);
            eval_graph_tick(graph, state, &press(pins), 0.016, out);
        };

        // south → east
        tick(&graph, &mut state, &mut out, &["btn_south"]);
        assert!(getb(&out, "btn_east"), "south→east must fire btn_east");
        assert!(!getb(&out, "btn_south"), "consumed btn_south must be suppressed");

        // east → rb
        tick(&graph, &mut state, &mut out, &["btn_east"]);
        assert!(getb(&out, "btn_rb"), "east→rb must fire btn_rb");
        assert!(!getb(&out, "btn_east"), "consumed btn_east must be suppressed");

        // rb → east
        tick(&graph, &mut state, &mut out, &["btn_rb"]);
        assert!(getb(&out, "btn_east"), "rb→east must fire btn_east");
        assert!(!getb(&out, "btn_rb"), "consumed btn_rb must be suppressed");

        // Pressing the swapped pair leaves both asserted (east↔rb swap).
        tick(&graph, &mut state, &mut out, &["btn_east", "btn_rb"]);
        assert!(getb(&out, "btn_east") && getb(&out, "btn_rb"),
            "east+rb swap should leave both asserted");
    }

    // Touchpad zone outputs synthesize finger touch points on the virtual pad,
    // and two simultaneous zone mappings stack onto the 2 hardware touch points.
    #[test]
    fn remapper_touch_zones_synthesize_and_stack() {
        let dev = "gilrs:switch_pro:0";
        let remap_uid = 1usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["btn_south"], "out": ["touch_left"]  },
            { "in": ["btn_east"],  "out": ["touch_right"] }
        ]));
        let sink = sink_node(2, "virtual.xinput:0", &format!("remap:{remap_uid}"), false);
        let graph = ProcessingGraph { nodes: vec![remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let getf = |out: &TickOutput, pin: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), pin.to_string())).map(|s| s.as_float());
        let getb = |out: &TickOutput, pin: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), pin.to_string())).map(|s| s.as_bool()).unwrap_or(false);

        // South only → one finger at the left zone.
        let mut dev_sigs = HashMap::new();
        dev_sigs.insert((dev.to_string(), "btn_south".to_string()), Signal::Bool(true));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!(getb(&out, "touch1_active"), "south→touch_left must activate a touch point");
        assert!((getf(&out, "touch1_x").unwrap_or(0.0) - (-0.66)).abs() < 0.05,
            "left zone x≈-0.66, got {:?}", getf(&out, "touch1_x"));
        assert!(!getb(&out, "touch2_active"), "only one finger for a single zone mapping");

        // South + East → two stacked fingers (left + right).
        dev_sigs.insert((dev.to_string(), "btn_east".to_string()), Signal::Bool(true));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!(getb(&out, "touch1_active") && getb(&out, "touch2_active"),
            "two zone mappings must stack onto 2 touch points");
        assert!((getf(&out, "touch2_x").unwrap_or(0.0) - 0.66).abs() < 0.05,
            "right zone x≈+0.66, got {:?}", getf(&out, "touch2_x"));

        // Release → both points report inactive (no latch).
        eval_graph_tick(&graph, &mut state, &HashMap::new(), 0.016, &mut out);
        assert!(!getb(&out, "touch1_active") && !getb(&out, "touch2_active"),
            "released zone mappings must release the touch points");
    }

    // "Hold zone": a tz_touch gesture that STARTS in a hold zone keeps firing that
    // zone's mapping even after the finger slides into a neighbour, and the
    // neighbour must NOT fire. Without the flag, crossing switches zones.
    #[test]
    fn touch_zones_hold_keeps_origin_zone_on_crossing() {
        let mk = |hold: bool| {
            let mut n = empty_node(1, "module.touch_zones");
            n.params.insert("zone_mode".into(), Value::String("mapping".into()));
            n.params.insert("_automap_device_id".into(), Value::String("pad".into()));
            n.params.insert("col_edges".into(), serde_json::json!([0.5])); // 2 columns
            n.params.insert("row_edges".into(), serde_json::json!([]));
            n.params.insert("zone_maps".into(), serde_json::json!([
                {"f":0,"z":0,"in":["tz_touch"],"out":["btn_south"]},
                {"f":0,"z":1,"in":["tz_touch"],"out":["btn_east"]},
            ]));
            if hold { n.params.insert("hold_zones".into(), serde_json::json!([[0,0]])); }
            n
        };
        // px in [-1,1] → unit x in [0,1]: -0.5→0.25 (zone 0), +0.5→0.75 (zone 1).
        let finger = |px: f32| {
            let mut m: HashMap<(String, String), Signal> = HashMap::new();
            m.insert(("pad".into(), "touch1_active".into()), Signal::Bool(true));
            m.insert(("pad".into(), "touch1_x".into()), Signal::Float(px));
            m.insert(("pad".into(), "touch1_y".into()), Signal::Float(0.0));
            m
        };
        let getb = |c: &HashMap<(String, String), Signal>, pin: &str|
            c.get(&("touchmap:1".to_string(), pin.to_string())).map(|s| s.as_bool()).unwrap_or(false);

        // WITH hold on zone 0.
        {
            let snap = mk(true);
            let mut state = HashMap::new();
            let mut c = HashMap::new();
            eval_touch_zones_map_node(&snap, 1, &finger(-0.5), &mut c, &mut state, 0.016);
            c.clear();
            eval_touch_zones_map_node(&snap, 1, &finger(-0.5), &mut c, &mut state, 0.016);
            assert!(getb(&c, "btn_south") && !getb(&c, "btn_east"), "zone0 fires btn_south");
            c.clear();
            eval_touch_zones_map_node(&snap, 1, &finger(0.5), &mut c, &mut state, 0.016);
            assert!(getb(&c, "btn_south"), "HOLD: origin zone still fires after crossing");
            assert!(!getb(&c, "btn_east"), "HOLD: crossed-into zone must NOT fire");
        }
        // WITHOUT hold — crossing switches zones.
        {
            let snap = mk(false);
            let mut state = HashMap::new();
            let mut c = HashMap::new();
            eval_touch_zones_map_node(&snap, 1, &finger(-0.5), &mut c, &mut state, 0.016);
            c.clear();
            eval_touch_zones_map_node(&snap, 1, &finger(-0.5), &mut c, &mut state, 0.016);
            c.clear();
            eval_touch_zones_map_node(&snap, 1, &finger(0.5), &mut c, &mut state, 0.016);
            assert!(!getb(&c, "btn_south") && getb(&c, "btn_east"),
                "no hold: crossing switches to the new zone");
        }
    }

    // Hold with an ANALOG origin zone: the analog output holds AND a button
    // mapped in the crossed-into zone must NOT fire (the held finger belongs
    // wholly to its origin; other zones ignore it).
    #[test]
    fn touch_zones_hold_analog_origin_suppresses_crossed_button() {
        let mut n = empty_node(1, "module.touch_zones");
        n.params.insert("zone_mode".into(), Value::String("mapping".into()));
        n.params.insert("_automap_device_id".into(), Value::String("pad".into()));
        n.params.insert("col_edges".into(), serde_json::json!([0.5]));
        n.params.insert("row_edges".into(), serde_json::json!([]));
        n.params.insert("zone_maps".into(), serde_json::json!([
            {"f":0,"z":0,"in":["tz_touch"],"out":["left_stick"]},
            {"f":0,"z":1,"in":["tz_touch"],"out":["btn_east"]},
        ]));
        n.params.insert("hold_zones".into(), serde_json::json!([[0,0]]));
        let finger = |px: f32| {
            let mut m: HashMap<(String, String), Signal> = HashMap::new();
            m.insert(("pad".into(), "touch1_active".into()), Signal::Bool(true));
            m.insert(("pad".into(), "touch1_x".into()), Signal::Float(px));
            m.insert(("pad".into(), "touch1_y".into()), Signal::Float(0.0));
            m
        };
        let mut state = HashMap::new();
        let mut c = HashMap::new();
        // Land in zone 0 (analog origin), establish the start zone.
        eval_touch_zones_map_node(&n, 1, &finger(-0.5), &mut c, &mut state, 0.016);
        c.clear();
        eval_touch_zones_map_node(&n, 1, &finger(-0.5), &mut c, &mut state, 0.016);
        // Cross into zone 1 (button). left_stick keeps outputting; btn_east silent.
        c.clear();
        eval_touch_zones_map_node(&n, 1, &finger(0.5), &mut c, &mut state, 0.016);
        assert!(c.contains_key(&("touchmap:1".to_string(), "left_stick".to_string())),
            "HOLD: analog origin keeps driving left_stick after crossing");
        let btn_east = c.get(&("touchmap:1".to_string(), "btn_east".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false);
        assert!(!btn_east, "HOLD: crossed-into button zone must NOT fire");
    }

    // A zone mapped to the analog scroll pins publishes a Float rate that tracks
    // the finger's deflection (+Y up, +X right) — the variable-speed scroll dest.
    #[test]
    fn touch_zones_analog_scroll_rate_tracks_deflection() {
        let mut n = empty_node(1, "module.touch_zones");
        n.params.insert("zone_mode".into(), Value::String("mapping".into()));
        n.params.insert("_automap_device_id".into(), Value::String("pad".into()));
        n.params.insert("col_edges".into(), serde_json::json!([])); // single zone
        n.params.insert("row_edges".into(), serde_json::json!([]));
        n.params.insert("zone_maps".into(), serde_json::json!([
            {"f":0,"z":0,"in":["tz_touch"],"out":["scroll_y","scroll_x"],"mode":"analog"},
        ]));
        let finger = |px: f32, py: f32| {
            let mut m: HashMap<(String, String), Signal> = HashMap::new();
            m.insert(("pad".into(), "touch1_active".into()), Signal::Bool(true));
            m.insert(("pad".into(), "touch1_x".into()), Signal::Float(px));
            m.insert(("pad".into(), "touch1_y".into()), Signal::Float(py));
            m
        };
        let getf = |c: &HashMap<(String, String), Signal>, pin: &str|
            c.get(&("touchmap:1".to_string(), pin.to_string())).map(|s| s.as_float()).unwrap_or(0.0);
        let mut state = HashMap::new();
        let mut c = HashMap::new();
        // Land at centre to establish the adaptive centre, then deflect up-right
        // (raw pad +Y is up; pad_point_to_unit flips it into y-down unit space).
        eval_touch_zones_map_node(&n, 1, &finger(0.0, 0.0), &mut c, &mut state, 0.016);
        c.clear();
        eval_touch_zones_map_node(&n, 1, &finger(0.8, 0.8), &mut c, &mut state, 0.016);
        assert!(getf(&c, "scroll_y") > 0.0, "upward deflection → scroll up (scroll_y > 0)");
        assert!(getf(&c, "scroll_x") > 0.0, "rightward deflection → scroll right (scroll_x > 0)");
    }

    // A zone can carry BOTH an analog (tz_touch) card and a click (tz_click) card;
    // clicking must still fire the click mapping while the analog output runs.
    #[test]
    fn touch_zones_analog_zone_click_still_fires() {
        let mut n = empty_node(1, "module.touch_zones");
        n.params.insert("zone_mode".into(), Value::String("mapping".into()));
        n.params.insert("_automap_device_id".into(), Value::String("pad".into()));
        n.params.insert("col_edges".into(), serde_json::json!([])); // single zone
        n.params.insert("row_edges".into(), serde_json::json!([]));
        n.params.insert("zone_maps".into(), serde_json::json!([
            {"f":0,"z":0,"in":["tz_touch"],"out":["mouse"],"mode":"analog"},
            {"f":0,"z":0,"in":["tz_click"],"out":["btn_east"],"mode":"down"},
        ]));
        let input = |click: bool| {
            let mut m: HashMap<(String, String), Signal> = HashMap::new();
            m.insert(("pad".into(), "touch1_active".into()), Signal::Bool(true));
            m.insert(("pad".into(), "touch1_x".into()), Signal::Float(0.3));
            m.insert(("pad".into(), "touch1_y".into()), Signal::Float(0.3));
            m.insert(("pad".into(), "btn_touchpad".into()), Signal::Bool(click));
            m
        };
        let getb = |c: &HashMap<(String, String), Signal>, pin: &str|
            c.get(&("touchmap:1".to_string(), pin.to_string())).map(|s| s.as_bool()).unwrap_or(false);
        let mut state = HashMap::new();
        let mut c = HashMap::new();
        eval_touch_zones_map_node(&n, 1, &input(false), &mut c, &mut state, 0.016);
        c.clear();
        eval_touch_zones_map_node(&n, 1, &input(true), &mut c, &mut state, 0.016);
        assert!(getb(&c, "btn_east"), "click on an analog zone must still fire the click mapping");
        assert!(c.contains_key(&("touchmap:1".to_string(), "mouse".to_string())),
            "analog output still runs alongside the click");
    }

    // Analog swipe drives a finger coordinate continuously (absolute position).
    #[test]
    fn remapper_swipe_tracks_analog_input() {
        let dev = "gilrs:switch_pro:0";
        let remap_uid = 1usize;
        let src = source_node(3, dev, 0.0);
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_right"], "out": ["touch_swipe_x"], "mode": "analog" }
        ]));
        let sink = sink_node(2, "virtual.xinput:0", &format!("remap:{remap_uid}"), false);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let getf = |out: &TickOutput, pin: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), pin.to_string())).map(|s| s.as_float());
        let getb = |out: &TickOutput, pin: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), pin.to_string())).map(|s| s.as_bool()).unwrap_or(false);

        // Half deflection → finger at ~+0.5.
        let mut dev_sigs = HashMap::new();
        dev_sigs.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(0.5));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!(getb(&out, "touch1_active"), "deflected swipe must activate the finger");
        assert!((getf(&out, "touch1_x").unwrap_or(0.0) - 0.5).abs() < 0.05,
            "swipe finger x should track deflection ~0.5, got {:?}", getf(&out, "touch1_x"));

        // Neutral stick → finger released.
        dev_sigs.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(0.0));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!(!getb(&out, "touch1_active"), "neutral swipe must release the finger");
    }

    // Combo: a BUTTON gates the finger, the LS axes drive both swipe axes (routed
    // by orientation). Buttons must NOT contribute a value (regression for the
    // "stuck at full" bug). Both directions of an axis cover both halves.
    #[test]
    fn remapper_swipe_button_gate_with_two_axis_inputs() {
        let dev = "gilrs:switch_pro:0";
        let remap_uid = 1usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        // Button gate + LS in all 4 directions → swipe X + swipe Y.
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["btn_lb", "left_stick_left", "left_stick_right",
                     "left_stick_up", "left_stick_down"],
              "out": ["touch_swipe_x", "touch_swipe_y"], "mode": "analog" }
        ]));
        // Zero-deadzone source so the test measures the mapping, not the curve.
        let src = source_node(3, dev, 0.0);
        let sink = sink_node(2, "virtual.xinput:0", &format!("remap:{remap_uid}"), false);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let getf = |out: &TickOutput, pin: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), pin.to_string())).map(|s| s.as_float()).unwrap_or(0.0);
        let getb = |out: &TickOutput, pin: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), pin.to_string())).map(|s| s.as_bool()).unwrap_or(false);

        // Stick deflected but button UP → no finger (button gates).
        let mut dev_sigs = HashMap::new();
        dev_sigs.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(0.6));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!(!getb(&out, "touch1_active"), "button not held → no finger");

        // Button held, stick centered → finger DOWN at center (button gates,
        // analog at rest → NOT stuck at full).
        dev_sigs.insert((dev.to_string(), "btn_lb".to_string()), Signal::Bool(true));
        dev_sigs.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(0.0));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!(getb(&out, "touch1_active"), "button held → finger active even centered");
        assert!(getf(&out, "touch1_x").abs() < 0.05 && getf(&out, "touch1_y").abs() < 0.05,
            "centered stick → finger at center, got ({},{})", getf(&out,"touch1_x"), getf(&out,"touch1_y"));

        // Button held + stick right → X tracks; right uses the positive half.
        dev_sigs.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(0.6));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!((getf(&out, "touch1_x") - 0.6).abs() < 0.05,
            "stick right → swipe x ~+0.6, got {}", getf(&out, "touch1_x"));

        // Button held + stick left → negative half of the SAME axis.
        dev_sigs.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(-0.8));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!((getf(&out, "touch1_x") - (-0.8)).abs() < 0.05,
            "stick left → swipe x ~-0.8, got {}", getf(&out, "touch1_x"));

        // Vertical axis drives swipe Y independently (stick up = +Y).
        dev_sigs.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(0.0));
        dev_sigs.insert((dev.to_string(), "left_stick_y".to_string()), Signal::Float(0.5));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!((getf(&out, "touch1_y") - 0.5).abs() < 0.05,
            "stick up → swipe y ~+0.5, got {}", getf(&out, "touch1_y"));
    }

    // A touch combo that mixes opposite cardinals of one axis (left+right) can
    // never be "all held at once", so the generic suppression test would never
    // consume its gate button — the button would leak through to pass-through.
    // The touch-combo activation rule must drive suppression: while the combo is
    // active, the gate button (and the driving stick) are consumed.
    #[test]
    fn remapper_touch_combo_suppresses_gate_button_with_multi_axis() {
        let dev = "gilrs:switch_pro:0";
        let remap_uid = 1usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        // Gate button + LS in all 4 directions → swipe X + swipe Y.
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["btn_lb", "left_stick_left", "left_stick_right",
                     "left_stick_up", "left_stick_down"],
              "out": ["touch_swipe_x", "touch_swipe_y"], "mode": "analog" }
        ]));
        let src = source_node(3, dev, 0.0);
        let sink = sink_node(2, "virtual.xinput:0", &format!("remap:{remap_uid}"), false);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let getf = |out: &TickOutput, pin: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), pin.to_string())).map(|s| s.as_float()).unwrap_or(0.0);
        let getb = |out: &TickOutput, pin: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), pin.to_string())).map(|s| s.as_bool()).unwrap_or(false);

        // Button up: combo inactive → button passes through normally.
        let mut dev_sigs = HashMap::new();
        dev_sigs.insert((dev.to_string(), "btn_lb".to_string()), Signal::Bool(false));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!(!getb(&out, "btn_lb"), "button up → nothing to pass through");

        // Button held + stick deflected (one direction, combo active): the finger
        // is down AND the gate button is suppressed from pass-through, even though
        // the opposite cardinal of the same axis is also in the combo.
        dev_sigs.insert((dev.to_string(), "btn_lb".to_string()), Signal::Bool(true));
        dev_sigs.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(0.6));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!(getb(&out, "touch1_active"), "combo active → finger down");
        assert!((getf(&out, "touch1_x") - 0.6).abs() < 0.05, "stick drives swipe x");
        assert!(!getb(&out, "btn_lb"),
            "active touch combo must suppress its gate button (was leaking with multi-axis)");

        // Button held, stick centered: finger down at center, button still consumed.
        dev_sigs.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(0.0));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!(getb(&out, "touch1_active"), "button held → finger active even centered");
        assert!(!getb(&out, "btn_lb"), "gate button stays suppressed while combo held");

        // Button released → combo inactive → finger up, button no longer consumed.
        dev_sigs.insert((dev.to_string(), "btn_lb".to_string()), Signal::Bool(false));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!(!getb(&out, "touch1_active"), "button released → finger up");
    }

    // The DualSense mic button is a canonical pin: a normal button→btn_mute map
    // reaches the sink with no special handling.
    #[test]
    fn remapper_mic_button_reaches_sink() {
        let dev = "gilrs:switch_pro:0";
        let remap_uid = 1usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["btn_south"], "out": ["btn_mute"] }
        ]));
        let sink = sink_node(2, "virtual.xinput:0", &format!("remap:{remap_uid}"), false);
        let graph = ProcessingGraph { nodes: vec![remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let mut dev_sigs = HashMap::new();
        dev_sigs.insert((dev.to_string(), "btn_south".to_string()), Signal::Bool(true));
        eval_graph_tick(&graph, &mut state, &dev_sigs, 0.016, &mut out);
        assert!(out.sink_outputs.get(&("virtual.xinput:0".to_string(), "btn_mute".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false), "btn_south→btn_mute must reach the sink");
    }

    // The implicit digital→analog bridge must RELEASE: pressing then releasing
    // the Switch digital ZR should drive the virtual analog RT to 1.0 then back
    // to 0.0 (regression guard for the "stuck at full press" bug).
    #[test]
    fn digital_bridge_presses_and_releases() {
        // Direct device → sink (no remapper); src_dev is the physical device.
        let sink = sink_node(1, "virtual.xinput:0", "gilrs:switch_pro:0", true);
        let graph = ProcessingGraph { nodes: vec![sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // ZR pressed.
        let mut dev = HashMap::new();
        dev.insert(("gilrs:switch_pro:0".to_string(), "btn_rt_dig".to_string()), Signal::Bool(true));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let v = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()))
            .map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((v - 1.0).abs() < 0.01, "pressed ZR should give full RT, got {v}");

        // ZR released → must go back to 0, not latch.
        dev.insert(("gilrs:switch_pro:0".to_string(), "btn_rt_dig".to_string()), Signal::Bool(false));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let v = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()))
            .map(|s| s.as_float()).unwrap_or(-1.0);
        assert!(v.abs() < 0.01, "released ZR should release RT to 0, got {v}");
    }

    // With the bridge DISABLED (analog-capable pad, toggle off), the digital
    // button must NOT leak into the analog trigger.
    #[test]
    fn digital_bridge_disabled_does_not_leak() {
        let sink = sink_node(1, "virtual.xinput:0", "gilrs:xinput:0", false);
        let graph = ProcessingGraph { nodes: vec![sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        let mut dev = HashMap::new();
        dev.insert(("gilrs:xinput:0".to_string(), "btn_rt_dig".to_string()), Signal::Bool(true));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let leaked = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "right_trigger".to_string()));
        assert!(leaked.is_none(), "bridge off: digital button must not drive analog trigger, got {leaked:?}");
    }

    // "Digital triggers" opt-in on an analog-capable pad: the calibrated analog
    // trigger must SNAP to full/zero at the Calibration threshold (not pass
    // through continuously), and the digital LT/RT buttons must be re-derived
    // from that SAME threshold rather than the pad's early-firing L2/R2 flag.
    #[test]
    fn digital_triggers_snap_analog_and_rederive_button() {
        let dev_id = "gilrs:dualsense:0";
        let mut src = empty_node(1, "device.source");
        src.device_id = Some(dev_id.to_string());
        src.params.insert("digital_triggers".into(), Value::Bool(true));
        src.params.insert("ltrig_digital_threshold".into(), Value::from(0.5));
        let graph = ProcessingGraph { nodes: vec![src] };

        // Below threshold: analog snaps to 0, digital button ignores the pad's
        // early-fired flag and stays off.
        let mut dev = HashMap::new();
        dev.insert((dev_id.to_string(), "left_trigger".to_string()), Signal::Float(0.3));
        dev.insert((dev_id.to_string(), "btn_lt_dig".to_string()),  Signal::Bool(true));
        let out = preprocess_dev_sigs(&graph, &dev);
        assert_eq!(out.get(&(dev_id.to_string(), "left_trigger".to_string())).map(|s| s.as_float()),
            Some(0.0), "below threshold must snap analog trigger to 0");
        assert_eq!(out.get(&(dev_id.to_string(), "btn_lt_dig".to_string())).map(|s| s.as_bool()),
            Some(false), "digital button must follow the calibration threshold, not the pad flag");

        // Above threshold: analog snaps to full (staying Float), button on.
        dev.insert((dev_id.to_string(), "left_trigger".to_string()), Signal::Float(0.7));
        dev.insert((dev_id.to_string(), "btn_lt_dig".to_string()),  Signal::Bool(false));
        let out = preprocess_dev_sigs(&graph, &dev);
        assert_eq!(out.get(&(dev_id.to_string(), "left_trigger".to_string())).copied(),
            Some(Signal::Float(1.0)), "above threshold must snap analog trigger to full Float(1.0)");
        assert_eq!(out.get(&(dev_id.to_string(), "btn_lt_dig".to_string())).map(|s| s.as_bool()),
            Some(true), "above threshold must fire the digital button");
    }

    // With "Digital triggers" OFF the analog trigger passes through unchanged —
    // no thresholding, full continuous travel.
    #[test]
    fn digital_triggers_off_passes_analog_through() {
        let dev_id = "gilrs:dualsense:0";
        let mut src = empty_node(1, "device.source");
        src.device_id = Some(dev_id.to_string());
        // digital_triggers absent → defaults to off.
        let graph = ProcessingGraph { nodes: vec![src] };

        let mut dev = HashMap::new();
        dev.insert((dev_id.to_string(), "left_trigger".to_string()), Signal::Float(0.3));
        let out = preprocess_dev_sigs(&graph, &dev);
        let v = out.get(&(dev_id.to_string(), "left_trigger".to_string())).map(|s| s.as_float()).unwrap_or(-1.0);
        assert!((v - 0.3).abs() < 1e-4, "digital triggers off must pass analog through, got {v}");
    }

    /// Build a `device.source` node carrying a `deadzone` param so
    /// `preprocess_dev_sigs` picks it up for the named device.
    fn source_node(uid: usize, device_id: &str, deadzone: f32) -> NodeSnap {
        let mut n = empty_node(uid, "device.source");
        n.device_id = Some(device_id.to_string());
        n.params.insert("deadzone".into(), Value::from(deadzone as f64));
        n
    }

    // A direct AutoMap wire (device.source → sink) must apply the source
    // node's stick deadzone. A small stick value inside the deadzone must
    // collapse to 0 at the sink; a value past it must pass through (rescaled).
    #[test]
    fn automap_stick_respects_source_deadzone() {
        let src = source_node(1, "gilrs:xinput:0", 0.2);
        let sink = sink_node(2, "virtual.xinput:0", "gilrs:xinput:0", false);
        let graph = ProcessingGraph { nodes: vec![src, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Stick nudged to 0.1 — inside the 0.2 deadzone → sink must read 0.
        let mut dev = HashMap::new();
        dev.insert(("gilrs:xinput:0".to_string(), "left_stick_x".to_string()), Signal::Float(0.1));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let x = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "left_stick_x".to_string()))
            .map(|s| s.as_float());
        let lstick_x = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "left_stick".to_string()))
            .and_then(|s| if let Signal::Vec2(v) = s { Some(v.x) } else { None });
        let v = x.or(lstick_x).unwrap_or(0.0);
        assert!(v.abs() < 1e-4, "stick inside deadzone must collapse to 0 at sink, got {v}");

        // Stick pushed to 0.6 — past the deadzone → passes through (rescaled).
        dev.insert(("gilrs:xinput:0".to_string(), "left_stick_x".to_string()), Signal::Float(0.6));
        eval_graph_tick(&graph, &mut state, &dev, 0.016, &mut out);
        let x = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "left_stick_x".to_string()))
            .map(|s| s.as_float());
        let lstick_x = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "left_stick".to_string()))
            .and_then(|s| if let Signal::Vec2(v) = s { Some(v.x) } else { None });
        let v = x.or(lstick_x).unwrap_or(0.0);
        assert!(v > 0.01, "stick past deadzone must reach sink, got {v}");
    }

    /// device.source → Remapper (analog stick-cardinal → key) → keymouse sink.
    /// Reproduces the user-reported case: WASD via analog-mode stick mapping.
    fn keymouse_sink_from_remap(uid: usize, remap_uid: usize) -> NodeSnap {
        let mut n = empty_node(uid, "device.sink");
        n.sink_target = Some(SinkTarget {
            device_id: "virtual.keymouse:0".to_string(),
            pin_ids: canonical_pins(),
            multi_sources: vec![Vec::new(); canonical_pins().len()],
            automap_source: Some((format!("remap:{remap_uid}"), canonical_pins())),
            automap_fallback_dev: None,
            feedback_sources: Vec::new(),
            is_self_sink: false,
            digital_trigger_bridge: false,
        });
        n
    }

    // End-to-end: a zone mapped touch→mouse_left must drive the keymouse sink's
    // mouse_left pin (regression guard for "touch/click → mouse button does
    // nothing"). Exercises the full graph tick: tz node → touchmap bus → sink.
    #[test]
    fn touch_zone_button_reaches_keymouse_sink() {
        let dev = "pad";
        let tz_uid = 2usize;
        let mut tz = empty_node(tz_uid, "module.touch_zones");
        tz.params.insert("zone_mode".into(), Value::String("mapping".into()));
        tz.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        tz.params.insert("col_edges".into(), serde_json::json!([]));
        tz.params.insert("row_edges".into(), serde_json::json!([]));
        tz.params.insert("zone_maps".into(), serde_json::json!([
            {"f":0,"z":0,"in":["tz_touch"],"out":["mouse_left"],"mode":"down"},
        ]));
        let mut sink = empty_node(3, "device.sink");
        sink.sink_target = Some(SinkTarget {
            device_id: "virtual.keymouse:0".to_string(),
            pin_ids: canonical_pins(),
            multi_sources: vec![Vec::new(); canonical_pins().len()],
            automap_source: Some((format!("touchmap:{tz_uid}"), canonical_pins())),
            automap_fallback_dev: None,
            feedback_sources: Vec::new(),
            is_self_sink: false,
            digital_trigger_bridge: false,
        });
        let graph = ProcessingGraph { nodes: vec![tz, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "touch1_active".to_string()), Signal::Bool(true));
        sigs.insert((dev.to_string(), "touch1_x".to_string()), Signal::Float(0.0));
        sigs.insert((dev.to_string(), "touch1_y".to_string()), Signal::Float(0.0));
        eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);
        let lmb = out.sink_outputs.get(&("virtual.keymouse:0".to_string(), "mouse_left".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false);
        assert!(lmb, "touch→mouse_left must reach the keymouse sink");
    }

    #[test]
    fn analog_stick_to_key_respects_source_deadzone() {
        let dev = "gilrs:xinput:0";
        let remap_uid = 2usize;
        let src = source_node(1, dev, 0.3); // 0.3 deadzone on the device.
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["key_w"], "mode": "analog" }
        ]));
        let sink = keymouse_sink_from_remap(3, remap_uid);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Stick pushed UP to 0.15 — INSIDE the 0.3 deadzone. key_w must NOT fire.
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "left_stick_y".to_string()), Signal::Float(0.15));
        eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);
        let w = out.sink_outputs.get(&("virtual.keymouse:0".to_string(), "key_w".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false);
        assert!(!w, "stick inside deadzone must NOT fire key_w, but it did");

        // Stick pushed UP to 0.8 — past the deadzone. key_w must fire.
        sigs.insert((dev.to_string(), "left_stick_y".to_string()), Signal::Float(0.8));
        eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);
        let w = out.sink_outputs.get(&("virtual.keymouse:0".to_string(), "key_w".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false);
        assert!(w, "stick past deadzone must fire key_w, but it didn't");
    }

    /// Even when no device.source node carries the deadzone (the device feeds
    /// AutoMap consumers like the Remapper without its source node present in
    /// the graph), stick pins must still get the default deadzone rather than
    /// passing through raw. Regression guard for the "analog stick→key ignores
    /// deadzone" report.
    #[test]
    fn analog_stick_to_key_default_deadzone_without_source_node() {
        let dev = "gilrs:xinput:0";
        let remap_uid = 2usize;
        // No source_node: only remapper + sink. The default deadzone must
        // still apply (DEFAULT_STICK_DEADZONE), not 0.
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["key_w"], "mode": "analog" }
        ]));
        let sink = keymouse_sink_from_remap(3, remap_uid);
        let graph = ProcessingGraph { nodes: vec![remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // 0.05 is below the default deadzone → key_w must NOT fire.
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "left_stick_y".to_string()), Signal::Float(0.05));
        eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);
        let w = out.sink_outputs.get(&("virtual.keymouse:0".to_string(), "key_w".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false);
        assert!(!w, "small stick (below default deadzone) must NOT fire key_w");
    }

    /// Run the graph for `ticks` frames at `dt`, holding `stick_y`, and return
    /// (on_count, edge_count) for the keymouse `out_pin`. Edges count rising
    /// transitions so we can tell a tap train apart from a steady gate.
    fn count_pulses(
        dev: &str, remap_uid: usize, out_pin: &str, mode_extra: serde_json::Value,
        stick_y: f32, ticks: usize, dt: f32,
    ) -> (usize, usize) {
        let src = source_node(1, dev, 0.0); // zero deadzone: measure modulation only.
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        let mut mapping = serde_json::json!(
            { "in": ["left_stick_up"], "out": [out_pin], "mode": "analog" }
        );
        if let Some(obj) = mapping.as_object_mut() {
            if let Some(extra) = mode_extra.as_object() {
                for (k, v) in extra { obj.insert(k.clone(), v.clone()); }
            }
        }
        remap.params.insert("mappings".into(), serde_json::Value::Array(vec![mapping]));
        let sink = keymouse_sink_from_remap(3, remap_uid);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "left_stick_y".to_string()), Signal::Float(stick_y));

        let mut on = 0usize;
        let mut edges = 0usize;
        let mut prev = false;
        for _ in 0..ticks {
            eval_graph_tick(&graph, &mut state, &sigs, dt, &mut out);
            let w = out.sink_outputs.get(&("virtual.keymouse:0".to_string(), out_pin.to_string()))
                .map(|s| s.as_bool()).unwrap_or(false);
            if w { on += 1; }
            if w && !prev { edges += 1; }
            prev = w;
        }
        (on, edges)
    }

    // Plain analog → digital must produce a TAP TRAIN, and a harder push must
    // tap MORE often than a light push (frequency tracks amplitude).
    #[test]
    fn analog_digital_tap_train_frequency_tracks_amplitude() {
        let extra = serde_json::json!({ "window_ms": 30.0 });
        // 1 second at 1ms ticks for clean frequency counting.
        let (_, edges_light) = count_pulses("gilrs:xinput:0", 2, "key_w", extra.clone(), 0.3, 1000, 0.001);
        let (_, edges_hard)  = count_pulses("gilrs:xinput:0", 2, "key_w", extra, 1.0, 1000, 0.001);
        assert!(edges_light >= 2, "light push should still tap a few times, got {edges_light}");
        assert!(edges_hard > edges_light,
            "harder push must tap more often: hard={edges_hard} light={edges_light}");
    }

    // Hold mode → PWM: duty cycle (ON fraction) must track amplitude. A
    // light push has a low duty; full deflection is (near) always on.
    #[test]
    fn analog_digital_hold_pwm_duty_tracks_amplitude() {
        let extra = serde_json::json!({ "window_ms": 40.0, "sustain": true });
        let (on_light, _) = count_pulses("gilrs:xinput:0", 2, "key_w", extra.clone(), 0.25, 1000, 0.001);
        let (on_full, _)  = count_pulses("gilrs:xinput:0", 2, "key_w", extra, 1.0, 1000, 0.001);
        let duty_light = on_light as f32 / 1000.0;
        let duty_full  = on_full as f32 / 1000.0;
        assert!(duty_light > 0.05 && duty_light < 0.6,
            "light Hold duty should be low-ish, got {duty_light}");
        assert!(duty_full > 0.9, "full deflection Hold should be near-always-on, got {duty_full}");
    }

    // Turbo (no Hold) doubles the max frequency, so at full deflection it
    // taps more often than plain analog at the same window_ms.
    #[test]
    fn analog_digital_turbo_doubles_frequency() {
        let plain = serde_json::json!({ "window_ms": 30.0 });
        let turbo = serde_json::json!({ "window_ms": 30.0, "turbo": true });
        let (_, edges_plain) = count_pulses("gilrs:xinput:0", 2, "key_w", plain, 1.0, 1000, 0.001);
        let (_, edges_turbo) = count_pulses("gilrs:xinput:0", 2, "key_w", turbo, 1.0, 1000, 0.001);
        assert!(edges_turbo > edges_plain,
            "turbo must tap faster at full deflection: turbo={edges_turbo} plain={edges_plain}");
    }

    // Unit-level coverage of the shared analog→digital modulator.
    #[test]
    fn analog_digital_pulse_unit() {
        let dt = 0.001;
        let run = |mag: f32, window_ms: f32, sustain: bool, turbo: bool| -> (usize, usize) {
            let mut slots = [0.0f32; PRESS_SLOTS_PER_MAPPING];
            let mut on = 0usize;
            let mut edges = 0usize;
            let mut prev = false;
            for _ in 0..1000 {
                let v = analog_digital_pulse(mag, window_ms, sustain, turbo, &mut slots, dt);
                if v { on += 1; }
                if v && !prev { edges += 1; }
                prev = v;
            }
            (on, edges)
        };

        // Zero magnitude → never on.
        assert_eq!(run(0.0, 30.0, false, false).0, 0, "mag 0 must be silent");

        // Plain tap train: more deflection → more taps.
        let (_, e_light) = run(0.3, 30.0, false, false);
        let (_, e_hard)  = run(1.0, 30.0, false, false);
        assert!(e_hard > e_light, "freq must rise with mag: {e_hard} > {e_light}");

        // Regression: at the REALISTIC default window_ms (200ms) the plain
        // tap train must be ~50% duty (a clean tap), NOT a near-held key.
        // The old tap_on=window_ms made this ~90% duty → felt held.
        let (on_default, edges_default) = run(1.0, 200.0, false, false);
        let duty = on_default as f32 / 1000.0;
        assert!(duty > 0.35 && duty < 0.65,
            "plain tap at default window must be ~50% duty, got {duty} (held-key regression)");
        assert!(edges_default >= 4, "must actually tap multiple times in 1s, got {edges_default}");

        // Hold PWM: duty tracks magnitude; full → always on.
        let (on_q, _)    = run(0.25, 40.0, true, false);
        let (on_full, _) = run(1.0, 40.0, true, false);
        assert!(on_q > 0 && (on_q as f32 / 1000.0) < 0.6, "quarter duty should be low, got {on_q}/1000");
        assert!(on_full as f32 / 1000.0 > 0.9, "full Hold should be near always-on, got {on_full}/1000");

        // Turbo doubles frequency at full deflection.
        let (_, e_plain) = run(1.0, 30.0, false, false);
        let (_, e_turbo) = run(1.0, 30.0, false, true);
        assert!(e_turbo > e_plain, "turbo faster: {e_turbo} > {e_plain}");
    }

    // on_press / on_release now honor `window_ms` as the emitted trigger
    // duration (floored at the 10ms minimum pulse).
    #[test]
    fn on_press_release_trigger_duration_tracks_window_ms() {
        let dt = 0.001; // 1 ms/tick

        // Drive a press: hold for `hold_ticks`, then release; count how many
        // ticks the output stays ON after the relevant edge.
        let run_on_press = |window_ms: f32| -> usize {
            let mut slots = [0.0f32; PRESS_SLOTS_PER_MAPPING];
            let mut on = 0usize;
            // rising edge at tick 0; hold a few ticks then release.
            for t in 0..1000 {
                let raw = t < 5; // pressed for 5 ms
                if apply_press_mode(raw, PressMode::OnPress, window_ms, false, &mut slots, dt) {
                    on += 1;
                }
            }
            on
        };

        // ~50 ms window → ~50 on-ticks (within tolerance for the dt countdown).
        let n50 = run_on_press(50.0);
        assert!((45..=55).contains(&n50), "50ms on_press should stay ~50 ticks, got {n50}");
        // ~200 ms window → ~200 on-ticks.
        let n200 = run_on_press(200.0);
        assert!((190..=210).contains(&n200), "200ms on_press should stay ~200 ticks, got {n200}");
        // Longer window → strictly longer trigger.
        assert!(n200 > n50, "larger window_ms must lengthen the trigger");

        // Floor: a 0 ms window still emits at least the 10ms minimum pulse.
        let n0 = run_on_press(0.0);
        assert!(n0 >= 9 && n0 <= 12, "0ms window floors to ~10ms pulse, got {n0}");

        // on_release fires on the falling edge with the same duration rule.
        let run_on_release = |window_ms: f32| -> usize {
            let mut slots = [0.0f32; PRESS_SLOTS_PER_MAPPING];
            let mut on = 0usize;
            for t in 0..1000 {
                let raw = t < 5; // release happens at tick 5
                if apply_press_mode(raw, PressMode::OnRelease, window_ms, false, &mut slots, dt) {
                    on += 1;
                }
            }
            on
        };
        let r100 = run_on_release(100.0);
        assert!((95..=105).contains(&r100), "100ms on_release should stay ~100 ticks, got {r100}");
    }

    // Processing wired BEFORE a Collector (explicit input port) must be what
    // the downstream Remapper sees — not the raw device sample. Here a
    // `module.constant` stands in for a Response Curve that re-maps the stick
    // amplitude: the device pushes left_stick_y small (raw), but the constant
    // feeds left_stick_y = 0.9 into the collector port. The Remapper's analog
    // stick→key mapping must therefore see ~0.9, firing key_w.
    #[test]
    fn processing_through_collector_drives_remapper_amplitude() {
        let dev = "gilrs:xinput:0";
        let src = source_node(1, dev, 0.0);

        // Constant node → emulates Response Curve output (Float 0.9).
        let mut konst = empty_node(2, "module.constant");
        konst.n_outputs = 1;
        konst.params.insert("value".into(), Value::from(0.9_f64));

        // Collector: AutoMap bus (input 0, from device) + explicit port for
        // left_stick_y (input 1, from the constant). _collect_pin_ids[0] names
        // that port's pin.
        let mut collect = empty_node(3, "module.automap_collect");
        collect.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        collect.params.insert("_collect_pin_ids".into(),
            Value::Array(vec![Value::String("left_stick_y".into())]));
        // input_sources: [0]=bus (device.source idx 0 out 0), [1]=constant out 0.
        collect.input_sources = vec![Some((0, 0)), Some((1, 0))];

        // Remapper reads the collector, maps left_stick_up (analog) → key_w.
        let remap_uid = 4usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_collector_id".into(),
            Value::String("collector:3".into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["key_w"], "mode": "analog", "window_ms": 30.0 }
        ]));
        let sink = keymouse_sink_from_remap(5, remap_uid);

        let graph = ProcessingGraph { nodes: vec![src, konst, collect, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Device stick is near-neutral (0.05) — raw would not fire. The
        // collector override (0.9) should drive the Remapper instead.
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "left_stick_y".to_string()), Signal::Float(0.05));

        // Run several ticks; key_w should tap at least once (proves the
        // processed 0.9 amplitude reached the Remapper, not the raw 0.05).
        let mut fired = false;
        for _ in 0..200 {
            eval_graph_tick(&graph, &mut state, &sigs, 0.001, &mut out);
            if out.sink_outputs.get(&("virtual.keymouse:0".to_string(), "key_w".to_string()))
                .map(|s| s.as_bool()).unwrap_or(false)
            {
                fired = true; break;
            }
        }
        assert!(fired, "processed amplitude through Collector must drive the Remapper (key_w never fired)");
    }

    // Combiner hierarchy: a pin a Remapper CONSUMED (mapped away) must not leak
    // through a lower-priority raw-device port under a non-ADD policy, but ADD
    // explicitly opts back into mixing.
    fn combiner_node(
        uid: usize, remap_uid: usize, raw_dev: &str, policy: &str,
    ) -> NodeSnap {
        let mut n = empty_node(uid, "module.automap_combiner");
        // Port 0 = Remapper collector; Port 1 = raw device.
        n.params.insert("_automap_input_devs".into(), Value::Array(vec![
            Value::String(String::new()),
            Value::String(raw_dev.into()),
        ]));
        n.params.insert("_automap_input_collectors".into(), Value::Array(vec![
            Value::String(format!("remap:{remap_uid}")),
            Value::String(String::new()),
        ]));
        let mut policy_obj = serde_json::Map::new();
        policy_obj.insert("btn_south".into(), Value::String(policy.into()));
        n.params.insert("combiner_pin_policy".into(), Value::Object(policy_obj));
        n.input_sources = vec![Some((0, 0)), Some((1, 0))]; // shape only
        n
    }

    fn run_combiner_leak(policy: &str) -> bool {
        let dev = "gilrs:xinput:0";
        let src = source_node(1, dev, 0.0);
        // Remapper consumes btn_south (maps it to btn_west), so btn_south is
        // claimed and should be suppressed downstream.
        let remap_uid = 2usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["btn_south"], "out": ["btn_west"], "mode": "down" }
        ]));
        let combiner = combiner_node(3, remap_uid, dev, policy);
        // Sink auto-maps FROM the combiner.
        let sink = sink_node(4, "virtual.xinput:0", "combiner:3", false);
        let graph = ProcessingGraph { nodes: vec![src, remap, combiner, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Physical btn_south held.
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "btn_south".to_string()), Signal::Bool(true));
        eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);
        // Did btn_south leak to the sink?
        out.sink_outputs.get(&("virtual.xinput:0".to_string(), "btn_south".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false)
    }

    #[test]
    fn combiner_suppresses_consumed_pin_unless_add() {
        // SORT (default) and OR must NOT leak the consumed btn_south.
        assert!(!run_combiner_leak("SORT"), "SORT must suppress consumed btn_south");
        assert!(!run_combiner_leak("OR"),   "OR must suppress consumed btn_south");
        // ADD explicitly mixes → the raw-port btn_south is allowed through.
        assert!(run_combiner_leak("ADD"), "ADD must let the raw btn_south mix through");
    }

    // Per-PORT default policy: setting the raw port's default to ADD opts that
    // port back into mixing for ALL its pins (no per-pin override needed), so a
    // consumed pin leaks through exactly as an explicit per-pin ADD would.
    #[test]
    fn combiner_per_port_default_add_opts_into_mixing() {
        let dev = "gilrs:xinput:0";
        let src = source_node(1, dev, 0.0);
        let remap_uid = 2usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["btn_south"], "out": ["btn_west"], "mode": "down" }
        ]));
        // Combiner with NO per-pin policy, but port 1 (raw) default = ADD.
        let mut combiner = empty_node(3, "module.automap_combiner");
        combiner.params.insert("_automap_input_devs".into(), Value::Array(vec![
            Value::String(String::new()), Value::String(dev.into()),
        ]));
        combiner.params.insert("_automap_input_collectors".into(), Value::Array(vec![
            Value::String(format!("remap:{remap_uid}")), Value::String(String::new()),
        ]));
        let mut port_def = serde_json::Map::new();
        port_def.insert("1".into(), Value::String("ADD".into()));
        combiner.params.insert("combiner_port_default".into(), Value::Object(port_def));
        combiner.input_sources = vec![Some((0, 0)), Some((1, 0))];

        let sink = sink_node(4, "virtual.xinput:0", "combiner:3", false);
        let graph = ProcessingGraph { nodes: vec![src, remap, combiner, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "btn_south".to_string()), Signal::Bool(true));
        eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);

        let leaked = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "btn_south".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false);
        assert!(leaked, "port-default ADD must opt the raw port into mixing (btn_south should pass)");
    }

    // D-pad PER-SIDE suppression: mapping only `dpad_left` away must suppress
    // the left direction across ALL three representations (Bool, dpad_x
    // negative side, dpad Vec2 x-negative) — but leave `dpad_right`, the
    // positive X side, and the entire Y axis / up-down untouched.
    #[test]
    fn dpad_left_mapped_away_suppresses_only_left_side() {
        let dev = "gilrs:xinput:0";
        let src = source_node(1, dev, 0.0);
        let remap_uid = 2usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["dpad_left"], "out": ["btn_south"], "mode": "down" }
        ]));
        let sink = sink_node(3, "virtual.xinput:0", &format!("remap:{remap_uid}"), false);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Physical D-pad: LEFT held (claimed) AND DOWN held (NOT claimed).
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "dpad_left".to_string()), Signal::Bool(true));
        sigs.insert((dev.to_string(), "dpad_down".to_string()), Signal::Bool(true));
        sigs.insert((dev.to_string(), "dpad_x".to_string()),    Signal::Float(-1.0));
        sigs.insert((dev.to_string(), "dpad_y".to_string()),    Signal::Float(-1.0));
        sigs.insert((dev.to_string(), "dpad".to_string()),      Signal::Vec2(Vec2::new(-1.0, -1.0)));
        eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);

        let get_b = |p: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), p.to_string())).map(|s| s.as_bool()).unwrap_or(false);
        let get_f = |p: &str| out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), p.to_string())).map(|s| s.as_float()).unwrap_or(0.0);
        let dpad_vec = || out.sink_outputs
            .get(&("virtual.xinput:0".to_string(), "dpad".to_string()))
            .and_then(|s| if let Signal::Vec2(v) = s { Some(*v) } else { None }).unwrap_or(Vec2::ZERO);

        // Mapped target fires.
        assert!(get_b("btn_south"), "dpad_left→btn_south must fire");

        // The sink resolves Vec2-vs-axis conflicts by keeping ONE form, so read
        // the effective X/Y as (axis pin) OR (Vec2 component) — whichever the
        // sink kept.
        let eff_x = if out.sink_outputs.contains_key(&("virtual.xinput:0".to_string(), "dpad_x".to_string())) {
            get_f("dpad_x") } else { dpad_vec().x };
        let eff_y = if out.sink_outputs.contains_key(&("virtual.xinput:0".to_string(), "dpad_y".to_string())) {
            get_f("dpad_y") } else { dpad_vec().y };

        // LEFT is fully suppressed across all representations.
        assert!(!get_b("dpad_left"), "dpad_left Bool must be suppressed");
        assert!(eff_x >= -1e-4, "dpad left (x-negative) must be clamped, got {eff_x}");

        // DOWN (not claimed) must SURVIVE.
        assert!(get_b("dpad_down"), "unmapped dpad_down Bool must pass through");
        assert!((eff_y - (-1.0)).abs() < 1e-4, "dpad_y (down) must be untouched, got {eff_y}");
    }

    // Vec2-authoritative: when the device provides a strong `left_stick` Vec2
    // but near-zero axis floats, a Collector forwards both, and the Remapper
    // must derive its axes (and cardinals) from the Vec2 — so an analog
    // stick→key mapping fires. Guards the "processed whole-stick Vec2 before a
    // Collector doesn't reach the Remapper" gap.
    #[test]
    fn processed_vec2_on_collector_drives_remapper_axes() {
        let dev = "gilrs:xinput:0";
        let src = source_node(1, dev, 0.0);

        // Collector: pure AutoMap bus pass-through from the device (no explicit
        // ports). Phase-1 forwards left_stick Vec2 AND the axis floats.
        let mut collect = empty_node(3, "module.automap_collect");
        collect.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        collect.input_sources = vec![Some((0, 0))];

        let remap_uid = 4usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_collector_id".into(), Value::String("collector:3".into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["left_stick_up"], "out": ["key_w"], "mode": "analog", "window_ms": 30.0 }
        ]));
        let sink = keymouse_sink_from_remap(5, remap_uid);
        let graph = ProcessingGraph { nodes: vec![src, collect, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Axes near-zero, but the left_stick VEC2 pushed up (y=0.9).
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "left_stick_x".to_string()), Signal::Float(0.0));
        sigs.insert((dev.to_string(), "left_stick_y".to_string()), Signal::Float(0.0));
        sigs.insert((dev.to_string(), "left_stick".to_string()), Signal::Vec2(Vec2::new(0.0, 0.9)));

        let mut fired = false;
        for _ in 0..200 {
            eval_graph_tick(&graph, &mut state, &sigs, 0.001, &mut out);
            if out.sink_outputs.get(&("virtual.keymouse:0".to_string(), "key_w".to_string()))
                .map(|s| s.as_bool()).unwrap_or(false) { fired = true; break; }
        }
        assert!(fired, "processed left_stick Vec2 must drive the axes the Remapper reads (key_w never fired)");
    }

    // A consumed input must stay suppressed for as long as it is HELD, even in
    // a press mode whose output gate is momentary (on-press fires a ~10ms pulse
    // then closes). Regression for "on-press mapping fires its output then leaks
    // the raw input while still held".
    #[test]
    fn consumed_input_suppressed_while_held_in_on_press_mode() {
        let dev = "gilrs:xinput:0";
        let src = source_node(1, dev, 0.0);
        let remap_uid = 2usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["dpad_left"], "out": ["btn_west"], "mode": "on_press" }
        ]));
        let sink = sink_node(3, "virtual.xinput:0", &format!("remap:{remap_uid}"), false);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        // Hold D-pad LEFT across many frames (well past the on-press pulse).
        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "dpad_left".to_string()), Signal::Bool(true));
        sigs.insert((dev.to_string(), "dpad_x".to_string()),    Signal::Float(-1.0));
        sigs.insert((dev.to_string(), "dpad".to_string()),      Signal::Vec2(Vec2::new(-1.0, 0.0)));

        let mut leaked_after_pulse = false;
        for frame in 0..60 {
            eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out);
            // After the first few frames (pulse over), dpad_left must NOT leak.
            if frame >= 10 {
                let dl = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "dpad_left".to_string()))
                    .map(|s| s.as_bool()).unwrap_or(false);
                let dx = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "dpad_x".to_string()))
                    .map(|s| s.as_float()).unwrap_or(0.0);
                let dvx = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "dpad".to_string()))
                    .and_then(|s| if let Signal::Vec2(v) = s { Some(v.x) } else { None }).unwrap_or(0.0);
                // effective left value via whichever representation the sink kept
                let eff = if dl { -1.0 } else { dx.min(dvx) };
                if eff < -1e-4 { leaked_after_pulse = true; }
            }
        }
        assert!(!leaked_after_pulse, "held dpad_left leaked through after the on-press pulse ended");
    }

    // The self-map exception: a mapping that routes an input back to ITSELF must
    // NOT suppress it (deliberate pass-through), even alongside another mapping.
    #[test]
    fn self_mapped_input_is_not_suppressed() {
        let dev = "gilrs:xinput:0";
        let src = source_node(1, dev, 0.0);
        let remap_uid = 2usize;
        let mut remap = empty_node(remap_uid, "module.remapper");
        remap.params.insert("_automap_device_id".into(), Value::String(dev.into()));
        remap.params.insert("mappings".into(), serde_json::json!([
            { "in": ["dpad_left"], "out": ["btn_west"],  "mode": "on_press" },
            { "in": ["dpad_left"], "out": ["dpad_left"],  "mode": "down" }
        ]));
        let sink = sink_node(3, "virtual.xinput:0", &format!("remap:{remap_uid}"), false);
        let graph = ProcessingGraph { nodes: vec![src, remap, sink] };
        let mut state = HashMap::new();
        let mut out = TickOutput::default();

        let mut sigs = HashMap::new();
        sigs.insert((dev.to_string(), "dpad_left".to_string()), Signal::Bool(true));
        for _ in 0..20 { eval_graph_tick(&graph, &mut state, &sigs, 0.016, &mut out); }

        let dl = out.sink_outputs.get(&("virtual.xinput:0".to_string(), "dpad_left".to_string()))
            .map(|s| s.as_bool()).unwrap_or(false);
        assert!(dl, "self-mapped dpad_left must pass through (not be suppressed)");
    }
}

#[cfg(test)]
mod menu_eval_tests {
    use super::*;

    fn menu_snap(uid: usize) -> NodeSnap {
        let mut n = NodeSnap {
            node_uid: uid,
            module_id: "module.menu".to_string(),
            params: HashMap::new(),
            n_outputs: 3,
            input_sources: Vec::new(),
            device_id: None,
            output_pin_ids: vec![
                "automap_pass".to_string(), "menu_open".to_string(), "menu_hover".to_string(),
            ],
            aux_f32_override: None,
            sink_target: None,
            inline_subgraph: None,
        };
        n.params.insert("_automap_device_id".into(), Value::String("dev".into()));
        n.params.insert("menu_id".into(), Value::String("abcd1234".into()));
        // 2x2 grid: zone ids row-major (0 TL, 1 TR, 2 BL, 3 BR).
        n.params.insert("col_edges".into(), serde_json::json!([0.5]));
        n.params.insert("row_edges".into(), serde_json::json!([0.5]));
        n.params.insert("zone_mode".into(), Value::String("mapping".into()));
        n
    }

    fn dev_stick(x: f32, y: f32) -> HashMap<(String, String), Signal> {
        let mut m = HashMap::new();
        m.insert(("dev".to_string(), "left_stick".to_string()), Signal::Vec2(Vec2::new(x, y)));
        m
    }

    fn show(b: bool) -> Vec<Option<Signal>> {
        vec![None, Some(Signal::Bool(b)), None, None]
    }

    // Hold activation + release-select: wired Show opens, stick bottom-right
    // highlights zone 3 (hover is sticky once the stick returns to center),
    // releasing Show closes AND selects — the zone's card fires for the pulse.
    #[test]
    fn hold_open_stick_hover_release_selects() {
        let mut snap = menu_snap(1);
        snap.params.insert("zone_maps".into(), serde_json::json!([
            { "f": 0, "z": 3, "in": ["menu_sel"], "out": ["btn_south"] }
        ]));
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        let mut state = HashMap::new();

        let out = eval_menu_node(&snap, 1, &show(false), &dev_stick(0.0, 0.0), &mut c, &mut state, 0.016);
        assert_eq!(out[1], Some(Signal::Bool(false)));
        assert_eq!(out[2], Some(Signal::Float(-1.0)));

        // Open + point bottom-right (stick +x, -y => unit (0.9, 0.9) => zone 3).
        let out = eval_menu_node(&snap, 1, &show(true), &dev_stick(0.8, -0.8), &mut c, &mut state, 0.016);
        assert_eq!(out[1], Some(Signal::Bool(true)));
        assert_eq!(out[2], Some(Signal::Float(3.0)));
        // Trailing mirror slots: no selection yet, live pointer present.
        assert_eq!(out[out.len() - 2], None);
        assert!(matches!(out[out.len() - 1], Some(Signal::Vec2(_))));
        // Not selected yet — the card hasn't fired.
        assert!(c.get(&("menumap:1".to_string(), "btn_south".to_string()))
            .map(|s| !s.as_bool()).unwrap_or(true));
        // Suppression: the pointing stick is zeroed on the passthrough while open.
        assert_eq!(c.get(&("menumap:1".to_string(), "left_stick".to_string())).copied(),
            Some(Signal::Vec2(Vec2::ZERO)));

        // Stick back to center — hover STAYS on 3 (sticky).
        let out = eval_menu_node(&snap, 1, &show(true), &dev_stick(0.0, 0.0), &mut c, &mut state, 0.016);
        assert_eq!(out[2], Some(Signal::Float(3.0)));

        // Release Show: closes, selects zone 3, card fires during the pulse.
        let out = eval_menu_node(&snap, 1, &show(false), &dev_stick(0.0, 0.0), &mut c, &mut state, 0.016);
        assert_eq!(out[1], Some(Signal::Bool(false)));
        assert_eq!(out[2], Some(Signal::Float(-1.0)));
        assert_eq!(c.get(&("menumap:1".to_string(), "btn_south".to_string())).copied(),
            Some(Signal::Bool(true)));
        // Selection mirror: zone 3, seq 1 — persists for the overlay's linger.
        assert_eq!(out[out.len() - 2], Some(Signal::Vec2(Vec2::new(3.0, 1.0))));
        assert_eq!(out[out.len() - 1], None, "pointer mirror clears when closed");
    }

    // The menu republishes the FULL upstream bus under `menumap:{uid}` (like
    // Touch Zones' touchmap:), not a sparse override map — so the AutoMap output
    // port glows and downstream reads a complete, coherently-suppressed bus.
    #[test]
    fn passthrough_republishes_full_bus() {
        // Closed menu = pure passthrough: the stick passes straight through.
        let snap = menu_snap(8);
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        let mut state = HashMap::new();
        eval_menu_node(&snap, 8, &show(false), &dev_stick(0.6, -0.3), &mut c, &mut state, 0.016);
        assert_eq!(
            c.get(&("menumap:8".to_string(), "left_stick".to_string())).copied(),
            Some(Signal::Vec2(Vec2::new(0.6, -0.3))),
            "closed menu passes the stick through under menumap:"
        );

        // Open with suppression OFF: still passes through (suppression is opt-in).
        let mut snap2 = menu_snap(9);
        snap2.params.insert("suppress_while_open".into(), Value::Bool(false));
        let mut c2: HashMap<(String, String), Signal> = HashMap::new();
        let mut st2 = HashMap::new();
        eval_menu_node(&snap2, 9, &show(true), &dev_stick(0.6, -0.3), &mut c2, &mut st2, 0.016);
        assert_eq!(
            c2.get(&("menumap:9".to_string(), "left_stick".to_string())).copied(),
            Some(Signal::Vec2(Vec2::new(0.6, -0.3))),
            "suppression off → stick passes through even while open"
        );
    }

    // Suppression zeros the enabled pointer pins on the menu's OWN passthrough
    // AND publishes a SOURCE-BLOCK request keyed by the physical source device
    // (`__src_block__:{dev}`), drained into `dev_sigs` next tick so the input
    // reaches ONLY the menu's navigation — not a mouse mapping, another module,
    // or the pad.
    #[test]
    fn suppress_publishes_source_block() {
        let snap = menu_snap(11); // default: suppress = true, left-stick pointer
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        let mut state = HashMap::new();
        // Open (wired Show) while the left stick is deflected (active).
        eval_menu_node(&snap, 11, &show(true), &dev_stick(0.8, -0.8), &mut c, &mut state, 0.016);

        let sk = format!("{SRC_BLOCK_PREFIX}dev");
        // Zeroed on the menu's own passthrough bus …
        assert_eq!(
            c.get(&("menumap:11".to_string(), "left_stick".to_string())).copied(),
            Some(Signal::Vec2(Vec2::ZERO)),
        );
        // … and the active driver's pins are flagged on the source-block channel.
        for pin in ["left_stick", "left_stick_x", "left_stick_y"] {
            assert_eq!(
                c.get(&(sk.clone(), pin.to_string())).map(|s| s.as_bool()),
                Some(true),
                "{pin} must be requested for source-block while open",
            );
        }
        // A source that ISN'T an enabled driver is not blocked.
        assert!(c.get(&(sk.clone(), "right_stick".to_string())).is_none());

        // Partial mode: an IDLE enabled driver (stick inside deadzone) is NOT
        // blocked, so it still reaches the game.
        let mut c_idle: HashMap<(String, String), Signal> = HashMap::new();
        let mut st_idle = HashMap::new();
        eval_menu_node(&snap, 11, &show(true), &dev_stick(0.05, 0.0), &mut c_idle, &mut st_idle, 0.016);
        assert!(c_idle.get(&(sk.clone(), "left_stick".to_string())).is_none(),
            "partial mode must not block an idle driver");

        // Full mode: the enabled driver is blocked even when idle.
        let mut snap_full = menu_snap(14);
        snap_full.params.insert("suppress_mode".into(), Value::String("full".into()));
        let mut c_full: HashMap<(String, String), Signal> = HashMap::new();
        let mut st_full = HashMap::new();
        eval_menu_node(&snap_full, 14, &show(true), &dev_stick(0.0, 0.0), &mut c_full, &mut st_full, 0.016);
        assert_eq!(
            c_full.get(&(format!("{SRC_BLOCK_PREFIX}dev"), "left_stick".to_string())).map(|s| s.as_bool()),
            Some(true), "full mode blocks the enabled driver even when idle",
        );

        // Suppression OFF → no block published.
        let mut snap2 = menu_snap(12);
        snap2.params.insert("suppress_while_open".into(), Value::Bool(false));
        let mut c2: HashMap<(String, String), Signal> = HashMap::new();
        let mut st2 = HashMap::new();
        eval_menu_node(&snap2, 12, &show(true), &dev_stick(0.8, -0.8), &mut c2, &mut st2, 0.016);
        assert!(c2.get(&(sk, "left_stick".to_string())).is_none());
    }

    // Partial suppression must LATCH gyro as the active driver off the menu
    // CURSOR being out of the deadzone: the rotation rate drops to ~0 whenever
    // the user holds the cursor on a target, and a rate-only flag would unblock
    // the source for those ticks — leaking gyro to e.g. a mouse mapping one
    // tick at a time while the menu is being steered.
    #[test]
    fn gyro_partial_suppress_latches_while_cursor_deflected() {
        let mut snap = menu_snap(15);
        snap.params.insert("ptr_ls".into(), Value::Bool(false));
        snap.params.insert("ptr_gyro".into(), Value::Bool(true));
        let dev_gyro = |rate: f32| -> HashMap<(String, String), Signal> {
            let mut m = HashMap::new();
            for pin in ["gyro_x", "gyro_y", "gyro_z"] {
                m.insert(("dev".to_string(), pin.to_string()), Signal::Float(rate));
            }
            m
        };
        let gk = (format!("{SRC_BLOCK_PREFIX}dev"), "gyro_x".to_string());
        let mut state = HashMap::new();

        // Tick 1 opens (prev_open false → no integration yet); tick 2 rotates,
        // driving the cursor past the deadzone. Fresh collector map per tick,
        // like the real pipeline — a stale block entry can't fake a pass.
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        eval_menu_node(&snap, 15, &show(true), &dev_gyro(1.0), &mut c, &mut state, 0.016);
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        eval_menu_node(&snap, 15, &show(true), &dev_gyro(1.0), &mut c, &mut state, 0.016);
        assert_eq!(c.get(&gk).map(|s| s.as_bool()), Some(true),
            "rotating gyro must be source-blocked in partial mode");

        // Rotation stops (holding on a target): the cursor is still deflected,
        // so the block must hold.
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        eval_menu_node(&snap, 15, &show(true), &dev_gyro(0.0), &mut c, &mut state, 0.016);
        assert_eq!(c.get(&gk).map(|s| s.as_bool()), Some(true),
            "block must latch while the gyro cursor is out of the deadzone");

        // A menu opened without ever tilting keeps gyro passing in partial mode.
        let mut st2 = HashMap::new();
        let mut c2: HashMap<(String, String), Signal> = HashMap::new();
        for _ in 0..3 {
            c2 = HashMap::new();
            eval_menu_node(&snap, 15, &show(true), &dev_gyro(0.0), &mut c2, &mut st2, 0.016);
        }
        assert!(c2.get(&gk).is_none(),
            "an untouched gyro driver must not be blocked in partial mode");
    }

    // Latch mode: the first driver to engage owns the menu exclusively — it is
    // blocked at the source while every OTHER enabled driver keeps passing to
    // the game — until it disengages, at which point the next engaged driver
    // takes over.
    #[test]
    fn latch_suppress_first_engaged_driver_owns_menu() {
        let mut snap = menu_snap(16);
        snap.params.insert("ptr_ls".into(), Value::Bool(true));
        snap.params.insert("ptr_gyro".into(), Value::Bool(true));
        snap.params.insert("suppress_mode".into(), Value::String("latch".into()));
        let dev = |x: f32, y: f32, rate: f32| -> HashMap<(String, String), Signal> {
            let mut m = HashMap::new();
            m.insert(("dev".to_string(), "left_stick".to_string()), Signal::Vec2(Vec2::new(x, y)));
            for pin in ["gyro_x", "gyro_y", "gyro_z"] {
                m.insert(("dev".to_string(), pin.to_string()), Signal::Float(rate));
            }
            m
        };
        let sk = format!("{SRC_BLOCK_PREFIX}dev");
        let lk = (sk.clone(), "left_stick".to_string());
        let gk = (sk.clone(), "gyro_x".to_string());
        let mut state = HashMap::new();

        // Tick 1 opens (no engagement while prev_open is false); tick 2: LS
        // deflects AND gyro rotates at once — LS engages first and takes the
        // latch, so LS is blocked while gyro passes untouched.
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        eval_menu_node(&snap, 16, &show(true), &dev(0.8, -0.8, 1.0), &mut c, &mut state, 0.016);
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        eval_menu_node(&snap, 16, &show(true), &dev(0.8, -0.8, 1.0), &mut c, &mut state, 0.016);
        assert_eq!(c.get(&lk).map(|s| s.as_bool()), Some(true),
            "the latched driver must be source-blocked");
        assert!(c.get(&gk).is_none(),
            "a non-latched driver must keep passing while another owns the menu");

        // LS returns to the deadzone while gyro keeps rotating: ownership hands
        // over — gyro is now blocked, LS passes again.
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        eval_menu_node(&snap, 16, &show(true), &dev(0.0, 0.0, 1.0), &mut c, &mut state, 0.016);
        assert!(c.get(&lk).is_none(),
            "a disengaged driver must be released from the block");
        assert_eq!(c.get(&gk).map(|s| s.as_bool()), Some(true),
            "the next engaged driver must take over the latch");
    }

    // A selected zone card must fire a momentary PULSE and then RELEASE, even
    // when its output pin isn't on the passthrough bus (the source device never
    // emits it). Without an explicit off-write the pin would latch "pressed" on
    // the virtual sink forever — the stuck-selection regression.
    #[test]
    fn selected_card_pulse_releases_not_latches() {
        let mut snap = menu_snap(13);
        snap.params.insert("select_on".into(), Value::String("press".into()));
        snap.params.insert("zone_maps".into(), serde_json::json!([
            { "f": 0, "z": 3, "in": ["menu_sel"], "out": ["btn_north"] }
        ]));
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        let mut state = HashMap::new();
        // Wired Show + Select: [_, Show, Select, _].
        let sel = |sh: bool, se: bool|
            vec![None, Some(Signal::Bool(sh)), Some(Signal::Bool(se)), None];

        // Open + hover zone 3, then press Select (rising edge selects).
        eval_menu_node(&snap, 13, &sel(true, false), &dev_stick(0.8, -0.8), &mut c, &mut state, 0.016);
        eval_menu_node(&snap, 13, &sel(true, true), &dev_stick(0.8, -0.8), &mut c, &mut state, 0.016);
        assert_eq!(
            c.get(&("menumap:13".to_string(), "btn_north".to_string())).map(|s| s.as_bool()),
            Some(true), "the select pulse asserts the zone card's output",
        );

        // Hold past the pulse (120 ms / 16 ms ≈ 8 ticks) with Select released.
        for _ in 0..12 {
            eval_menu_node(&snap, 13, &sel(true, false), &dev_stick(0.0, 0.0), &mut c, &mut state, 0.016);
        }
        assert_eq!(
            c.get(&("menumap:13".to_string(), "btn_north".to_string())).map(|s| s.as_bool()),
            Some(false), "after the pulse the card output must RELEASE (explicit off, no latch)",
        );
    }

    // The macro-style Show target (published by a Remapper mapping via
    // merge_macro_scalar) opens the menu with nothing wired.
    #[test]
    fn macro_style_show_target_opens() {
        let snap = menu_snap(2);
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        merge_macro_scalar(&mut c, "menu:abcd1234_show", Signal::Bool(true));
        let mut state = HashMap::new();
        let out = eval_menu_node(&snap, 2, &[], &dev_stick(0.0, 0.0), &mut c, &mut state, 0.016);
        assert_eq!(out[1], Some(Signal::Bool(true)));
    }

    // Toggle activation: rising Show opens, holding/releasing changes nothing,
    // the next rising edge closes.
    #[test]
    fn toggle_mode_edges() {
        let mut snap = menu_snap(3);
        snap.params.insert("activation_mode".into(), Value::String("toggle".into()));
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        let mut state = HashMap::new();
        let dev = dev_stick(0.0, 0.0);

        let out = eval_menu_node(&snap, 3, &show(true), &dev, &mut c, &mut state, 0.016);
        assert_eq!(out[1], Some(Signal::Bool(true)), "rising edge opens");
        let out = eval_menu_node(&snap, 3, &show(true), &dev, &mut c, &mut state, 0.016);
        assert_eq!(out[1], Some(Signal::Bool(true)), "held: stays open");
        let out = eval_menu_node(&snap, 3, &show(false), &dev, &mut c, &mut state, 0.016);
        assert_eq!(out[1], Some(Signal::Bool(true)), "released: stays open");
        let out = eval_menu_node(&snap, 3, &show(true), &dev, &mut c, &mut state, 0.016);
        assert_eq!(out[1], Some(Signal::Bool(false)), "second rising edge closes");
    }

    // select_on = "press": the wired Select input commits the hovered zone
    // while the menu stays open.
    #[test]
    fn press_select_fires_card_while_open() {
        let mut snap = menu_snap(4);
        snap.params.insert("select_on".into(), Value::String("press".into()));
        snap.params.insert("zone_maps".into(), serde_json::json!([
            { "f": 0, "z": 0, "in": ["menu_sel"], "out": ["btn_west"] }
        ]));
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        let mut state = HashMap::new();
        let sel = |show_b: bool, sel_b: bool| {
            vec![None, Some(Signal::Bool(show_b)), Some(Signal::Bool(sel_b)), None]
        };
        // Open + hover top-left (stick -x, +y => unit (0.1, 0.1) => zone 0).
        let out = eval_menu_node(&snap, 4, &sel(true, false), &dev_stick(-0.8, 0.8), &mut c, &mut state, 0.016);
        assert_eq!(out[2], Some(Signal::Float(0.0)));
        // Select press: card fires; menu stays open.
        let out = eval_menu_node(&snap, 4, &sel(true, true), &dev_stick(-0.8, 0.8), &mut c, &mut state, 0.016);
        assert_eq!(out[1], Some(Signal::Bool(true)));
        assert_eq!(c.get(&("menumap:4".to_string(), "btn_west".to_string())).copied(),
            Some(Signal::Bool(true)));
    }

    // Radial mode: the pointer picks a sector by ANGLE (sector 0 up,
    // clockwise), the dead center hovers nothing, and hover stays sticky
    // when the stick returns to rest.
    #[test]
    fn radial_mode_sector_by_angle() {
        let mut snap = menu_snap(6);
        snap.params.insert("menu_radial".into(), Value::Bool(true));
        // Synthetic 1×4 strip = 4 sectors (up, right, down, left).
        snap.params.insert("col_edges".into(), serde_json::json!([0.25, 0.5, 0.75]));
        snap.params.insert("row_edges".into(), serde_json::json!([] as [f32; 0]));
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        let mut state = HashMap::new();

        // Stick up (+y = up in stick coords) → sector 0.
        let out = eval_menu_node(&snap, 6, &show(true), &dev_stick(0.0, 0.9), &mut c, &mut state, 0.016);
        assert_eq!(out[1], Some(Signal::Bool(true)));
        assert_eq!(out[2], Some(Signal::Float(0.0)));
        // Stick right → sector 1; down → 2; left → 3.
        let out = eval_menu_node(&snap, 6, &show(true), &dev_stick(0.9, 0.0), &mut c, &mut state, 0.016);
        assert_eq!(out[2], Some(Signal::Float(1.0)));
        let out = eval_menu_node(&snap, 6, &show(true), &dev_stick(0.0, -0.9), &mut c, &mut state, 0.016);
        assert_eq!(out[2], Some(Signal::Float(2.0)));
        let out = eval_menu_node(&snap, 6, &show(true), &dev_stick(-0.9, 0.0), &mut c, &mut state, 0.016);
        assert_eq!(out[2], Some(Signal::Float(3.0)));
        // Back to rest: hover stays sticky on the last sector.
        let out = eval_menu_node(&snap, 6, &show(true), &dev_stick(0.0, 0.0), &mut c, &mut state, 0.016);
        assert_eq!(out[2], Some(Signal::Float(3.0)));
    }

    // hover_sticky = false: returning to the deadzone clears the highlight,
    // and a release there selects nothing (no card fires).
    #[test]
    fn non_sticky_hover_clears_in_deadzone() {
        let mut snap = menu_snap(7);
        snap.params.insert("hover_sticky".into(), Value::Bool(false));
        snap.params.insert("zone_maps".into(), serde_json::json!([
            { "f": 0, "z": 3, "in": ["menu_sel"], "out": ["btn_south"] }
        ]));
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        let mut state = HashMap::new();

        let out = eval_menu_node(&snap, 7, &show(true), &dev_stick(0.8, -0.8), &mut c, &mut state, 0.016);
        assert_eq!(out[2], Some(Signal::Float(3.0)));
        // Back inside the deadzone: highlight clears instead of sticking.
        let out = eval_menu_node(&snap, 7, &show(true), &dev_stick(0.0, 0.0), &mut c, &mut state, 0.016);
        assert_eq!(out[2], Some(Signal::Float(-1.0)));
        // Release with nothing highlighted: closes without selecting.
        let out = eval_menu_node(&snap, 7, &show(false), &dev_stick(0.0, 0.0), &mut c, &mut state, 0.016);
        assert_eq!(out[1], Some(Signal::Bool(false)));
        assert_eq!(out[out.len() - 2], None, "no selection mirrored");
        assert!(c.get(&("menumap:7".to_string(), "btn_south".to_string()))
            .map(|s| !s.as_bool()).unwrap_or(true));
    }

    // A mapping card targeting ANOTHER macro-style pin routes into the macro
    // namespace instead of the bus (a menu selection can raise a Macro port /
    // another menu's Show).
    #[test]
    fn menu_card_can_target_macro_pin() {
        let mut snap = menu_snap(5);
        snap.params.insert("select_on".into(), Value::String("press".into()));
        snap.params.insert("zone_maps".into(), serde_json::json!([
            { "f": 0, "z": 0, "in": ["menu_sel"], "out": ["macro:ff00ff00"] }
        ]));
        let mut c: HashMap<(String, String), Signal> = HashMap::new();
        let mut state = HashMap::new();
        // Two frames: hover establishes on frame 1, the Select edge rises on
        // frame 2 (prev_sel must be low for the edge to register).
        let f1 = vec![None, Some(Signal::Bool(true)), Some(Signal::Bool(false)), None];
        let f2 = vec![None, Some(Signal::Bool(true)), Some(Signal::Bool(true)), None];
        eval_menu_node(&snap, 5, &f1, &dev_stick(-0.8, 0.8), &mut c, &mut state, 0.016);
        eval_menu_node(&snap, 5, &f2, &dev_stick(-0.8, 0.8), &mut c, &mut state, 0.016);
        assert_eq!(c.get(&("macro".to_string(), "macro:ff00ff00".to_string())).copied(),
            Some(Signal::Bool(true)));
        assert!(!c.contains_key(&("menumap:5".to_string(), "macro:ff00ff00".to_string())),
            "macro-style targets must not leak onto the menu's bus key");
    }
}

#[cfg(test)]
mod source_namespace_tests {
    use super::*;

    #[test]
    fn every_prefix_is_recognised() {
        for p in NAMESPACED_SOURCE_PREFIXES {
            assert!(
                is_namespaced_source(&format!("{p}42")),
                "{p} is listed but not recognised"
            );
        }
    }

    #[test]
    fn physical_devices_are_not_namespaced() {
        // Real ids the graph carries alongside node-produced ones. If a device
        // backend ever adopts one of these prefixes, the engine would start
        // routing it through collector_sigs instead of the device bus.
        for id in [
            "gilrs:dualsense:0",
            "gilrs:xinput:v0",
            "midi_in:0",
            "virtual.hm.xinput",
            "virtual.keymouse",
        ] {
            assert!(!is_namespaced_source(id), "{id} must read as a physical source");
        }
    }
}

#[cfg(test)]
mod lean_axis_tests {
    use super::*;

    /// Output slot 3 of `compute_gyro_3dof` is the lean scalar.
    const LEAN_OUT: usize = 3;

    /// Drive the module through its direct accel pin overrides (inputs 5/6/7 =
    /// Accel X/Y/Z) and read the lean scalar back.
    fn lean_for(accel: [f32; 3]) -> f32 {
        let inputs: Vec<Option<Signal>> = vec![
            None, None,                       // 0,1: unused here
            Some(Signal::Float(0.0)),         // 2: gyro X
            Some(Signal::Float(0.0)),         // 3: gyro Y
            Some(Signal::Float(0.0)),         // 4: gyro Z
            Some(Signal::Float(accel[0])),    // 5: accel X
            Some(Signal::Float(accel[1])),    // 6: accel Y
            Some(Signal::Float(accel[2])),    // 7: accel Z
        ];
        let mut state = NodeState::default();
        let out = compute_gyro_3dof(
            &inputs,
            &mut state,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            1.0 / 60.0,
        );
        match out.get(LEAN_OUT) {
            Some(Some(Signal::Float(f))) => *f,
            other => panic!("lean output slot held {other:?}"),
        }
    }

    /// Run the module until its smoothed gravity settles, then report lean.
    /// `axis` picks the mode; `accel` is held steady throughout.
    fn settled_lean(axis: &str, accel: [f32; 3]) -> f32 {
        let inputs: Vec<Option<Signal>> = vec![
            None, None,
            Some(Signal::Float(0.0)),
            Some(Signal::Float(0.0)),
            Some(Signal::Float(0.0)),
            Some(Signal::Float(accel[0])),
            Some(Signal::Float(accel[1])),
            Some(Signal::Float(accel[2])),
        ];
        let mut params = HashMap::new();
        params.insert("family".to_string(), Value::String("pointer".into()));
        params.insert("axis".to_string(), Value::String(axis.into()));
        let mut state = NodeState::default();
        let mut lean = 0.0;
        // 10 s at 60 Hz — past the longest smoothing tau (World, 3 s).
        for _ in 0..600 {
            let out = compute_gyro_3dof(
                &inputs, &mut state, &params,
                &HashMap::new(), &HashMap::new(), 1.0 / 60.0,
            );
            lean = match out.get(LEAN_OUT) {
                Some(Some(Signal::Float(f))) => *f,
                other => panic!("lean output slot held {other:?}"),
            };
        }
        lean
    }

    // Gravity in the pad's frame for the two ways it gets held, tilted
    // sideways by ~45°. Axes are (side, forward, vertical).
    const FLAT_TILTED: [f32; 3] = [0.707, 0.0, 0.707];      // held flat, rolled
    const UPRIGHT_TILTED: [f32; 3] = [0.707, 0.707, 0.0];   // nose-up, grips down

    /// Pitch+Yaw assumes the pad points forward, so it leans on roll — and
    /// should go quiet if the pad is actually being held nose-up, where the
    /// same side tilt means something else to the player.
    #[test]
    fn pitch_yaw_leans_when_held_flat_only() {
        assert!(
            settled_lean("pitch_yaw", FLAT_TILTED).abs() > 0.5,
            "held flat and rolled: this is Pitch+Yaw's lean",
        );
        assert!(
            settled_lean("pitch_yaw", UPRIGHT_TILTED).abs() < 0.1,
            "held nose-up: not the orientation this mode assumes",
        );
    }

    /// Pitch+Roll assumes a handheld / ceiling-pointing pad, where the lean the
    /// player performs reads to them as a yaw. Same accel axis, opposite gate.
    #[test]
    fn pitch_roll_leans_when_held_nose_up_only() {
        assert!(
            settled_lean("pitch_roll", UPRIGHT_TILTED).abs() > 0.5,
            "held nose-up and tilted: this is Pitch+Roll's lean",
        );
        assert!(
            settled_lean("pitch_roll", FLAT_TILTED).abs() < 0.1,
            "held flat: not the orientation this mode assumes",
        );
    }

    /// Player and World adapt to however the pad is held, so they lean in both.
    #[test]
    fn adaptive_modes_lean_however_the_pad_is_held() {
        for axis in ["player", "world"] {
            assert!(
                settled_lean(axis, FLAT_TILTED).abs() > 0.5,
                "{axis} must lean when held flat",
            );
            assert!(
                settled_lean(axis, UPRIGHT_TILTED).abs() > 0.5,
                "{axis} must lean when held nose-up",
            );
        }
    }

    /// Whatever the mode, forward tilt is pitch and must never read as lean.
    #[test]
    fn no_mode_leans_on_forward_tilt() {
        for axis in ["pitch_yaw", "pitch_roll", "player", "world"] {
            assert!(
                settled_lean(axis, [0.0, 0.707, 0.707]).abs() < 0.1,
                "{axis} leaned on a forward tilt",
            );
        }
    }

    /// The device layer normalizes every pad to (x = side, y = forward-tilt,
    /// z = vertical) — see `flexinput_devices::gyro::build`. Lean means SIDE
    /// tilt, so it must follow X and ignore Y. It read Y until 2026-07, which
    /// made leaning fire on pitch.
    #[test]
    fn lean_follows_side_tilt_not_forward_tilt() {
        // Flat: gravity straight down the vertical axis, no lean.
        assert!(lean_for([0.0, 0.0, 1.0]).abs() < 1e-3, "flat must not lean");

        // Tipped forward/back — pitch. Lean must stay silent.
        assert!(
            lean_for([0.0, 0.7, 0.7]).abs() < 1e-3,
            "forward tilt is pitch, not lean",
        );
        assert!(
            lean_for([0.0, -0.7, 0.7]).abs() < 1e-3,
            "backward tilt is pitch, not lean",
        );

        // Rolled onto its side — this is what lean means.
        let right = lean_for([0.7, 0.0, 0.7]);
        let left = lean_for([-0.7, 0.0, 0.7]);
        assert!(right.abs() > 0.5, "side tilt must lean, got {right}");
        assert!(left.abs() > 0.5, "side tilt must lean, got {left}");
        assert!(
            right.signum() != left.signum(),
            "opposite side tilts must lean opposite ways ({right} vs {left})",
        );

        // Fully on its side reads as full deflection.
        assert!((lean_for([1.0, 0.0, 0.0]).abs() - 1.0).abs() < 1e-3);
    }

    /// Which way is positive. Verified against a physical pad — deriving it
    /// from the documented device frame gives the opposite answer, so this is
    /// pinned here rather than left to a comment that can drift.
    #[test]
    fn positive_lean_is_a_right_lean() {
        // +accel X is the pad leaning LEFT, so lean reads negative.
        assert!(lean_for([0.707, 0.0, 0.707]) < 0.0, "+X side tilt is a left lean");
        assert!(lean_for([-0.707, 0.0, 0.707]) > 0.0, "-X side tilt is a right lean");
    }
}
