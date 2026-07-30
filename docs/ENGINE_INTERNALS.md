# FlexInput Engine Internals & Graph Evaluation

## Overview

The engine crate (`crates/engine`) implements the real-time signal processing pipeline that evaluates the node graph at configurable rates (500 Hz - 2 kHz). It transforms raw device signals through user-defined processing modules and routes results to virtual output devices.

**Key Characteristics:**
- Three-phase evaluation: preprocess → main loop → post-passes
- Topologically-sorted graph execution
- Namespaced state for sub-patch isolation
- ArcSwap-based lock-free graph publishing from UI thread
- Catchup tick mechanism for frame rate compensation

---

## Thread Architecture

### Processing Thread (`spawn_processing_thread`)

**Responsibilities:**
1. Read `ArcGraph` (ProcessingGraph) published by UI thread
2. Evaluate graph at `sample_rate_hz` (configurable 500-2000 Hz)
3. Write computed signals to `SinkBus` for I/O thread consumption
4. Publish scope samples and display state back to UI

**Thread Loop:**
Real signature (`crates/engine/src/thread.rs`) — returns the `JoinHandle`:

```rust
pub fn spawn_processing_thread(
    graph: ArcGraph,                            // UI→Processing graph (ArcSwap)
    device_signals: ArcSignals,                 // I/O→Processing signals (ArcSwap)
    output: Arc<Mutex<ProcessingOutput>>,       // Processing→UI display state
    sink_bus: SinkBus,                          // Processing→I/O signals (Arc<RwLock<…>>)
    sample_rate: Arc<AtomicU32>,
    ui_source_block: UiSourceBlock,             // Arc<RwLock<HashSet<(String,String)>>>
) -> std::thread::JoinHandle<()>;
```

Simplified loop (pacing details elided):

```rust
let mut state = HashMap::<usize, NodeState>::new();
let mut tick_out = TickOutput::default();   // reused; eval_graph_tick clears it internally
loop {
    let graph_snap = graph.load();                  // lock-free ArcSwap load
    let dev_sigs   = device_signals.load_full();
    // Merge the UI's source-block set into engine state BEFORE the tick.
    // NOTE: eval_graph_tick does NOT take ui_source_block — the block is applied
    // via state[MACRO_CARRY_UID].source_block, which the tick reads and the
    // tick-end pass repopulates from collector_sigs.
    {
        let carry = state.entry(eval::MACRO_CARRY_UID).or_default();
        carry.source_block.clear();
        for k in ui_source_block.read().unwrap().iter() {
            carry.source_block.insert(k.clone());
        }
    }
    eval_graph_tick(&graph_snap, &mut state, &dev_sigs, dt, &mut tick_out);   // 5 args
    // Hand sink signals to the I/O thread (direct assignment under the RwLock):
    *sink_bus.write().unwrap() = tick_out.sink_outputs.clone();
    // Publish display state to the UI (non-blocking):
    if let Ok(mut o) = output.try_lock() { /* copy scope_samples/last_inputs/… */ }
    // …pace to `sample_rate`…
}
```

### UI Thread (Graph Publishing)

**Responsibilities:**
1. Rebuild `ProcessingGraph` from Snarl on every frame
2. Publish via `ArcSwap::store()` (lock-free for processing thread)
3. Read display state from `proc_outputs` mutex (try_lock to avoid blocking)

```rust
// In FlexInputApp::update()
let (graph_snap, dirty_uids) = build_processing_graph(&snarl, defaults);
self.proc_graph.store(Arc::new(graph_snap));  // ArcSwap publish
```

**Why ArcSwap instead of RwLock?**
- Processing thread reads every tick (500-2000 Hz)
- UI thread writes every frame (~60-144 Hz)
- ArcSwap provides lock-free reads with refcount bump only
- RwLock would cause contention at high processing rates

### I/O Thread (Device Polling)

**Responsibilities:**
1. Poll physical devices at `polling_hz` (500-4000 Hz)
2. Write signals to `proc_device_signals` ArcSwap
3. Read computed signals from `sink_bus` RwLock
4. Send to virtual devices via `VirtualDevice::send()`

---

## Graph Evaluation Pipeline (`eval_graph_tick`)

### Phase 1: Preprocessing

```rust
pub fn eval_graph_tick(
    graph: &ProcessingGraph,
    state: &mut HashMap<usize, NodeState>,
    dev_sigs: &HashMap<(String, String), Signal>,
    dt: f32,
    out: &mut TickOutput,        // NOTE: no ui_source_block param — see below
) {
    out.clear();                 // eval clears its own output (reused buffer)

    // 1. Apply per-device source-side post-processing
    let mut dev_sigs_owned = preprocess_dev_sigs(graph, dev_sigs);
    
    // 2. Zero the blocked physical pins (menu-open / config-overlay suppression).
    //    The block set is read from state[MACRO_CARRY_UID].source_block, which the
    //    processing thread populated from the UI's ui_source_block BEFORE this call
    //    — it is NOT a parameter of eval_graph_tick. Blocked pins are replaced with
    //    a neutral value (pointer_block_off) and their pre-block values snapshotted.
    
    // 3. Destructure TickOutput for mutable access
    let TickOutput {
        ref mut outputs,
        ref mut scope_samples,
        ref mut last_inputs,
        ref mut last_outputs,
        ref mut sink_outputs,
    } = *out;
    
    // 4. Initialize collector_sigs and fb_routes maps
    let mut collector_sigs: HashMap<(String, String), Signal> = HashMap::new();
    let mut fb_routes: HashMap<String, String> = HashMap::new();
```

**`preprocess_dev_sigs()`:**
- Applies deadzone to analog sticks based on device calibration
- Scales gyro signals by multiplier from settings
- Filters noise spikes if spike filter enabled

**Source-block suppression (menu / config-overlay):**
- When a Virtual Menu is open OR the config overlay is tweaking a param, specific
  physical input pins are zeroed so the game doesn't receive nav inputs.
- The block set lives in `state[MACRO_CARRY_UID].source_block`. Two feeders populate it:
  the processing thread unions the UI's `ui_source_block` in before each tick, and the
  tick-end pass repopulates it from `collector_sigs` entries under `SRC_BLOCK_PREFIX`
  (nodes like the menu publish their own blocks).
- Blocked pins are replaced with a neutral value (`pointer_block_off`); the pre-block
  values are snapshotted so nav can still read raw input.

### Phase 2: Main Node Loop

```rust
    // Iterate through all nodes in topological order
    for (idx, snap) in graph.nodes.iter().enumerate() {
        // ── Special node types handled before compute_node ───────────────
        
        // AutoMap Collector: inject signals into collector_sigs map
        if snap.module_id == "module.automap_collect" {
            automap_collect_publish(snap, idx, &computed, dev_sigs, &mut collector_sigs);
            computed[idx] = vec![None];
            continue;
        }
        
        // AutoMap Remapper: publish per-mapping overrides to remap_sigs
        if snap.module_id == "module.remapper" {
            remapper_publish(snap, idx, dev_sigs, &mut collector_sigs, state, dt);
            computed[idx] = vec![None];
            continue;
        }
        
        // Touch Zones (mapping mode): publish zone behaviors to touchmap
        if snap.module_id == "module.touch_zones" 
            && snap.params.get("zone_mode") == Some(&Value::String("mapping".into())) 
        {
            touch_zones_map_publish(snap, idx, dev_sigs, &mut collector_sigs, state, dt);
            computed[idx] = vec![None];
            continue;
        }
        
        // Virtual Menu: state machine + suppression publishing
        if snap.module_id == "module.menu" {
            let inputs = resolve_inputs(&snap.input_sources, &computed);
            computed[idx] = eval_menu_node(snap, idx, &inputs, dev_sigs, &mut collector_sigs, state, dt);
            last_outputs.insert(idx, computed[idx].clone());
            continue;
        }
        
        // AutoMap Fork/Selector/Combiner: bus duplication/routing
        if snap.module_id == "module.automap_fork" {
            automap_fork_publish(snap, idx, &computed, dev_sigs, &mut collector_sigs);
            computed[idx] = vec![None; snap.n_outputs];
            continue;
        }
        
        // Feedback Control: inject inlets into physical pad's feedback channel
        if snap.module_id == "module.feedback_control" {
            let out = feedback_control_publish(snap, &computed, dev_sigs, &mut collector_sigs);
            last_outputs.insert(idx, out.clone());
            computed[idx] = out;
            continue;
        }
        
        // Network Send/Receive: publish hooks for Phase C modules
        if let Some(publish) = eval_hooks(&snap.module_id).and_then(|h| h.publish) {
            let out = publish(snap, idx, dev_sigs, &mut collector_sigs);
            last_outputs.insert(idx, out.clone());
            computed[idx] = out;
            continue;
        }
        
        // ── Inline sub-patch: recursive evaluation ──────────────────────
        if let Some(ref sg) = snap.inline_subgraph {
            let outer_inputs = resolve_inputs(&snap.input_sources, &computed);
            let inner_computed = eval_subgraph(
                &sg.graph, &outer_inputs, state, dev_sigs, 
                &mut collector_sigs, scope_samples, last_inputs, 
                last_outputs, &mut fb_routes, snap.node_uid, dt,
            );
            computed[idx] = map_outlets(&sg.outlet_locs, &inner_computed);
            last_outputs.insert(snap.node_uid, computed[idx].clone());
            continue;
        }
        
        // ── device.sink: collect combined inputs for virtual devices ────
        if let Some(ref st) = snap.sink_target {
            collect_sink_inputs(st, &computed, dev_sigs, &mut collector_sigs, 
                              &mut sink_outputs, idx);
            continue;
        }
        
        // ── Standard module evaluation via compute_node ─────────────────
        let inputs = resolve_inputs(&snap.input_sources, &computed);
        let node_state = state.entry(snap.node_uid).or_insert_with(NodeState::default);
        
        if let Some(ref vals) = snap.aux_f32_override {
            node_state.aux_f32 = vals.clone();
        }
        
        let node_outputs = compute_node(snap, &inputs, node_state, dev_sigs, 
                                       &collector_sigs, dt);
        
        // ── 3DOF lean dispatch (special case for gyro module) ───────────
        if snap.module_id == "processing.gyro_3dof" {
            lean_dispatch_into_collector_sigs(snap, idx, &node_outputs, 
                                            node_state, &mut collector_sigs, dt);
        }
        
        // ── Display state mirroring for UI rendering ────────────────────
        mirror_display_state(snap.module_id.as_str(), snap.node_uid, &inputs, 
                           &node_outputs, scope_samples, last_inputs, last_outputs);
        
        computed[idx] = node_outputs;
    }
```

**`resolve_inputs()`:**
- Maps `input_sources: Vec<Option<(usize, usize)>>` to actual signals
- Returns `Vec<Option<Signal>>` for module evaluation
- Handles None (unwired inputs) gracefully

**`mirror_display_state()`:**
- Populates `scope_samples`, `last_inputs`, `last_outputs` maps
- Used by UI to render oscilloscopes, readouts, signal glow effects
- Keyed by node UID for lookup during display state application

### Phase 3: Post-Passes

```rust
    // ── Post-pass 1: Self-sink feedback routing ───────────────────────
    // device.source nodes whose feedback inputs loop back to their own outputs
    for (idx, snap) in graph.nodes.iter().enumerate() {
        let Some(ref st) = snap.sink_target else { continue; };
        if !st.is_self_sink { continue; }
        
        for (in_idx, pin_id) in st.pin_ids.iter().enumerate() {
            if pin_id.is_empty() { continue; }
            let mut combined: Option<Signal> = None;
            if let Some(sources) = st.multi_sources.get(in_idx) {
                for &(src_idx, out_pin) in sources {
                    // Read from computed[] (main loop already filled this)
                    if let Some(sig) = computed.get(src_idx).and_then(|v| v.get(out_pin)) {
                        combined = Some(combine_signals(combined.unwrap_or(None), *sig));
                    }
                }
            }
            if let Some(sig) = combined {
                sink_outputs.insert((st.device_id.clone(), pin_id.clone()), sig);
            }
        }
    }
    
    // ── Post-pass 2: Reverse-feedback routing through AutoMap Selectors ─
    // ASTH/Feedback Control nodes placed AFTER a Selector need feedback to flow
    // backward along the gate chain to reach the physical pad or network recv
    if !fb_routes.is_empty() {
        for from_id in fb_routes.keys().cloned().collect::<Vec<_>>() {
            let terminal = resolve_feedback_terminal(&from_id, &fb_routes);
            copy_feedback_injections(from_id, terminal, &mut collector_sigs);
        }
    }
    
    // ── Post-pass 3: Feedback Control injection drain ─────────────────
    // Drain all feedback_inject:{device_id} entries into sink_outputs
    let has_injection = collector_sigs.keys().any(|(dev, _)| 
        dev.starts_with("feedback_inject:"));
    
    if has_injection {
        for snap in graph.nodes.iter() {
            let Some(ref st) = snap.sink_target else { continue; };
            if st.device_id.starts_with("virtual.") { continue; }  // Skip virtual sinks
            
            let inject_key = format!("feedback_inject:{}", st.device_id);
            for pin in FEEDBACK_INLET_PINS {
                if let Some(&sig) = collector_sigs.get(&(inject_key.clone(), pin.id.to_string())) {
                    let dst_pin = resolve_feedback_pin(pin.id, &st.pin_ids);
                    if let Some(dst_pin) = dst_pin {
                        if !directly_wired.contains(dst_pin) {
                            sink_outputs.entry((st.device_id.clone(), dst_pin.to_string()))
                                .and_modify(|cur| *cur = combine_signals(*cur, sig))
                                .or_insert(sig);
                        }
                    }
                }
            }
        }
    }
    
    // ── Post-pass 4: Network Receive feedback aggregation ─────────────
    if graph_has_net_recv(&graph.nodes) {
        let mut sink_sources = build_sink_source_index(&graph.nodes);
        publish_recv_feedback_frames(&graph.nodes, 0, false, dev_sigs, 
                                    &collector_sigs, &sink_sources);
    }
    
    // ── Post-pass 5: Macro namespace snapshot ─────────────────────────
    // Save this tick's macro signals for next-tick readers (one tick stale)
    let carry = state.entry(MACRO_CARRY_UID).or_default();
    carry.macro_prev.clear();
    carry.source_block.clear();
    for ((k, pin), sig) in collector_sigs.iter() {
        if k == SIGS_NS || k == SIGS_NS_VEC2 {
            carry.macro_prev.insert((k.clone(), pin.clone()), *sig);
        } else if let Some(dev) = k.strip_prefix(SRC_BLOCK_PREFIX) {
            carry.source_block.insert((dev.to_string(), pin.clone()));
        }
    }
}
```

**Post-pass ordering rationale:**
1. **Self-sink routing** must run after main loop (computed[] is filled)
2. **Reverse-feedback** resolves through Selector chains before injection drain
3. **Feedback Control drain** uses fb_routes populated by Selectors in pass 2
4. **Network Receive aggregation** needs all injectors to have drained first
5. **Macro snapshot** preserves state for next tick's readers

---

## Sub-patch Evaluation (`eval_subgraph`)

### Recursive Structure

Sub-patches are evaluated recursively with namespaced UIDs to prevent collisions:

```rust
fn eval_subgraph(
    graph: &ProcessingGraph,
    outer_inputs: &[Option<Signal>],
    state: &mut HashMap<usize, NodeState>,
    dev_sigs: &HashMap<(String, String), Signal>,
    collector_sigs: &mut HashMap<(String, String), Signal>,
    scope_samples: &mut Vec<(usize, Vec<Option<f32>>)>,
    last_inputs: &mut HashMap<usize, Vec<Option<Signal>>>,
    last_outputs: &mut HashMap<usize, Vec<Option<Signal>>>,
    fb_routes: &mut HashMap<String, String>,
    outer_uid: usize,       // UID of containing sub-patch node
    dt: f32,
) -> Vec<Vec<Option<Signal>>> {
    let n = graph.nodes.len();
    let mut computed: Vec<Vec<Option<Signal>>> = vec![vec![]; n];
    
    for (idx, snap) in graph.nodes.iter().enumerate() {
        // Compute namespaced UID to avoid collisions with outer graph nodes
        let ns_uid = namespaced_uid(outer_uid, snap.node_uid);
        
        // ── Inlet node: bridge from outer inputs ────────────────────────
        if snap.module_id == "subpatch.inlet" {
            let pin_idx = snap.params.get("pin_index")
                .and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            computed[idx] = vec![outer_inputs.get(pin_idx).copied().flatten()];
            continue;
        }
        
        // ── Nested sub-patch: recursive evaluation ──────────────────────
        if let Some(ref sg) = snap.inline_subgraph {
            let inner_inputs = resolve_inputs(&snap.input_sources, &computed);
            let nested_uid = namespaced_uid(outer_uid, snap.node_uid);
            let inner_computed = eval_subgraph(
                &sg.graph, &inner_inputs, state, dev_sigs, 
                collector_sigs, scope_samples, last_inputs, 
                last_outputs, fb_routes, nested_uid, dt,
            );
            computed[idx] = map_outlets(&sg.outlet_locs, &inner_computed);
            continue;
        }
        
        // ── AutoMap Collector inside sub-patch: namespaced injection ────
        if snap.module_id == "module.automap_collect" {
            let inputs = resolve_inputs(&snap.input_sources, &computed);
            let collect_ids = get_collect_pin_ids(&snap.params);
            let uid_key = format!("collector:{}", ns_uid);
            
            // Phase 1: Pass-through from upstream AutoMap source
            pass_through_upstream(snap, &uid_key, dev_sigs, collector_sigs);
            
            // Phase 2: Explicit collected-pin overrides (win over pass-through)
            for (i, pin_id) in collect_ids.iter().enumerate() {
                if let Some(sig) = inputs.get(i + 1).and_then(|s| *s) {
                    collector_sigs.insert((uid_key.clone(), pin_id.clone()), sig);
                }
            }
            
            computed[idx] = vec![None];
            continue;
        }
        
        // ── Remapper/Touch Zones/Menu inside sub-patch: namespaced publish ─
        if snap.module_id == "module.remapper" {
            eval_remapper_node(snap, ns_uid, dev_sigs, collector_sigs, state, dt);
            computed[idx] = vec![None];
            continue;
        }
        
        // ... (similar arms for other special nodes)
        
        // ── Standard module evaluation with namespaced UID ──────────────
        let inputs = resolve_inputs(&snap.input_sources, &computed);
        let node_state = state.entry(ns_uid).or_insert_with(NodeState::default);
        
        if let Some(ref vals) = snap.aux_f32_override {
            node_state.aux_f32 = vals.clone();
        }
        
        let node_outputs = compute_node(snap, &inputs, node_state, dev_sigs, 
                                       collector_sigs, dt);
        
        // Mirror display state using namespaced UID
        mirror_display_state_for_subgraph(snap.module_id.as_str(), ns_uid, 
                                         &inputs, &node_outputs, scope_samples);
        
        computed[idx] = node_outputs;
    }
    
    computed
}
```

### Namespaced UID Generation (`namespaced_uid`)

Prevents collisions when multiple sub-patches share inner node indices:

```rust
pub fn namespaced_uid(outer: usize, inner: usize) -> usize {
    // Splitmix64-style finalizer over 128→64 fold of the two operands
    let mut z = (outer as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add((inner as u64).wrapping_add(1));
    
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    
    // Force high bit set so namespaced UIDs never alias plain node UIDs
    (z | 0x8000_0000_0000_0000) as usize
}
```

**Why not `(outer << 20) + inner + 1`?**
- Left-shift by 20 discards high bits
- Two different `(outer, inner)` pairs could alias to same UID
- Observed bug: Remapper in top-level sub-patch collided with differently-nested one

---

## Node State Management (`NodeState`)

### Structure

> Representative fields (see `crates/engine/src/state.rs` for the exact set). The real
> struct also carries `press_state`, `gesture_state`, and turbo/gesture bookkeeping;
> some types differ from below (`dc_blend: Vec<f64>`, `twoway_lane: Vec<i8>`). All state
> vectors grow lazily per channel.

```rust
pub struct NodeState {
    // Auxiliary floating-point state (module-specific usage)
    pub aux_f32: Vec<f32>,
    
    // Previous frame inputs (for change detection, delay modules)
    pub prev_signals: Vec<Option<Signal>>,
    
    // Last evaluated outputs (mirrored to UI for display)
    pub last_signals: Vec<Option<Signal>>,
    
    // Specialized buffers (grown lazily per channel)
    pub delay_bufs: Vec<VecDeque<(Instant, f32)>>,      // Delay module
    pub avg_bufs: Vec<VecDeque<f32>>,                    // Average module
    pub avg_bufs_v2: Vec<VecDeque<Vec2>>,                // Average for Vec2
    pub dc_fast: Vec<f64>,                               // DC filter fast path
    pub dc_estimates: Vec<f64>,                          // DC filter estimate
    pub dc_corrections: Vec<f64>,                        // DC filter correction
    pub dc_timers: Vec<f32>,                             // DC filter stability timer
    pub dc_frozen: Vec<f64>,                             // DC filter frozen output
    pub dc_blend: Vec<f32>,                              // DC filter blend factor
    pub twoway_lane: Vec<i32>,                           // Two-way curve lane (1=up, -1=down)
    pub twoway_dir_buf: Vec<VecDeque<f32>>,              // Two-way hysteresis window
    pub twoway_blend: Vec<f32>,                          // Two-way interpolation factor
    pub twoway_prev_input: Vec<f32>,                     // Two-way previous input
    pub twoway_old_output: Vec<f32>,                     // Two-way output at switch point
    
    // Macro namespace carry-over (one tick stale for readers)
    pub macro_prev: HashMap<(String, String), Signal>,
    
    // Source-block suppression set
    pub source_block: HashSet<(String, String)>,
}

impl Default for NodeState {
    fn default() -> Self {
        Self {
            aux_f32: Vec::new(),
            prev_signals: Vec::new(),
            last_signals: Vec::new(),
            delay_bufs: Vec::new(),
            avg_bufs: Vec::new(),
            avg_bufs_v2: Vec::new(),
            dc_fast: Vec::new(),
            dc_estimates: Vec::new(),
            dc_corrections: Vec::new(),
            dc_timers: Vec::new(),
            dc_frozen: Vec::new(),
            dc_blend: Vec::new(),
            twoway_lane: Vec::new(),
            twoway_dir_buf: Vec::new(),
            twoway_blend: Vec::new(),
            twoway_prev_input: Vec::new(),
            twoway_old_output: Vec::new(),
            macro_prev: HashMap::new(),
            source_block: HashSet::new(),
        }
    }
}
```

### State Growth Pattern

Modules grow state vectors lazily to avoid per-tick allocation:

```rust
// In compute_dc_filter():
while state.dc_fast.len() < inputs.len() { 
    state.dc_fast.push(0.0); 
}
while state.dc_estimates.len() < inputs.len() { 
    state.dc_estimates.push(0.0); 
}
// ... etc

// In compute_delay():
while state.delay_bufs.len() < inputs.len() {
    state.delay_bufs.push(VecDeque::new());
}
```

**Why lazy growth?**
- Processing thread runs at 2 kHz (500 µs budget)
- Allocating Vec/HashMap every tick causes GC pressure
- Lazy growth amortizes allocation cost over first use

### State Override Mechanism

UI can override `aux_f32` values for counter reset, knob adjustments, etc.:

```rust
// In NodeSnap:
pub aux_f32_override: Option<Vec<f32>>,

// Applied during evaluation:
if let Some(ref vals) = snap.aux_f32_override {
    node_state.aux_f32 = vals.clone();
}
```

**Use cases:**
- Counter reset button in UI
- Knob value change from slider
- Dropdown selection change

---

## Signal Combination & Clamping

### combine_signals()

Multi-source sinks add signals additively:

```rust
pub fn combine_signals(a: Option<Signal>, b: Option<Signal>) -> Option<Signal> {
    match (a, b) {
        (Some(Signal::Float(x)), Some(Signal::Float(y))) => 
            Some(Signal::Float(x + y)),
        (Some(Signal::Vec2(x)), Some(Signal::Vec2(y))) => 
            Some(Signal::Vec2(x + y)),
        (a, _) => a,  // First wins for incompatible types
    }
}
```

### combine_feedback_max()

Haptic feedback uses max-combine (loudest wins):

```rust
pub fn combine_feedback_max(a: Signal, b: Signal) -> Signal {
    match (a, b) {
        (Signal::Float(x), Signal::Float(y)) => 
            Signal::Float(x.max(y)),
        _ => a,
    }
}
```

**Why max instead of add?**
- Multiple virtual sinks can auto-map from same physical device
- Additive rumble would clip/distort
- Max preserves intended feedback level

### shape_hd_feedback()

Perceptual shaping for HD voice-coil amplitude pins:

```rust
pub fn shape_hd_feedback(sig: Signal, floor: f32, max: f32, exp: f32) -> Signal {
    let value = sig.as_float();
    if value < floor {
        Signal::Float(0.0)  // Below perceptual threshold
    } else {
        let remapped = ((value - floor) / (1.0 - floor)).powf(exp) * max;
        Signal::Float(remapped.clamp(0.0, max))
    }
}
```

**Parameters:**
- `floor`: Minimum amplitude to trigger (default 0.35)
- `max`: Maximum output amplitude (default 1.0)
- `exp`: Power curve exponent (<1 boosts low values, >1 attenuates)

---

## Display State Mirroring

### scope_samples

Populated by display modules for oscilloscope/vector scope rendering:

```rust
// In eval_graph_tick main loop:
match snap.module_id.as_str() {
    "display.oscilloscope" | "display.readout" => {
        let sample = inputs.iter().map(|s| sig_to_f32(*s)).collect();
        scope_samples.push((snap.node_uid, sample));
        last_inputs.insert(snap.node_uid, inputs.clone());
    }
    "display.trigscope" => {
        // Trigger input is first, data channels follow
        let sample = inputs.iter().map(|s| sig_to_f32(*s)).collect();
        scope_samples.push((snap.node_uid, sample));
        last_inputs.insert(snap.node_uid, inputs.clone());
    }
    "display.vectorscope" => {
        // Flatten Vec2 inputs to [x1, y1, x2, y2, ...]
        let sample = inputs.iter().flat_map(|sig| match sig {
            Some(Signal::Vec2(v)) => [Some(v.x), Some(v.y)],
            _ => [None, None],
        }).collect();
        scope_samples.push((snap.node_uid, sample));
        last_inputs.insert(snap.node_uid, inputs.clone());
    }
    "display.controller3d" => {
        // Store all inputs for 3D viewer orientation reading
        last_inputs.insert(snap.node_uid, inputs.clone());
    }
    _ => {}
}
```

### last_inputs / last_outputs

Mirrored for UI signal glow effects on downstream pins:

```rust
// After compute_node():
last_inputs.insert(ns_uid, inputs.clone());
last_outputs.insert(ns_uid, node_outputs.clone());
```

**UI consumption:**
- `apply_display_state()` in `crates/ui/src/app/graph.rs`
- Reads from `proc_outputs.last_inputs` and `last_outputs` maps
- Updates `NodeData.extra.last_signals` for per-pin glow rendering

---

## TickOutput Structure

### Definition

```rust
// crates/engine/src/eval/config.rs
pub struct TickOutput {
    pub outputs: HashMap<(usize, usize), Option<Signal>>,   // (node_uid, pin) → signal
    pub scope_samples: Vec<(usize, Vec<Option<f32>>)>,      // (uid, per-channel samples)
    pub last_inputs: HashMap<usize, Vec<Option<Signal>>>,   // uid → inputs
    pub last_outputs: HashMap<usize, Vec<Option<Signal>>>,  // uid → captured outputs
    pub sink_outputs: HashMap<(String, String), Signal>,    // (device_id, pin) → signal
}

impl TickOutput {
    pub fn clear(&mut self) {
        self.outputs.clear();
        self.scope_samples.clear();
        self.last_inputs.clear();
        self.last_outputs.clear();
        self.sink_outputs.clear();
    }
}
```

### Reuse Pattern

`TickOutput` is reused across ticks to avoid allocator pressure:

```rust
// In processing thread loop:
let mut out = TickOutput::default();  // Allocated once, reused each tick

loop {
    // eval_graph_tick calls out.clear() itself (retains capacity) — no need to
    // clear beforehand, and note the 5-arg signature (no ui_source_block).
    eval_graph_tick(&graph, &mut state, &dev_sigs, dt, &mut out);
    // Publish out to I/O thread (sink_outputs) and UI thread (display state)
}
```

**Why not drop/reallocate?**
- Processing thread runs at 2 kHz
- Allocating HashMap/Vec every tick causes GC pressure
- `clear()` retains capacity for next use

---

## Performance Profiling

### Puffin Integration

The engine uses `puffin` profiler for CPU profiling:

```rust
// In eval_graph_tick():
puffin::profile_function!();

// In main node loop:
puffin::profile_scope!("main_node_loop");

// In compute_node dispatch:
match snap.module_id.as_str() {
    "math.add" => {
        puffin::profile_scope!("math.add");
        // ... evaluation code
    }
}
```

**Profiling via `puffin_http`:**
1. Enable profiling in Settings → Developer → Profiler
2. Run `cargo install puffin_viewer`
3. Execute `puffin_viewer --url 127.0.0.1:8585`
4. View real-time flame graphs in browser

### Hot Paths

**`compute_node()` dispatch:**
- Pattern match on `module_id` string (cold path optimization)
- Inline function calls for frequently-evaluated modules (device.source, automap_split)

**`eval_subgraph()` recursion:**
- Namespaced UID computation via `namespaced_uid()` (splitmix64 finalizer)
- Recursive call overhead for deeply-nested sub-patches

**Post-pass iteration:**
- Four post-passes iterate over all nodes again
- Early-outs skip empty maps (feedback_inject, net_recv checks)

---

## Debugging Techniques

### Enable Verbose Logging

```rust
// In eval_graph_tick():
#[cfg(debug_assertions)]
if std::env::var("FLEXINPUT_ENGINE_DEBUG").is_ok() {
    eprintln!("[eval] tick idx={} module={}", snap.node_uid, snap.module_id);
}
```

### Scope Sample Inspection

Display modules push samples to `scope_samples` vector. Inspect via:
1. Open oscilloscope node in canvas
2. Check `proc_outputs.scope_pending` after tick
3. Verify sample count matches expected channel count

### State Override Testing

Force `aux_f32` values to test module behavior:
```rust
// In NodeSnap params:
"snap.aux_f32_override = Some(vec![1.0, 0.5, 0.0])";
```

Apply via UI knob/counter reset button and observe output change.

---

## Key Files Summary

| File | Purpose | Lines |
|------|---------|-------|
| `src/lib.rs` | Engine struct, re-exports | ~60 |
| `src/graph.rs` | ProcessingGraph, NodeSnap, SinkTarget | ~90 |
| `src/state.rs` | NodeState definition | ~50 |
| `src/thread.rs` | spawn_processing_thread, ArcSwap types | ~200 |
| `src/eval.rs` | Main eval_graph_tick function | ~1200 |
| `src/eval/compute.rs` | compute_node dispatch, pure eval | ~1300 |
| `src/eval/config.rs` | Curve evaluation helpers | ~200 |
| `src/eval/curves.rs` | apply_curve, sample_curve, bias functions | ~300 |
| `src/eval/device_cal.rs` | Deadzone/gyro calibration application | ~150 |
| `src/eval/publish.rs` | Module publish hooks (ASTH, network) | ~400 |
| `src/eval/registry.rs` | eval_hooks() registry for Phase C modules | ~100 |
| `src/eval/activation.rs` | Switch module state machine | ~150 |
| `src/eval/modules/lean.rs` | 3DOF lean mapping evaluation | ~400 |
| `src/eval/modules/map_action.rs` | Map Action card evaluation | ~300 |
| `src/eval/modules/remapper.rs` | Remapper override publishing | ~250 |
| `src/eval/modules/shared.rs` | Shared AutoMap / mapping helpers | ~200 |
| `src/eval/modules/touch_zones.rs` | Touch Zones + Virtual Menu zone eval | ~350 |
| `src/eval/modules/menu.rs` | Virtual Menu state machine | ~400 |
| `src/eval/modules/gyro3dof.rs` | Gyro 3DOF processing | ~500 |

---

## References

- Engine struct: `crates/engine/src/lib.rs`
- Graph types: `crates/engine/src/graph.rs`
- State management: `crates/engine/src/state.rs`
- Thread spawn: `crates/engine/src/thread.rs`
- Main evaluation: `crates/engine/src/eval.rs` (eval_graph_tick)
- Node dispatch: `crates/engine/src/eval/compute.rs` (compute_node, eval_pure)
- Curve math: `crates/engine/src/eval/curves.rs`
- Publish hooks: `crates/engine/src/eval/publish.rs`
