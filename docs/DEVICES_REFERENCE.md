# FlexInput Devices Reference

## Overview

FlexInput handles two categories of devices:
- **Physical devices** - Gamepads, MIDI controllers connected to the system
- **Virtual devices** - Emulated outputs (Xbox 360, DualShock 4, keyboard/mouse) created via HIDMaestro driver

The device system bridges raw hardware input to the signal graph through `device.source` nodes and routes processed signals back to hardware through `device.sink` nodes.

---

## Physical Device Backends (`crates/devices`)

### Backend Abstraction

```rust
pub trait DeviceBackend: Send {
    fn poll(&mut self) -> Vec<PhysicalDevice>;
    fn name(&self) -> &str;
}
```

Each backend implements this trait to provide device enumeration and signal polling.

### Supported Backends

#### 1. gilrs Backend (`gilrs_backend.rs`)

**Primary backend for game controllers.** Supports:
- Xbox 360/One (XInput)
- DualShock 4
- DualSense
- Switch Pro Controller
- Generic HID gamepads

**Device Identification:**
```rust
pub struct PhysicalDevice {
    pub id: String,                    // "gilrs:{family}:{instance}"
    pub name: String,
    pub vid: u16,                      // Vendor ID
    pub pid: u16,                      // Product ID
    pub kind: ControllerKind,          // XInput, DS4, DualSense, etc.
    pub is_connected: bool,
}

// crates/devices/src/identification.rs
pub enum ControllerKind {
    XInput,
    DualShock4,   // NOT "DS4"
    DualSense,
    SwitchPro,
    Generic,
    MidiIn,
    MidiOut,
}
// Detected via `ControllerKind::detect(name, vid, pid)` — VID/PID is authoritative
// when available, name-sniffing is the fallback.
```

**Signal Polling:**
- Called at `polling_hz` (configurable 500-4000 Hz)
- Returns `HashMap<(device_id, pin_id), Signal>` for all active pins
- Deadzone applied to analog sticks based on device calibration

#### 2. SDL3 Backend (`sdl_backend.rs`)

**Fallback for controllers with special features:**
- Gyroscope (DS4, DualSense, Switch Pro)
- Accelerometer
- Extra buttons (paddles, touchpad clicks)
- Adaptive triggers (DualSense)

**Why separate backend?**
- gilrs doesn't expose all SDL3 features consistently
- SDL3 provides direct access to motion sensors
- Filtered to avoid duplicates with gilrs devices

#### 3. MIDI Backend (`midi.rs`)

**MIDI input/output support:**
```rust
pub struct MidiBackend {
    inputs: Vec<MidiInputPort>,
    outputs: Vec<MidiOutputPort>,
    learning_devices: HashMap<String, bool>,  // device_id → is_learning
}

pub struct MidiInputPort {
    pub id: String,           // "midi_in:{port_index}"
    pub name: String,
    pub cc_learned: Option<u8>,  // Last learned CC number
}
```

**CC Learn Feature:**
- User enables learning on a `device.source` node
- Press any MIDI knob/fader → records CC number to node params
- Output pin named `cc_{number}` appears in the snarl

---

## Device Layouts (`layouts.rs`)

### Canonical Pin Definitions

Each controller family exposes a standardized set of pins via `standard_outputs()`:

```rust
pub fn standard_outputs(kind: &ControllerKind) -> Vec<SourcePin> {
    match kind {
        ControllerKind::XInput => vec![
            SourcePin { id: "left_stick",    type: SignalType::Vec2 },
            SourcePin { id: "right_stick",   type: SignalType::Vec2 },
            SourcePin { id: "dpad",          type: SignalType::Vec2 },
            SourcePin { id: "left_trigger",  type: SignalType::Float },
            SourcePin { id: "right_trigger", type: SignalType::Float },
            SourcePin { id: "btn_south",     type: SignalType::Bool },
            // ... all standard buttons
        ],
        ControllerKind::DS4 => vec![
            // DS4-specific pins (touchpad, lightbar feedback)
        ],
        // ... etc
    }
}
```

### Haptic Input Pins

Devices that accept feedback signals expose haptic inputs:

```rust
pub fn haptic_inputs(kind: &ControllerKind) -> Vec<FeedbackInlet> {
    match kind {
        ControllerKind::XInput => vec![
            FeedbackInlet { id: "rumble_strong", type: SignalType::Float },
            FeedbackInlet { id: "rumble_weak",   type: SignalType::Float },
        ],
        ControllerKind::DualSense => vec![
            FeedbackInlet { id: "rumble_strong",  type: SignalType::Float },
            FeedbackInlet { id: "rumble_weak",    type: SignalType::Float },
            FeedbackInlet { id: "hd_l_amp",       type: SignalType::Float },
            FeedbackInlet { id: "hd_r_amp",       type: SignalType::Float },
            FeedbackInlet { id: "trigger_l_mode", type: SignalType::Float },
            // ... adaptive trigger controls
        ],
    }
}
```

---

## Virtual Devices (`crates/virtual`)

### VirtualDevice Trait

```rust
// crates/virtual/src/lib.rs
pub trait VirtualDevice: Send {
    fn id(&self) -> &str;
    fn display_name(&self) -> &str;
    fn sink_pins(&self) -> &'static [SinkPin];      // ordered input pin layout
    fn send(&mut self, pin: &str, value: Signal);   // ONE pin at a time…
    fn flush(&mut self);                            // …then commit (submit HID report)
    fn reset_outputs(&mut self) {}                  // zero + flush (bypass)
    fn is_connected(&self) -> bool { true }
    fn output_pins(&self) -> &'static [FeedbackOutlet] { &[] }  // feedback back into graph
    // …persistence is handled by the HIDMaestro helper, not a trait method…
}
```

> There is no `DeviceKind { XInput, DS4, DualSense, KeyMouse }` enum on the trait, and
> `send` does NOT take a whole `HashMap` — the I/O thread calls `send(pin, value)` per
> pin, then `flush()`. Device kind is a `ControllerKind` (above) / the device id prefix,
> not a separate virtual enum.

### Virtual Device Implementations

#### XInput Virtual Pad (`hidmaestro_device.rs`)

Emulates Xbox 360 controller via HIDMaestro:
- Creates virtual XUSB device node
- Maps signals to XInput report structure
- Supports rumble feedback (strong/weak motors)

**Signal Mapping:** each `send(pin, value)` writes one field of an internal report
struct (stick axes → i16, triggers → u8, buttons → bit flags, rumble → motor bytes);
`flush()` then submits the assembled HID report to the driver. Conceptually:

```rust
fn send(&mut self, pin: &str, value: Signal) {
    match pin {
        "left_stick_x"  => self.report.left_x  = to_i16(value),
        "left_trigger"  => self.report.lt      = to_u8(value),
        "btn_south"     => self.report.set_button(BTN_A, value.as_bool()),
        // …rumble_strong/weak come back via output_pins()…
        _ => {}
    }
}
fn flush(&mut self) { self.device.send_report(&self.report); }
```

#### DualShock 4 / DualSense Virtual Pads

Similar to XInput but with DS-specific report structures:
- Touchpad coordinates
- Light bar RGB control
- HD rumble (DualSense)
- Adaptive trigger resistance (DualSense)

#### Virtual Keyboard/Mouse (`keymouse_hm.rs`)

Emulates KB/M via HIDMaestro:
- Maps AutoMap pins to keycodes/mouse deltas
- Supports scroll wheel
- Configurable mouse sensitivity multiplier

**Pin Mapping:**
```rust
const KEYMOUSE_DEFAULT_PINS: &[&str] = &[
    "key_escape", "key_shift", "key_ctrl", "key_alt", "key_win",
    "mouse_left", "mouse_right", "mouse_middle",
    "mouse_back", "mouse_forward",
    "scroll_up", "scroll_down", "scroll_left", "scroll_right",
    "scroll_y", "scroll_x",  // Analog scroll rate
    "mouse", "mouse_x", "mouse_y",  // Mouse delta
    "mouse_move", "mouse_move_x", "mouse_move_y",  // Absolute move
];
```

---

## HIDMaestro Integration (`crates/hidmaestro`)

### Architecture

HIDMaestro is a pure-Rust UMDF2 (User-Mode Driver Framework 2) client that creates virtual HID devices without requiring ViGEmBus installation.

**Components:**
1. **Helper binary** - Elevated process that installs drivers and manages device lifecycle
2. **Main app** - Communicates with helper via named pipes / shared memory
3. **Device nodes** - Virtual HID devices created in helper, exposed to OS

### Helper Communication (`helper_ipc.rs`)

The real IPC enum is `Request` (with a matching `Response`) in
`crates/hidmaestro/src/helper_ipc.rs` — there is no `HelperMessage`/`DeviceKind`; the
device kind is carried inside `profile_json`, and the helper allocates the controller
index itself:

```rust
pub enum Request {
    Hello { parent_pid: u32, persist: bool },   // first msg: watch parent, set persistence
    Ping,
    EnsureDriver,                               // install driver if missing (idempotent)
    ReinstallDriver,                            // clean reinstall
    UninstallDriver,
    Create { device_id: String, profile_json: String, index_hint: u32, /* +poll_interval_ms */ },
    Destroy { instance_id: String },
    ListDevices,                                // reclaim-on-startup enumeration
    HidHideApply { /* whitelist, blacklist, active */ },
    // …
}
```

### Device Creation Flow

1. **App** calls `helper::create_device(kind, id)`
2. **Helper** loads UMDF2 driver DLL
3. **Driver** creates virtual HID device node in OS
4. **Helper** returns success/failure to app
5. **App** can now send signals to the virtual device

### Persistence Mode

When `persist_virtual_devices` is enabled:
- Helper keeps devices alive after app exits
- Devices are reclaimed on next app launch
- Prevents "orphaned" virtual controllers

**Implementation:**
```rust
// In helper.rs
pub fn set_persist(persist: bool) {
    // Send message to helper process via IPC
    // Helper updates its persistence flag and adjusts cleanup behavior
}
```

### Rumble Feedback Shaping

Virtual pads apply perceptual shaping to rumble signals before sending to hardware:

```rust
fn shape_rumble(signal: Signal, floor: f32, max: f32, exp: f32) -> Signal {
    let value = signal.as_float();
    if value < floor {
        Signal::Float(0.0)  // Below threshold, silent
    } else {
        // Remap from [floor, 1.0] to [0, max] with power curve
        let remapped = ((value - floor) / (1.0 - floor)).powf(exp) * max;
        Signal::Float(remapped.clamp(0.0, max))
    }
}
```

**Why shape?**
- Classic rumble signals are often weak (0.1-0.3)
- HD voice-coil actuators have perceptual thresholds
- Shaping ensures faint game rumble is still felt on Switch Pro / DualSense

---

## Device Calibration (`calibration.rs`)

### Analog Stick Deadzone Calibration

Users can calibrate deadzones per device:

```rust
pub struct CalibrationData {
    pub center_x: f32,     // Resting X position
    pub center_y: f32,     // Resting Y position
    pub deadzone_radius: f32,  // Minimum deflection to register
}
```

**Calibration Window:**
- Shows live stick position as dot on 2D plane
- User moves stick through full range
- App computes center and deadzone from sampled data
- Stores in `AppSettings::device_calibrations` map

### Gyroscope Multiplier

Per-device gyro sensitivity scaling:
```rust
pub struct GyroCalibration {
    pub multiplier: f32,     // 0.5 to 2.0 (1.0 = native)
    pub offset_x: f32,       // Bias subtraction
    pub offset_y: f32,
    pub offset_z: f32,
}
```

Applied in `preprocess_dev_sigs()` before graph evaluation.

---

## Device Polling Thread (`spawn_io_thread`)

### Responsibilities

1. **Poll physical devices** at `polling_hz`
2. **Collect MIDI input** if MIDI backend active
3. **Send virtual device outputs** from processing thread results
4. **Manage device lifecycle** (create/destroy via worker)

### Thread Structure

```rust
pub fn spawn_io_thread(
    backends: Vec<Box<dyn DeviceBackend>>,
    midi_backend: Arc<Mutex<Option<MidiBackend>>>,
    proc_device_signals: ArcSignals,  // Write to UI
    sink_bus: SinkBus,               // Read from processing
    shared_virtual_devices: SharedDevicePool,
    active_tab_device_ids: Arc<RwLock<HashSet<String>>>,
    io_bypass: Arc<AtomicBool>,
    ui_nav_suppress: Arc<AtomicBool>,
    // ... more params
) {
    std::thread::spawn(move || {
        loop {
            let start = Instant::now();
            
            // 1. Poll physical devices
            for backend in &mut backends {
                let devices = backend.poll();
                for dev in devices {
                    for (pin_id, signal) in dev.signals {
                        proc_device_signals.write(|map| {
                            map.insert((dev.id.clone(), pin_id), signal);
                        });
                    }
                }
            }
            
            // 2. Send virtual device outputs
            if !*io_bypass.load() && !*ui_nav_suppress.load() {
                for dev in shared_virtual_devices.lock().unwrap().iter_mut() {
                    if active_tab_device_ids.read().unwrap().contains(dev.id()) {
                        let signals = sink_bus.read().unwrap()
                            .get(&(dev.id().to_string(), "*"))
                            .cloned()
                            .unwrap_or_default();
                        dev.send(&signals);
                    } else {
                        dev.reset_outputs();
                    }
                }
            }
            
            // 3. Sleep until next tick
            let elapsed = start.elapsed();
            let period = std::time::Duration::from_secs_f64(1.0 / polling_hz as f64);
            if elapsed < period {
                thread::sleep(period - elapsed);
            }
        }
    });
}
```

### Bypass Modes

**Manual bypass:** User toggles tab bypass button → `io_bypass = true`
- All virtual devices receive `reset_outputs()` (zero signals)
- Physical devices still polled (for display purposes)

**Auto-bypass:** Tab bound to process not in foreground → `auto_bypass = true`
- Same as manual bypass but triggered by foreground detection

**Panic mode:** Global shortcut engaged → `panic_active = true`
- Forces bypass on active tab only
- Other tabs unaffected

**UI navigation suppress:** Gamepad driving FlexInput UI → `ui_nav_suppress = true`
- Mapped output silenced so game doesn't receive nav inputs
- Raw input still published for live graph updates

---

## Device Rate Monitoring (`device_rates`)

Per-device measured polling rates displayed in canvas:

```rust
pub struct DeviceRates {
    rates: HashMap<String, u32>,  // device_id → actual Hz
}
```

**Measurement method:**
- I/O thread timestamps each poll iteration
- UI reads rates every few seconds (not every frame)
- Smoothed with exponential moving average to avoid flicker

Used by `device_rates` parameter in canvas viewer to show live feedback.

---

## Ping Request System

Users can "ping" a physical device to verify it's the correct one:

```rust
pub struct PingRequests {
    pending: Vec<String>,  // Device IDs to ping
}

// In I/O thread:
for dev_id in ping_requests.lock().unwrap().drain(..) {
    if let Some(dev) = shared_virtual_devices.lock().unwrap()
        .iter_mut()
        .find(|d| d.id() == dev_id) 
    {
        // Send 200ms rumble pulse
        dev.send_rumble_pulse(0.5, 200);
    }
}
```

Triggered by clicking device icon in virtual devices panel.

---

## Spike Filter Settings

Per-device analog signal filtering to remove noise spikes:

```rust
pub struct SpikeFilterSettings {
    enabled: bool,
    sensitivity: f32,  // 0..100 (higher = more aggressive)
}
```

Applied in I/O thread after polling, before signals reach processing thread.

**Algorithm:**
- Compare current sample to previous sample
- If delta exceeds threshold (based on sensitivity), replace with previous value
- Smooths jittery analog sticks without affecting legitimate fast movements

---

## Key Files Summary

| File | Purpose | Lines |
|------|---------|-------|
| `crates/devices/src/lib.rs` | Backend trait, initialization | ~100 |
| `crates/devices/src/gilrs_backend.rs` | gilrs gamepad polling | ~400 |
| `crates/devices/src/sdl_backend.rs` | SDL3 motion sensor access | ~300 |
| `crates/devices/src/midi.rs` | MIDI input/output handling | ~250 |
| `crates/devices/src/layouts.rs` | Pin definitions per controller kind | ~500 |
| `crates/devices/src/hidhide.rs` | HidHide client wrapper | ~150 |
| `crates/virtual/src/lib.rs` | VirtualDevice trait | ~50 |
| `crates/virtual/src/hidmaestro_device.rs` | XInput/DS4/DualSense impls | ~600 |
| `crates/virtual/src/keymouse_hm.rs` | Keyboard/mouse emulation | ~300 |
| `crates/virtual/src/layouts.rs` | Virtual device pin layouts | ~200 |
| `crates/hidmaestro/src/lib.rs` | HIDMaestro driver client | ~100 |
| `crates/hidmaestro/src/helper_ipc.rs` | Helper process communication | ~200 |
| `crates/hidmaestro/src/deploy.rs` | Driver installation logic | ~300 |

---

## Common Pitfalls & Gotchas

### 1. Device ID Format Consistency

Device IDs must match exactly across all systems:
- I/O thread writes to `proc_device_signals` using `{backend}:{family}:{instance}`
- Processing graph reads from same format in `dev_sigs`
- Virtual devices use `virtual.{kind}.{id}` format

**Mismatch symptom:** Signals appear in I/O but not in processing (or vice versa)

### 2. Thread Safety for Shared State

`Arc<RwLock<T>>` and `Arc<Mutex<T>>` are used extensively:
- **RwLock** for read-heavy data (device lists, settings)
- **Mutex** for write-heavy or complex invariants (virtual device pool)

**Never hold locks across await points or long computations.**

### 3. Polling Rate vs Sample Rate

- **Polling rate** (I/O thread): How often devices are read (500-4000 Hz)
- **Sample rate** (Processing thread): How often graph is evaluated (500-2000 Hz)

If polling > sample rate, multiple polls accumulate between evaluations. The engine handles this via catchup ticks.

### 4. Virtual Device Lifecycle

Devices persist across tab switches but NOT across app restarts (unless `persist_virtual_devices` enabled). Always check `is_connected()` before sending signals.

### 5. HIDMaestro Helper Elevation

Helper binary must run elevated (Administrator) to install UMDF2 drivers. The app self-re-execs with elevation on first launch or when driver is missing.

**User-facing symptom:** "HIDMaestro driver not installed" error in settings.

### 6. Rumble Feedback Direction

Feedback signals flow **backward** along AutoMap wires:
- Virtual pad rumble → physical pad haptic inputs
- Network receive → local pad haptics
- ASTH output → physical pad HD rumble

This is handled automatically by `resolve_feedback_pin()` in automap.rs.

### 7. Changing a virtual device's HID report descriptor requires a PID bump

The HIDMaestro helper reclaims persisted virtual devices by `device_id` and never
re-applies the report descriptor to an existing node. If you change a virtual device's
HID report descriptor (e.g. widen a mouse report to add a scroll field), you MUST also
bump that profile's PID — otherwise the stale-node guard keeps the OLD device and the
driver desyncs from the new report layout (this once broke LMB/RMB after a mouse report
change). Bumping the PID forces destroy+recreate. See DEVELOPMENT_GUIDELINES.md →
*"HIDMaestro … requires a PID bump"*.

---

## References

- Device trait: `crates/devices/src/lib.rs`
- Virtual device trait: `crates/virtual/src/lib.rs`
- HIDMaestro client: `crates/hidmaestro/src/lib.rs`
- Layout definitions: `crates/devices/src/layouts.rs`, `crates/virtual/src/layouts.rs`
- I/O thread spawn: `crates/ui/src/app/threads.rs`
