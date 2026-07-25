# FlexInput Patch Formats Reference

## Overview

FlexInput uses three file formats for persistence:
- **`.fxp`** - Full patch files (graph, nodes, wires, parameters)
- **`.fxsp`** - Sub-patch preset files (reusable complex blocks)
- **`.fxc`** - Response curve files (shared curve definitions)

All formats use JSON serialization with serde. The engine maintains backward compatibility across versions via migration functions.

---

## Patch File Format (`.fxp`)

### Structure

```json
{
  "version": 1,
  "nodes": [
    {
      "id": "550e8400-e29b-41d4-a716-446655440000",
      "module_id": "math.add",
      "position": [100.0, 200.0],
      "params": {
        "value": 0.5
      }
    },
    {
      "id": "61a9f400-f39c-42e5-b827-557766551111",
      "module_id": "device.source",
      "position": [50.0, 100.0],
      "params": {
        "device_id": "gilrs:dualsense:0"
      }
    },
    {
      "id": "72b0e500-a4ad-53f6-c938-668877662222",
      "module_id": "subpatch",
      "position": [300.0, 150.0],
      "params": {},
      "subpatch": {
        "display_name": "Gyro Pre-processing",
        "pins_in": [
          {"name": "X", "signal_type": "Float"},
          {"name": "Y", "signal_type": "Float"}
        ],
        "pins_out": [
          {"name": "Out X", "signal_type": "Float"},
          {"name": "Out Y", "signal_type": "Float"}
        ],
        "patch": {
          "version": 1,
          "nodes": [...],
          "wires": [...]
        }
      }
    }
  ],
  "wires": [
    {
      "from_node": "550e8400-e29b-41d4-a716-446655440000",
      "from_pin": "A",
      "to_node": "61a9f400-f39c-42e5-b827-557766551111",
      "to_pin": "left_stick_x"
    }
  ]
}
```

### Node Instance Schema

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInstance {
    pub id: Uuid,                          // Unique identifier
    pub module_id: String,                 // Matches ModuleDescriptor::id
    pub position: [f32; 2],               // Canvas coordinates (x, y)
    
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub params: HashMap<String, serde_json::Value>,  // Configurable parameters
    
    /// Inline sub-patch definition (only for module_id == "subpatch")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subpatch: Option<Box<SubPatch>>,
}
```

### Wire Schema

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wire {
    pub from_node: Uuid,
    pub from_pin: String,       // Pin name (e.g., "A", "left_stick_x")
    pub to_node: Uuid,
    pub to_pin: String,         // Pin name on destination node
}
```

### Special Node Types

#### device.source

Physical input device node:
```json
{
  "id": "...",
  "module_id": "device.source",
  "position": [50.0, 100.0],
  "params": {
    "device_id": "gilrs:dualsense:0"
  }
}
```

**Parameters:**
- `device_id` - Device identifier (format: `{backend}:{family}:{instance}`)
- `deadzone` - Stick deadzone radius (optional, defaults to settings)
- `gyro_multiplier` - Gyro sensitivity scale (optional)

#### device.sink

Virtual output device node:
```json
{
  "id": "...",
  "module_id": "device.sink",
  "position": [400.0, 200.0],
  "params": {
    "device_id": "virtual.hm.xinput.0"
  }
}
```

**Parameters:**
- `device_id` - Virtual device identifier (format: `virtual.{kind}.{id}`)
- `deadzone` - Output stick deadzone (optional)
- `mouse_sensitivity` - KB/M mouse sensitivity multiplier (optional)
- `rumble_floor`, `rumble_max`, `rumble_exp` - Rumble shaping parameters

#### subpatch

Inline sub-patch definition:
```json
{
  "id": "...",
  "module_id": "subpatch",
  "position": [300.0, 150.0],
  "params": {},
  "subpatch": {
    "display_name": "My Sub-patch",
    "pins_in": [
      {"name": "Input A", "signal_type": "Float"},
      {"name": "Input B", "signal_type": "Vec2"}
    ],
    "pins_out": [
      {"name": "Output X", "signal_type": "Float"}
    ],
    "patch": {
      "version": 1,
      "nodes": [...],
      "wires": [...]
    }
  }
}
```

### Migration Functions

Loaded patches are migrated to current schema via `migrate_loaded_snarl()`:

```rust
pub fn migrate_loaded_snarl(snarl: &mut Snarl<NodeData>) {
    for (_, node) in snarl.nodes_ids_data_mut() {
        // Map Action gained second output (out_analog)
        if node.value.module_id == "module.map_action" 
            && node.value.outputs.len() < 2 
        {
            node.value.outputs.push(PinDescriptor::new("Analog", SignalType::Float));
        }
        
        // Audio Stream Haptics gained raw-analysis outputs
        if node.value.module_id == "module.audio_stream_haptics" 
            && node.value.outputs.len() < 7 
        {
            let want = [
                ("AutoMap", SignalType::AutoMap),
                ("LF EF L", SignalType::Float),
                ("HF EF L", SignalType::Float),
                // ... more outputs
            ];
            node.value.outputs = want.iter()
                .map(|(name, ty)| PinDescriptor::new(*name, *ty))
                .collect();
        }
        
        // ViGEm → HIDMaestro device ID migration
        if matches!(node.value.module_id.as_str(), "device.sink" | "device.source") {
            if let Some(new_id) = node.value.params.get("device_id")
                .and_then(|v| v.as_str())
                .and_then(migrate_vigem_device_id) 
            {
                node.value.params.insert("device_id".into(), Value::from(new_id));
            }
        }
        
        // Backfill legacy rumble defaults for old virtual pads
        if node.value.module_id == "device.sink" {
            let dev = node.value.params.get("device_id")
                .and_then(|v| v.as_str()).unwrap_or("");
            if dev.starts_with("virtual.") && !dev.starts_with("virtual.keymouse")
                && !node.value.params.contains_key("rumble_floor") 
            {
                node.value.params.insert("rumble_floor".into(), Value::from(0.35));
                node.value.params.insert("rumble_max".into(), Value::from(1.0));
                node.value.params.insert("rumble_exp".into(), Value::from(0.6));
            }
        }
        
        // Recurse into nested sub-patches
        if let Some(sp) = node.value.subpatch.as_mut() {
            migrate_loaded_snarl(&mut sp.snarl);
        }
    }
}
```

---

## Sub-Patch Preset Format (`.fxsp`)

### Structure

Sub-patch presets are simplified `.fxp` files containing only a single sub-patch node:

```json
{
  "version": 1,
  "nodes": [
    {
      "id": "preset-root-node-id",
      "module_id": "subpatch",
      "position": [0.0, 0.0],
      "params": {},
      "subpatch": {
        "display_name": "Easy Mode Preset",
        "pins_in": [
          {"name": "Source", "signal_type": "AutoMap"}
        ],
        "pins_out": [
          {"name": "Destination", "signal_type": "AutoMap"}
        ],
        "patch": {
          "version": 1,
          "nodes": [...],
          "wires": [...]
        }
      }
    }
  ],
  "wires": []
}
```

### Easy Mode Compatibility Check

When loading a `.fxsp` file, the UI checks if it's compatible with Easy mode:

```rust
fn is_easy_compatible_canvas(canvas: &Canvas) -> bool {
    use flexinput_core::SignalType;
    
    let mut subpatch_node: Option<&NodeData> = None;
    
    for (_, n) in canvas.snarl.nodes_ids_data() {
        match n.value.module_id.as_str() {
            "subpatch" => {
                if subpatch_node.is_some() { return false; }  // Only one allowed
                subpatch_node = Some(&n.value);
            }
            "device.source" | "device.sink" => {}  // Allowed
            _ => return false,  // No foreign nodes
        }
    }
    
    let Some(node) = subpatch_node else { return false; };
    let Some(sp) = node.subpatch.as_ref() else { return false; };
    
    // Must have AutoMap inlet AND outlet
    let has_in  = sp.pins_in.iter().any(|p| p.signal_type == SignalType::AutoMap);
    let has_out = sp.pins_out.iter().any(|p| p.signal_type == SignalType::AutoMap);
    
    // Must have non-empty layout (items) for center panel rendering
    has_in && has_out && !sp.items.is_empty()
}
```

### Factory Presets

Shipping presets are located in `app/assets/sub-patches/`:
- `default.fxp` - Basic pass-through mapping
- `mouse_mode.fxp` - Stick-to-mouse conversion
- `gyro_aim.fxp` - Gyro aiming assist

Users can save custom presets via File → Save Sub-Patch Preset.

---

## Response Curve Format (`.fxc`)

### Structure

Response curves are standalone files that can be loaded into any curve module:

```json
{
  "points": [
    [-1.0, -1.0],
    [-0.5, -0.8],
    [0.0, 0.0],
    [0.5, 0.6],
    [1.0, 1.0]
  ],
  "biases": [
    0.0,  // Between point 0 and 1
    0.0,  // Between point 1 and 2
    0.0,  // Between point 2 and 3
    0.0   // Between point 3 and 4
  ],
  "absolute": true,
  "in_min": -1.0,
  "in_max": 1.0,
  "out_min": -1.0,
  "out_max": 1.0
}
```

### Loading into Modules

Response curve modules accept `.fxc` file paths:

```rust
// In module.params:
"curve_file": "/path/to/preset.fxc"

// Engine loads and merges params:
let loaded = serde_json::from_str::<CurveFile>(&fs::read_to_string(path)?);
params.extend(loaded.points.into_iter().map(|p| (format!("point_{}", p.0), Value::Number(p.1))));
```

### Curve Evaluation

Curves are evaluated via `apply_curve()` in `crates/engine/src/eval/curves.rs`:

```rust
pub fn apply_curve(
    x: f32,
    points: &[[f32; 2]],
    biases: &[f32],
    absolute: bool,
    in_min: f32,
    in_max: f32,
    out_min: f32,
    out_max: f32,
    scale_t: f32,
) -> f32 {
    if points.is_empty() { return 0.0; }
    
    // Normalize input to [0, 1] range
    let t = (x - in_min) / (in_max - in_min);
    let t = t.clamp(0.0, 1.0);
    
    // Find enclosing segment
    let seg_idx = (t * (points.len() as f32 - 1.0)).floor() as usize;
    let seg_idx = seg_idx.min(points.len() - 2);
    
    // Interpolate within segment
    let seg_t = (t * (points.len() as f32 - 1.0) - seg_idx as f32).clamp(0.0, 1.0);
    let p0 = points[seg_idx];
    let p1 = points[seg_idx + 1];
    
    // Apply bias (cubic interpolation control)
    let bias = if biases.len() > seg_idx { biases[seg_idx] } else { 0.0 };
    let y = cubic_interpolate(p0[1], p1[1], bias, seg_t);
    
    // Scale output to range
    ((y - out_min) / (out_max - out_min)).clamp(0.0, 1.0) * scale_t + out_min
}
```

---

## Workspace Format (`workspace.json`)

### Structure

The workspace file stores multiple tabs and their settings:

```json
{
  "active_tab": 0,
  "tabs": [
    {
      "title": "Main Patch",
      "file_path": "/path/to/patch.fxp",
      "bound_exes": ["game.exe"],
      "snarl": { /* Full Snarl<NodeData> serialization */ },
      "easy_preset_path": null,
      "overlay": { /* OverlayLayout */ },
      "config": { /* OverlayLayout */ }
    },
    {
      "title": "Untitled 2",
      "file_path": null,
      "bound_exes": [],
      "snarl": { /* ... */ },
      "easy_preset_path": null,
      "overlay": {},
      "config": {}
    }
  ]
}
```

### Persistence Triggers

Workspace is saved:
- On application exit (`on_exit()`)
- When `keep_workspace` setting is enabled and tabs change
- During GPU recovery relaunch (before snapshot)

---

## Crash Recovery Format (`recovery.json`)

### Structure

Identical to workspace format but consumed exactly once:

```rust
// In settings.rs
pub fn save_recovery(ws: &Workspace) {
    let path = appdata_dir().join("recovery.json");
    serde_json::to_writer_pretty(&File::create(path).unwrap(), ws).unwrap();
}

pub fn load_recovery() -> Option<Workspace> {
    let path = appdata_dir().join("recovery.json");
    if path.exists() {
        let ws: Workspace = serde_json::from_reader(File::open(path).ok()?).ok()?;
        Some(ws)
    } else {
        None
    }
}

pub fn delete_recovery() {
    let path = appdata_dir().join("recovery.json");
    let _ = std::fs::remove_file(path);
}
```

### Recovery Flow

1. **GPU loss detected** → `save_recovery()` called
2. **App relaunches** → `load_recovery()` returns snapshot
3. **Tabs restored** from recovery data
4. **`delete_recovery()`** called after successful restore

---

## Serialization Details

### Serde Attributes

Key serde attributes used throughout:

```rust
#[derive(Serialize, Deserialize)]
pub struct NodeInstance {
    // Skip empty HashMaps to reduce file size
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub params: HashMap<String, Value>,
    
    // Skip None subpatch (only present for subpatch nodes)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subpatch: Option<Box<SubPatch>>,
}

#[derive(Serialize, Deserialize)]
pub struct SubPatch {
    // Required fields (no defaults)
    pub display_name: String,
    pub pins_in: Vec<SubPatchPin>,
    pub pins_out: Vec<SubPatchPin>,
    
    // Nested patch (always present for valid subpatch)
    pub patch: Patch,
}
```

### Version Field

All patch files include a `version` field for future compatibility:

```rust
pub const PATCH_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
pub struct Patch {
    pub version: u32,
    pub nodes: Vec<NodeInstance>,
    pub wires: Vec<Wire>,
}
```

**Migration strategy:**
- Increment `PATCH_VERSION` for breaking changes
- Check version on load, apply migration if needed
- Maintain backward compatibility for at least 2 versions

---

## File I/O Operations

### Save Patch

```rust
// In Canvas::save_patch():
pub fn save_patch(&mut self, vids: Vec<String>, bound: Vec<String>, ...) -> Option<PathBuf> {
    let path = rfd::FileDialog::new()
        .add_filter("FlexInput Patch", &["fxp"])
        .set_file_name("patch.fxp")
        .save_file()?;
    
    let ui_patch = UiPatch {
        version: PATCH_VERSION,
        snarl: self.snarl.clone(),
        virtual_device_ids: vids,
        bound_exes: bound,
        // ... other fields
    };
    
    let json = serde_json::to_string_pretty(&ui_patch).unwrap();
    fs::write(&path, json).ok()?;
    
    Some(path)
}
```

### Load Patch

```rust
// In Canvas::load_patch():
pub fn load_patch() -> Option<(Canvas, Vec<String>, ...)> {
    let path = rfd::FileDialog::new()
        .add_filter("FlexInput Patch", &["fxp"])
        .pick_file()?;
    
    let json = fs::read_to_string(&path).ok()?;
    let ui_patch: UiPatch = serde_json::from_str(&json).ok()?;
    
    // Migrate loaded snarl to current schema
    let mut canvas = Canvas::new();
    canvas.snarl = ui_patch.snarl;
    migrate_loaded_snarl(&mut canvas.snarl);
    
    Some((canvas, ui_patch.virtual_device_ids, ...))
}
```

---

## Common Issues & Troubleshooting

### 1. Missing Parameters After Load

**Symptom:** Module parameters reset to defaults after loading patch.

**Cause:** Parameter name changed between versions (e.g., `deadzone` → `stick_deadzone`).

**Fix:** Add migration in `migrate_loaded_snarl()` to rename old keys:
```rust
if let Some(old_val) = node.value.params.remove("deadzone") {
    node.value.params.insert("stick_deadzone".into(), old_val);
}
```

### 2. Sub-patch Node Missing After Load

**Symptom:** `subpatch` nodes appear as empty rectangles in canvas.

**Cause:** Inline sub-patch definition not serialized (only present when `module_id == "subpatch"`).

**Fix:** Ensure `subpatch` field is included during serialization:
```rust
#[serde(skip_serializing_if = "Option::is_none")]
pub subpatch: Option<Box<SubPatch>>,
```

### 3. Wire Connections Lost After Reload

**Symptom:** Nodes present but wires missing.

**Cause:** Pin names changed (e.g., `"A"` → `"Input A"`) without migration.

**Fix:** Map old pin names to new in `migrate_loaded_snarl()`:
```rust
for wire in &mut snarl.wires {
    if wire.from_pin == "A" { wire.from_pin = "Input A".into(); }
    if wire.to_pin == "B" { wire.to_pin = "Output B".into(); }
}
```

### 4. Large File Sizes

**Symptom:** `.fxp` files unexpectedly large (>1 MB).

**Cause:** `params` HashMap includes all default values (even when unchanged).

**Fix:** Use `skip_serializing_if` for optional fields:
```rust
#[serde(skip_serializing_if = "Option::is_none")]
pub deadzone: Option<f32>,  // Only serialize if non-None
```

### 5. Corrupted Recovery Snapshot

**Symptom:** App crashes on launch after GPU loss recovery.

**Cause:** `recovery.json` contains invalid JSON or schema mismatch.

**Fix:** Delete `%APPDATA%\FlexInput\recovery.json` and restart app normally.

---

## References

- Patch types: `crates/core/src/patch.rs`
- Migration: `crates/ui/src/canvas/mod.rs` (`migrate_loaded_snarl`)
- File I/O: `crates/ui/src/app/persistence.rs`
- Settings persistence: `crates/ui/src/settings.rs`
