# FlexInput

A node-based HID/MIDI input routing and mapping application for Windows. Connect physical controllers, MIDI devices, and networked inputs to virtual outputs through a visual signal graph — remap axes, apply response curves, mix and transform inputs, and route the result to virtual gamepads, keyboard/mouse, or even another PC over the network.

## Features

### Devices

#### Physical Inputs
- **Gamepads** — XInput, DualShock 4, DualSense, Switch Pro via gilrs (tuned paths)
- **SDL3 support** — Third-party controllers with special features (gyro, extra buttons) - filtered to avoid duplicates with gilrs
- **MIDI IN/OUT** — Per-CC output pins with CC Learn for easy mapping

#### Virtual Outputs
- **HIDMaestro XInput** — Virtual Xbox 360/XInput controller (pure-Rust UMDF2 client)
- **HIDMaestro DS4/DualSense** — Virtual DualShock 4/DualSense via HIDMaestro
- **Virtual Keyboard & Mouse** — HIDMaestro-based with enigo fallback

#### Advanced Features
- **Gyro / IMU** — Raw gyroscope and accelerometer data from DualShock 4, DualSense, Switch Pro
- **HidHide integration** — Hide remapped physical devices from applications (requires separate driver installation; [HidHide repo](https://github.com/nefarius/HidHide))
- **Physical-mouse suppression** — Configurable settings (50–2000 ms window)
- **Braided mixed input** — Experimental option to pace gamepad and KB/M packets in strict alternation, never landing at the same instant (improves reliability in games like Division 2)

### Network Input Routing (v0.11.0)

Carry the AutoMap gamepad bus between two FlexInput instances over the network:

| Transport | Description | Encryption | Use Case |
|-----------|-------------|------------|----------|
| **LAN (UDP)** | Plaintext, IP + port only | None | Same LAN, trusted networks |
| **Secure (PSK)** | UDP with ChaCha20-Poly1305 | Shared passphrase | Internet, untrusted networks |
| **P2P (iroh)** | Dial-by-code over iroh | Cryptographic keypair | NAT/CGNAT/VPN environments |

**Features:**
- Bidirectional haptics — rumble, light bar, adaptive triggers travel back to physical pad
- Fail-safe — neutral frame on connection loss (no stuck inputs)
- Configurable staleness window (default 200 ms)
- Keep-saved toggle for P2P codes (optional persistence)

### Signal Graph

- **Drag-and-drop canvas** — egui-snarl based visual node editing with Snarl graph UI
- **Wire operations** — Right-click to delete or insert processing nodes between existing nodes
- **Node status** — Live (green) vs. disconnected (red) dots for device nodes
- **Sub-patches** — Nested graphs with declared I/O pins for reusable complex blocks
  - **UI building tools** — Pinnable widgets on sub-patch body for customizable presets; each module provides its own pinnable controls (knobs, sliders, dropdowns)
  - **Easy mode presets** — Pre-made factory sub-patches for simplified usage without deep technical knowledge

### Processing Modules

| Category | Modules |
|----------|---------|
| **Utility** | Constant, Switch, Knob, Selector, Dropdown, Text, SVG, Split, Sub-patch |
| **Math** | Add, Subtract, Multiply, Divide, Clamp, Abs, Inverse, Min/Max, Quantize, Map Range |
| **Logic** | AND, OR, NOT, XOR, Equal, NotEqual, GreaterThan, LessThan, Has Changed, Logic Delay, Counter |
| **Processing** | Delay, Average, DC Filter, Response Curve, Vec Response Curve, Vec Reshaper, Two-way Response Curve, Vec→Axis, Axis→Vec, Vec→Deflection, Gyro 3DOF |
| **Display** | Readout, Oscilloscope, Trigger Scope, Vectorscope |
| **Generator** | Oscillator, Envelope |
| **AutoMap** | AutoMap Splitter, AutoMap Collector, AutoMap Fork, AutoMap Selector, AutoMap Combiner, Touch Zones, Remapper, Map Action, Feedback Control, Audio Stream Haptics |
| **SubPatch** | Inlet, Outlet |
| **Network** | Network Send, Network Receive |

### New in v0.11.0

- **Vec Reshaper** — Directional reshaping of Vec2 with visual editor (boundary/gain per direction)
- **Audio Stream Haptics** — WASAPI loopback → rumble routing without driver installation
- **Network Send/Receive** — Multi-tier network transport (LAN, PSK, P2P) with bidirectional haptics

### Patch System

- **Save/load patches** — `.fxp` files via File menu or canvas context menu
- **Response curve files** — Save/load response curves as separate `.fxc` files for reuse across patches
- **Sub-patch files** — `.fxsp` for reusable complex blocks with user-friendly UI
- **Easy/Advanced modes** — Easy mode uses sub-patch presets (factory preset included) for simplified usage; optional gamepad navigation of UI
- **Full state persistence** — Canvas graph, node positions, all parameters, active virtual devices, SVG graphics
- **Auto mode** — Automatic tab-switching based on foreground process; bypass option on focus loss

## Requirements

- **Windows 10/11** (x64)
- **HIDMaestro driver** — Built into the app via self-re-exec (no separate download needed for virtual devices)

### Optional Dependencies
- **ViGEmBus** — Legacy support, not required (HIDMaestro is now the default)
- **HidHide** — For hiding physical devices from applications ([HidHide repo](https://github.com/nefarius/HidHide))

## Installation

Download `FlexInput-vX.Y.Z-windows-x64.zip` from the [Releases](https://github.com/x-iso/FlexInput/releases) page, extract, and run `flexinput.exe`. No installer needed.

## Building from Source

```bash
cargo build --release
```

Requires:
- **Rust stable** (MSVC toolchain on Windows)
- **C compiler** and **cmake** — for building SDL3 from source (static linking, no DLL to ship)

The vendored patches (`egui-snarl`, `egui-wgpu`) are included; no extra setup needed.

### Features

- **p2p** — Enables P2P transport tier using iroh for NAT traversal (default: enabled)
- **probe-bin** — Builds `hm_shm_probe` binary for Phase-1 verification
- **helper-bin** — Builds elevated helper binary for driver installation

## Architecture

FlexInput uses a three-thread architecture for real-time signal routing:

| Thread | Role | Frequency |
|--------|------|-----------|
| **UI** | egui event loop, canvas rendering | Monitor refresh rate when in focus; optional throttle when backgrounded to save resources |
| **Processing** | Signal graph evaluation | Variable (configurable max polling rate) |
| **I/O** | Device polling, output dispatch | Variable (configurable max polling rate) |

State is shared via `Arc<RwLock<T>>` and `Arc<Mutex<T>>` with careful lock hierarchy to avoid contention. The processing thread uses catchup ticks to recover from UI frame hiccups.

### Configuration
- **Polling rate** — Configurable maximum polling rate for device I/O (typically 500 Hz - 4 kHz)
- **Processing rate** — Configurable signal evaluation rate (tuned for real-time response)

## Credits

- 3D controller models in `app/assets/models` are adapted from
  [larfingshnew/3d-controller-overlay](https://github.com/larfingshnew/3d-controller-overlay)
  (MIT). That repo is also the reference for the model format when adding
  custom controllers — see `app/assets/models/README.md`.
- Macro & menu icons in `app/assets/general` are from
  [game-icons.net](https://game-icons.net/) by their respective authors,
  licensed under [CC BY 3.0](https://creativecommons.org/licenses/by/3.0/).
  Each icon's individual author is credited on game-icons.net (and in the
  metadata of the downloaded pack); see
  [`app/assets/general/ATTRIBUTION.md`](app/assets/general/ATTRIBUTION.md)
  for the per-author list.
- Input-prompt SVG icons are by [Kenney](https://kenney.nl/assets/input-prompts)
  (CC0).

## License

MIT — see [LICENSE](LICENSE).