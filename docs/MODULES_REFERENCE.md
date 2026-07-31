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

### 1. Utility / Control Modules (`crates/modules/src/controls.rs`)

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

#### Text (Label)
- **ID:** `module.label` (display name "Text")
- **Purpose:** Displays or edits a text label
- **Inputs:** None
- **Outputs:** None (display-only)
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

#### Inverse
- **ID:** `math.negate` (id kept from the old "Negate" name for patch compatibility;
  patches load with the node retitled unless it was renamed by hand)
- **Purpose:** Inverts a signal — sign flip, or a unipolar mirror inside `0..max`
- **Inputs:** Input signal
- **Outputs:** Inverted signal
- **Params:** `unipolar` (bool, default false), `unipolar_max` (float, default 1.0)
- **Behavior:**
  - Bipolar (default) — Vec2: `(-x, -y)`; Float: `-value`
  - Unipolar — `clamp(max - value, 0, max)`, component-wise for Vec2. A `0 → max`
    ramp comes out as `max → 0`; input past either end clips instead of going
    negative. `max <= 0` outputs 0.

#### Min/Max
- **ID:** `math.min_max`
- **Purpose:** Reports the largest and smallest of all its inputs
- **Inputs:** A, B, ... (Any) — variadic, `+`/`−` on the body adds or removes pins
- **Outputs:**
  - Output 0: `max`
  - Output 1: `min`
- **Behavior:**
  - Only **wired** inputs are considered, so an unconnected spare pin doesn't
    peg the min at 0. Nothing wired at all → both outputs are 0.
  - Vec2: component-wise min/max (scalars splat); Float otherwise

#### Quantize
- **ID:** `math.quantize`
- **Purpose:** Snaps a signal to a grid
- **Inputs:**
  - Input 0: Value to quantize
  - Input 1 (optional): Factor — overrides the body value while wired
- **Outputs:** Quantized value
- **Parameters:**
  - `factor: f64` - Grid steps per unit (1.0). 1 = whole integers, 2 = halves,
    4 = quarters; non-integer factors are fine.
  - `mode: String` - `"round"` (default, nearest), `"floor"`, `"ceil"`, `"trunc"`
- **Behavior:**
  - `snap(value × factor) / factor`; Vec2 quantizes component-wise
  - `floor` and `trunc` differ only below zero (−1.2 → −2 vs −1)
  - A factor of 0 or less has no grid — the value passes through untouched

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

#### RWS Aim
- **ID:** `processing.rws` — display name "RWS Aim"
- **Purpose:** Real-World-Sensitivity camera aiming. Scales a rotation-rate Vec2
  so a **physical** controller rotation maps 1:1 to the in-game camera once
  calibrated; `rws` is then a user multiplier on that ground truth. Also does
  flick-stick.
- **Inputs:**
  - Input 0: `Rotation` (Vec2) — the aim rate. In `gyro` mode it's a true angular
    rate (`±1 == ±GYRO_REF_DPS` = ±2000 °/s); in `stick_rate` mode a stick
    deflection treated as a rate up to `max_rate_dps` at full tilt.
  - Input 1: `Flick` (Vec2, optional) — stick position for flick-stick.
- **Outputs:**
  - Output 0: `Mouse` (Vec2) — per-tick mouse **displacement**; wire to the KB/M
    `mouse_move` sink (which is NOT scaled by the card's `mouse_sensitivity`, so
    the calibration is portable).
  - Output 1: `Stick` (Vec2) — right-stick **deflection** (unit range) for
    stick-aim games: desired turn rate ÷ `stick_out_dps`, clamped to ±1. Wire to
    a virtual Right Stick.
- **Key parameters:**
  - `scale: f32` (default 100) — mouse counts per degree (THE calibrated value).
  - `rws: f32` (default 1) — sensitivity multiplier over the calibrated ground truth.
  - `input_mode: "gyro" | "stick_rate"`, `max_rate_dps: f32` (stick_rate turn rate).
  - `stick_out_dps: f32` (default 360) — game camera turn rate at full stick, for
    the Stick output.
  - `calibrating: bool`, `cal_speed: f32` (rev/s) — calibration spin (below).
  - `flick_enabled: bool`, `flick_deadzone: f32` (default 0.85),
    `flick_smooth_ms: f32` (default 100) — flick-stick.
  - `suppress_source: "off" | "full" | "deadzone"` — flick-stick source suppression.
  - `field_mode: "ruler" | "room" | "both"`, `field_fov`, `field_bg_alpha`,
    `field_tick_deg`, `field_labels` — the calibration viewport style.
  - `_rws_flick_device` / `_rws_flick_stick` — injected at graph-build time (the
    physical device + stick feeding the Flick input); DO NOT set by hand.
- **Evaluation:** intercepted in **both** eval loops (`eval_rws_node`), NOT plain
  `compute_node`, mirroring the Virtual Menu — so it can publish a source-block
  and read the pre-block snapshot. Core math is `compute_rws`
  (`crates/engine/src/eval/modules/rws.rs`). `dx = yaw_dps·dt·scale·rws +
  flick_deg·scale` (flick is 1:1 through `scale` only — `rws` does NOT apply to a
  flick). Flick state lives in `NodeState::aux_f32[0..6]`.
- **Calibration viewport:** a pinnable element (`field`) rendered to the Config
  Overlay — a scrolling degree **ruler**, a painter-based perspective **cube room**
  (FOV-matched to the game), or **both**. **Calibrate** (disabled on the module
  itself for safety — mouse hijack; run it from the overlay) spins the reference
  at a known `cal_speed` (scale-independent); dial `scale` until the game matches.
  When stopped it follows the live input rate (RWS applied).
- **Flick-stick:** pushing Flick past `flick_deadzone` snaps the camera to the
  stick heading (`atan2(x, y)`, up = 0°, right = +90°), smoothed over
  `flick_smooth_ms`; holding it out and rotating traces the camera 1:1. Tracking
  is accumulated and paid out over a short window so poll-rate quantization
  doesn't reach the mouse as pulses; a brief input dropout holds the heading
  (only a sustained release disengages).
- **Source suppression (like the Virtual Menu):** with `suppress_source` ≠ `off`,
  the stick feeding Flick (auto-detected at build time) is source-blocked
  downstream so it can't leak to its default mapping (e.g. the virtual Right
  Stick), while THIS module keeps steering from it via `unblocked_src`. `full` =
  always block while Flick is on; `deadzone` = block only past the deadzone
  (small movements inside still reach the default mapping).

#### Vec to Axis / Axis to Vec
- **IDs:** `module.vec_to_axis`, `module.axis_to_vec`
- **Category:** Converters
- **Purpose:** Split a Vec2 into X/Y floats, or recombine two floats into a Vec2

#### Vec to Deflection
- **ID:** `module.vec_to_deflection`
- **Category:** Converters
- **Purpose:** Cartesian → polar: how far a vector is pushed, and which way
- **Inputs:** Input 0: In (Vec2)
- **Outputs:**
  - Output 0: `Deflection` — the vector's distance from centre (`length()`,
    raw, so a square-gated stick can exceed 1.0 in the corners)
  - Output 1: `Angle`
- **Parameters:**
  - `degrees: bool` - `false` (default) outputs the angle as `0..1` of a full
    turn; `true` outputs `0..360`
- **Behavior:**
  - Angle 0 is straight **up** (+Y) and grows **clockwise**, so right is
    0.25 / 90°, down 0.5 / 180°, left 0.75 / 270°
  - The top of the range is the same direction as 0, so the output wraps back
    to 0 — it is always in `[0, 1)` / `[0, 360)`, never exactly 1.0 / 360.0
  - A zero vector has no direction: both outputs read 0 (no NaN)

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

### 7. AutoMap / Mapping Modules (`crates/modules/src/processing.rs`)

> Registered in `processing.rs` (alongside the response-curve / gyro / reshape modules),
> not a separate `automap.rs`. The AutoMap PIN VOCABULARY (`ALL_PINS`, `resolve_mapping`,
> feedback pairs) lives in `crates/core/src/automap.rs` — see AUTOMAP_SYSTEM.md.

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
  - `zone_tree{N}: Object` - **Authoritative** BSP zone tree for field N
    (`flexinput_core::touchzones::ZoneNode`). Both **ports** and **mapping** modes run
    on this tree — per-zone (partial) dividers with stable leaf ids. The engine resolves
    a touch via `tree.locate(x,y) -> (leaf_id, lx, ly)`; ports/pins are keyed by leaf id.
  - `col_edges{N}: Array<f64>` / `row_edges{N}: Array<f64>` - Legacy full-width/height
    grid dividers. **Migration source only** — present on un-edited/old patches;
    `ZoneNode::from_grid` migrates them losslessly (leaf id == row-major grid index) the
    first time a field is read, and the first structural edit persists `zone_tree{N}`,
    which is authoritative thereafter. Never deleted from a patch (kept as the migration
    source). See DEVELOPMENT_GUIDELINES.md → *"Touch Zones / Virtual Menu geometry"*.
  - `zone_maps: Array<Mapping>` - Per-zone mapping cards (mapping mode; shared card
    schema with Remapper, incl. per-card `curve`/`threshold`).
  - `zone_meta: Array<Object>` - Per-zone icon + name overrides (shared with Virtual
    Menu). Icons use the shared `icon_key` scheme, including dynamic `gp:<pin>` glyphs.

#### Virtual Menu
- **ID:** `module.menu`
- **Purpose:** A summoned on-screen radial/grid menu whose zones are pointed at with an
  analog source and selected to fire mappings. Shares the BSP zone tree, per-zone
  mapping cards, and `zone_meta` icon/name overrides with Touch Zones.
- **Inputs:** touch/pointer X/Y/active + Show/Select gate pins (plus an optional wired
  Pointer inlet that overrides the configured sources).
- **Outputs:** per-zone signals + menu state; drives the menu overlay viewport.
- **Geometry:** same `zone_tree{N}` / `col_edges`/`row_edges` migration story as Touch
  Zones. `menu_radial: bool` switches between the grid and radial ring layouts;
  `menu_radial_origin: f64` rotates the radial ring's origin seam. **Both** ports and
  mapping modes render on the tree (a `ports { grid } else { tree }` split used to break
  radial-ports — do not reintroduce it).
- **Key parameters (the pinnable `options` element, gamepad-field-editable):**
  - Pointer sources (additive): `ptr_ls`, `ptr_rs`, `ptr_touch` (+ `ptr_touch_which`),
    `ptr_gyro` (+ `ptr_gyro_axes`, `ptr_gyro_sens`). Absent flags fall back to the legacy
    single-choice `pointer_source`.
  - Session behaviour: `activation_mode` (hold/toggle/touch), `select_on`
    (release/press/click), `pointer_deadzone`, `select_linger`, `hover_sticky`.
  - Header: `menu_name`, `menu_icon`/`menu_icon_svg`.

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
