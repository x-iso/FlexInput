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
  - `hysteresis_pct: f64` - Hysteresis band as percentage of range. The curves are read at a
    held point that follows the input in the lane's direction with no lag and stays put while
    the input comes back by less than the band (noise suppression); coming back further flips
    the lane. Set just above the input's peak-to-peak jitter. (`hysteresis_ms` is no longer read.)
  - `hyst_points: Array<[f64, f64]>` / `hyst_biases: Array<f64>` - The Hyst tab's graph: band
    over the input (same X as the curves; Y 0..1 = 0..10 %), read at the held point. With two
    dots or more it replaces `hysteresis_pct`; a single dot is `hysteresis_pct` itself
  - `hyst_dir: String` - Which flip the band guards: `both` (default), `up` (only Down → Up,
    a release flips at once) or `down` (only Up → Down, a press flips at once)
  - `hyst_start_up: bool` - Start on Up: at the bottom of the input range (within 0.2 %) the
    tracker sits on the Up lane, unheld, so a pull from rest needs no band to clear
  - `interp_ms: f64` - Blend time from the last emitted output into the new lane after a flip
  - `peak_hold_ms: f64` - Peak hold (0 = off): put out the highest the curve got across each step
    of the held point — a fast pull steps over narrow peaks between device reports — and keep it
    up at least this long; a lane flip drops it at once. On a Down → Up flip the sweep starts
    from the low the press came up from
  - `peak_hold_dir: String` - Lane(s) Peak hold works on: `up` (default, the press), `down`, `both`
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
  - Each Mapping: `{ out, mode, window_ms, sustain, turbo }`, plus `midi_vel` /
    `midi_on` / `midi_off` when it sends MIDI
- **MIDI:** a lean card can play MIDI (the section's **MIDI…** button, or Learn while
  playing a MIDI controller). In Analog mode a controller follows how far the pad
  leans, and a bend goes down leaning left. Published by the shared
  `publish_card_midi` (AUTOMAP_SYSTEM.md → *MIDI on the Bus*).

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
  - Input 1: `Flick` (Vec2, optional) — stick position for flick-stick and
    Stick aim.
  - Input 2: `Flick On` (Bool, optional; unwired = on, added to older nodes on
    load) — a mode-shift control for everything done with the Flick stick: off
    turns off the flick, Stick aim and the stick's suppression, so the stick
    behaves normally again (a weapon wheel, say).
- **Outputs:**
  - Output 0: `Mouse Move (XY)` (Vec2; formerly `Mouse`, renamed on load) —
    per-tick mouse **displacement**; wire to the KB/M
    `mouse_move` sink (which is NOT scaled by the card's `mouse_sensitivity`, so
    the calibration is portable).
  - Output 1: `Stick` (Vec2) — right-stick **deflection** (unit range) for
    stick-aim games: desired turn rate ÷ `stick_out_dps`, clamped to ±1. Wire to
    a virtual Right Stick.
  - Output 2: `Flick` (Vec2; added to older nodes on load) — the flick alone,
    when `flick_output` sends it there.
- **Key parameters:**
  - `scale: f32` (default 100) — mouse counts per degree (THE calibrated value).
  - `scale_unit: "deg" | "360"` (default `"deg"`) — header toggle for how `scale`
    is shown and edited (per degree, or ×360 as dots per 360° like Steam Input).
    Display only; not part of `.fxrws` presets.
  - `rws: f32` (default 1) — sensitivity multiplier over the calibrated ground truth.
  - `input_mode: "gyro" | "stick_rate"`, `max_rate_dps: f32` (stick_rate turn rate).
  - `stick_out_dps: f32` (default 360) — game camera turn rate at full stick, for
    the Stick output.
  - `calibrating: bool`, `cal_speed: f32` (rev/s) — calibration spin (below).
  - `flick_output: "mouse" | "stick" | "both"` (default `"mouse"`) — where the
    flick goes: the `Flick` pin alone in mouse counts, the `Flick` pin alone as
    stick deflection (past full tilt the rest of the turn carries over, so it is
    conserved), or merged into BOTH the Mouse Move and Stick outputs. The flick
    is on only where it reaches something — `"both"`, or a wired `Flick` pin
    (`_rws_flick_out_wired`, injected at build time) — and while `Flick On` is.
    Replaces the old `flick_enabled` checkbox (migrated on load: ticked → `"both"`,
    unticked → `"mouse"`).
  - `flick_deadzone: f32` (default 0.85), `flick_smooth_ms: f32` (default 100) —
    flick engage deadzone and initial-snap smoothing.
  - `flick_stabilise_ms: f32` (default 0) — one-euro stabiliser on the tracked
    heading (the Gyro to Stick Rotation's); lag still held back at release is
    turned then.
  - `flick_fwd_dz_deg: f32` (default 0) — forward deadzone: a flick engaging
    within ±this of straight up doesn't snap, only engages (primes a rotation).
  - `stick_aim_enabled: bool`, `stick_aim_rws: f32` — Stick aim on the Flick
    stick (inside the flick deadzone while the flick is on, else full range).
  - `flick_speed_ms: f32` (default 0 = off) — speed gate: each push out of the
    centre (past 10%) is a gesture. Reaching the flick deadzone within this time
    is a flick — the Stick aim held back on the way out is discarded and Stick
    aim stays off until the centre; slower is steering — the held-back aim
    catches up over `flick_smooth_ms`. `flick_slow_lock: bool` — a steering
    gesture keeps the flick zone off until the centre (past the deadzone it aims
    at full rate). Gesture state: `NodeState::aux_f32[12..19]`.
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

#### AutoMap Response Curve
- **ID:** `module.automap_response_curve`
- **Purpose:** Response Curve on one signal of an AutoMap bus. The header's **Signal**
  dropdown picks a Float or Vec2 signal the connected bus carries; every other signal
  passes through unchanged. A Float is shaped like the Response Curve; a Vec2 (stick,
  D-pad) by its length like the Vec Response Curve, and its `_x`/`_y` axis pins on the
  bus are rewritten to match (curving an axis updates its stick's Vec2 the same way).
  **Gyro (X, Y, Z)** / **Accel (X, Y, Z)** / **Touch 1 (X, Y)** / **Touch 2 (X, Y)** pick
  a whole sensor or touch point: each axis is curved as its own channel (like a 3-channel Response Curve), drawn on one graph with a colour
  legend naming the axes.
- **Inputs:** AutoMap bus
- **Outputs:** AutoMap bus (republished under `collector:{uid}`)
- **Parameters:**
  - `am_curve_pin: String` - The bus pin to reshape, or `gyro` / `accel` / `touch1` /
    `touch2` for every axis of that sensor or touch point (empty = pure pass-through)
  - Curve params as Response Curve / Vec Response Curve; picking a Vec2 sets `absolute`

#### AutoMap Two-way Curve
- **ID:** `module.automap_twoway_response_curve`
- **Purpose:** Two-way Response Curve on one signal of an AutoMap bus — same pick and
  pass-through as the AutoMap Response Curve, with separate rising/falling lanes.
- **Inputs:** AutoMap bus
- **Outputs:** AutoMap bus (republished under `collector:{uid}`)
- **Parameters:**
  - `am_curve_pin: String` - The bus pin to reshape
  - Curve params as Two-way Response Curve; `vec_mode` follows the picked pin's type

#### Gyro to Stick Rotation
- **ID:** `module.stick_rotation`
- **Purpose:** Turning the pad turns one stick of an AutoMap bus that is already
  pushed, for finer aim in twin-stick games: the thumb picks the direction, the wrist
  fine-tunes it. While the stick is out past the inner deadzone the pad's turn rate
  winds up an offset angle and the stick goes out rotated by it, its length unchanged.
  Turning right turns the stick clockwise. Back inside the deadzone the stick passes
  through untouched and the offset is dropped, so the next push starts from the thumb.
  A deadzone of 0 means none: the offset is never dropped and keeps building and
  applying with the stick centred.
- **Absolute / Relative** — Absolute (default) holds the offset: the stick stays turned
  by however far the pad has turned. Relative is the same sum leaking away with the
  **Return** time constant: a quick turn registers nearly whole, a steady turn holds
  an offset of its rate times the return time, and when the pad stops the stick folds
  back to the thumb (about a third left after one return time).
  Every other signal passes through. The body's circle shows the deadzone, the stick
  where the thumb has it (hollow), where the gyro has taken it (filled, with the arc
  between) and the offset in degrees. Every row and the circle pin on their own, and a
  pinned one passes the stick and the motion sensors through the config overlay.
- **Turn** — **Yaw**: the pad's own vertical (`gyro_z`), however it is held.
  **World**: the turn about gravity (low-passed accelerometer, with the accel/gyro
  basis difference accounted for), so a pad pitched up towards you turns by rolling;
  without an accelerometer it falls back to Yaw.
- **Stabilise** smooths the thumb's DIRECTION with a one-euro filter (never the
  length, never the gyro's offset): a tremor reverses too fast to open it, a deliberate
  sweep opens it. A plain speed threshold would do the reverse — tremor is fast.
- **Inputs:** AutoMap bus
- **Outputs:** AutoMap bus (republished under `collector:{uid}`, the stick in all
  three forms); live values in `last_out` (`STICK_ROT_OUT_*`)
- **Parameters:**
  - `rot_stick: String` - `right_stick` (default) or `left_stick`
  - `rot_mode: String` - `yaw` (default) or `world`
  - `rot_sens: f32` - degrees of stick turn per degree of pad turn (default 1.0)
  - `rot_invert: bool` - turn the other way
  - `rot_deadzone: f32` - inner deadzone, 0..1 of stick travel (default 0.2)
  - `rot_smooth_ms: f32` - stabilising time, ms (default 0 = off)
  - `rot_relative: bool` - relative mode (default off = absolute)
  - `rot_return_ms: f32` - relative mode's return time constant, ms (default 250)
- **The same rotation in the JSM Config module:** `GYRO_OUTPUT = RIGHT_STICK_ROTATION`
  (or `LEFT_`), with `GYRO_STICK_ROTATION_DEADZONE` and `GYRO_STICK_ROTATION_SMOOTH_TIME` — see
  JSM Config below. Both run `eval/modules/stick_rotation.rs`.

#### Area Mapper
- **ID:** `module.area_mapper`
- **Purpose:** Lays one XY pair of an AutoMap bus (the header's **Input**: left stick,
  right stick, Touch 1 or Touch 2) onto cells, and fires each cell's mapping cards.
  The area is **circular** (default: a centre and a 4-way ring; rings each cut into
  their own sectors) or **rectangular** (rows, each cut into its own columns), so a
  circle can be 4-way near the centre and 8-way at the rim. Cells are numbered by
  direction — 0 the centre, 1 up, then clockwise in eighths to 8 up-left (a 4-way ring
  uses 1, 3, 5, 7) — so cards keep their direction across shapes and presets. The
  geometry lives in `flexinput_core::area`.
  - **Layers:** tabs under the options row; each layer has its own layout, cards,
    cell names and touch gates, and a colour. The other layers show faintly in theirs:
    their borders, and their mappings where their own cells are, tinted (the edited
    layer's mappings stay white).
    All layers act on the same input together — e.g. an inner/outer ring layer holding
    Shift whichever way the stick points, alongside a 4-way direction layer.
  - **Presets** (options row): Circle 4-way / 8-way / inner-outer, Rectangle 3×3 for the
    edited layer; **Save… / Load…** the whole module (every layer, its cards and
    settings) as a `.fxarea` file.
  - **One press mode for all** (row above the cards): its mode, time, Hold and Turbo
    rule every card of every layer.
  - **Analog layers:** a layer's **Analog** switch drives its keys from the stick's
    deflection instead of its borders and gradients. The field then shows the deadzone
    (the centre disc, its ring still draggable) and each direction's mapping out along
    its spoke, lit as far as that key is held. Past the deadzone:
    - **Mix — Direction** (default): the stick's angle falls between two moves the ring
      offers, a key alone or two neighbouring keys a right angle apart together (W+D),
      split by how close it is to each; the push past the deadzone is how much of the
      time either holds. A full push never stops walking, only steers. For games that
      walk every way at the same speed.
    - **Mix — Per key:** each direction cell gets the stick's push toward it (W and D
      0.71 each on a full diagonal). For games whose diagonal is faster.
    - **Pulse — Smooth** (default): error diffusion. Every press and every gap lasts at
      least **Min** (1–500 ms, default 34 = two frames at 60 fps), they come as often as
      that allows, and owed time is paid back at once, so a change of push shows
      immediately. Letting go releases at once.
    - **Pulse — Fixed PWM:** one press per period (1–1000 ms), its length the push.
    - **Ramp** (Smooth, 0–1000 ms, default 0): how long the game takes from standing to
      full speed. When set, presses are timed against a model of that acceleration, so
      the game's own smoothing holds its speed at the push instead of stopping and
      starting. With a Direction mix the keys then follow the direction's per-key shares.
    - **Half at** (5–95 %, default 50): the push that holds half the time; lower lifts
      light pushes.
    - **Full** (default 95 %): from this radius out the push counts as full, a stable
      zone for holding the keys. It is the outer ring on the field: drag it like the
      deadzone's; with the gamepad, walk to it, South grabs, the stick sizes it, North
      resets it.
  - **Editing:** drag a border to move it, double-click to centre it between its
    neighbours, right-click to add a border through the point or remove the one under
    it; the field's corner grip resizes it. With **Sym** on (default) every edit applies
    to the border's mirror images across the vertical and horizontal axes, and they
    move together; a border lying on an axis is pinned. A whole circle ring has no
    border at all: its first cut is a diameter, halving it, which under Sym turns as
    one (double-click squares it to the nearest 45°); the next cut under Sym makes four
    (the diameter's mirror image, or the perpendicular diameter when it lies on an
    axis). A ring cut back down to one border is whole again. Centring under Sym counts a
    neighbouring mirror image as moving along with the border: a lone cut in a
    quarter goes to 45°, and a cut beside its own mirror splits the cell they share
    into three equal parts.
  - **Pinnable elements:** `options` (Input, Shape, Sym, Pass, the selected cell's
    Touched gate), `field` (editable like the body), `border` (the selected border's
    gradient settings; the curve editor stays in the node body, its presets pin) and
    `cards`.
  - **Gamepad (Easy mode / config overlay):** on the field, the dpad / left stick walks
    cell ↔ border and the cursor picks directly; on a cell RT adds a sector / column
    border through it and LT a ring / row border through its band; on a border South
    grabs it (then the dpad moves it), North centres, West removes, RT toggles its
    gradient. In the config overlay, an Area Mapper reading the left stick passes it
    through to be felt, so the right stick drives the editor (as with AutoMap curves).
    Focusing a border selects it for the `border` element; focusing a cell
    selects it for the cards. Cards work as the Virtual Menu's, with the trigger as the
    last action-row item and as field 9 of an entered card.
  - **Squareness / roundness:** a circle's ring border has a **Square** setting
    (border settings) taking it from a circle to a square — e.g. a square deadzone
    inside round outer rings — either by blending the round and the square distance,
    or with **Round corners** on as a square whose corners are rounded off (corner
    radius shrinking from the whole circle to nothing). Its gradient follows the shape,
    and it stops short where it would touch a neighbouring ring in any direction; a new
    ring takes the shape of the ring inside it. A rectangle's cell has a **Round**
    and a **Pressure** setting (options row, for the selected cell). Round makes it a
    bubble: its rectangle becomes a rounded one of the same area, up to an ellipse (a
    circle for a square cell). Where shapes overlap (a bubble bulging past its sides
    into its neighbours' rectangles, or two bubbles meeting), the higher-pressure cell
    pushes into the lower; at equal pressure they share the overlap. So a round cell at
    no pressure among plain ones rounds only as far as they let it, at full pressure it
    takes its whole circle (a round deadzone in a square grid), equal bubbles meet in
    flat seams and rows cut differently interlock honeycomb-like. The corners a bubble
    gives up go to the plain cells around it (bubbles share a gap only among
    themselves). Gradients crossfade across the seams. The field draws the seams; the
    grid stays as faint lines you drag.
  - **Gradient borders:** any border can crossfade the cells on either side across a
    band of its own width. A cell's *share* of the point is 1 inside it, 0 outside, and
    in between across a gradient, shaped by the border's crossfade curve; at a corner of
    two gradients the shares multiply and still sum to 1. Analog outputs (stick
    directions, triggers) take the share; keys follow the border's own settings — PWM
    (duty = share; **Alternate** phase-locks the cells so they take turns, never both on,
    or **Independent** clocks), a tap train, or a threshold. A cell bounded by two
    gradients follows the one fading it more. A gradient on the centre ring's border
    makes outputs proportional to deflection.
  - **Cards:** the Touch Zones / Virtual Menu cards, keyed by cell id, each firing
    **While inside**, **On enter** or **On leave** (pulses for the card's window as the
    point crosses the cell's border; lifting a finger leaves). A whole-stick, mouse or
    scroll target gets the point itself, scaled by the share, so a cell can pass the
    stick on as aim. Cards in one Area Mapper OR their outputs (W in both the up and
    up-right cells is no conflict).
  - **Touch:** a touchpad point exists only while touched. A cell marked **Touched**
    counts only while the stick reports touch (`{stick}_touch`, a capacitive stick);
    no backend publishes that pin yet.
- **Inputs:** AutoMap bus
- **Outputs:** AutoMap bus (republished under `collector:{uid}`; the picked pair is
  consumed — carried on at rest — unless `area_pass_source`)
- **Parameters:**
  - `area_input: String` - `left_stick` | `right_stick` | `touch1` | `touch2`
  - `area_layout: Object` - `AreaLayout::to_value`: `shape`, `edges` (band borders),
    `bands` (`cuts` + stable `cells` ids), `cell_round` / `cell_pressure` (rectangle
    cell id → 0..1); a circle edge may carry `sq` (squareness, 0..1) and `rc` (square by
    rounding corners); a border may carry `g` (gradient: `w` width,
    `curve`, `dig` = `pwm`|`taps`|`threshold`, `period_ms`, `thr`, `phase` =
    `alternate`|`independent`)
  - `area_sym: bool` - Symmetric editing (UI only, default true)
  - `area_pass_source: bool` - Keep the picked pair on the bus (default false)
  - `area_touch_cells: Array<u32>` - Cells that only count while the stick is touched
  - `zone_maps: Array` - Cards `{ f: layer, z: cell id, in: ["area_in"|"area_enter"|"area_leave"], out, … }`
  - `area_layers: Array` - Every layer: `{ area_layout, zone_meta, area_touch_cells,
    area_layer_color, area_layer_analog, area_layer_ms (Fixed PWM period),
    area_layer_mix (`direction`|`keys`), area_layer_pulse (`smooth`|`pwm`),
    area_layer_hold_ms (Smooth minimum), area_layer_ramp_ms, area_layer_mid,
    area_layer_full }`; the edited layer also lives
    under those plain keys (`_area_layer_loaded` says which), `area_layer` asks for one
  - `area_press_lock`, `area_press_mode`, `area_press_ms`, `area_press_hold`,
    `area_press_turbo` - One press mode for all cards
  - `zone_meta: Object` - Per-cell icon / name override (as the Virtual Menu)
  - `area_shape: String` - The shape as a request the layout follows (switching starts
    the layout over); `area_sq` / `area_sq_rc` mirror the selected ring border's
    squareness and corner mode, `area_cell_round` / `area_cell_press` the selected cell's
    rounding and pressure, `area_g_on` / `area_g_w` / `area_g_curve` / `area_g_keys` /
    `area_g_ms` / `area_g_thr` / `area_g_phase` mirror the selected border's gradient and
    `area_cell_touch` the selected cell's gate. The gamepad's field editor writes only
    params, so `area_sync` carries an edited mirror into `area_layout` and otherwise
    rewrites it from the layout.
- **Config overlay:** tuning it passes its picked pair (with the stick's axes and touch
  flag) through to the game, like the AutoMap curves.
- **Live mirror:** `last_out[1]` the point (centred, +Y up), `last_out[2..]` one
  `Vec2(layer · 4096 + cell id, share)` per cell with a share.

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
- **MIDI:** a zone card can play MIDI — the **MIDI…** button beside Assign…, or the 🎮
  gamepad-learn while playing a MIDI controller — at the card's `midi_vel` / `midi_on` /
  `midi_off` levels.

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
- **MIDI:** selecting a zone can play MIDI, exactly as a Touch Zones card does.

#### Remapper
- **ID:** `module.remapper`
- **Purpose:** Maps AutoMap signals to other AutoMap signals with per-mapping overrides
- **Inputs:** 
  - Input 0: Source AutoMap bus (AutoMap type)
- **Outputs:** None (publishes to remap_sigs map)
- **Parameters:**
  - `mappings: Array<Mapping>` - List of source→destination mappings
  - Each Mapping: `{ src, dst, mode }`
- **MIDI, both ways:**
  - *In:* a MIDI port upstream is read like a pad — a note while it sounds, a knob or
    bend while it moves (Learn captures a continuous one as an Analog card). A card's
    MIDI in-chip picks its channel, or any channel.
  - *Out:* Learn plays the message on any MIDI In in the patch (a picker chooses one when
    there are several), or **MIDI…** builds it by hand in the MIDI editor window, for
    either side of the card. Per card: `midi_vel`, `midi_on`, `midi_off` (0–127).
  - A note is one input with two sides: its gate, and its live value — aftertouch, else
    the channel's pressure, else velocity. On a note card the response curve shapes that
    value and the threshold is a **velocity** threshold (which strikes count). As an
    output from an Analog card, a note sounds from the threshold, struck with the value
    then, and follows it as aftertouch. Velocity and poly aftertouch are no longer picked
    or learned on their own.
  - The Analog press mode is offered only for a card with an analog input — a stick
    direction, a trigger, a note or a continuous MIDI value — alone or chorded with buttons
    (`flexinput_engine::card_allows_analog_mode`); loading resets any other card to
    Normal. An Analog card passes that input's live value, from the first bit of
    movement.

#### Map Action
- **ID:** `module.map_action`
- **Purpose:** Evaluates mapping cards and outputs gate/analog signals
- **Inputs:** 
  - Input 0: Source AutoMap bus (AutoMap type)
- **Outputs:** 
  - Output 0: Gate (Bool, true when mapping is active)
  - Output 1: Analog (Float, deflection magnitude or 0.0)
- **MIDI:** reads the bus's MIDI pins, so a note or a knob can trigger the action; Learn
  hears a MIDI port upstream. Analog mode follows the same analog-input rule as the
  Remapper.

#### Feedback Control
- **ID:** `module.feedback_control`
- **Purpose:** Drives the upstream physical pad's feedback (rumble, light bar,
  LEDs, adaptive triggers) from wired signals, and taps what the game asks of
  the downstream virtual pad
- **Inputs:** 
  - Input 0: Source AutoMap bus (AutoMap type)
  - Inputs 1..N: one per feedback pin (`FEEDBACK_INLET_PINS`), optional
- **Outputs:** AutoMap pass-through + the game's basic feedback taps (`FEEDBACK_OUTLET_PINS`)
- **Parameters:**
  - `fb_override: bool` (header toggle "Override game feedback", default off) —
    off: wired values ADD to the game's feedback; on: each wired kind of
    feedback (`FeedbackGroup`: rumble, light bar, player LED, mic LED, left /
    right trigger) REPLACES the game's. Unwired kinds stay the game's.
  - `_fb_source_dev`, `_fb_dest_dev`, `_fb_inlet_ids`, `_fb_outlet_ids` — stamped
    by the graph builder
- **Engine:** see *Feedback layers* in AUTOMAP_SYSTEM.md (`eval/feedback.rs`)

#### JSM Config
- **ID:** `module.jsm` — display name "JSM Config" (optional module, cargo feature `jsm`)
- **Purpose:** Applies a JoyShockMapper config text to the bus with JSM's own
  rules, so an existing JSM config can be loaded and kept editable. NOT a
  front-end for the Remapper — the press semantics, timings and chord layering
  are JSM's. See `docs/JSM_MODULE_PLAN.md`.
- **Inputs:** Input 0: Source AutoMap bus (AutoMap)
- **Outputs:** Output 0: the bus with the config applied, republished as
  `collector:{uid}`
- **Parameters:**
  - `jsm_tabs: Array<{name, text}>` — one config file per tab; a binding that
    loads a config by name finds the tab of that name (paths and `.txt` ignored)
  - `jsm_active_tab: u32` — the tab the editor has open, and where a config starts
    running; a binding can switch the running tab away from it — the tab being applied
  - `jsm_strict: bool` (header toggle "Only what the config says") — off: inputs
    the config never mentions pass through and the mentioned ones are taken over
    (and marked consumed); on: only the config's own output is published
  - `jsm_editor_w`, `jsm_editor_h: f32` — the editor's size in the node body,
    dragged by the grip in its bottom-right corner
  - `_jsm_dest_dev` — the physical pad its rumble / light bar / trigger effects go
    back to, stamped by the graph builder (same resolution as ASTH's
    `_asth_dest_dev`)
  - `jsm_show_knobs: bool` — the header's **Tune** toggle: the curve preview and the
    per-setting faders. Off by default, since a config with a dozen numeric settings
    would otherwise double the body's height unasked
  - `jsm_knob_side: "bottom" | "top" | "left" | "right"` — where the tuning strip
    sits relative to the editor. Anything unrecognised reads as `bottom`
- **MIDI** (FlexInput's own; JSM has none — `eval/modules/jsm/midi.rs`, plan phase 10):
  `MIDI_*` names on both sides of the `=` (`S = MIDI_C4`, `MIDI_CC64 = GYRO_ON`),
  `MIDI_CHANNEL` / `MIDI_IN_CHANNEL` / `MIDI_VELOCITY` / `MIDI_IN_THRESHOLD`, sticks,
  the touchpad, the gyro and the accelerometer as MIDI values (`LEFT_STICK_MODE = MIDI`
  with `LEFT_MIDI_X`, …), and values carried through a binding where MIDI is involved
  (`ZL = MIDI_CC7`). The editor's command list has a **MIDI…** row that builds a name in
  the MIDI editor window.
- **Engine:** `eval/modules/jsm/` — `parse` (grammar + a status per line),
  `analog` (triggers and all five of JSM's sticks as its buttons), `aim` (gyro and
  sticks as mouse movement), `pad` (whatever drives a virtual pad instead),
  `motion` (gravity: lean, the motion stick, the gravity gyro spaces), `touch`
  (the touchpad), `feedback` (what goes back to the pad), `cc` (the
  JSM_custom_curve fork's additions), `bind` (the press machinery), `eval` (bus in,
  bus out).
  Registered through the registry seam's stateful publisher hook; state lives in
  `NodeState::jsm`
- **Editor:** the body is the config text, each line tinted by what the parser
  made of it, with the lines worth explaining listed underneath (scrolling once
  they outgrow their room). A **Tune** toggle in the header adds, below them, the
  sensitivity curve drawn live and a horizontal fader for every numeric setting
  the config sets.
  A fader rewrites the number on its own line, so the text stays the source of
  truth and the parser sees a drag like any other edit; a setting that takes a
  *pair* of numbers gets none, since one control cannot honestly stand for two.
  The faders carry no wheel adjustment on purpose — the strip they sit in scrolls,
  and on the canvas the wheel pans the Scene, so a fader that changed a
  sensitivity as you scrolled past it would be a nasty surprise. Pinned on its
  own (where nothing else wants the wheel) a fader keeps it.
  A **Log ⟷ Exp** row under the curve stretches the speed axis (`jsm_curve_warp`
  on the node — a view setting, never written to the config text). A gyro
  deadzone is a couple of degrees per second wide and a resting pad's noise floor
  smaller still; against a linear 500°/s axis both are inside the first pixel.
  Pulling to Log opens that end up; double-click restores linear. The samples move
  with the axis (`jsm_sens_curve_warped`), or the stretched end would be drawn as
  one straight line between two far-apart points and the zoom would reveal
  nothing. The gridline labels are read off the warp rather than assumed, and the
  live dot has no lower cutoff — the noise floor is the thing you zoomed in for.
  The curve folds in `GYRO_CUTOFF_SPEED`/`_RECOVERY` (`aim::cutoff_factor`, shared
  with the pipeline so the two can't drift): the cutoff scales the VELOCITY before
  the sensitivity ramp reads it, so a config's gyro deadzone shows as the curve
  falling to nothing below it instead of being invisible.
  The **curve** is drawn the way the `JSM_custom_curve` fork's GUI draws it, so a
  curve read here and the same curve read there are the same picture: both axes
  anchored at zero over a labelled grid, the speed axis a fixed 500°/s unless a
  setting puts the action further out. Two lines — solid is the sensitivity at
  that turn speed, dashed is the resulting camera speed (turn speed ×
  sensitivity) normalised to the same axis. The dashed one is the one that
  catches a fault: where it sags, turning the pad *faster* aims *slower*. Dots
  mark where the pad is right now, and hovering reads off any speed. The axis
  labels drop out when the graph is too small for them to fit.
  A dropdown beside **Tune** puts the strip **Below** (default), **Above**,
  **Left** or **Right** of the editor — beside it the two become columns, which
  suits a wide node; above or below suits a tall one.
  The curve and each fader pin to the config overlay on their own, so a tuning
  session can live there with the editor left on the canvas. Pinned, a fader
  takes the Knob module's shape — wide is a horizontal fader, tall a vertical
  one, square a rotary — and pinned the editor budgets its height between the
  text and the tuning strip, which scrolls, so a config setting thirty numbers is
  as reachable as one setting three.
  Every JSM pin is a **graph pin** for the layout inspector, so its colours come
  from the same `PinGraphOverride` the Response Curve and the scopes use: on the
  curve, `background` / `outline` / `gridline` plus channel 1 = the sensitivity
  line and channel 2 = the camera-speed line; on a fader, `gridline` is the track
  and channel 1 the fill; the editor takes background and outline (reserved as
  shape slots before the body is laid out, since its rect isn't known until
  after and the plate has to sit under what it contains).
  Both the **editor** pin and a single **fader** pin are gamepad-nav targets. A
  lone fader is a `Value` widget; the editor is a multi-field one — and unlike
  every other multi-field element, which is one ROW of controls, it is one
  COLUMN of faders, so its axes are swapped: **up/down steps between settings,
  left/right adjusts** the focused one (`nav_fields_are_a_column`). A stick held
  near a diagonal does not count as a step (`nav_axis_is_clear`, 1.6× margin
  above a 0.5 engage threshold) — slipping onto the next setting and then editing
  *that* is the failure worth preventing, and it leaves no trace on screen. West
  toggles fine, and the focus ring lands on that fader's row. When the strip
  scrolls, the focused fader is brought into view: the nav driver publishes the
  focused index (`publish_nav_focus_field`) and the body scrolls to it, since the
  scroll offset belongs to the body and the focus index to the nav state and
  neither can do it alone. Only a CHANGE of focus scrolls, or holding focus on a
  row would drag the strip back every frame and the wheel would feel stuck. Rows
  are published clipped to the visible band, so one scrolled out rings nothing
  (`nav_ring_is_worth_drawing`) rather than drawing across the container's edge.
  **Tuning passthrough:** while a setting is focused, the input it governs keeps
  reaching the game so you can judge it by feel — and nothing else does.
  `jsm_feel_of` decides what that input is, reading the CONFIG rather than
  guessing: a gyro setting passes the IMU pins, `LEFT_STICK_*` passes the left
  stick, and the ones that don't name a side (`STICK_POWER`, the flick settings)
  pass whichever stick that config actually aims with. A virtual-pad *output*
  setting (`LEFT_STICK_UNDEADZONE_*`, `WIND_STICK_RANGE`…) passes whatever drives
  that stick, which is the gyro when `GYRO_OUTPUT` is routed there. A setting with
  nothing to feel (a press timing) passes the block-all sentinel, never an empty
  list — the block filter reads empty as "the whole pad passes".
  The stick handed to the game stops being nav's: a `LEFT_STICK_*` setting is
  adjusted with the RIGHT stick, and the left no longer walks the list either
  (the dpad still does), or you would be aiming and tuning with one thumb.
  **North restores** the value the setting held before this tuning pass, not a
  default: JSM's own default for a gyro sensitivity is 0, and writing that over
  someone's config mid-game is a silent edit rather than a reset.
  A fine nudge too small to survive the two decimals a config is written with
  moves by the least step the config can express (0.01, or 1 for a whole-numbered
  setting) instead of rounding back and looking broken. The field list is
  built from the config TEXT each frame (`knobs_of`), so a setting typed while the
  overlay is open is navigable on the next frame with no registration step; both
  it and the nudge resolve the active tab through one helper so they can't end up
  walking one tab and writing another. The curve is deliberately not a
  target — there is nothing a pad can do on it.
  A fader pinned small enough to become a knob or a vertical slider puts its name
  above and its value below, painted through a painter whose clip is **replaced**
  (`Painter::set_clip_rect`) rather than narrowed — `with_clip_rect` intersects,
  so the obvious-looking builder call is silently a no-op against the very
  container clip it is trying to escape.
  Where text has to give way it is always the setting's **name** that truncates,
  never its value — a trimmed number can read as a different number. Beside the
  editor the layout falls back to stacking below the minimum width the two
  columns need, rather than showing clipped captions next to a sliver of text.
  Resolving the pad feeding the node goes through
  `find_automap_device_id_for_viewer`, **never** the `_automap_device_id` param —
  the graph builder injects that into a clone on its way to the engine and never
  writes it back to the canvas, so in the UI it is always absent. While the editor
  has keyboard focus the config's key and mouse output pauses, so a binding under
  test can't type into it. The wheel over the editor scrolls the config instead of
  panning the canvas (`canvas/wheel.rs`).
- **Live so far (plan phases 1-9):** digital bindings (tap/hold, all modifiers,
  chord, simultaneous, diagonal, double press, turbo) and JSM's timing settings;
  analog triggers (`TRIGGER_THRESHOLD` including the hair trigger, `ZL_MODE` /
  `ZR_MODE` full pull with every skip mode, `TRIGGER_SKIP_DELAY`); digital sticks
  (`NO_MOUSE` directions, `SCROLL_WHEEL`, ring modes, deadzones, axis inversion,
  `CONTROLLER_ORIENTATION`, `SCROLL_SENS`); and aiming — gyro mouse with the
  sensitivity ramp, smoothing, cutoff, trackball and `GYRO_ON`/`GYRO_OFF`, stick
  `AIM`, flick stick and `MOUSE_AREA`, written to the bus's `mouse_move`; and
  modeshifts — any of those settings can be chorded (`ZL,GYRO_SENS = 4`), latest
  chord winning per setting, with JSM's stick-recentre and unfinished-flick rules;
  and virtual pad output — `LEFT_STICK` / `RIGHT_STICK`, `*_ANGLE_TO_X/Y`,
  `*_WIND_X`, `GYRO_OUTPUT` and `FLICK_STICK_OUTPUT` to a stick, `ZL_MODE = X_LT`
  passing a trigger straight through, and the undeadzone / unpower / scale settings
  that shape them. JSM's own shipped `Xbox.txt` runs end to end.
  And phase 6: the touchpad (`TOUCHPAD_MODE`'s grid, relative touch sticks and
  mouse mode, the grid buttons `T1`-`T25`, and the touchpad as a dual-stage trigger
  where a finger is the soft pull and a click the full one), the motion stick
  (`MOTION_STICK_MODE` including `LEFT_STEER_X`, plus `SET_MOTION_STICK_NEUTRAL`),
  the lean buttons, and the gravity-referenced gyro spaces (`PLAYER_TURN`,
  `PLAYER_LEAN`, `WORLD_TURN`, `WORLD_LEAN`). **Every button JSM has can now be
  read**, with a test asserting it. And phase 7: feedback — `RUMBLE`, `LIGHT_BAR`,
  `ADAPTIVE_TRIGGER`, the two `*_TRIGGER_EFFECT` settings, and rumble bindings
  (`SMALL_RUMBLE`, `BIG_RUMBLE`, `Rhhhh`), published as
  `feedback_override:{_jsm_dest_dev}` so they replace the game's rather than add to
  it. And phase 8: action layers — a quoted config file name switches which tab is
  running (`HOME = "driving.txt"`), a bare one applies that tab where it stands, and
  `RESET_MAPPINGS` works both ways. And phase 9: the `JSM_custom_curve` fork's
  additions — five acceleration curves (`ACCEL_CURVE`), decay smoothing, the
  one-euro filter, `GYRO_ANGLE_SNAP`, the deceleration brake, `YAW_PLUS_ROLL`, and
  its `MISC1`-`MISC6` buttons. Every fork line says it is the fork's, since a config
  using one won't load in a stock JSM. Only `MOUSE_RING` and `HYBRID_AIM` are still
  pending, and their lines say why.
  FlexInput's own addition: `GYRO_OUTPUT = LEFT_STICK_ROTATION` / `RIGHT_STICK_ROTATION`
  turns that stick by the gyro while it is pushed past `GYRO_STICK_ROTATION_DEADZONE`
  (default 0.2), instead of pushing it — the Gyro to Stick Rotation module's behaviour,
  fed JSM's own horizontal gyro rate, so `GYRO_SENS` (degrees of stick per degree of
  pad), `GYRO_SPACE` (`LOCAL` = yaw, `WORLD_TURN` = about gravity), smoothing, cutoff
  and `GYRO_OFF` all apply. `GYRO_STICK_ROTATION_SMOOTH_TIME` (seconds) stabilises the
  thumb's direction; `GYRO_STICK_ROTATION_MODE = ABSOLUTE | RELATIVE` and
  `GYRO_STICK_ROTATION_RETURN_TIME` (seconds, default 0.25) are the module's
  Absolute/Relative and Return. A deadzone of 0 never drops the turn. It turns whatever that stick is about to send — passed through,
  or routed by a stick mode — and, like any non-mouse `GYRO_OUTPUT`, silences the
  gyro mouse. The Tune panel shows the same live circle as the module (pinnable as
  `rotation`); tuning one of these settings lets the gyro and that stick through.
- **What it takes over:** only what it actually runs. A stick left in a mode a
  later phase owns, and a full pull the trigger mode never fires, keep passing
  through rather than going quiet for a binding that can't run; the line says why.
  `GYRO_OUTPUT = PS_MOTION` is the clearest case: it means "let the pad's own
  motion reach the pad", so the gyro pins are simply left unclaimed.

#### Audio Stream Haptics (ASTH)
- **ID:** `module.audio_stream_haptics`
- **Purpose:** Routes audio loopback to haptic feedback without HIDMaestro driver
- **Inputs:** 
  - Input 0: Source AutoMap bus (AutoMap type)
- **Outputs:** 
  - Output 0: Passthrough AutoMap bus
  - Outputs 1..7: Band energy + carrier frequency signals
- **Parameters:**
  - `_asth_dest_dev` — the pad (or Network Receive) it drives, stamped by the graph builder
  - `asth_modulator` ("Rumble mix", 0..1) — how the game's rumble shapes the
    audio: 0 gate (audio only while the game rumbles), 0.5 boost, 1 replace
    (pure audio)
- **Feedback:** always OVERRIDES the target's rumble — the game's rumble never
  plays alongside; it reaches the node through `feedback_game:` (the pad it
  drives, and the virtual pads fed from its own bus) and only acts through the
  modulator

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
