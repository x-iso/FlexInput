# FlexInput AutoMap System Deep Dive

## Overview

The AutoMap system is the backbone of FlexInput's cross-device compatibility. It provides a unified signal vocabulary that allows any physical controller to map to any virtual output device, regardless of their native button layouts or feature sets. The system handles three critical concerns:

1. **Canonical pin definitions** - Standardized signal names across all devices
2. **Cross-family mapping** - Automatic translation between different controller button layouts
3. **Bidirectional feedback** - Haptic signals (rumble, lightbar) flow backward along AutoMap wires

---

## Canonical Pin Definitions (`ALL_PINS`)

### Signal Vocabulary

The `ALL_PINS` constant in `crates/core/src/automap.rs` defines every signal that can flow through an AutoMap wire:

```rust
pub const ALL_PINS: &[AutoMapPin] = &[
    // ── Bundled vectors (stick, D-pad) ────────────────────────────────
    AutoMapPin { id: "left_stick",    display_name: "Left Stick",           signal_type: SignalType::Vec2 },
    AutoMapPin { id: "right_stick",   display_name: "Right Stick",          signal_type: SignalType::Vec2 },
    AutoMapPin { id: "dpad",          display_name: "D-Pad",                signal_type: SignalType::Vec2 },
    
    // ── Individual axes (decomposed from vectors) ───────────────────────
    AutoMapPin { id: "left_stick_x",  display_name: "L.Stick X",            signal_type: SignalType::Float },
    AutoMapPin { id: "left_stick_y",  display_name: "L.Stick Y",            signal_type: SignalType::Float },
    AutoMapPin { id: "right_stick_x", display_name: "R.Stick X",            signal_type: SignalType::Float },
    AutoMapPin { id: "right_stick_y", display_name: "R.Stick Y",            signal_type: SignalType::Float },
    AutoMapPin { id: "dpad_x",        display_name: "D-Pad X",              signal_type: SignalType::Float },
    AutoMapPin { id: "dpad_y",        display_name: "D-Pad Y",              signal_type: SignalType::Float },
    
    // ── Triggers (analog) ─────────────────────────────────────────────
    AutoMapPin { id: "left_trigger",  display_name: "Left Trigger (analog)", signal_type: SignalType::Float },
    AutoMapPin { id: "right_trigger", display_name: "Right Trigger (analog)",signal_type: SignalType::Float },
    
    // ── Gyro / IMU ────────────────────────────────────────────────────
    AutoMapPin { id: "gyro_x",        display_name: "Gyro X (roll)",        signal_type: SignalType::Float },
    AutoMapPin { id: "gyro_y",        display_name: "Gyro Y (pitch)",       signal_type: SignalType::Float },
    AutoMapPin { id: "gyro_z",        display_name: "Gyro Z (yaw)",         signal_type: SignalType::Float },
    AutoMapPin { id: "accel_x",       display_name: "Accel X",              signal_type: SignalType::Float },
    AutoMapPin { id: "accel_y",       display_name: "Accel Y",              signal_type: SignalType::Float },
    AutoMapPin { id: "accel_z",       display_name: "Accel Z",              signal_type: SignalType::Float },
    
    // ── Standard buttons (positional naming) ──────────────────────────
    AutoMapPin { id: "btn_south",     display_name: "South",                signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_east",      display_name: "East",                 signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_west",      display_name: "West",                 signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_north",     display_name: "North",                signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_lb",        display_name: "LB",                   signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_rb",        display_name: "RB",                   signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_lt_dig",    display_name: "LT (dig)",             signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_rt_dig",    display_name: "RT (dig)",             signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_ls",        display_name: "L.Stick Click",        signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_rs",        display_name: "R.Stick Click",        signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_start",     display_name: "Start",                signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_back",      display_name: "Back",                 signal_type: SignalType::Bool },
    AutoMapPin { id: "btn_guide",     display_name: "Guide",                signal_type: SignalType::Bool },
    
    // ── Touchpad ──────────────────────────────────────────────────────
    AutoMapPin { id: "btn_touchpad",  display_name: "Touchpad Click",       signal_type: SignalType::Bool },
    AutoMapPin { id: "touch1_x",      display_name: "Touch 1 X",            signal_type: SignalType::Float },
    AutoMapPin { id: "touch1_y",      display_name: "Touch 1 Y",            signal_type: SignalType::Float },
    AutoMapPin { id: "touch1_active", display_name: "Touch 1 Active",       signal_type: SignalType::Bool },
    
    // ── Extra buttons (paddles, misc) ─────────────────────────────────
    AutoMapPin { id: "btn_paddle_l1", display_name: "Paddle L1",            signal_type: SignalType::Bool },
    // ... more paddles and misc buttons
    
    // ── Virtual KB/M keys ─────────────────────────────────────────────
    AutoMapPin { id: "key_escape",    display_name: "Key: Escape",          signal_type: SignalType::Bool },
    AutoMapPin { id: "mouse_left",    display_name: "Mouse: LMB",           signal_type: SignalType::Bool },
    // ... more keys and mouse buttons
    
    // ── Mouse movement ────────────────────────────────────────────────
    AutoMapPin { id: "mouse",         display_name: "Mouse: XY (delta)",    signal_type: SignalType::Vec2 },
    AutoMapPin { id: "mouse_move",    display_name: "Mouse: XY (move)",     signal_type: SignalType::Vec2 },
];
```

### Design Principles

**Positional naming over vendor-specific:**
- `btn_south` instead of `btn_a` or `btn_cross`
- `left_stick_x` instead of `l_stick_x` or `ls_x`
- This allows cross-family mapping without per-vendor pin lists

**Bundled vectors + individual axes:**
- Both `left_stick` (Vec2) and `left_stick_x`/`left_stick_y` (Float) are available
- Sink devices choose which representation they prefer
- Conflict resolution: directly-wired axes beat auto-mapped Vec2

**Digital/analog trigger duality:**
- `btn_lt_dig` / `btn_rt_dig` for digital-only triggers (Switch Pro ZL/ZR)
- `left_trigger` / `right_trigger` for analog triggers
- Bridge logic ensures digital presses reach analog destinations

---

## Cross-Family Mapping (`resolve_mapping`)

### The Problem

Different controllers use different button names for the same physical position:

| Position | XInput    | DS4        | DualSense  | Switch Pro |
|----------|-----------|------------|------------|------------|
| Bottom   | A         | Cross      | Cross      | B          |
| Right    | B         | Circle     | Circle     | A          |
| Left     | X         | Square     | Square     | Y          |
| Top      | Y         | Triangle   | Triangle   | X          |

### The Solution: Three-Pass Algorithm

```rust
pub fn resolve_mapping<'a>(src_pins: &[&'a str], dst_pins: &[&'a str]) -> Vec<(&'a str, &'a str)> {
    let mut result = Vec::new();
    let mut claimed_dst = HashSet::new();

    // Pass 1: Direct ID matches across ALL sources first.
    for &src_id in src_pins {
        if let Some(&dst_id) = dst_pins.iter().find(|&&d| d == src_id) {
            if claimed_dst.insert(dst_id) {
                result.push((src_id, dst_id));
            }
        }
    }

    // Pass 2: Semantic group fan-out for destinations a direct match didn't claim.
    for &src_id in src_pins {
        if let Some(group) = SEMANTIC_GROUPS.iter().find(|g| g.contains(&src_id)).copied() {
            for &group_id in group {
                if group_id == src_id { continue; }
                if let Some(&dst_id) = dst_pins.iter().find(|&&d| d == group_id) {
                    if claimed_dst.insert(dst_id) {
                        result.push((src_id, dst_id));
                    }
                }
            }
        }
    }

    // Pass 3: Digital↔analog trigger bridge (special case).
    for &(dig, analog) in &[("btn_lt_dig", "left_trigger"), ("btn_rt_dig", "right_trigger")] {
        let has_dig_src = src_pins.contains(&dig);
        let has_analog_dst = dst_pins.contains(&analog);
        if has_dig_src && has_analog_dst && !result.iter().any(|&(s, d)| s == dig && d == analog) {
            result.push((dig, analog));
        }
    }

    result
}
```

**Pass 1 - Direct ID match:**
- Same pin name on both devices → direct mapping
- Example: `btn_south` on source maps to `btn_south` on destination
- This handles the common case where both devices use positional naming

**Pass 2 - Semantic group fan-out:**
- Unique buttons that share purpose across families
- Example: `btn_capture` (PS5) ↔ `btn_mute` (Xbox) bridge
- Only fires for destinations not already claimed by Pass 1

**Pass 3 - Digital/analog trigger bridge:**
- Special case: digital trigger press should also drive analog trigger destination
- Critical for Switch Pro / DualSense where ZL/ZR are digital-only
- Emitted in addition to (not instead of) the direct mapping

### Semantic Groups

```rust
const SEMANTIC_GROUPS: &[&[&str]] = &[
    // Device-unique special buttons that share purpose across families.
    &["btn_capture", "btn_mute"],
];
```

Only truly unique cross-device aliases stay here. Most cross-family connections work via direct ID match because all devices use positional naming (`btn_south`, `btn_east`, etc.).

### Family-Specific Labels (`family_label`)

For UI display purposes, the same AutoMap pin can show different labels depending on the connected device:

```rust
pub fn family_label(pin_id: &str, family_slug: Option<&str>) -> Option<&'static str> {
    let fam = family_slug?;
    let (xi, ps, sw) = match pin_id {
        "btn_south"  => ("A", "Cross", "B"),
        "btn_east"   => ("B", "Circle", "A"),
        "btn_west"   => ("X", "Square", "Y"),
        "btn_north"  => ("Y", "Triangle", "X"),
        // ... more button families
    };
    Some(match fam {
        "xinput"       => xi,
        "ds4" | "dualsense" => ps,
        "switch_pro"   => sw,
        _              => return None,
    })
}
```

Used by AutoMap Splitter body to swap "South (B/Cross/X)" for the single label that matches the actually-connected upstream device.

---

## Feedback System (`FEEDBACK_PAIRS`)

### The Problem

Haptic feedback signals (rumble, lightbar) need to flow **backward** from virtual output devices to physical input devices. This is needed for:
- Game rumble on virtual pad → felt on physical pad
- Light bar color from virtual DS4 → shown on physical DualSense
- Adaptive trigger resistance from virtual pad → applied to physical triggers

### The Solution: Feedback Pin Pairs

```rust
pub const FEEDBACK_PAIRS: &[(&str, &[&str])] = &[
    ("rumble_strong", &["rumble_strong", "hd_l_amp", "hd_rumble_l"]),
    ("rumble_weak",   &["rumble_weak",   "hd_r_amp", "hd_rumble_r"]),
    ("hd2_l_amp",     &["hd2_l_amp"]),
    ("hd2_l_freq",    &["hd2_l_freq"]),
    ("hd2_r_amp",     &["hd2_r_amp"]),
    ("hd2_r_freq",    &["hd2_r_freq"]),
    ("lightbar_r",    &["lightbar_r"]),
    ("lightbar_g",    &["lightbar_g"]),
    ("lightbar_b",    &["lightbar_b"]),
    ("player_led",    &["player_led"]),
    ("mic_led",       &["mic_led"]),
    // DualSense adaptive triggers
    ("trigger_r_mode",     &["trigger_r_mode"]),
    ("trigger_r_start",    &["trigger_r_start"]),
    ("trigger_r_end",      &["trigger_r_end"]),
    ("trigger_r_strength", &["trigger_r_strength"]),
    ("trigger_r_freq",     &["trigger_r_freq"]),
    // ... more trigger pins
];
```

**Structure:** `(virtual_output_pin, &[matching_physical_input_pins])`

The engine looks up the virtual sink's output signal under `virtual_output_pin`, and if a matching physical haptic input pin exists on the source device, routes the value there.

### Resolution Function

```rust
pub fn resolve_feedback_pin<'a>(
    virtual_out_pin: &str,
    physical_input_pins: &[&'a str],
) -> Option<&'a str> {
    let entry = FEEDBACK_PAIRS.iter().find(|(src, _)| *src == virtual_out_pin)?;
    for &candidate in entry.1 {
        if let Some(&p) = physical_input_pins.iter().find(|&&p| p == candidate) {
            return Some(p);
        }
    }
    None
}
```

### Feedback Inlet Union (`FEEDBACK_INLET_PINS`)

The complete set of all haptic input ports across every controller family:

```rust
pub const FEEDBACK_INLET_PINS: &[AutoMapPin] = &[
    // Classic rumble (every pad)
    AutoMapPin { id: "rumble_strong", display_name: "Rumble (strong)", signal_type: SignalType::Float },
    AutoMapPin { id: "rumble_weak",   display_name: "Rumble (weak)",   signal_type: SignalType::Float },
    
    // Light bar (DS4 / DualSense)
    AutoMapPin { id: "lightbar_r",    display_name: "Light Bar R",     signal_type: SignalType::Float },
    AutoMapPin { id: "lightbar_g",    display_name: "Light Bar G",     signal_type: SignalType::Float },
    AutoMapPin { id: "lightbar_b",    display_name: "Light Bar B",     signal_type: SignalType::Float },
    
    // HD rumble carrier 1 (LF) - per-side amp + carrier freq
    AutoMapPin { id: "hd_l_amp",      display_name: "HD Rumble L: Amp", signal_type: SignalType::Float },
    AutoMapPin { id: "hd_l_freq",     display_name: "HD Rumble L: Freq",signal_type: SignalType::Float },
    AutoMapPin { id: "hd_r_amp",      display_name: "HD Rumble R: Amp", signal_type: SignalType::Float },
    AutoMapPin { id: "hd_r_freq",     display_name: "HD Rumble R: Freq",signal_type: SignalType::Float },
    
    // HD rumble carrier 2 (HF) - second simultaneous carrier
    AutoMapPin { id: "hd2_l_amp",     display_name: "HD Rumble L: Amp (HF)", signal_type: SignalType::Float },
    // ... more HF pins
    
    // DualSense HD haptics (USB only)
    AutoMapPin { id: "ds_l_amp",      display_name: "DS Haptic L: Amplitude", signal_type: SignalType::Float },
    
    // DualSense LEDs + adaptive triggers
    AutoMapPin { id: "player_led",    display_name: "Player LED", signal_type: SignalType::Float },
    AutoMapPin { id: "trigger_r_mode", display_name: "R.Trigger Mode", signal_type: SignalType::Float },
    // ... more trigger pins
];
```

**Keep in sync with `layouts.rs`** - guarded by test `feedback_inlet_union_covers_all_families`.

### Feedback Outlet Pins (`FEEDBACK_OUTLET_PINS`)

The subset of feedback signals that virtual source-pins actually publish via `poll_outputs`:

```rust
pub const FEEDBACK_OUTLET_PINS: &[AutoMapPin] = &[
    AutoMapPin { id: "rumble_strong", display_name: "Rumble (strong)", signal_type: SignalType::Float },
    AutoMapPin { id: "rumble_weak",   display_name: "Rumble (weak)",   signal_type: SignalType::Float },
    AutoMapPin { id: "lightbar_r",    display_name: "Light Bar R",     signal_type: SignalType::Float },
    AutoMapPin { id: "lightbar_g",    display_name: "Light Bar G",     signal_type: SignalType::Float },
    AutoMapPin { id: "lightbar_b",    display_name: "Light Bar B",     signal_type: SignalType::Float },
];
```

These are exposed as outlets by the Feedback Control module for user wiring.

---

## AutoMap Node Types

### AutoMap Splitter (`module.automap_split`)

Extracts individual pins from an AutoMap bus wire:

**Evaluation:**
```rust
"module.automap_split" => {
    let dev_id = snap.params.get("_automap_device_id")...;
    let collector_id = snap.params.get("_automap_collector_id")...;
    
    (0..snap.n_outputs).map(|i| {
        let pin_id = snap.output_pin_ids.get(i)...;
        if pin_id.is_empty() || pin_id == "automap_pass" { return None; }
        
        // Prefer collector's injected/overridden signals over raw device samples
        if !collector_id.is_empty() {
            if let Some(&sig) = collector_sigs.get(&(collector_id.to_string(), pin_id.to_string())) {
                return Some(sig);
            }
        }
        
        dev_sigs.get(&(dev_id.to_string(), pin_id.to_string())).copied()
    }).collect()
}
```

**Key design decision:** Collector overrides take priority over raw device signals. This ensures that when a Remapper or Touch Zones node has modified a signal, the Splitter reflects those modifications rather than the raw hardware value.

### AutoMap Collector (`module.automap_collect`)

Injects signals into an AutoMap bus for downstream routing:

**Two-phase write:**
1. **Pass-through from upstream source** - Copy all signals from upstream collector or raw device
2. **Explicit collected-pin overrides** - User-wired values win over pass-through

```rust
// Phase 1: Pass-through
if !upstream_collector.is_empty() {
    // Copy EVERY entry from upstream collector key
    let copies: Vec<(String, Signal)> = collector_sigs.iter()
        .filter(|((dev, _), _)| dev == &upstream_collector)
        .map(|((_, pin), sig)| (pin.clone(), *sig))
        .collect();
    for (pin, sig) in copies {
        collector_sigs.insert((uid_key.clone(), pin), sig);
    }
    // Fall back to raw device samples for canonical pins not on upstream collector
}

// Phase 2: Explicit overrides (win over pass-through)
for (i, pin_id) in collect_ids.iter().enumerate() {
    if let Some(sig) = inputs.get(i + 1).and_then(|s| *s) {
        if !pin_id.is_empty() {
            collector_sigs.insert((uid_key.clone(), pin_id.clone()), sig);
        }
    }
}
```

**Why iterate actual collector_sigs entries instead of ALL_PINS?**
- Off-spec pin names (custom keyboard keys, mouse buttons) also flow through
- ALL_PINS only covers canonical gamepad pins
- Remapper's mapped keys like `key_f` need to reach downstream sinks

### AutoMap Fork (`module.automap_fork`)

Duplicates an AutoMap bus to multiple outputs:
- Each output carries the full AutoMap signal set
- Used when the same bus needs to feed multiple downstream nodes

### AutoMap Selector (`module.automap_selector`)

Selects one of N AutoMap buses based on a selector input:
- Routes feedback signals backward along the gate chain
- Records `fb_routes` for post-pass reverse-feedback resolution

### AutoMap Combiner (`module.automap_combiner`)

Merges multiple AutoMap buses using configurable per-pin policy:
- **OR:** Bool = logical OR; Float = max(|x|) preserving sign of max
- **AND:** Bool = logical AND; Float = min(|x|) preserving sign of min
- **XOR:** Bool = parity; Float = |a - b| folded across all inputs
- **ADD:** Sum, clamped per pin (triggers [0,1], sticks/axes [-1,1])
- **MULT:** Product, clamped per pin

Default policy is SORT: walk inputs top-down (lowest port = highest priority); first asserted value wins.

---

## Sink AutoMap Resolution

When a `device.sink` node has an `automap_source`, the engine resolves mappings during evaluation:

```rust
// In eval_graph_tick, inside device.sink handling:
if let Some((ref src_dev, ref src_pins)) = st.automap_source {
    let dst_ids: Vec<&str> = st.pin_ids.iter()
        .filter(|pid| !pid.is_empty())
        .map(|pid| pid.as_str())
        .collect();
    
    let src_ids: Vec<&str> = src_pins.iter()
        .filter(|p| !p.is_empty() && p.as_str() != "automap_out")
        .map(|p| p.as_str())
        .collect();
    
    // Digital→analog trigger bridges are LOWEST-PRIORITY fallback
    let mut deferred_digital_triggers: Vec<(&str, &str)> = Vec::new();
    
    for (mapped_src, mapped_dst) in automap::resolve_mapping(&src_ids, &dst_ids) {
        if directly_wired.contains(mapped_dst) { continue; }
        
        let is_digital_trigger_bridge = matches!((mapped_src, mapped_dst),
            ("btn_lt_dig", "left_trigger") | ("btn_rt_dig", "right_trigger"));
        
        if is_digital_trigger_bridge {
            if st.digital_trigger_bridge {
                deferred_digital_triggers.push((mapped_src, mapped_dst));
            }
            continue;  // Process in second pass
        }
        
        if let Some(sig) = resolve_sig(mapped_src) {
            sink_outputs.entry((st.device_id.clone(), mapped_dst.to_string()))
                .or_insert(scale_for_sink(mapped_dst, sig));
        }
    }
    
    // Second pass: digital-trigger fallback
    for (mapped_src, mapped_dst) in deferred_digital_triggers {
        let key = (st.device_id.clone(), mapped_dst.to_string());
        if sink_outputs.contains_key(&key) { continue; }  // Primary already wrote it
        if let Some(sig) = resolve_sig(mapped_src) {
            let v = if sig.as_bool() { 1.0 } else { 0.0 };
            sink_outputs.insert(key, Signal::Float(v));
        }
    }
}
```

### Vec2 vs Axis Conflict Resolution

When both a Vec2 pin and its component axes are auto-mapped:

```rust
const STICK_GROUPS: &[(&str, &[&str])] = &[
    ("left_stick",  &["left_stick_x", "left_stick_y"]),
    ("right_stick", &["right_stick_x", "right_stick_y"]),
    ("dpad",        &["dpad_x", "dpad_y"]),
];

for &(vec2_pin, axis_pins) in STICK_GROUPS {
    let has_vec2     = sink_outputs.contains_key(&(st.device_id.clone(), vec2_pin.to_string()));
    let has_any_axis = axis_pins.iter().any(|p| sink_outputs.contains_key(&(st.device_id.clone(), p.to_string())));
    
    if !has_vec2 || !has_any_axis { continue; }
    
    let vec2_direct     = directly_wired.contains(vec2_pin);
    let any_axis_direct = axis_pins.iter().any(|p| directly_wired.contains(*p));
    
    if any_axis_direct && !vec2_direct {
        // Direct-wired axes beat auto-mapped Vec2
        sink_outputs.remove(&(st.device_id.clone(), vec2_pin.to_string()));
    } else {
        // Vec2 wins in all other cases (auto-mapped Vec2 beats auto-mapped axes)
        for &axis_pin in axis_pins {
            sink_outputs.remove(&(st.device_id.clone(), axis_pin.to_string()));
        }
    }
}
```

**Priority rules:**
1. Direct-wired axes > Auto-mapped Vec2
2. Auto-mapped Vec2 > Direct-wired axes (when Vec2 is also auto-mapped)
3. Both Vec2 and axes direct-wired: Vec2 wins (hardware registers conflict)

---

## Virtual Keyboard/Mouse Wildcard Pass-Through

When a collector feeds a `virtual.keymouse` sink, every collector-injected signal is forwarded as-is:

```rust
if is_collector && st.device_id.starts_with("virtual.keymouse") {
    for ((dev, pin), &sig) in collector_sigs.iter() {
        if dev != src_dev { continue; }
        if directly_wired.contains(pin.as_str()) { continue; }
        sink_outputs
            .entry((st.device_id.clone(), pin.clone()))
            .or_insert(scale_for_sink(pin, sig));
    }
}
```

**Why wildcard?** The KB/M sink's `learned_keys` fallback handles arbitrary key names. Users can drive any custom key (F1, Space, letters) by adding it to the Collector via the Learn-key UI.

---

## Mouse Sensitivity Scaling

Virtual KB/M sinks apply a configurable mouse sensitivity multiplier:

```rust
let mouse_sens = snap.params.get("mouse_sensitivity")
    .and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;

let scale_for_sink = |pin_id: &str, sig: Signal| -> Signal {
    if st.device_id.starts_with("virtual.keymouse") && is_mouse_pin(pin_id)
        && (mouse_sens - 1.0).abs() > f32::EPSILON
    {
        match sig {
            Signal::Float(v) => Signal::Float(v * mouse_sens),
            Signal::Vec2(v)  => Signal::Vec2(v * mouse_sens),
            other => other,
        }
    } else { sig }
};
```

---

## Testing AutoMap Logic

### Unit Tests in `automap.rs`

```rust
#[test]
fn digital_trigger_reaches_analog_destination() {
    let src = ["left_trigger", "right_trigger", "btn_lt_dig", "btn_rt_dig"];
    let dst = ["left_trigger", "right_trigger", "btn_lt_dig", "btn_rt_dig"];
    let m = resolve_mapping(&src, &dst);

    assert!(m.contains(&("btn_lt_dig", "left_trigger")), 
            "digital LT must reach analog LT: {m:?}");
    assert!(m.contains(&("btn_rt_dig", "right_trigger")), 
            "digital RT must reach analog RT: {m:?}");
    assert!(m.contains(&("left_trigger", "left_trigger")));
    assert!(m.contains(&("btn_lt_dig", "btn_lt_dig")));
}

#[test]
fn capture_mute_bridge_still_works() {
    let m = resolve_mapping(&["btn_capture"], &["btn_mute"]);
    assert_eq!(m, vec![("btn_capture", "btn_mute")]);
}

#[test]
fn no_duplicate_pairs() {
    let src = ["left_trigger", "btn_lt_dig"];
    let dst = ["left_trigger", "btn_lt_dig"];
    let mut m = resolve_mapping(&src, &dst);
    let n = m.len();
    m.sort();
    m.dedup();
    assert_eq!(m.len(), n, "duplicate pairs in resolve_mapping output");
}
```

---

## Integration with Other Systems

### Touch Zones Mapping Mode

Touch Zones in mapping mode publishes overrides under `touchmap:{uid}` keys (similar to Remapper's `remap:{uid}`):

```rust
// In eval_graph_tick:
if snap.module_id == "module.touch_zones" 
    && snap.params.get("zone_mode").and_then(|v| v.as_str()) == Some("mapping") 
{
    eval_touch_zones_map_node(snap, ns_uid, dev_sigs, &mut collector_sigs, state, dt);
    computed[idx] = vec![None];
    continue;
}
```

### Virtual Menu Source Block

When a Virtual Menu is open, specific physical input pins are suppressed from the game:

```rust
// In eval_graph_tick preprocessing:
let req: Vec<(String, String)> = state.get(&MACRO_CARRY_UID)
    .map(|s| s.source_block.iter().cloned().collect())
    .unwrap_or_default();

for key in &req {
    if let Some(&v) = dev_sigs_owned.get(key) {
        snap.insert(key.clone(), v);  // Save pre-block value
    }
    dev_sigs_owned.insert(key.clone(), pointer_block_off(&key.1));  // Zero the pin
}
```

### Macro Namespace

Mapping evaluators (Remapper, Touch Zones cards, 3DOF-Lean) publish signals into named macro namespaces:

```rust
pub const SIGS_NS: &str = "macro_sigs";       // Scalar macros
pub const SIGS_NS_VEC2: &str = "macro_sigs_vec2";  // Vec2 macros
pub const SRC_BLOCK_PREFIX: &str = "src_block:";    // Source block pins

// In macro_output evaluation:
let scalar = collector_sigs.get(&(SIGS_NS.to_string(), pin_id.to_string())).copied();
let vec2 = collector_sigs.get(&(SIGS_NS_VEC2.to_string(), pin_id.to_string())).copied();
```

---

## References

- ALL_PINS definition: `crates/core/src/automap.rs`
- resolve_mapping(): `crates/core/src/automap.rs`
- FEEDBACK_PAIRS: `crates/core/src/automap.rs`
- family_label(): `crates/core/src/automap.rs`
- Sink AutoMap resolution: `crates/engine/src/eval.rs` (lines 684-769)
- Vec2/axis conflict resolution: `crates/engine/src/eval.rs` (lines 771-791)
- KB/M wildcard pass-through: `crates/engine/src/eval.rs` (lines 755-768)
