# FlexInput

A node-based HID/MIDI input routing and mapping application for Windows (Linux support is considered in near future). Connect physical controllers and MIDI devices to virtual outputs through a visual signal graph — remap axes, apply response curves, mix and transform inputs, and route the result to virtual gamepads or keyboard/mouse.

## Features

### Devices
- **Physical inputs** — XInput, DualShock 4, DualSense, Switch Pro, generic HID gamepads, MIDI IN/OUT ports
- **Gyro / IMU** — raw gyroscope and accelerometer data from DualShock 4 and DualSense
- **Virtual outputs** — Virtual XInput controller, Virtual DualShock 4, Virtual Keyboard & Mouse (requires [ViGEmBus](https://github.com/nefarius/ViGEmBus))
- **HidHide integration** — currently postponed, for hiding physical device from other apps you can use HIDHide driver with HIDHide configuration client for the time being. It's a manual approach, but it works.
- **Bluetooth support** — gamepads connected over Bluetooth are detected alongside USB

### Signal graph
- Drag-and-drop node canvas — connect outputs to inputs with wires
- Right-click a wire to delete it or insert a processing node between two existing nodes
- Nodes persist on canvas when a device is disconnected; a status dot shows live (green) vs. disconnected (red)

### Processing modules
| Category | Modules |
|---|---|
| Utility | Constant, Select, Split, Switch, Knob, Text, SVG, **Sub-patch** |
| Math | Add/Subtract, Multiply/Divide, Clamp, Abs, Scale, Negate |
| Logic | AND, OR, XOR, NOT, Equal, Not Equal, Greater than, Less than, Has changed, Logic Delay, Counter |
| Processing | Average, Delay, Response Curve/Vec Response Curve (with log/exp grid scale and animated input trails), DC Filter, 3DOF to 2D |
| Display | Readout, Oscilloscope (auto-scale, bi/uni mode, adjustable window), Vectorscope |
| Converters | Vec to Axis/Axis to Vec |
| Auto-Map | Auto-Map Splitter, Auto-Map Collelctor (modules for splicing and injecting signals along Auto-map routing |
| Generator | Oscillator |

### MIDI
- MIDI IN nodes with per-CC output pins — use CC Learn to map by wiggling a knob or actively changing target CC value
- MIDI OUT nodes accept Float/Bool signals and send CC messages

### Patch system
- Save and load `.fxp` patch files from **File → Save Patch / Load Patch**
- Save and load `.fxsp` sub-patch files inside Sub-patch editor, for re-using and sharing complex blocks with user-friendly UI.
- Patches store the full canvas graph, node positions, all parameters, and which virtual devices were active, as well as any loaded SVG graphics
- Auto mode for automatic tab-switching according to process in focus. Process binding for the patch is done through File menu and is saved with the patch. Option to bypass patch on focus loss is also present. 

## Requirements

- **Windows 10/11** (x64)
- [ViGEmBus](https://github.com/nefarius/ViGEmBus/releases) — optional, required for virtual DS4 output
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
