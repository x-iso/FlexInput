# FlexInput

A node-based input routing application for Windows. Connect physical controllers and MIDI devices to virtual outputs through a visual signal graph — remap axes, apply response curves, mix and transform inputs, and route the result to virtual gamepads or keyboard/mouse.

## Features

### Devices
- **Physical inputs** — XInput, DualShock 4, DualSense, Switch Pro, generic HID gamepads, MIDI IN/OUT ports
- **Gyro / IMU** — raw gyroscope and accelerometer data from DualShock 4 and DualSense
- **Virtual outputs** — Virtual XInput controller, Virtual DualShock 4, Virtual Keyboard & Mouse (requires [ViGEmBus](https://github.com/nefarius/ViGEmBus))
- **HidHide integration** — hide physical devices from other applications while FlexInput reads them (requires [HidHide](https://github.com/nefarius/HidHide))
- **Bluetooth support** — gamepads connected over Bluetooth are detected alongside USB

### Signal graph
- Drag-and-drop node canvas — connect outputs to inputs with wires
- Right-click a wire to delete it or insert a processing node between two existing nodes
- Nodes persist on canvas when a device is disconnected; a status dot shows live (green) vs. disconnected (red)

### Processing modules
| Category | Modules |
|---|---|
| Math | Add, Multiply, Clamp, Abs, Scale, Lerp |
| Logic | Select, Switch |
| Filter | Lowpass, Delay |
| Mapping | Response Curve (with log/exp grid scale and animated input trails) |
| Display | Oscilloscope (auto-scale, bi/uni mode, adjustable window), Vectorscope |

### MIDI
- MIDI IN nodes with per-CC output pins — use CC Learn to map by wiggling a knob
- MIDI OUT nodes accept Float/Bool signals and send CC messages
- Pitch bend output pin on MIDI IN nodes

### Patch system
- Save and load `.fxp` patch files from **File → Save Patch / Load Patch**
- Patches store the full canvas graph, node positions, all parameters, and which virtual devices were active

## Requirements

- **Windows 10/11** (x64)
- [ViGEmBus](https://github.com/nefarius/ViGEmBus/releases) — optional, required for virtual gamepad output
- [HidHide](https://github.com/nefarius/HidHide/releases) — optional, required for hiding physical devices from other apps

## Installation

Download `FlexInput-vX.Y.Z-windows-x64.zip` from the [Releases](https://github.com/x-iso/FlexInput/releases) page, extract, and run `flexinput.exe`. No installer needed.

## Building from source

```
cargo build --release
```

Requires Rust stable (MSVC toolchain on Windows). The vendored `egui-snarl` patch is included; no extra setup needed.

## License

MIT — see [LICENSE](LICENSE).
