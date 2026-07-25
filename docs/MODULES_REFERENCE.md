# FlexInput Modules Reference

## Overview

FlexInput uses a module-based signal processing system where each module transforms input signals into output signals. Modules are registered with descriptors defining their ID, name, category, inputs, and outputs. The engine evaluates modules during graph tick processing.

**Module Registration Pattern:**
```rust
pub struct ModuleRegistration {
    pub descriptor: ModuleDescriptor,
    pub factory: ModuleFactory,  // fn() -> Box<dyn Module>
}

pub struct ModuleDescriptor {
    pub id: &'static str,           // Stable dot-namespaced ID
    pub display_name: &'static str,
    pub category: &'static str,
    pub inputs: Vec<PinDescriptor>,
    pub outputs: Vec<PinDescriptor>,
}
```

---

## Module Categories

### 1. Utility Modules (`crates/modules/src/util.rs`)

#### Constant
- **ID:** `module.constant`
- **Purpose:** Outputs a fixed float value
- **Inputs:** None (parameter-driven)
- **Outputs:** Float
- **Parameters:**
  - `value: f64` - Output value (-1.0 to 1.0 default range)

#### Switch
- **ID:** `module.switch`
- **Purpose:** Toggles between on/off states, can be triggered by inputs or UI
- **Inputs:** 
  - Input 0: Direct trigger (Bool)
  - Input 1: Latch trigger (Bool, rising edge)
- **Outputs:** Bool
- **Parameters:**
  - `active: bool` - Current state (persisted)
  - `ui_toggle_seq: u64` - UI click sequence counter for race-free toggling

#### Knob
- **ID:** `module.knob`
- **Purpose:** User-adjustable float parameter (persistent across patches)
- **Inputs:** None
- **Outputs:** Float
- **Parameters:**
  - `value: f64` - Current knob position (-1.0 to 1.0 default range)

#### Selector
- **ID:** `module.selector`
- **Purpose:** Selects one of N inputs based on a selector value (0.0 to 1.0)
- **Inputs:** 
  - Input 0: Selector (Float, 0.0 = first input, 1.0 = last input)
  - Inputs 1..N+1: Signal sources
- **Outputs:** Float (selected signal)
- **Parameters:**
  - `interpolate: bool` - Enable smooth interpolation between inputs

#### Dropdown
- **ID:** `module.dropdown`
- **Purpose:** Provides discrete selection from a list of options
- **Inputs:** None
- **Outputs:** 
  - Output 0: Float (normalized position, 0.5/N to (N-0.5)/N)
  - Output 1: Int (selected index)
- **Parameters:**
  - `options: Array<String>` - List of option labels
  - `selected_index: u64` - Currently selected index

#### Text
- **ID:** `module.text`
- **Purpose:** Displays or edits text strings
- **Inputs:** None
- **Outputs:** String (via UI binding)
- **Parameters:**
  - `text: String` - Current text content

#### SVG
- **ID:** `module.svg`
- **Purpose:** Renders SVG graphics in node body
- **Inputs:** None
- **Outputs:** None (display-only)
- **Parameters:**
  - `svg_data: String` - Base64-encoded or inline SVG

#### Split
- **ID:** `module.split`
- **Purpose:** Splits a signal into multiple outputs based on selector
- **Inputs:** 
  - Input 0: Selector (Float, 0.0 to 1.0)
  - Input 1: Signal to split
- **Outputs:** N signals (one per output pin)
- **Parameters:**
  - `interpolate: bool` - Enable smooth interpolation between outputs

#### Sub-patch
- **ID:** `subpatch`
- **Purpose:** Contains an inline sub-graph with declared I/O pins
- **Inputs:** Declared in subpatch definition
- **Outputs:** Declared in subpatch definition
- **Parameters:** None (uses inlet/outlet nodes internally)

---

### 2. Math Modules (`crates/modules/src/math.rs`)

#### Add
- **ID:** `math.add`
- **Purpose:** Sums multiple signals (Float or Vec2)
- **Inputs:** A, B, ... (Any compatible type)
- **Outputs:** Sum (same type as inputs)
- **Behavior:** 
  - If any input is Vec2, performs vector addition
  - Otherwise performs scalar addition

#### Subtract
- **ID:** `math.subtract`
- **Purpose:** Subtracts subsequent inputs from the first
- **Inputs:** A, B, ... (Any compatible type)
- **Outputs:** Result (A - B - ...)
- **Behavior:** 
  - Vec2: component-wise subtraction
  - Float: sequential subtraction

#### Multiply
- **ID:** `math.multiply`
- **Purpose:** Multiplies signals together
- **Inputs:** A, B, ... (Any compatible type)
- **Outputs:** Product
- **Behavior:** 
  - Vec2: component-wise multiplication
  - Float: scalar multiplication

#### Divide
- **ID:** `math.divide`
- **Purpose:** Divides first signal by subsequent signals
- **Inputs:** A, B, ... (Any compatible type)
- **Outputs:** Result (A / B / ...)
- **Behavior:** 
  - Division by zero returns 0.0
  - Vec2: component-wise division

#### Clamp
- **ID:** `math.clamp`
- **Purpose:** Constrains signal to a range
- **Inputs:** 
  - Input 0: Signal to clamp
  - Input 1 (optional): Minimum value
  - Input 2 (optional): Maximum value
- **Outputs:** Clamped signal
- **Parameters:**
  - `min: f64` - Default minimum (-1.0)
  - `max: f64` - Default maximum (1.0)

#### Abs
- **ID:** `math.abs`
- **Purpose:** Returns absolute value
- **Inputs:** Input signal
- **Outputs:** Absolute value
- **Behavior:** 
  - Vec2: component-wise abs
  - Float: |value|

#### Negate
- **ID:** `math.negate`
- **Purpose:** Inverts signal polarity
- **Inputs:** Input signal
- **Outputs:** Negated signal
- **Behavior:** 
  - Vec2: (-x, -y)
  - Float: -value

#### Map Range
- **ID:** `math.map_range`
- **Purpose:** Remaps value from one range to another
- **Inputs:** 
  - Input 0: Value to remap
  - Input 1 (optional): Source min
  - Input 2 (optional): Source max
  - Input 3 (optional): Target min
  - Input 4 (optional): Target max
- **Outputs:** Remapped value
- **Parameters:**
  - `in_min: f64` - Default source minimum (-1.0)
  - `in_max: f64` - Default source maximum (1.0)
  - `out_min: f64` - Default target minimum (-1.0)
  - `out_max: f64` - Default target maximum (1.0)

---

### 3. Logic Modules (`crates/modules/src/logic.rs`)

#### AND
- **ID:** `logic.and`
- **Purpose:** Logical AND of two boolean inputs
- **Inputs:** A, B (Bool)
- **Outputs:** Result (A && B)

#### OR
- **ID:** `logic.or`
- **Purpose:** Logical OR of two boolean inputs
- **Inputs:** A, B (Bool)
- **Outputs:** Result (A || B)

#### NOT
- **ID:** `logic.not`
- **Purpose:** Logical negation
- **Inputs:** Input (Bool)
- **Outputs:** Negated input

#### XOR
- **ID:** `logic.xor`
- **Purpose:** Exclusive OR of two boolean inputs
- **Inputs:** A, B (Bool)
- **Outputs:** Result (A ^ B)

#### Equal
- **ID:** `logic.equal`
- **Purpose:** Compares two signals for equality
- **Inputs:** A, B (Float coerced)
- **Outputs:** Bool (true if equal)

#### NotEqual
- **ID:** `logic.not_equal`
- **Purpose:** Compares two signals for inequality
- **Inputs:** A, B (Float coerced)
- **Outputs:** Bool (true if not equal)

#### GreaterThan
- **ID:** `logic.greater_than`
- **Purpose:** Tests if first signal is greater than second
- **Inputs:** A, B (Float)
- **Outputs:** Bool
- **Parameters:**
  - `or_equal: bool` - Include equality in comparison

#### LessThan
- **ID:** `logic.less_than`
- **Purpose:** Tests if first signal is less than second
- **Inputs:** A, B (Float)
- **Outputs:** Bool
- **Parameters:**
  - `or_equal: bool` - Include equality in comparison

#### Has Changed
- **ID:** `logic.has_changed`
- **Purpose:** Detects when input signal changes value
- **Inputs:** Input signal
- **Outputs:** 
  - Output 0: Bool (true if changed)
  - Output 1: Bool (true if increased)
  - Output 2: Bool (true if decreased)

#### Logic Delay
- **ID:** `logic.delay`
- **Purpose:** Delays boolean signal by specified time
- **Inputs:** Input (Bool)
- **Outputs:** Delayed Bool
- **Parameters:**
  - `mode: String` - "delay_true" or "delay_false"
  - `time: f64` - Delay duration in milliseconds
  - `unit: String` - Time unit ("ms" or "s")

#### Counter
- **ID:** `logic.counter`
- **Purpose:** Counts rising edges on increment input
- **Inputs:** 
  - Input 0: Increment trigger (Bool, rising edge)
  - Input 1: Decrement trigger (Bool, rising edge)
  - Input 2: Reset trigger (Bool, rising edge)
  - Input 3 (optional): Step size
  - Input 4 (optional): Minimum value
  - Input 5 (optional): Maximum value
- **Outputs:** Int (current count) or Float (normalized if enabled)
- **Parameters:**
  - `mode: String` - "loop", "limit", "bounce", "unlimited"
  - `step_param: f64` - Default step size (1.0)
  - `min_param: f64` - Default minimum (0.0)
  - `max_param: f64` - Default maximum (10.0)
  - `normalized: bool` - Output as normalized float

---

### 4. Processing Modules (`crates/modules/src/processing.rs`)

#### Delay
- **ID:** `module.delay`
- **Purpose:** Delays signal by specified time with per-channel buffers
- **Inputs:** N channels (Float)
- **Outputs:** N delayed channels (Float)
- **Parameters:**
  - `delay_ms: f64` - Delay duration in milliseconds (0 to 60,000)

#### Average
- **ID:** `module.average`
- **Purpose:** Averages signal over a sliding window with optional spike rejection
- **Inputs:** N channels (Float or Vec2)
- **Outputs:** N averaged channels
- **Parameters:**
  - `buf_size: f64` - Window size in samples (1 to 10,000)
  - `spike_mad: f64` - MAD threshold for spike rejection (0.0 = disabled)

#### DC Filter
- **ID:** `module.dc_filter`
- **Purpose:** Removes DC offset from signal using adaptive estimation
- **Inputs:** N channels (Float)
- **Outputs:** N filtered channels
- **Parameters:**
  - `window_ms: f64` - Estimation window (10 to 60,000 ms)
  - `decay_ms: f64` - Correction decay time (10 to 60,000 ms)

#### Response Curve
- **ID:** `module.response_curve`
- **Purpose:** Applies a user-defined curve to signal with bias control
- **Inputs:** N channels (Float)
- **Outputs:** N curved channels (Float)
- **Parameters:**
  - `points: Array<[f64, f64]>` - Curve control points [(x, y), ...]
  - `biases: Array<f64>` - Per-segment bias values
  - `absolute: bool` - Apply curve to absolute value
  - `in_min: f64` - Input range minimum (-1.0)
  - `in_max: f64` - Input range maximum (1.0)
  - `out_min: f64` - Output range minimum (-1.0)
  - `out_max: f64` - Output range maximum (1.0)

#### Vec Response Curve
- **ID:** `module.vec_response_curve`
- **Purpose:** Applies response curve to vector magnitude, preserving direction
- **Inputs:** N channels (Vec2)
- **Outputs:** N curved vectors
- **Parameters:** Same as Response Curve (magnitude-only application)

#### Vec Reshaper
- **ID:** `module.vec_reshape`
- **Purpose:** Directionally reshapes 2D vectors with boundary and gain curves
- **Inputs:** N channels (Vec2)
- **Outputs:** N reshaped vectors
- **Parameters:**
  - `boundary_pts: Array<[f64, f64]>` - Boundary curve control points
  - `gain_pts: Array<[f64, f64]>` - Gain curve control points
  - `gain_biases: Array<f64>` - Per-segment gain biases
  - `symmetry: String` - Symmetry mode ("quad4", "quad2", etc.)
  - `renorm: bool` - Renormalize output vectors
  - `in_max: f64` - Input range maximum (1.0)
  - `out_max: f64` - Output range maximum (1.0)

#### Two-way Response Curve
- **ID:** `module.twoway_response_curve`
- **Purpose:** Applies different curves for rising vs falling input (hysteresis)
- **Inputs:** N channels (Float or Vec2)
- **Outputs:** N curved channels
- **Parameters:**
  - `points: Array<[f64, f64]>` - Up-lane curve control points
  - `biases: Array<f64>` - Up-lane biases
  - `points_dn: Array<[f64, f64]>` - Down-lane curve (falls back to up-lane)
  - `biases_dn: Array<f64>` - Down-lane biases
  - `hysteresis_pct: f64` - Hysteresis threshold as percentage of range
  - `hysteresis_ms: f64` - Hysteresis detection window in milliseconds
  - `interp_ms: f64` - Transition interpolation time in milliseconds
  - `vec_mode: bool` - Apply to Vec2 magnitude

#### Gyro 3DOF
- **ID:** `processing.gyro_3dof`
- **Purpose:** Processes gyroscope data for 3-degree-of-freedom mapping
- **Inputs:** 
  - Input 0: X-axis gyro (Float)
  - Input 1: Y-axis gyro (Float)
  - Input 2: Z-axis gyro (Float)
  - Input 3: Accelerometer X (optional, Float)
  - Input 4: Accelerometer Y (optional, Float)
  - Input 5: Accelerometer Z (optional, Float)
- **Outputs:** 
  - Output 0: Orientation quaternion (Vec4)
  - Outputs 1..N: Lean mappings (Bool/Float per configured mapping)
- **Parameters:**
  - `lean_left: Array<Mapping>` - Left lean mappings
  - `lean_right: Array<Mapping>` - Right lean mappings
  - Each Mapping: `{ out, mode, window_ms, sustain, turbo }`

---

### 5. Display Modules (`crates/modules/src/display.rs`)

#### Readout
- **ID:** `display.readout`
- **Purpose:** Displays current signal value numerically
- **Inputs:** N channels (any type)
- **Outputs:** None (display-only)
- **Behavior:** Shows last input values in node body

#### Oscilloscope
- **ID:** `display.oscilloscope`
- **Purpose:** Real-time waveform visualization
- **Inputs:** N channels (Float)
- **Outputs:** None (display-only)
- **Behavior:** Plots signal history over time window

#### Trigger Scope
- **ID:** `display.trigscope`
- **Purpose:** Oscilloscope with trigger input for stable display of repetitive signals
- **Inputs:** 
  - Input 0: Trigger signal (Float)
  - Inputs 1..N: Channels to display (Float)
- **Outputs:** None (display-only)

#### Vectorscope
- **ID:** `display.vectorscope`
- **Purpose:** 2D vector visualization (sticks, touchpad)
- **Inputs:** N channels (Vec2)
- **Outputs:** None (display-only)
- **Behavior:** Plots XY coordinates as dots on scope

#### Controller 3D Viewer
- **ID:** `display.controller3d`
- **Purpose:** 3D model visualization with orientation tracking
- **Inputs:** 
  - Input 0: Model identifier or path
  - Input 1: Orientation quaternion (Vec4)
- **Outputs:** None (display-only)

---

### 6. Generator Modules (`crates/modules/src/generator.rs`)

#### Oscillator
- **ID:** `generator.oscillator`
- **Purpose:** Generates periodic waveforms
- **Inputs:** 
  - Input 0 (optional): Frequency multiplier (Float)
  - Input 1 (optional): Phase offset (Float, 0.0 to 1.0)
  - Input 2 (optional): Retrigger trigger (Bool, rising edge)
- **Outputs:** Float (waveform sample)
- **Parameters:**
  - `shape: String` - Waveform shape ("sine", "triangle", "saw", "square")
  - `freq_unit: String` - Frequency unit ("hz" or "ms")
  - `bipolar: bool` - Output range (-1.0 to 1.0) or (0.0 to 1.0)
  - `freq_param: f64` - Base frequency in Hz

#### Envelope Generator
- **ID:** `generator.envelope`
- **Purpose:** Generates amplitude envelopes with configurable shape
- **Inputs:** 
  - Input 0: Trigger (Bool, rising edge starts envelope)
  - Input 1 (optional): Time multiplier (Float)
- **Outputs:** Float (envelope output, 0.0 to 1.0)
- **Parameters:**
  - `hold: bool` - Sustain while triggered
  - `bounce: bool` - Forward/reverse motion
  - `loop: bool` - Continuous looping
  - `timebase: String` - Time unit ("ms", "s", "hz")
  - `time_mul: f64` - Base time parameter (500 ms default)
  - `sustain: f64` - Sustain level (0.0 to 1.0)
  - `points: Array<[f64, f64]>` - Envelope curve control points
  - `biases: Array<f64>` - Per-segment biases

---

### 7. AutoMap Modules (`crates/modules/src/automap.rs`)

#### AutoMap Splitter
- **ID:** `module.automap_split`
- **Purpose:** Extracts individual pins from an AutoMap bus wire
- **Inputs:** 
  - Input 0: AutoMap bus (AutoMap type)
- **Outputs:** N channels (one per configured pin)
- **Parameters:**
  - `_automap_device_id: String` - Source device ID
  - `_automap_collector_id: String` - Upstream collector ID (for override priority)
  - `pin: String` - Pin ID to extract

#### AutoMap Collector
- **ID:** `module.automap_collect`
- **Purpose:** Injects signals into an AutoMap bus for downstream routing
- **Inputs:** 
  - Input 0: AutoMap passthrough (AutoMap type)
  - Inputs 1..N+1: Signals to inject (one per configured pin)
- **Outputs:** None (injects into collector_sigs map)
- **Parameters:**
  - `_automap_device_id: String` - Upstream device ID
  - `_automap_collector_id: String` - Upstream collector ID
  - `_collect_pin_ids: Array<String>` - Pin IDs to inject from inputs

#### AutoMap Fork
- **ID:** `module.automap_fork`
- **Purpose:** Duplicates an AutoMap bus to multiple outputs
- **Inputs:** 
  - Input 0: AutoMap bus (AutoMap type)
- **Outputs:** N copies of the bus
- **Behavior:** Each output carries the full AutoMap signal set

#### AutoMap Selector
- **ID:** `module.automap_selector`
- **Purpose:** Selects one of N AutoMap buses based on selector input
- **Inputs:** 
  - Input 0: Selector (Float, 0.0 to 1.0)
  - Inputs 1..N+1: AutoMap bus sources
- **Outputs:** Selected AutoMap bus
- **Behavior:** Routes feedback routes for reverse haptic flow

#### AutoMap Combiner
- **ID:** `module.automap_combiner`
- **Purpose:** Merges multiple AutoMap buses using configurable policy per pin
- **Inputs:** N AutoMap buses
- **Outputs:** Combined AutoMap bus
- **Parameters:**
  - `combiner_pin_policy: Object<String, String>` - Per-pin merge policy
  - Policies: "OR", "AND", "XOR", "ADD", "MULT"

#### Touch Zones
- **ID:** `module.touch_zones`
- **Purpose:** Divides touchpad into configurable zones with typed outputs
- **Inputs:** 
  - Input 0: Touch X (Float)
  - Input 1: Touch Y (Float)
  - Input 2: Touch Active (Bool)
- **Outputs:** N zone signals (Float/Bool per configured zone)
- **Parameters:**
  - `zone_mode: String` - "ports" or "mapping"
  - `field_mode: String` - "single" or "split"
  - `col_edges_N: Array<f64>` - Column divider positions for field N
  - `row_edges_N: Array<f64>` - Row divider positions for field N

#### Remapper
- **ID:** `module.remapper`
- **Purpose:** Maps AutoMap signals to other AutoMap signals with per-mapping overrides
- **Inputs:** 
  - Input 0: Source AutoMap bus (AutoMap type)
- **Outputs:** None (publishes to remap_sigs map)
- **Parameters:**
  - `mappings: Array<Mapping>` - List of source→destination mappings
  - Each Mapping: `{ src, dst, mode }`

#### Map Action
- **ID:** `module.map_action`
- **Purpose:** Evaluates mapping cards and outputs gate/analog signals
- **Inputs:** 
  - Input 0: Source AutoMap bus (AutoMap type)
- **Outputs:** 
  - Output 0: Gate (Bool, true when mapping is active)
  - Output 1: Analog (Float, deflection magnitude or 0.0)

#### Feedback Control
- **ID:** `module.feedback_control`
- **Purpose:** Routes virtual device feedback signals to physical haptic inputs
- **Inputs:** 
  - Input 0: Source AutoMap bus (AutoMap type)
- **Outputs:** N haptic channel outputs (Float)
- **Parameters:**
  - `target_device_id: String` - Physical device to inject into
  - `inlet_mappings: Array<InletMapping>` - Virtual pin → physical inlet mappings

#### Audio Stream Haptics (ASTH)
- **ID:** `module.audio_stream_haptics`
- **Purpose:** Routes audio loopback to haptic feedback without HIDMaestro driver
- **Inputs:** 
  - Input 0: Source AutoMap bus (AutoMap type)
- **Outputs:** 
  - Output 0: Passthrough AutoMap bus
  - Outputs 1..7: Band energy + carrier frequency signals
- **Parameters:**
  - `target_device_id: String` - Physical device for haptic injection
  - `audio_device: String` - WASAPI loopback device name

---

### 8. SubPatch Modules (`crates/modules/src/subpatch.rs`)

#### Inlet
- **ID:** `subpatch.inlet`
- **Purpose:** Bridge from outer graph to inner sub-patch
- **Inputs:** None (reads from outer_inputs array)
- **Outputs:** Signal from corresponding outer input pin
- **Parameters:**
  - `pin_index: u64` - Index into outer_inputs array

#### Outlet
- **ID:** `subpatch.outlet`
- **Purpose:** Bridge from inner sub-patch to outer graph
- **Inputs:** 
  - Input 0: Signal to export
- **Outputs:** None (writes to inline_subgraph.outlet_locs)

---

### 9. Network Modules (`crates/modules/src/network.rs`)

#### Network Send
- **ID:** `module.network_send`
- **Purpose:** Transmits AutoMap bus to remote instance over network
- **Inputs:** 
  - Input 0: Source AutoMap bus (AutoMap type)
- **Outputs:** Passthrough AutoMap bus
- **Parameters:**
  - `target_ip: String` - Remote instance IP address
  - `target_port: u64` - Remote instance port
  - `transport: String` - "lan", "psk", or "p2p"
  - `passphrase: String` - PSK encryption key (if transport == "psk")
  - `peer_code: String` - P2P connection code (if transport == "p2p")

#### Network Receive
- **ID:** `module.network_recv`
- **Purpose:** Receives AutoMap bus from remote instance and injects into graph
- **Inputs:** None
- **Outputs:** Injected signals into collector_sigs map
- **Parameters:**
  - `_automap_device_id: String` - Synthetic device ID for received signals
  - `listen_port: u64` - Local UDP port to listen on

---

### 10. Macro Module (`crates/modules/src/macro_module.rs`)

#### Macro Output
- **ID:** `module.macro`
- **Purpose:** Reads from macro namespace published by mapping evaluators
- **Inputs:** None (reads from collector_sigs macro namespace)
- **Outputs:** N channels based on configured ports
- **Parameters:**
  - `ports: Array<Port>` - Declared output port definitions
  - Each Port: `{ id, signal_type }`

---

## Module Evaluation Flow

### Pure Modules (`eval_pure`)

Modules without internal state evaluate via pattern match in `compute_node`:
```rust
match snap.module_id.as_str() {
    "math.add" => { /* ... */ }
    "logic.and" => { /* ... */ }
    // ... etc
}
```

### Stateful Modules

Modules with memory maintain state in `NodeState`:
- `aux_f32: Vec<f32>` - Auxiliary floats (phase, counters, flags)
- `prev_signals: Vec<Option<Signal>>` - Previous frame inputs
- Specialized buffers (delay_bufs, avg_bufs, dc_* arrays)

### Special Node Types

Some modules bypass `compute_node()` and are handled directly in the evaluation loop:
- `device.source` - Reads from dev_sigs map
- `module.automap_split/collect` - Injects into collector_sigs
- `module.remapper` - Publishes to remap_sigs
- `module.touch_zones` (mapping mode) - Publishes to touchmap
- `module.menu` - State machine + suppression
- `processing.gyro_3dof` - Lean dispatch

---

## Adding New Modules

### Step 1: Define Module Struct

```rust
pub struct MyModule {
    state: f32,
}

impl Default for MyModule {
    fn default() -> Self {
        Self { state: 0.0 }
    }
}
```

### Step 2: Implement Module Trait

```rust
impl Module for MyModule {
    fn descriptor() -> ModuleDescriptor {
        ModuleDescriptor {
            id: "custom.my_module",
            display_name: "My Module",
            category: "Custom",
            inputs: vec![PinDescriptor::new("Input", SignalType::Float)],
            outputs: vec![PinDescriptor::new("Output", SignalType::Float)],
        }
    }

    fn process(&mut self, inputs: &[Option<Signal>]) -> SmallVec<[Signal; 4]> {
        let input = inputs[0].map(|s| s.as_float()).unwrap_or(0.0);
        // Process...
        vec![Some(Signal::Float(self.state))]
    }
}
```

### Step 3: Register Module

In `crates/modules/src/lib.rs`:
```rust
pub fn all_modules() -> Vec<ModuleRegistration> {
    let mut modules = Vec::new();
    // ... existing registrations
    modules.extend(custom::registrations());
    modules
}
```

### Step 4: Add to Engine Dispatch

In `crates/engine/src/eval/compute.rs`:
```rust
match snap.module_id.as_str() {
    // ... existing arms
    "custom.my_module" => {
        let out = compute_my_module(inputs, state, &snap.params, dt);
        state.last_signals = out.clone();
        out
    }
}
```

### Step 5: (Optional) Register Publish Hook for Phase C

For modules needing custom injection into collector_sigs:
```rust
pub fn eval_hooks(module_id: &str) -> Option<ModuleHook> {
    match module_id {
        "module.audio_stream_haptics" => Some(ModuleHook {
            publish: Some(audio_stream_haptics_publish),
        }),
        _ => None,
    }
}
```

---

## Module Parameter Conventions

### Common Parameter Patterns

**Numeric ranges:**
- Signals: typically -1.0 to 1.0 (sticks, triggers)
- Frequencies: Hz or ms depending on context
- Time delays: milliseconds (0 to 60,000)

**Boolean flags:**
- `interpolate: bool` - Enable smooth transitions
- `absolute: bool` - Apply to absolute value
- `normalized: bool` - Output as 0.0 to 1.0

**Arrays:**
- Curve points: `Array<[f64, f64]>` for (x, y) pairs
- Pin IDs: `Array<String>` for AutoMap collections

**Strings:**
- Mode selectors: "loop", "limit", "bounce", etc.
- Device IDs: "gilrs:dualsense:0" format
- Transport types: "lan", "psk", "p2p"

### Parameter Access in compute_node

```rust
let param_f = |name: &str, default: f32| -> f32 {
    params.get(name).and_then(|v| v.as_f64()).map(|f| f as f32).unwrap_or(default)
};

let value = param_f("my_param", 0.5);
```

---

## Testing Modules

### Unit Tests

Place tests in `crates/engine/src/eval/modules/` or module-specific test files:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_module_evaluation() {
        let mut module = MyModule::default();
        let inputs = vec![Some(Signal::Float(0.5))];
        let outputs = module.process(&inputs);
        assert_eq!(outputs[0], Some(Signal::Float(expected_value)));
    }
}
```

### Integration with Graph Evaluation

Test modules in context of `eval_graph_tick`:
1. Create a ProcessingGraph with test nodes
2. Call `eval_graph_tick()` with known dev_sigs
3. Verify sink_outputs contain expected signals

---

## Performance Notes

### Pure vs Stateful Modules

- **Pure modules** (`eval_pure`): No state allocation, ideal for math/logic
- **Stateful modules**: Require `aux_f32` growth checks and buffer management

### Module Dispatch Optimization

The `compute_node()` function uses pattern match on `module_id`:
- Hot path: device.source, automap_split (frequently evaluated)
- Cold path: display modules (rarely called)

### State Reuse

`NodeState` is reused across ticks via `state.entry(uid).or_insert_with(...)`. Modules should grow buffers lazily and avoid reallocation.

---

## References

- Module trait definition: `crates/core/src/module.rs`
- Compute dispatch: `crates/engine/src/eval/compute.rs`
- Pure evaluation: `eval_pure()` function in compute.rs
- Registry hooks: `eval_hooks()` function in eval/registry.rs
- All module implementations: `crates/modules/src/*.rs`
